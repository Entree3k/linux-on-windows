use std::{
	ffi::{OsStr, OsString},
	mem::take,
	path::PathBuf,
};

use clap::{
	builder::TypedValueParser,
	error::{Error, ErrorKind},
	Parser, ValueEnum, ValueHint,
};
use miette::{IntoDiagnostic, Result};
use tracing::{info, warn};
use watchexec_signals::Signal;

use crate::socket::{SocketSpec, SocketSpecValueParser};

use super::{TimeSpan, OPTSET_COMMAND};

#[derive(Debug, Clone, Parser)]
pub struct CommandArgs {
	/// Use a different shell to run the command
	///
	/// Defaults to the running shell (pwsh/cmd on Windows, $SHELL on Unix).
	/// Use 'none' to run the command directly without a shell (more efficient, no glob expansion).
	///
	///   watchexec --shell=cmd -- dir
	///   watchexec --shell=pwsh -- Get-Process
	///   watchexec --shell=none -- cargo build
	#[arg(
		long,
		help_heading = OPTSET_COMMAND,
		value_name = "SHELL",
		display_order = 190,
	)]
	pub shell: Option<String>,

	/// Shorthand for '--shell=none'
	#[arg(
		short = 'n',
		help_heading = OPTSET_COMMAND,
		display_order = 140,
	)]
	pub no_shell: bool,

	/// Deprecated shorthand for '--emit-events=none'
	///
	/// This is the old way to disable event emission into the environment. See '--emit-events' for
	/// more. Will be removed at next major release.
	#[arg(
		long,
		help_heading = OPTSET_COMMAND,
		hide = true, // deprecated
	)]
	pub no_environment: bool,

	/// Add env vars to the command
	///
	/// This is a convenience option for setting environment variables for the command, without
	/// setting them for the Watchexec process itself.
	///
	/// Use key=value syntax. Multiple variables can be set by repeating the option.
	#[arg(
		long,
		short = 'E',
		help_heading = OPTSET_COMMAND,
		value_name = "KEY=VALUE",
		value_parser = EnvVarValueParser,
		display_order = 50,
	)]
	pub env: Vec<EnvVar>,

	/// Don't use a process group
	///
	/// By default, Watchexec will run the command in a process group, so that signals and
	/// terminations are sent to all processes in the group. Sometimes that's not what you want, and
	/// you can disable the behaviour with this option.
	///
	/// Deprecated, use '--wrap-process=none' instead.
	#[arg(
		long,
		help_heading = OPTSET_COMMAND,
		display_order = 141,
	)]
	pub no_process_group: bool,

	/// Configure how the process is wrapped
	///
	/// Controls whether the command runs in a process group, session, or directly.
	/// On Windows, 'group' and 'session' both use a Job Object. Use 'none' to run without wrapping.
	#[arg(
		long,
		help_heading = OPTSET_COMMAND,
		value_name = "MODE",
		default_value = WRAP_DEFAULT,
		display_order = 231,
	)]
	pub wrap_process: WrapMode,

	/// Signal to send to stop the command (default: SIGTERM on Unix, KILL on Windows)
	///
	/// Used by 'restart' and 'signal' modes of '--on-busy-update'.
	/// Accepts full names (SIGTERM), short names (TERM), or numbers (15).
	#[arg(
		long,
		help_heading = OPTSET_COMMAND,
		value_name = "SIGNAL",
		display_order = 191,
	)]
	pub stop_signal: Option<Signal>,

	/// Time to wait for the command to exit gracefully
	///
	/// This is used by the 'restart' mode of '--on-busy-update'. After the graceful stop signal
	/// is sent, Watchexec will wait for the command to exit. If it hasn't exited after this time,
	/// it is forcefully terminated.
	///
	/// Takes a unit-less value in seconds, or a time span value such as "5min 20s".
	/// Providing a unit-less value is deprecated and will warn; it will be an error in the future.
	///
	/// The default is 10 seconds. Set to 0 to immediately force-kill the command.
	///
	/// This has no practical effect on Windows as the command is always forcefully terminated; see
	/// '--stop-signal' for why.
	#[arg(
		long,
		help_heading = OPTSET_COMMAND,
		default_value = "10s",
		hide_default_value = true,
		value_name = "TIMEOUT",
		display_order = 192,
	)]
	pub stop_timeout: TimeSpan,

	/// Kill the command if it runs longer than this duration
	///
	/// Takes a time span value such as "30s", "5min", or "1h 30m".
	///
	/// When the timeout is reached, the command is gracefully stopped using --stop-signal, then
	/// forcefully terminated after --stop-timeout if still running.
	///
	/// Each run of the command has its own independent timeout.
	#[arg(
		long,
		help_heading = OPTSET_COMMAND,
		value_name = "TIMEOUT",
		display_order = 193,
	)]
	pub timeout: Option<TimeSpan>,

	/// Sleep before running the command
	///
	/// This option will cause Watchexec to sleep for the specified amount of time before running
	/// the command, after an event is detected. This is like using "sleep 5 && command" in a shell,
	/// but portable and slightly more efficient.
	///
	/// Takes a unit-less value in seconds, or a time span value such as "2min 5s".
	/// Providing a unit-less value is deprecated and will warn; it will be an error in the future.
	#[arg(
		long,
		help_heading = OPTSET_COMMAND,
		value_name = "DURATION",
		display_order = 40,
	)]
	pub delay_run: Option<TimeSpan>,

	/// Set the working directory
	///
	/// By default, the working directory of the command is the working directory of Watchexec. You
	/// can change that with this option. Note that paths may be less intuitive to use with this.
	#[arg(
		long,
		help_heading = OPTSET_COMMAND,
		value_hint = ValueHint::DirPath,
		value_name = "DIRECTORY",
		display_order = 230,
	)]
	pub workdir: Option<PathBuf>,

	/// Pass an open socket to the command (systemd socket-activation protocol)
	///
	/// Value: PORT, HOST:PORT, or TYPE::PORT (tcp/udp). Can be repeated for multiple sockets.
	/// Keeps sockets open across restarts so connections aren't dropped.
	#[arg(
		long,
		help_heading = OPTSET_COMMAND,
		value_name = "PORT",
		value_parser = SocketSpecValueParser,
		display_order = 60,
	)]
	pub socket: Vec<SocketSpec>,
}

