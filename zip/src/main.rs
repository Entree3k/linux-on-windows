//! zip — create and manage ZIP archives.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use colored::Colorize;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

struct Args {
    archive:  PathBuf,
    inputs:   Vec<PathBuf>,
    recurse:  bool,
    level:    i64,
    verbose:  bool,
    list:     bool,
    test:     bool,
    update:   bool,
    freshen:  bool,
    junk:     bool,
    move_in:  bool,
    store:    bool,
    exclude:  Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        std::process::exit(0);
    }

    let mut a = Args {
        archive: PathBuf::new(), inputs: Vec::new(),
        recurse: false, level: 6, verbose: false,
        list: false, test: false, update: false, freshen: false,
        junk: false, move_in: false, store: false, exclude: Vec::new(),
    };

    let mut archive_set = false;
    let mut i = 0;
    while i < raw.len() {
        let s = raw[i].as_str();
        match s {
            "-r" | "--recurse-paths" => a.recurse  = true,
            "-v" | "--verbose"       => a.verbose  = true,
            "-l" | "--list"          => a.list     = true,
            "-T" | "--test"          => a.test     = true,
            "-u" | "--update"        => a.update   = true,
            "-f" | "--freshen"       => a.freshen  = true,
            "-j" | "--junk-paths"    => a.junk     = true,
            "-m" | "--move"          => a.move_in  = true,
            "-0"                     => { a.store = true; a.level = 0; }
            "-1" => a.level = 1, "-2" => a.level = 2, "-3" => a.level = 3,
            "-4" => a.level = 4, "-5" => a.level = 5, "-6" => a.level = 6,
            "-7" => a.level = 7, "-8" => a.level = 8, "-9" => a.level = 9,
            "-x" | "--exclude" => {
                i += 1;
                if let Some(pat) = raw.get(i) { a.exclude.push(pat.clone()); }
            }
            "--" => {
                // rest are inputs
                for arg in raw[i+1..].iter() { a.inputs.push(PathBuf::from(arg)); }
                break;
            }
            s if s.starts_with('-') && s.len() > 1 && !s.starts_with("--") => {
                for c in s.chars().skip(1) {
                    match c {
                        'r' => a.recurse  = true,
                        'v' => a.verbose  = true,
                        'l' => a.list     = true,
                        'T' => a.test     = true,
                        'u' => a.update   = true,
                        'f' => a.freshen  = true,
                        'j' => a.junk     = true,
                        'm' => a.move_in  = true,
                        '0' => { a.store = true; a.level = 0; }
                        '1'..='9' => a.level = c as i64 - '0' as i64,
                        _ => {}
                    }
                }
            }
            _ => {
                if !archive_set { a.archive = PathBuf::from(s); archive_set = true; }
                else             { a.inputs.push(PathBuf::from(s)); }
            }
        }
        i += 1;
    }

    if !archive_set { return Err("zip: missing archive name".into()); }
    Ok(a)
}

fn print_help() {
    println!("Usage: zip [options] archive.zip [file/dir...]");
    println!();
    println!("  -r              recurse into directories");
    println!("  -0              store (no compression)");
    println!("  -1 .. -9        compression level (1=fast, 9=best, default 6)");
    println!("  -j              junk paths (store just filenames)");
    println!("  -u              update: add changed and new files only");
    println!("  -f              freshen: update changed files only");
    println!("  -m              move files into archive (delete after adding)");
    println!("  -T              test archive integrity");
    println!("  -l              list archive contents");
    println!("  -v              verbose");
    println!("  -x PATTERN      exclude files matching pattern");
    println!("  -h              show this help");
    println!();
    println!("Examples:");
    println!("  zip archive.zip file.txt");
    println!("  zip -r archive.zip src/");
    println!("  zip -9 -r backup.zip . -x .git");
}

fn collect(inputs: &[PathBuf], recurse: bool, junk: bool, exclude: &[String]) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for inp in inputs {
        if inp.is_dir() && recurse {
            for e in WalkDir::new(inp).min_depth(1).into_iter().filter_map(|e| e.ok()) {
                let p = e.path().to_owned();
                if excluded(&p, exclude) { continue; }
                let name = if junk {
                    p.file_name().unwrap().to_string_lossy().replace('\\', "/")
                } else {
                    p.strip_prefix(inp.parent().unwrap_or(Path::new(".")))
                      .unwrap_or(&p).to_string_lossy().replace('\\', "/")
                };
                out.push((p, name));
            }
        } else if inp.is_file() {
            if excluded(inp, exclude) { continue; }
            let name = if junk {
                inp.file_name().unwrap().to_string_lossy().replace('\\', "/")
            } else {
                inp.to_string_lossy().replace('\\', "/")
            };
            out.push((inp.clone(), name));
        } else if inp.is_dir() && !recurse {
            eprintln!("zip: {}: is a directory -- use -r", inp.display());
        } else {
            eprintln!("zip: {}: no such file or directory", inp.display());
        }
    }
    out
}

