use std::{
	ffi::{OsStr, OsString},
	str::FromStr,
	time::Duration,
};

use clap::{Parser, ValueEnum, ValueHint};
use miette::Result;
use tracing::{debug, info, warn};
use tracing_appender::non_blocking::WorkerGuard;

pub(crate) mod command;
pub(crate) mod events;
pub(crate) mod filtering;
pub(crate) mod logging;
pub(crate) mod output;

const OPTSET_COMMAND: &str = "Command";
const OPTSET_DEBUGGING: &str = "Debugging";
const OPTSET_EVENTS: &str = "Events";
const OPTSET_FILTERING: &str = "Filtering";
const OPTSET_OUTPUT: &str = "Output";

include!(env!("BOSION_PATH"));

/// Re-run a command automatically when files change.
///
/// Watches the current directory (recursively) and reruns the given command whenever a file
/// change is detected. The command runs once at startup, then again on each change.
///
/// Examples:
///
///   watchexec cargo build                   # rebuild on any change
///   watchexec -e rs,toml cargo test         # only watch .rs and .toml files
///   watchexec -c -r node server.js          # clear screen, restart server on change
///   watchexec -w src -- npm run build       # watch src/ only
///   watchexec -p cargo test                 # wait for first change before running
///   watchexec -e py pytest                  # run pytest when .py files change
#[derive(Debug, Clone, Parser)]
#[command(
	name = "watchexec",
	bin_name = "watchexec",
	author,
	version,
	long_version = Bosion::LONG_VERSION,
	after_help = "Tip: use -h for a short summary, --help for full details.",
	after_long_help = "Tip: @argfile loads flags from a file (one per line). Use -h for a shorter view.",
	hide_possible_values = true,
)]
pub struct Args {
	/// Command to run when files change (runs once at startup too, unless --postpone)
	///
	/// If the command has flags of its own, separate it with --:
	///   watchexec -w src -- rsync -a src dest
	#[arg(
		trailing_var_arg = true,
		num_args = 1..,
		value_hint = ValueHint::CommandString,
		value_name = "COMMAND",
		required_unless_present_any = ["completions", "manual", "only_emit_events"],
	)]
	pub program: Vec<String>,

	/// Show the manual page
	///
	/// This shows the manual page for Watchexec, if the output is a terminal and the 'man' program
	/// is available. If not, the manual page is printed to stdout in ROFF format (suitable for
	/// writing to a watchexec.1 file).
	#[arg(
		long,
		conflicts_with_all = ["program", "completions", "only_emit_events"],
		display_order = 130,
	)]
	pub manual: bool,

	/// Generate a shell completions script
	///
	/// Provides a completions script or configuration for the given shell. If Watchexec is not
	/// distributed with pre-generated completions, you can use this to generate them yourself.
	///
	/// Supported shells: bash, elvish, fish, nu, powershell, zsh.
	#[arg(
		long,
		value_name = "SHELL",
		conflicts_with_all = ["program", "manual", "only_emit_events"],
		display_order = 30,
	)]
	pub completions: Option<ShellCompletion>,

	/// Only emit events to stdout, run no commands.
	///
	/// This is a convenience option for using Watchexec as a file watcher, without running any
	/// commands. It is almost equivalent to using `cat` as the command, except that it will not
	/// spawn a new process for each event.
	///
	/// This option implies `--emit-events-to=json-stdio`; you may also use the text mode by
	/// specifying `--emit-events-to=stdio`.
	#[arg(
		long,
		conflicts_with_all = ["program", "completions", "manual"],
		display_order = 150,
	)]
	pub only_emit_events: bool,

	/// Testing only: exit Watchexec after the first run and return the command's exit code
	#[arg(short = '1', hide = true)]
	pub once: bool,

	#[command(flatten)]
	pub command: command::CommandArgs,

	#[command(flatten)]
	pub events: events::EventsArgs,

	#[command(flatten)]
	pub filtering: filtering::FilteringArgs,

	#[command(flatten)]
	pub logging: logging::LoggingArgs,

	#[command(flatten)]
	pub output: output::OutputArgs,
}

#[derive(Clone, Copy, Debug)]
pub struct TimeSpan<const UNITLESS_NANOS_MULTIPLIER: u64 = { 1_000_000_000 }>(pub Duration);

impl<const UNITLESS_NANOS_MULTIPLIER: u64> FromStr for TimeSpan<UNITLESS_NANOS_MULTIPLIER> {
	type Err = humantime::DurationError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		s.parse::<u64>()
			.map_or_else(
				|_| humantime::parse_duration(s),
				|unitless| {
					if unitless != 0 {
						eprintln!("Warning: unitless non-zero time span values are deprecated and will be removed in an upcoming version");
					}
					Ok(Duration::from_nanos(unitless * UNITLESS_NANOS_MULTIPLIER))
				},
			)
			.map(TimeSpan)
	}
}

fn expand_args_up_to_doubledash() -> Result<Vec<OsString>, std::io::Error> {
	use argfile::Argument;
	use std::collections::VecDeque;

	let args = std::env::args_os();
	let mut expanded_args = Vec::with_capacity(args.size_hint().0);

	let mut todo: VecDeque<_> = args.map(|a| Argument::parse(a, argfile::PREFIX)).collect();
	while let Some(next) = todo.pop_front() {
		match next {
			Argument::PassThrough(arg) => {
				expanded_args.push(arg.clone());
				if arg == "--" {
					break;
				}
			}
			Argument::Path(path) => {
				let content = std::fs::read_to_string(path)?;
				let new_args = argfile::parse_fromfile(&content, argfile::PREFIX);
				todo.reserve(new_args.len());
				for (i, arg) in new_args.into_iter().enumerate() {
					todo.insert(i, arg);
				}
			}
		}
	}

	while let Some(next) = todo.pop_front() {
		expanded_args.push(match next {
			Argument::PassThrough(arg) => arg,
			Argument::Path(path) => {
				let path = path.as_os_str();
				let mut restored = OsString::with_capacity(path.len() + 1);
				restored.push(OsStr::new("@"));
				restored.push(path);
				restored
			}
		});
	}
	Ok(expanded_args)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ShellCompletion {
	Bash,
	Elvish,
	Fish,
	Nu,
	Powershell,
	Zsh,
}

#[derive(Debug, Default)]
pub struct Guards {
	_log: Option<WorkerGuard>,
}

pub async fn get_args() -> Result<(Args, Guards)> {
	let prearg_logs = logging::preargs();
	if prearg_logs {
		warn!(
			"⚠ WATCHEXEC_LOG environment variable set or hardcoded, logging options have no effect"
		);
	}

	debug!("expanding @argfile arguments if any");
	let args = expand_args_up_to_doubledash().expect("while expanding @argfile");

	debug!("parsing arguments");
	let mut args = Args::parse_from(args);

	let _log = if !prearg_logs {
		logging::postargs(&args.logging).await?
	} else {
		None
	};

	args.output.normalise()?;
	args.command.normalise().await?;
	args.filtering.normalise(&args.command).await?;
	args.events
		.normalise(&args.command, &args.filtering, args.only_emit_events)?;

	info!(?args, "got arguments");
	Ok((args, Guards { _log }))
}

#[test]
fn verify_cli() {
	use clap::CommandFactory;
	Args::command().debug_assert()
}
