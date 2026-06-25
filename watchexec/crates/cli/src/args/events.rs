use std::{ffi::OsStr, path::PathBuf};

use clap::{
	builder::TypedValueParser, error::ErrorKind, Arg, Command, CommandFactory, Parser, ValueEnum,
};
use miette::Result;

use tracing::warn;
use watchexec_signals::Signal;

use super::{command::CommandArgs, filtering::FilteringArgs, TimeSpan, OPTSET_EVENTS};

#[derive(Debug, Clone, Parser)]
pub struct EventsArgs {
	/// What to do when receiving events while the command is running
	///
	/// Default is to 'do-nothing', which ignores events while the command is running, so that
	/// changes that occur due to the command are ignored, like compilation outputs. You can also
	/// use 'queue' which will run the command once again when the current run has finished if any
	/// events occur while it's running, or 'restart', which terminates the running command and starts
	/// a new one. Finally, there's 'signal', which only sends a signal; this can be useful with
	/// programs that can reload their configuration without a full restart.
	///
	/// The signal can be specified with the '--signal' option.
	#[arg(
		short,
		long,
		help_heading = OPTSET_EVENTS,
		default_value = "do-nothing",
		hide_default_value = true,
		value_name = "MODE",
		display_order = 150,
	)]
	pub on_busy_update: OnBusyUpdate,

	/// Restart the process if it's still running
	///
	/// This is a shorthand for '--on-busy-update=restart'.
	#[arg(
		short,
		long,
		help_heading = OPTSET_EVENTS,
		conflicts_with_all = ["on_busy_update"],
		display_order = 180,
	)]
	pub restart: bool,

	/// Send a signal to the process when it's still running
	///
	/// Specify a signal to send to the process when it's still running. This implies
	/// '--on-busy-update=signal'; otherwise the signal used when that mode is 'restart' is
	/// controlled by '--stop-signal'.
	///
	/// See the long documentation for '--stop-signal' for syntax.
	///
	/// Signals are not supported on Windows at the moment, and will always be overridden to 'kill'.
	/// See '--stop-signal' for more on Windows "signals".
	#[arg(
		short,
		long,
		help_heading = OPTSET_EVENTS,
		conflicts_with_all = ["restart"],
		value_name = "SIGNAL",
		display_order = 190,
	)]
	pub signal: Option<Signal>,

	/// Remap a signal: FROM:TO (e.g. TERM:INT forwards SIGTERM as SIGINT to the command)
	///
	/// Omit the TO to discard the signal (e.g. TERM: ignores SIGTERM).
	/// Can be repeated. Mapping SIGINT or SIGTERM prevents them from quitting watchexec.
	#[arg(
		long = "map-signal",
		help_heading = OPTSET_EVENTS,
		value_name = "SIGNAL:SIGNAL",
		value_parser = SignalMappingValueParser,
		display_order = 130,
	)]
	pub signal_map: Vec<SignalMapping>,

	/// Time to wait for new events before taking action
	///
	/// When an event is received, Watchexec will wait for up to this amount of time before handling
	/// it (such as running the command). This is essential as what you might perceive as a single
	/// change may actually emit many events, and without this behaviour, Watchexec would run much
	/// too often. Additionally, it's not infrequent that file writes are not atomic, and each write
	/// may emit an event, so this is a good way to avoid running a command while a file is
	/// partially written.
	///
	/// An alternative use is to set a high value (like "30min" or longer), to save power or
	/// bandwidth on intensive tasks, like an ad-hoc backup script. In those use cases, note that
	/// every accumulated event will build up in memory.
	///
	/// Takes a unit-less value in milliseconds, or a time span value such as "5sec 20ms".
	/// Providing a unit-less value is deprecated and will warn; it will be an error in the future.
	///
	/// The default is 50 milliseconds. Setting to 0 is highly discouraged.
	#[arg(
		long,
		short,
		help_heading = OPTSET_EVENTS,
		default_value = "50ms",
		hide_default_value = true,
		value_name = "TIMEOUT",
		display_order = 40,
	)]
	pub debounce: TimeSpan<1_000_000>,

	/// Exit when stdin closes
	///
	/// This watches the stdin file descriptor for EOF, and exits Watchexec gracefully when it is
	/// closed. This is used by some process managers to avoid leaving zombie processes around.
	#[arg(
		long,
		help_heading = OPTSET_EVENTS,
		display_order = 191,
	)]
	pub stdin_quit: bool,

	/// Respond to keypresses to quit, restart, or pause
	///
	/// In interactive mode, Watchexec listens for keypresses and responds to them. Currently
	/// supported keys are: 'r' to restart the command, 'p' to toggle pausing the watch, and 'q'
	/// to quit. This requires a terminal (TTY) and puts stdin into raw mode, so the child process
	/// will not receive stdin input.
	#[arg(
		long,
		short = 'I',
		help_heading = OPTSET_EVENTS,
		display_order = 90,
	)]
	pub interactive: bool,

	/// Exit when the command has an error
	///
	/// By default, Watchexec will continue to watch and re-run the command after the command
	/// exits, regardless of its exit status. With this option, it will instead exit when the
	/// command completes with any non-success exit status.
	///
	/// This is useful when running Watchexec in a process manager or container, where you want
	/// the container to restart when the command fails rather than hang waiting for file changes.
	#[arg(
		long,
		help_heading = OPTSET_EVENTS,
		display_order = 91,
	)]
	pub exit_on_error: bool,

	/// Wait until first change before running command
	///
	/// By default, Watchexec will run the command once immediately. With this option, it will
	/// instead wait until an event is detected before running the command as normal.
	#[arg(
		long,
		short,
		help_heading = OPTSET_EVENTS,
		display_order = 161,
	)]
	pub postpone: bool,

	/// Poll for filesystem changes
	///
	/// By default, and where available, Watchexec uses the operating system's native file system
	/// watching capabilities. This option disables that and instead uses a polling mechanism, which
	/// is less efficient but can work around issues with some file systems (like network shares) or
	/// edge cases.
	///
	/// Optionally takes a unit-less value in milliseconds, or a time span value such as "2s 500ms",
	/// to use as the polling interval. If not specified, the default is 30 seconds.
	/// Providing a unit-less value is deprecated and will warn; it will be an error in the future.
	///
	/// Aliased as '--force-poll'.
	#[arg(
		long,
		help_heading = OPTSET_EVENTS,
		alias = "force-poll",
		num_args = 0..=1,
		default_missing_value = "30s",
		value_name = "INTERVAL",
		display_order = 160,
	)]
	pub poll: Option<TimeSpan<1_000_000>>,

	/// Send changed-file info to the command
	///
	/// Controls how watchexec passes event data to the command. Default is 'none'.
	///
	///   stdio      — write changed paths to the command's stdin, one per line (create:/remove:/etc.)
	///   file       — same as stdio but via $WATCHEXEC_EVENTS_FILE
	///   json-stdio — write JSON event objects to stdin
	///   json-file  — same as json-stdio but via $WATCHEXEC_EVENTS_FILE
	///   environment — (deprecated) set $WATCHEXEC_*_PATH env vars
	#[arg(
		long,
		help_heading = OPTSET_EVENTS,
		verbatim_doc_comment,
		default_value = "none",
		hide_default_value = true,
		value_name = "MODE",
		display_order = 50,
	)]
	pub emit_events_to: EmitEvents,
}

