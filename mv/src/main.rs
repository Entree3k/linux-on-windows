use clap::{Arg, ArgAction, Command};
use colored::*;
use filetime::{set_file_times, FileTime};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const BUF_SIZE: usize = 4 * 1024 * 1024;

fn prompt(msg: &str) -> bool {
    eprint!("{}", msg);
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line).ok();
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

// Cross device copy used as fallback when rename fails

fn copy_file_raw(src: &Path, dst: &Path) -> io::Result<()> {
    // Preserve timestamps from source
    let meta  = src.metadata()?;
    let atime = FileTime::from_last_access_time(&meta);
    let mtime = FileTime::from_last_modification_time(&meta);

    let mut reader = BufReader::with_capacity(BUF_SIZE, File::open(src)?);
    let mut writer = File::create(dst)?;
    let mut buf    = vec![0u8; BUF_SIZE];

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 { break; }
        writer.write_all(&buf[..n])?;
    }

    set_file_times(dst, atime, mtime).ok();
    Ok(())
}

fn copy_dir_raw(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in WalkDir::new(src).min_depth(1) {
        let entry = entry.map_err(|e| e.to_string())?;
        let rel   = entry.path().strip_prefix(src).map_err(|e| e.to_string())?;
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        } else {
            copy_file_raw(entry.path(), &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

// Core logic

struct Opts {
    force:       bool,
    interactive: bool,
    no_clobber:  bool,
    verbose:     bool,
}

fn move_path(src: &Path, dst: &Path, opts: &Opts) -> Result<(), String> {
    if dst.exists() {
        if opts.no_clobber { return Ok(()); }
        if opts.interactive && !opts.force {
            if !prompt(&format!("mv: overwrite '{}'? ", dst.display())) {
                return Ok(());
            }
        }
    }

    match fs::rename(src, dst) {
        Ok(_) => {
            if opts.verbose {
                eprintln!("'{}' -> '{}'", src.display(), dst.display());
            }
            return Ok(());
        }
        Err(e) => {
            let is_cross = e.raw_os_error().map(|c| c == 17 || c == 18 || c == 0x11).unwrap_or(false);
            if !is_cross {
                return Err(format!("{} -> {}: {}", src.display(), dst.display(), e));
            }
        }
    }

    eprintln!("{}: cross-device move, using copy+delete", "mv".dimmed());

    if src.is_dir() {
        copy_dir_raw(src, dst)?;
        fs::remove_dir_all(src).map_err(|e| format!("removing '{}': {}", src.display(), e))?;
    } else {
        copy_file_raw(src, dst).map_err(|e| format!("{}: {}", dst.display(), e))?;
        fs::remove_file(src).map_err(|e| format!("removing '{}': {}", src.display(), e))?;
    }

    if opts.verbose {
        eprintln!("'{}' -> '{}'", src.display(), dst.display());
    }

    Ok(())
}

// Entry point

fn main() {
    let matches = Command::new("mv")
        .version("1.0.0")
        .about("Move or rename files and directories")
        .after_help(
            "EXAMPLES\n\
             \x20 mv old.txt new.txt           # rename a file\n\
             \x20 mv file.txt dir/             # move into a directory\n\
             \x20 mv a.txt b.txt c.txt dest/   # move multiple files\n\
             \x20 mv -i file.txt dest/         # prompt before overwrite\n\
             \x20 mv -n file.txt dest/         # never overwrite existing\n\
             \x20 mv -v src/ dst/              # verbose output\n\
             \n\
             Cross-device moves (different drives) are handled automatically\n\
             by copying the content then deleting the source.",
        )
        .arg(Arg::new("force").short('f').long("force").action(ArgAction::SetTrue).help("Overwrite without prompting"))
        .arg(Arg::new("interactive").short('i').long("interactive").action(ArgAction::SetTrue).help("Prompt before overwrite"))
        .arg(Arg::new("no_clobber").short('n').long("no-clobber").action(ArgAction::SetTrue).help("Do not overwrite existing files"))
        .arg(Arg::new("verbose").short('v').long("verbose").action(ArgAction::SetTrue).help("Show each file as it is moved"))
        .arg(Arg::new("sources").value_name("SOURCE").num_args(1..).required(true).help("Sources and destination"))
        .get_matches();

    let opts = Opts {
        force:       matches.get_flag("force"),
        interactive: matches.get_flag("interactive"),
        no_clobber:  matches.get_flag("no_clobber"),
        verbose:     matches.get_flag("verbose"),
    };

    let paths: Vec<PathBuf> = matches
        .get_many::<String>("sources")
        .unwrap_or_default()
        .map(PathBuf::from)
        .collect();

    if paths.len() < 2 {
        eprintln!("{}: missing destination", "mv".bold());
        std::process::exit(1);
    }

    let dest = paths.last().unwrap().clone();
    let srcs = &paths[..paths.len() - 1];
    let mut ok = true;

    if srcs.len() > 1 && !dest.is_dir() {
        eprintln!("{}: target '{}' must be a directory for multiple sources",
            "mv".bold(), dest.display().to_string().yellow());
        std::process::exit(1);
    }

    for src in srcs {
        if !src.exists() {
            eprintln!("{}: '{}': No such file or directory",
                "mv".bold(), src.display().to_string().yellow());
            ok = false;
            continue;
        }

        let dst = if dest.is_dir() {
            dest.join(src.file_name().unwrap_or_default())
        } else {
            dest.clone()
        };

        if let Err(e) = move_path(src, &dst, &opts) {
            eprintln!("{}: {}", "mv".bold(), e);
            ok = false;
        }
    }

    std::process::exit(if ok { 0 } else { 1 });
}