impl CommandArgs {
	pub(crate) async fn normalise(&mut self) -> Result<()> {
		if self.no_process_group {
			warn!("--no-process-group is deprecated");
			self.wrap_process = WrapMode::None;
		}

		let workdir = if let Some(w) = take(&mut self.workdir) {
			w
		} else {
			let curdir = std::env::current_dir().into_diagnostic()?;
			dunce::canonicalize(curdir).into_diagnostic()?
		};
		info!(path=?workdir, "effective working directory");
		self.workdir = Some(workdir);

		debug_assert!(self.workdir.is_some());
		Ok(())
	}
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum WrapMode {
	#[default]
	Group,
	Session,
	None,
}

pub const WRAP_DEFAULT: &str = if cfg!(target_os = "macos") {
	"session"
} else {
	"group"
};

#[derive(Clone, Debug)]
pub struct EnvVar {
	pub key: String,
	pub value: OsString,
}

#[derive(Clone)]
pub(crate) struct EnvVarValueParser;

impl TypedValueParser for EnvVarValueParser {
	type Value = EnvVar;

	fn parse_ref(
		&self,
		_cmd: &clap::Command,
		_arg: Option<&clap::Arg>,
		value: &OsStr,
	) -> Result<Self::Value, Error> {
		let value = value
			.to_str()
			.ok_or_else(|| Error::raw(ErrorKind::ValueValidation, "invalid UTF-8"))?;

		let (key, value) = value
			.split_once('=')
			.ok_or_else(|| Error::raw(ErrorKind::ValueValidation, "missing = separator"))?;

		Ok(EnvVar {
			key: key.into(),
			value: value.into(),
		})
	}
}
