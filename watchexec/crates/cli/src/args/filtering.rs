use std::{
	collections::BTreeSet,
	mem::take,
	path::{Path, PathBuf},
};

use clap::{Parser, ValueEnum, ValueHint};
use miette::{IntoDiagnostic, Result};
use tokio::{
	fs::File,
	io::{AsyncBufReadExt, BufReader},
};
use tracing::{debug, info};
use watchexec::{paths::PATH_SEPARATOR, WatchedPath};

use crate::filterer::parse::FilterProgram;

use super::{command::CommandArgs, OPTSET_FILTERING};

#[derive(Debug, Clone, Parser)]
pub struct FilteringArgs {
	#[doc(hidden)]
	#[arg(skip)]
	pub paths: Vec<WatchedPath>,

	/// Watch a specific file or directory (default: current directory)
	///
	/// Can be repeated to watch multiple paths. Tip: watching a directory is more reliable than
	/// watching a single file directly.
	#[arg(
		short = 'w',
		long = "watch",
		help_heading = OPTSET_FILTERING,
		value_hint = ValueHint::AnyPath,
		value_name = "PATH",
		display_order = 230,
	)]
	pub recursive_paths: Vec<PathBuf>,

	/// Watch a specific directory, non-recursively
	///
	/// Unlike '-w', folders watched with this option are not recursed into.
	///
	/// This option can be specified multiple times to watch multiple directories non-recursively.
	#[arg(
		short = 'W',
		long = "watch-non-recursive",
		help_heading = OPTSET_FILTERING,
		value_hint = ValueHint::AnyPath,
		value_name = "PATH",
		display_order = 231,
	)]
	pub non_recursive_paths: Vec<PathBuf>,

	/// Watch files and directories from a file
	///
	/// Each line in the file will be interpreted as if given to '-w'.
	///
	/// For more complex uses (like watching non-recursively), use the argfile capability: build a
	/// file containing command-line options and pass it to watchexec with `@path/to/argfile`.
	///
	/// The special value '-' will read from STDIN; this in incompatible with '--stdin-quit'.
	#[arg(
		short = 'F',
		long,
		help_heading = OPTSET_FILTERING,
		value_hint = ValueHint::AnyPath,
		value_name = "PATH",
		display_order = 232,
	)]
	pub watch_file: Option<PathBuf>,

	/// Don't load gitignores
	///
	/// Among other VCS exclude files, like for Mercurial, Subversion, Bazaar, DARCS, Fossil. Note
	/// that Watchexec will detect which of these is in use, if any, and only load the relevant
	/// files. Both global (like '~/.gitignore') and local (like '.gitignore') files are considered.
	///
	/// This option is useful if you want to watch files that are ignored by Git.
	#[arg(
		long,
		help_heading = OPTSET_FILTERING,
		display_order = 145,
	)]
	pub no_vcs_ignore: bool,

	/// Don't load project-local ignore files (.gitignore, .ignore, etc.)
	///
	/// Useful when you want to watch files that your VCS ignores.
	#[arg(
		long,
		help_heading = OPTSET_FILTERING,
		verbatim_doc_comment,
		display_order = 144,
	)]
	pub no_project_ignore: bool,

	/// Don't load global/user ignore files (~/.gitignore, %APPDATA%/watchexec/ignore, etc.)
	#[arg(
		long,
		help_heading = OPTSET_FILTERING,
		verbatim_doc_comment,
		display_order = 142,
	)]
	pub no_global_ignore: bool,

	/// Don't use internal default ignores
	///
	/// Watchexec has a set of default ignore patterns, such as editor swap files, `*.pyc`, `*.pyo`,
	/// `.DS_Store`, `.bzr`, `_darcs`, `.fossil-settings`, `.git`, `.hg`, `.pijul`, `.svn`, and
	/// Watchexec log files.
	#[arg(
		long,
		help_heading = OPTSET_FILTERING,
		display_order = 140,
	)]
	pub no_default_ignore: bool,

	/// Don't discover ignore files at all
	///
	/// This is a shorthand for '--no-global-ignore', '--no-vcs-ignore', '--no-project-ignore', but
	/// even more efficient as it will skip all the ignore discovery mechanisms from the get go.
	///
	/// Note that default ignores are still loaded, see '--no-default-ignore'.
	#[arg(
		long,
		help_heading = OPTSET_FILTERING,
		display_order = 141,
	)]
	pub no_discover_ignore: bool,

	/// Don't ignore anything at all
	///
	/// This is a shorthand for '--no-discover-ignore', '--no-default-ignore'.
	///
	/// Note that ignores explicitly loaded via other command line options, such as '--ignore' or
	/// '--ignore-file', will still be used.
	#[arg(
		long,
		help_heading = OPTSET_FILTERING,
		display_order = 92,
	)]
	pub ignore_nothing: bool,

	/// Filename extensions to filter to
	///
	/// This is a quick filter to only emit events for files with the given extensions. Extensions
	/// can be given with or without the leading dot (e.g. 'js' or '.js'). Multiple extensions can
	/// be given by repeating the option or by separating them with commas.
	#[arg(
		long = "exts",
		short = 'e',
		help_heading = OPTSET_FILTERING,
		value_delimiter = ',',
		value_name = "EXTENSIONS",
		display_order = 50,
	)]
	pub filter_extensions: Vec<String>,

	/// Filename patterns to filter to
	///
	/// Provide a glob-like filter pattern, and only events for files matching the pattern will be
	/// emitted. Multiple patterns can be given by repeating the option. Events that are not from
	/// files (e.g. signals, keyboard events) will pass through untouched.
	#[arg(
		long = "filter",
		short = 'f',
		help_heading = OPTSET_FILTERING,
		value_name = "PATTERN",
		display_order = 60,
	)]
	pub filter_patterns: Vec<String>,

	/// Files to load filters from
	///
	/// Provide a path to a file containing filters, one per line. Empty lines and lines starting
	/// with '#' are ignored. Uses the same pattern format as the '--filter' option.
	///
	/// This can also be used via the $WATCHEXEC_FILTER_FILES environment variable.
	#[arg(
		long = "filter-file",
		help_heading = OPTSET_FILTERING,
		value_delimiter = PATH_SEPARATOR.chars().next().unwrap(),
		value_hint = ValueHint::FilePath,
		value_name = "PATH",
		env = "WATCHEXEC_FILTER_FILES",
		hide_env = true,
		display_order = 61,
	)]
	pub filter_files: Vec<PathBuf>,

	/// Override the auto-detected project root directory
	///
	/// Used to resolve ignore files and leading-'/' filter patterns. Setting this also skips
	/// the root discovery search, which can speed up startup.
	#[arg(
		long,
		help_heading = OPTSET_FILTERING,
		value_hint = ValueHint::DirPath,
		value_name = "DIRECTORY",
		display_order = 160,
	)]
	pub project_origin: Option<PathBuf>,

	/// Advanced: filter events using a jaq (jq-compatible) expression
	///
	/// The expression receives an event object and must return a boolean.
	/// Prefix with '@' to load from a file. Use -v to see runtime errors.
	///
	/// Example — only trigger on file creates:
	///   'any(.tags[] | select(.kind == "fs"); .simple == "create")'
	///
	/// Extra functions available: file_meta, file_size, file_read(n), file_hash, kv_store/kv_fetch.
	#[arg(
		long = "filter-prog",
		short = 'j',
		help_heading = OPTSET_FILTERING,
		value_name = "EXPRESSION",
		display_order = 62,
	)]
	pub filter_programs: Vec<String>,

	#[doc(hidden)]
	#[clap(skip)]
	pub filter_programs_parsed: Vec<FilterProgram>,

	/// Filename patterns to filter out
	///
	/// Provide a glob-like filter pattern, and events for files matching the pattern will be
	/// excluded. Multiple patterns can be given by repeating the option. Events that are not from
	/// files (e.g. signals, keyboard events) will pass through untouched.
	#[arg(
		long = "ignore",
		short = 'i',
		help_heading = OPTSET_FILTERING,
		value_name = "PATTERN",
		display_order = 90,
	)]
	pub ignore_patterns: Vec<String>,

	/// Files to load ignores from
	///
	/// Provide a path to a file containing ignores, one per line. Empty lines and lines starting
	/// with '#' are ignored. Uses the same pattern format as the '--ignore' option.
	///
	/// This can also be used via the $WATCHEXEC_IGNORE_FILES environment variable.
	#[arg(
		long = "ignore-file",
		help_heading = OPTSET_FILTERING,
		value_delimiter = PATH_SEPARATOR.chars().next().unwrap(),
		value_hint = ValueHint::FilePath,
		value_name = "PATH",
		env = "WATCHEXEC_IGNORE_FILES",
		hide_env = true,
		display_order = 91,
	)]
	pub ignore_files: Vec<PathBuf>,

	/// Filesystem events to filter to
	///
	/// This is a quick filter to only emit events for the given types of filesystem changes. Choose
	/// from 'access', 'create', 'remove', 'rename', 'modify', 'metadata'. Multiple types can be
	/// given by repeating the option or by separating them with commas. By default, this is all
	/// types except for 'access'.
	///
	/// This may apply filtering at the kernel level when possible, which can be more efficient, but
	/// may be more confusing when reading the logs.
	#[arg(
		long = "fs-events",
		help_heading = OPTSET_FILTERING,
		default_value = "create,remove,rename,modify,metadata",
		value_delimiter = ',',
		hide_default_value = true,
		value_name = "EVENTS",
		display_order = 63,
	)]
	pub filter_fs_events: Vec<FsEvent>,

	/// Don't emit fs events for metadata changes
	///
	/// This is a shorthand for '--fs-events create,remove,rename,modify'. Using it alongside the
	/// '--fs-events' option is non-sensical and not allowed.
	#[arg(
		long = "no-meta",
		help_heading = OPTSET_FILTERING,
		conflicts_with = "filter_fs_events",
		display_order = 142,
	)]
	pub filter_fs_meta: bool,
}

