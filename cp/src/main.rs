use clap::{Arg, ArgAction, Command};
use colored::*;
use filetime::{set_file_times, FileTime};
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use walkdir::WalkDir;

const PROGRESS_THRESHOLD: u64 = 1 * 1024 * 1024; // show bar for files > 1 MB
const BUF_SIZE: usize = 4 * 1024 * 1024;

// Helpers

fn prompt(msg: &str) -> bool {
    eprint!("{}", msg);
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line).ok();
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

fn humanize(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / (1u64 << 10) as f64),
        b                 => format!("{} B", b),
    }
}

// copy

struct Opts {
    force:       bool,
    interactive: bool,
    no_clobber:  bool,
    preserve:    bool,
    verbose:     bool,
    recursive:   bool,
}

/// Copy one file
fn copy_file(src: &Path, dst: &Path, opts: &Opts) -> Result<(), String> {
    if dst.exists() {
        if opts.no_clobber {
            return Ok(());
        }
        if opts.interactive && !opts.force {
            if !prompt(&format!("cp: overwrite '{}'? ", dst.display())) {
                return Ok(());
            }
        }
    }

    let size = src.metadata().map(|m| m.len()).unwrap_or(0);

    if size > PROGRESS_THRESHOLD {
        copy_with_progress(src, dst, size).map_err(|e| e.to_string())?;
    } else {
        fs::copy(src, dst).map_err(|e| format!("{}: {}", dst.display(), e))?;
    }

    if opts.preserve {
        if let Ok(meta) = src.metadata() {
            let at = FileTime::from_last_access_time(&meta);
            let mt = FileTime::from_last_modification_time(&meta);
            set_file_times(dst, at, mt).ok();
        }
    }

    if opts.verbose {
        eprintln!("'{}' -> '{}'", src.display(), dst.display());
    }

    Ok(())
}

fn copy_with_progress(src: &Path, dst: &Path, size: u64) -> io::Result<()> {
    let pb = ProgressBar::new(size);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} Copying  [{bar:36.green/237}]  {percent}%  {binary_bytes_per_sec}  ETA {eta_precise}",
        )
        .unwrap()
        .tick_strings(&["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏",""])
        .progress_chars("█▓░"),
    );
    pb.enable_steady_tick(Duration::from_millis(80));

    let mut src_f = BufReader::with_capacity(BUF_SIZE, File::open(src)?);
    let mut dst_f = File::create(dst)?;
    let mut buf   = vec![0u8; BUF_SIZE];
    let mut done  = 0u64;

    loop {
        let n = src_f.read(&mut buf)?;
        if n == 0 { break; }
        dst_f.write_all(&buf[..n])?;
        done += n as u64;
        pb.set_position(done);
    }

    pb.finish_and_clear();
    eprintln!("  {} Copied {}", "✔".green(), humanize(size));
    Ok(())
}

/// Recursively copy a directory tree
fn copy_dir(src: &Path, dst: &Path, opts: &Opts) -> Result<(), String> {
    if !opts.recursive {
        return Err(format!(
            "{}: omitting directory '{}' (use -r)",
            "cp".bold(), src.display()
        ));
    }

    fs::create_dir_all(dst).map_err(|e| format!("{}: {}", dst.display(), e))?;

    for entry in WalkDir::new(src).min_depth(1) {
        let entry = entry.map_err(|e| e.to_string())?;
        let rel   = entry.path().strip_prefix(src).map_err(|e| e.to_string())?;
        let target = dst.join(rel);

        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .map_err(|e| format!("{}: {}", target.display(), e))?;
        } else {
            copy_file(entry.path(), &target, opts)?;
        }
    }

    Ok(())
}

// Entry

fn main() {
    let matches = Command::new("cp")
        .version("1.0.0")
        .about("Copy files and directories")
        .after_help(
            "EXAMPLES\n\
             \x20 cp file.txt copy.txt            # copy a file\n\
             \x20 cp -r src/ dst/                 # copy a directory\n\
             \x20 cp -v *.txt backup/             # copy with progress\n\
             \x20 cp -rp src/ dst/                # recursive + preserve timestamps\n\
             \x20 cp -i file.txt dest/            # prompt before overwrite\n\
             \x20 cp -n file.txt dest/            # never overwrite existing",
        )
        .arg(Arg::new("recursive").short('r').long("recursive").visible_short_alias('R').action(ArgAction::SetTrue).help("Copy directories recursively"))
        .arg(Arg::new("force").short('f').long("force").action(ArgAction::SetTrue).help("Overwrite without prompting"))
        .arg(Arg::new("interactive").short('i').long("interactive").action(ArgAction::SetTrue).help("Prompt before overwrite"))
        .arg(Arg::new("no_clobber").short('n').long("no-clobber").action(ArgAction::SetTrue).help("Do not overwrite existing files"))
        .arg(Arg::new("preserve").short('p').long("preserve").action(ArgAction::SetTrue).help("Preserve timestamps and attributes"))
        .arg(Arg::new("verbose").short('v').long("verbose").action(ArgAction::SetTrue).help("Show each file as it is copied"))
        .arg(Arg::new("sources").value_name("SOURCE").num_args(1..).required(true).help("Source files/directories"))
        .get_matches();

    let opts = Opts {
        recursive:   matches.get_flag("recursive"),
        force:       matches.get_flag("force"),
        interactive: matches.get_flag("interactive"),
        no_clobber:  matches.get_flag("no_clobber"),
        preserve:    matches.get_flag("preserve"),
        verbose:     matches.get_flag("verbose"),
    };

    let sources: Vec<PathBuf> = matches
        .get_many::<String>("sources")
        .unwrap_or_default()
        .map(PathBuf::from)
        .collect();

    if sources.len() < 2 {
        eprintln!("{}: missing destination", "cp".bold());
        std::process::exit(1);
    }

    let dest   = sources.last().unwrap().clone();
    let srcs   = &sources[..sources.len() - 1];
    let mut ok = true;

    // Multiple sources → dest must be an existing directory.
    if srcs.len() > 1 && !dest.is_dir() {
        eprintln!("{}: target '{}' is not a directory",
            "cp".bold(), dest.display().to_string().yellow());
        eprintln!("  {} sources parsed: {}",
            srcs.len() + 1,
            sources.iter().map(|p| format!("'{}'", p.display())).collect::<Vec<_>>().join(", "));
        eprintln!("  {} If the destination path contains spaces, wrap it in quotes:",
            "hint:".cyan().bold());
        eprintln!("  cp {} \"{}\"",
            srcs.iter().map(|p| format!("\"{}\"", p.display())).collect::<Vec<_>>().join(" "),
            dest.display());
        std::process::exit(1);
    }

    for src in srcs {
        if !src.exists() {
            eprintln!("{}: '{}': No such file or directory",
                "cp".bold(), src.display().to_string().yellow());
            ok = false;
            continue;
        }

        // Determine destination path
        let dst = if dest.is_dir() {
            dest.join(src.file_name().unwrap_or_default())
        } else {
            dest.clone()
        };

        let result = if src.is_dir() {
            copy_dir(src, &dst, &opts)
        } else {
            copy_file(src, &dst, &opts)
        };

        if let Err(e) = result {
            eprintln!("{}: {}", "cp".bold(), e);
            ok = false;
        }
    }

    std::process::exit(if ok { 0 } else { 1 });
}
