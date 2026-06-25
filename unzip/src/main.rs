//! unzip — extract ZIP archives.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use colored::Colorize;
use zip::ZipArchive;

struct Args {
    archive:  PathBuf,
    dest:     PathBuf,
    files:    Vec<String>,
    list:     bool,
    verbose:  bool,
    test:     bool,
    overwrite: bool,
    no_over:  bool,
    stdout:   bool,
    quiet:    bool,
    exclude:  Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        std::process::exit(0);
    }

    let mut a = Args {
        archive: PathBuf::new(), dest: PathBuf::from("."),
        files: Vec::new(), list: false, verbose: false,
        test: false, overwrite: false, no_over: false,
        stdout: false, quiet: false, exclude: Vec::new(),
    };

    let mut archive_set = false;
    let mut i = 0;
    let mut after_archive = false;

    while i < raw.len() {
        let s = raw[i].as_str();
        match s {
            "-l" => { a.list = true; after_archive = true; }
            "-v" => { a.verbose = true; after_archive = true; }
            "-t" => { a.test = true; after_archive = true; }
            "-o" => a.overwrite = true,
            "-n" => a.no_over   = true,
            "-p" => a.stdout    = true,
            "-q" => a.quiet     = true,
            "-qq" => { a.quiet = true; }
            "-d" => {
                i += 1;
                if let Some(d) = raw.get(i) { a.dest = PathBuf::from(d); }
            }
            "-x" => {
                i += 1;
                if let Some(x) = raw.get(i) { a.exclude.push(x.clone()); }
            }
            _ if s.starts_with('-') && s.len() > 1 && !s.starts_with("--") => {
                let mut j = 1usize;
                let bytes = s.as_bytes();
                while j < bytes.len() {
                    match bytes[j] as char {
                        'l' => a.list      = true,
                        'v' => a.verbose   = true,
                        't' => a.test      = true,
                        'o' => a.overwrite = true,
                        'n' => a.no_over   = true,
                        'p' => a.stdout    = true,
                        'q' => a.quiet     = true,
                        'd' => {
                            let rest = &s[j+1..];
                            if !rest.is_empty() { a.dest = PathBuf::from(rest); }
                            else { i += 1; if let Some(d) = raw.get(i) { a.dest = PathBuf::from(d); } }
                            break;
                        }
                        _ => {}
                    }
                    j += 1;
                }
            }
            _ => {
                if !archive_set {
                    // Add .zip if no extension
                    let mut p = PathBuf::from(s);
                    if p.extension().is_none() { p.set_extension("zip"); }
                    a.archive = p;
                    archive_set = true;
                    after_archive = true;
                } else if after_archive {
                    a.files.push(s.to_owned());
                }
            }
        }
        i += 1;
    }

    if !archive_set { return Err("unzip: missing archive name".into()); }
    Ok(a)
}

fn print_help() {
    println!("Usage: unzip [options] archive[.zip] [files] [-x excludes] [-d dest]");
    println!();
    println!("  -l        list archive contents (short)");
    println!("  -v        list verbosely");
    println!("  -t        test compressed archive data");
    println!("  -o        overwrite files without prompting");
    println!("  -n        never overwrite existing files");
    println!("  -p        extract to stdout");
    println!("  -q        quiet mode");
    println!("  -d dest   extract files into dest/");
    println!("  -x pat    exclude files matching pattern");
    println!();
    println!("Examples:");
    println!("  unzip archive.zip");
    println!("  unzip archive.zip -d output/");
    println!("  unzip -l archive.zip");
    println!("  unzip archive.zip file.txt -d dest/");
}

fn matches_filter(name: &str, include: &[String], exclude: &[String]) -> bool {
    if !exclude.is_empty() && exclude.iter().any(|p| name.contains(p.as_str())) {
        return false;
    }
    if include.is_empty() { return true; }
    include.iter().any(|p| name.contains(p.as_str()) || glob_match(p, name))
}

fn glob_match(pat: &str, text: &str) -> bool {
    let (p, t) = (pat.as_bytes(), text.as_bytes());
    let mut pi = 0;
    let mut ti = 0;
    let mut star_pi = usize::MAX;
    let mut star_ti = 0;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) { pi += 1; ti += 1; }
        else if pi < p.len() && p[pi] == b'*' { star_pi = pi; star_ti = ti; pi += 1; }
        else if star_pi != usize::MAX { pi = star_pi + 1; star_ti += 1; ti = star_ti; }
        else { return false; }
    }
    while pi < p.len() && p[pi] == b'*' { pi += 1; }
    pi == p.len()
}