fn excluded(path: &Path, patterns: &[String]) -> bool {
    let s = path.to_string_lossy();
    patterns.iter().any(|pat| s.contains(pat.as_str()))
}

fn do_list(archive: &Path) -> io::Result<()> {
    let mut za = ZipArchive::new(File::open(archive)?)?;
    println!("  Length      Date    Time    Name");
    println!("---------  ---------- -----   ----");
    let mut total_size: u64 = 0;
    let mut count = 0;
    for i in 0..za.len() {
        let file = za.by_index(i)?;
        let dt = file.last_modified().unwrap_or_default();
        println!("{:>9}  {:04}-{:02}-{:02} {:02}:{:02}   {}",
            file.size(),
            dt.year(), dt.month(), dt.day(),
            dt.hour(), dt.minute(),
            file.name().cyan());
        total_size += file.size();
        count += 1;
    }
    println!("---------                     -------");
    println!("{:>9}                     {} file{}", total_size, count, if count == 1 { "" } else { "s" });
    Ok(())
}

fn do_test(archive: &Path) -> io::Result<()> {
    let mut za = ZipArchive::new(File::open(archive)?)?;
    let mut ok = 0;
    for i in 0..za.len() {
        let mut file = za.by_index(i)?;
        let name = file.name().to_owned();
        let mut buf = [0u8; 65536];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => { eprintln!("zip: {}: {}: FAILED", archive.display(), name); return Err(e); }
            }
        }
        if false { eprintln!("    testing: {}", name); } // verbose mode would print this
        ok += 1;
    }
    println!("{}: {} file{} OK", archive.display(), ok, if ok == 1 { "" } else { "s" });
    Ok(())
}

fn do_create(archive: &Path, entries: &[(PathBuf, String)], args: &Args) -> io::Result<()> {
    let w = BufWriter::new(File::create(archive)?);
    let mut zw = ZipWriter::new(w);

    let method = if args.store { CompressionMethod::Stored } else { CompressionMethod::Deflated };
    let options = SimpleFileOptions::default()
        .compression_method(method)
        .compression_level(if args.store { None } else { Some(args.level) });

    for (disk_path, arc_name) in entries {
        if disk_path.is_dir() {
            let dir_name = if arc_name.ends_with('/') { arc_name.clone() } else { format!("{}/", arc_name) };
            zw.add_directory(&dir_name, SimpleFileOptions::default())?;
        } else {
            let size = fs::metadata(disk_path)?.len();
            if args.verbose {
                eprintln!("  adding: {} ({} bytes)", arc_name.cyan(), size);
            }
            zw.start_file(arc_name, options)?;
            let mut f = File::open(disk_path)?;
            io::copy(&mut f, &mut zw)?;
        }
    }

    zw.finish()?;

    if args.move_in {
        for (disk_path, _) in entries {
            if disk_path.is_file() { fs::remove_file(disk_path).ok(); }
        }
    }

    let size = fs::metadata(archive)?.len();
    if !args.verbose {
        println!("  created: {} ({} bytes)", archive.display(), size);
    }
    Ok(())
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => { eprintln!("{}", e.red()); std::process::exit(1); }
    };

    if args.list {
        if let Err(e) = do_list(&args.archive) {
            eprintln!("zip: {}", e); std::process::exit(1);
        }
        return;
    }

    if args.test {
        if let Err(e) = do_test(&args.archive) {
            eprintln!("zip: {}", e); std::process::exit(1);
        }
        return;
    }

    if args.inputs.is_empty() {
        eprintln!("{}", "zip: no input files specified".red());
        std::process::exit(1);
    }

    let entries = collect(&args.inputs, args.recurse, args.junk, &args.exclude);
    if entries.is_empty() {
        eprintln!("{}", "zip: nothing to add".yellow());
        std::process::exit(1);
    }

    if let Err(e) = do_create(&args.archive, &entries, &args) {
        eprintln!("{}: {}", "zip error".red(), e);
        std::process::exit(1);
    }
}
