use clap::{Arg, ArgAction, Command};
use colored::*;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

fn prompt(msg: &str) -> bool {
    eprint!("{}", msg);
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line).ok();
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

fn is_protected(path: &Path) -> bool {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    let s = canon.to_string_lossy().to_lowercase();

    // Drive roots: C:\, D:\, etc.
    if s.len() == 3 && s.ends_with(":\\") { return true; }
    // Unix-style root
    if s == "/" { return true; }
    // Windows system directory
    if s.contains("windows\\system32") { return true; }

    false
}

struct Opts {
    recursive:   bool,
    force:       bool,
    interactive: bool,
    verbose:     bool,
}

fn remove_path(path: &Path, opts: &Opts) -> Result<u64, String> {
    if !path.exists() && !path.is_symlink() {
        if opts.force {
            return Ok(0);
        }
        return Err(format!("'{}': No such file or directory", path.display()));
    }

    if is_protected(path) {
        return Err(format!(
            "'{}': refusing to remove protected path",
            path.display().to_string().red()
        ));
    }

    if path.is_dir() && !path.is_symlink() {
        if !opts.recursive {
            return Err(format!(
                "'{}': is a directory (use -r to remove directories)",
                path.display()
            ));
        }

        if opts.interactive {
            if !prompt(&format!("rm: descend into directory '{}'? ", path.display())) {
                return Ok(0);
            }
        }

        let mut count = 0u64;
        for entry in WalkDir::new(path).contents_first(true) {
            let entry = entry.map_err(|e| e.to_string())?;
            let ep = entry.path();

            if opts.interactive {
                let kind = if ep.is_dir() { "directory" } else { "file" };
                if !prompt(&format!("rm: remove {} '{}'? ", kind, ep.display())) {
                    continue;
                }
            }

            let result = if entry.file_type().is_dir() {
                fs::remove_dir(ep)
            } else {
                fs::remove_file(ep)
            };

            match result {
                Ok(_) => {
                    if opts.verbose { eprintln!("removed '{}'", ep.display()); }
                    if !entry.file_type().is_dir() { count += 1; }
                }
                Err(e) => {
                    if !opts.force {
                        return Err(format!("'{}': {}", ep.display(), e));
                    }
                }
            }
        }
        return Ok(count);
    }

    if opts.interactive {
        let kind = if path.is_symlink() { "symlink" } else { "file" };
        if !prompt(&format!("rm: remove {} '{}'? ", kind, path.display())) {
            return Ok(0);
        }
    }

    fs::remove_file(path).map_err(|e| format!("'{}': {}", path.display(), e))?;
    if opts.verbose { eprintln!("removed '{}'", path.display()); }
    Ok(1)
}

fn main() {
    let matches = Command::new("rm")
        .version("1.0.0")
        .about("Remove files and directories")
        .after_help(
            "EXAMPLES\n\
             \x20 rm file.txt              # remove a file\n\
             \x20 rm a.txt b.txt c.txt     # remove multiple files\n\
             \x20 rm -r dir/               # remove a directory and its contents\n\
             \x20 rm -rf dir/              # force-remove without prompts\n\
             \x20 rm -i *.log              # prompt before each removal\n\
             \x20 rm -v file.txt           # show each removal\n\
             \n\
             SAFETY\n\
             \x20 Drive roots (C:\\, D:\\) and Windows\\System32 are always protected.\n\
             \x20 Use -i to confirm each file interactively.",
        )
        .arg(Arg::new("recursive")
            .short('r').long("recursive").visible_short_alias('R')
            .action(ArgAction::SetTrue)
            .help("Remove directories and their contents recursively"))
        .arg(Arg::new("force")
            .short('f').long("force")
            .action(ArgAction::SetTrue)
            .help("Ignore nonexistent files, never prompt"))
        .arg(Arg::new("interactive")
            .short('i').long("interactive")
            .action(ArgAction::SetTrue)
            .help("Prompt before every removal"))
        .arg(Arg::new("verbose")
            .short('v').long("verbose")
            .action(ArgAction::SetTrue)
            .help("Show each file as it is removed"))
        .arg(Arg::new("files")
            .value_name("FILE")
            .num_args(1..)
            .required(true)
            .help("Files or directories to remove"))
        .get_matches();

    let opts = Opts {
        recursive:   matches.get_flag("recursive"),
        force:       matches.get_flag("force"),
        interactive: matches.get_flag("interactive"),
        verbose:     matches.get_flag("verbose"),
    };

    let files: Vec<PathBuf> = matches
        .get_many::<String>("files")
        .unwrap_or_default()
        .map(PathBuf::from)
        .collect();

    let mut ok = true;

    for path in &files {
        match remove_path(path, &opts) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("{}: {}", "rm".bold(), e);
                ok = false;
            }
        }
    }

    std::process::exit(if ok { 0 } else { 1 });
}