fn do_list(archive: &Path, verbose: bool) -> io::Result<()> {
    let mut za = ZipArchive::new(File::open(archive)?)?;
    if verbose {
        println!(" Length   Method    Size  Cmpr    Date    Time   Name");
        println!("--------  ------  ------  ----    ----    ----   ----");
    } else {
        println!("  Length      Date    Time    Name");
        println!("---------  ---------- -----   ----");
    }
    let mut total_compressed: u64 = 0;
    let mut total_size: u64 = 0;
    let mut count = 0;
    for i in 0..za.len() {
        let file = za.by_index(i)?;
        let dt = file.last_modified().unwrap_or_default();
        let comp = file.compressed_size();
        let size = file.size();
        let method = match file.compression() {
            zip::CompressionMethod::Stored   => "Stored",
            zip::CompressionMethod::Deflated => "Deflated",
            _ => "Unknown",
        };
        let ratio = if size > 0 { 100u64.saturating_sub(comp * 100 / size) } else { 0 };
        if verbose {
            println!("{:>8}  {:8}  {:>6}  {:3}%  {:04}-{:02}-{:02} {:02}:{:02}   {}",
                size, method, comp, ratio,
                dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute(),
                file.name().cyan());
        } else {
            println!("{:>9}  {:04}-{:02}-{:02} {:02}:{:02}   {}",
                size, dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute(),
                file.name().cyan());
        }
        total_compressed += comp;
        total_size += size;
        count += 1;
    }
    if verbose {
        let ratio = if total_size > 0 { 100u64.saturating_sub(total_compressed * 100 / total_size) } else { 0 };
        println!("--------          ------  ----                     -------");
        println!("{:>8}          {:>6}  {:3}%                     {} file{}", total_size, total_compressed, ratio, count, if count == 1 { "" } else { "s" });
    } else {
        println!("---------                     -------");
        println!("{:>9}                     {} file{}", total_size, count, if count == 1 { "" } else { "s" });
    }
    Ok(())
}

fn do_extract(archive: &Path, args: &Args) -> io::Result<()> {
    let mut za = ZipArchive::new(File::open(archive)?)?;
    let mut extracted = 0u64;
    let mut errors = 0u64;

    for i in 0..za.len() {
        let mut file = za.by_index(i)?;
        let name = file.name().to_owned();

        if !matches_filter(&name, &args.files, &args.exclude) { continue; }

        if args.test {
            let mut buf = [0u8; 65536];
            loop {
                match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("  {:20} FAILED ({})", name, e);
                        errors += 1;
                        break;
                    }
                }
            }
            if !args.quiet { eprintln!("    testing: {:30}  OK", name); }
            continue;
        }

        if args.stdout {
            io::copy(&mut file, &mut io::stdout().lock())?;
            continue;
        }

        let out_path = args.dest.join(&name);

        if file.is_dir() {
            fs::create_dir_all(&out_path)?;
            continue;
        }

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if out_path.exists() {
            if args.no_over {
                if !args.quiet { eprintln!("  skipping: {}", name); }
                continue;
            }
            if !args.overwrite && !args.quiet {
                eprintln!("  inflating: {}", name.yellow());
            }
        } else if !args.quiet {
            eprintln!("  inflating: {}", name.cyan());
        }

        let mut out = BufWriter::new(File::create(&out_path)?);
        io::copy(&mut file, &mut out)?;
        extracted += 1;
    }

    if args.test {
        if errors == 0 {
            println!("No errors detected in compressed data of {}.", archive.display());
        } else {
            eprintln!("{} error(s) in {}.", errors, archive.display());
            std::process::exit(1);
        }
    } else if !args.quiet && !args.stdout {
        println!("  {} file{} extracted.", extracted, if extracted == 1 { "" } else { "s" });
    }
    Ok(())
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => { eprintln!("{}", e.red()); std::process::exit(1); }
    };

    if args.list || args.verbose && !args.test {
        if let Err(e) = do_list(&args.archive, args.verbose) {
            eprintln!("unzip: {}", e); std::process::exit(1);
        }
        return;
    }

    if let Err(e) = do_extract(&args.archive, &args) {
        eprintln!("{}: {}", "unzip error".red(), e);
        std::process::exit(1);
    }
}