impl FilteringArgs {
	pub(crate) async fn normalise(&mut self, command: &CommandArgs) -> Result<()> {
		if self.ignore_nothing {
			self.no_global_ignore = true;
			self.no_vcs_ignore = true;
			self.no_project_ignore = true;
			self.no_default_ignore = true;
			self.no_discover_ignore = true;
		}

		if self.filter_fs_meta {
			self.filter_fs_events = vec![
				FsEvent::Create,
				FsEvent::Remove,
				FsEvent::Rename,
				FsEvent::Modify,
			];
		}

		if let Some(watch_file) = self.watch_file.as_ref() {
			if watch_file == Path::new("-") {
				let file = tokio::io::stdin();
				let mut lines = BufReader::new(file).lines();
				while let Ok(Some(line)) = lines.next_line().await {
					self.recursive_paths.push(line.into());
				}
			} else {
				let file = File::open(watch_file).await.into_diagnostic()?;
				let mut lines = BufReader::new(file).lines();
				while let Ok(Some(line)) = lines.next_line().await {
					self.recursive_paths.push(line.into());
				}
			};
		}

		let project_origin = if let Some(p) = take(&mut self.project_origin) {
			p
		} else {
			crate::dirs::project_origin(&self, command).await?
		};
		debug!(path=?project_origin, "resolved project origin");
		let project_origin = dunce::canonicalize(project_origin).into_diagnostic()?;
		info!(path=?project_origin, "effective project origin");
		self.project_origin = Some(project_origin.clone());

		self.paths = take(&mut self.recursive_paths)
			.into_iter()
			.map(|path| {
				{
					if path.is_absolute() {
						Ok(path)
					} else {
						dunce::canonicalize(project_origin.join(path)).into_diagnostic()
					}
				}
				.map(WatchedPath::recursive)
			})
			.chain(take(&mut self.non_recursive_paths).into_iter().map(|path| {
				{
					if path.is_absolute() {
						Ok(path)
					} else {
						dunce::canonicalize(project_origin.join(path)).into_diagnostic()
					}
				}
				.map(WatchedPath::non_recursive)
			}))
			.collect::<Result<BTreeSet<_>>>()?
			.into_iter()
			.collect();

		if self.paths.len() == 1
			&& self
				.paths
				.first()
				.map_or(false, |p| p.as_ref() == Path::new("/dev/null"))
		{
			info!("only path is /dev/null, not watching anything");
			self.paths = Vec::new();
		} else if self.paths.is_empty() {
			info!("no paths, using current directory");
			self.paths.push(command.workdir.as_deref().unwrap().into());
		}
		info!(paths=?self.paths, "effective watched paths");

		for (n, prog) in self.filter_programs.iter().enumerate() {
			if let Some(progpath) = prog.strip_prefix('@') {
				self.filter_programs_parsed
					.push(FilterProgram::new_jaq_from_file(progpath).await?);
			} else {
				self.filter_programs_parsed
					.push(FilterProgram::new_jaq_from_arg(n, prog.clone())?);
			}
		}

		debug_assert!(self.project_origin.is_some());
		Ok(())
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum FsEvent {
	Access,
	Create,
	Remove,
	Rename,
	Modify,
	Metadata,
}
