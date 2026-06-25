use chrono::{Local, NaiveDateTime, TimeZone};
use clap::{Arg, ArgAction, Command};
use colored::*;
use filetime::{set_file_times, FileTime};
use std::fs::{self, OpenOptions};
use std::path::Path;

fn parse_timestamp(s: &str) -> Result<FileTime, String> {
    let formats: &[&str] = &[
        "%Y%m%d%H%M%S",
        "%Y%m%d%H%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ];

    for fmt in formats {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            let dt = Local.from_local_datetime(&ndt).single()
                .ok_or_else(|| format!("ambiguous local time: {}", s))?;
            return Ok(FileTime::from_unix_time(dt.timestamp(), 0));
        }
    }

    Err(format!(
        "unrecognised timestamp '{}'\n  accepted: YYYYMMDDHHMMSS  YYYYMMDDHHMM  YYYY-MM-DD HH:MM:SS",
        s
    ))
}

fn touch_file(
    path: &Path,
    no_create: bool,
    atime: FileTime,
    mtime: FileTime,
) -> Result<(), String> {
    if !path.exists() {
        if no_create {
            return Ok(());
        }
        OpenOptions::new()
            .create(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("{}: {}", path.display(), e))?;
    }

    set_file_times(path, atime, mtime)
        .map_err(|e| format!("{}: {}", path.display(), e))
}

fn main() {
    let matches = Command::new("touch")
        .version("1.0.0")
        .about("Create files or update their timestamps")
        .long_about(
            "Update the access and modification timestamps of each FILE to now.\n\
             If FILE does not exist it is created as an empty file (unless -c).",
        )
        .after_help(
            "TIMESTAMP FORMATS  (-t)\n\
             \x20 YYYYMMDDHHMMSS        e.g.  20260101143000\n\
             \x20 YYYYMMDDHHMM          e.g.  202601011430\n\
             \x20 YYYY-MM-DD HH:MM:SS   e.g.  \"2026-01-01 14:30:00\"\n\
             \x20 YYYY-MM-DDTHH:MM:SS   e.g.  2026-01-01T14:30:00\n\
             \n\
             EXAMPLES\n\
             \x20 touch new.txt                        # create or update to now\n\
             \x20 touch a.txt b.txt c.txt              # multiple files\n\
             \x20 touch -c maybe.txt                   # update only if exists\n\
             \x20 touch -t 20260101000000 file.txt     # set exact timestamp\n\
             \x20 touch -r source.txt dest.txt         # copy source's timestamps",
        )
        .arg(
            Arg::new("no_create")
                .short('c')
                .long("no-create")
                .action(ArgAction::SetTrue)
                .help("Do not create files that do not exist"),
        )
        .arg(
            Arg::new("timestamp")
                .short('t')
                .long("time")
                .value_name("STAMP")
                .help("Use STAMP instead of current time (see formats above)"),
        )
        .arg(
            Arg::new("reference")
                .short('r')
                .long("reference")
                .value_name("FILE")
                .help("Use this file's timestamps instead of current time"),
        )
        .arg(
            Arg::new("atime_only")
                .short('a')
                .long("atime")
                .action(ArgAction::SetTrue)
                .help("Change only the access time"),
        )
        .arg(
            Arg::new("mtime_only")
                .short('m')
                .long("mtime")
                .action(ArgAction::SetTrue)
                .help("Change only the modification time"),
        )
        .arg(
            Arg::new("files")
                .value_name("FILE")
                .num_args(1..)
                .required(true)
                .help("Files to create or update"),
        )
        .get_matches();

    let no_create   = matches.get_flag("no_create");
    let atime_only  = matches.get_flag("atime_only");
    let mtime_only  = matches.get_flag("mtime_only");

    // Resolve target timestamps
    let (target_atime, target_mtime): (Option<FileTime>, Option<FileTime>) =
        if let Some(ref_path) = matches.get_one::<String>("reference") {
            match fs::metadata(ref_path) {
                Err(e) => {
                    eprintln!("{}: reference {}: {}", "touch".bold(), ref_path.yellow(), e);
                    std::process::exit(1);
                }
                Ok(meta) => (
                    Some(FileTime::from_last_access_time(&meta)),
                    Some(FileTime::from_last_modification_time(&meta)),
                ),
            }
        } else if let Some(stamp) = matches.get_one::<String>("timestamp") {
            match parse_timestamp(stamp) {
                Err(e) => {
                    eprintln!("{}: {}", "touch".bold(), e);
                    std::process::exit(1);
                }
                Ok(ft) => (Some(ft), Some(ft)),
            }
        } else {
            (None, None) // use now
        };

    let files: Vec<String> = matches
        .get_many::<String>("files")
        .unwrap_or_default()
        .cloned()
        .collect();

    let mut exit_code = 0i32;

    for path_str in &files {
        let path = Path::new(path_str);

        let now = FileTime::now();
        let existing_atime = path.metadata().map(|m| FileTime::from_last_access_time(&m));
        let existing_mtime = path.metadata().map(|m| FileTime::from_last_modification_time(&m));

        let atime = if mtime_only {
            existing_atime.unwrap_or(now)
        } else {
            target_atime.unwrap_or(now)
        };

        let mtime = if atime_only {
            existing_mtime.unwrap_or(now)
        } else {
            target_mtime.unwrap_or(now)
        };

        if let Err(e) = touch_file(path, no_create, atime, mtime) {
            eprintln!("{}: {}", "touch".bold(), e);
            exit_code = 1;
        }
    }

    std::process::exit(exit_code);
}