impl EventsArgs {
	pub(crate) fn normalise(
		&mut self,
		command: &CommandArgs,
		filtering: &FilteringArgs,
		only_emit_events: bool,
	) -> Result<()> {
		if self.signal.is_some() {
			self.on_busy_update = OnBusyUpdate::Signal;
		} else if self.restart {
			self.on_busy_update = OnBusyUpdate::Restart;
		}

		if command.no_environment {
			warn!("--no-environment is deprecated");
			self.emit_events_to = EmitEvents::None;
		}

		if only_emit_events
			&& !matches!(
				self.emit_events_to,
				EmitEvents::JsonStdio | EmitEvents::Stdio
			) {
			self.emit_events_to = EmitEvents::JsonStdio;
		}

		if self.stdin_quit && filtering.watch_file == Some(PathBuf::from("-")) {
			super::Args::command()
				.error(
					ErrorKind::InvalidValue,
					"stdin-quit cannot be used when --watch-file=-",
				)
				.exit();
		}

		if self.interactive && filtering.watch_file == Some(PathBuf::from("-")) {
			super::Args::command()
				.error(
					ErrorKind::InvalidValue,
					"interactive mode cannot be used when --watch-file=-",
				)
				.exit();
		}

		Ok(())
	}
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum EmitEvents {
	#[default]
	Environment,
	Stdio,
	File,
	JsonStdio,
	JsonFile,
	None,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum OnBusyUpdate {
	#[default]
	Queue,
	DoNothing,
	Restart,
	Signal,
}

#[derive(Clone, Copy, Debug)]
pub struct SignalMapping {
	pub from: Signal,
	pub to: Option<Signal>,
}

#[derive(Clone)]
struct SignalMappingValueParser;

impl TypedValueParser for SignalMappingValueParser {
	type Value = SignalMapping;

	fn parse_ref(
		&self,
		_cmd: &Command,
		_arg: Option<&Arg>,
		value: &OsStr,
	) -> Result<Self::Value, clap::error::Error> {
		let value = value
			.to_str()
			.ok_or_else(|| clap::error::Error::raw(ErrorKind::ValueValidation, "invalid UTF-8"))?;
		let (from, to) = value
			.split_once(':')
			.ok_or_else(|| clap::error::Error::raw(ErrorKind::ValueValidation, "missing ':'"))?;

		let from = from
			.parse::<Signal>()
			.map_err(|sigparse| clap::error::Error::raw(ErrorKind::ValueValidation, sigparse))?;
		let to = if to.is_empty() {
			None
		} else {
			Some(to.parse::<Signal>().map_err(|sigparse| {
				clap::error::Error::raw(ErrorKind::ValueValidation, sigparse)
			})?)
		};

		Ok(Self::Value { from, to })
	}
}
