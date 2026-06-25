use colored::Colorize;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use regex::Regex;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

// Directories skipped during indexing inaccessible or irrelevant on Windows
const SKIP_DIRS: &[&str] = &[
    "$Recycle.Bin", "System Volume Information", "Recovery",
    "$WinREAgent", "WpSystem", "MSOCache", "Config.Msi",
];

// Database path

pub fn db_default() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("USERPROFILE").map(|h| format!("{}\\AppData\\Local", h)))
        .unwrap_or_else(|_| "C:\\Users\\Default\\AppData\\Local".into());
    PathBuf::from(base).join("stools").join("locate.db")
}

struct Args {
    updatedb:         bool,
    stats:            bool,
    pattern:          Option<String>,
    case_insensitive: bool,
    use_regex:        bool,
    basename_only:    bool,
    count_only:       bool,
    limit:            Option<usize>,
    existing_only:    bool,
    null_sep:         bool,
    scan_paths:       Vec<String>,
    db_path:          PathBuf,
}

fn print_help() {
    println!("Usage: locate [OPTIONS] PATTERN");
    println!("       locate -u [PATH...]");
    println!("Search a pre-built index of all files on the system.");
    println!();
    println!("Indexing:");
    println!("  -u, --updatedb [PATH...]   build or refresh the file index");
    println!("                             (default: all local drives)");
    println!("  --stats                    show database info");
    println!("  --db PATH                  use a custom database file");
    println!();
    println!("Search:");
    println!("  -i                         case-insensitive matching");
    println!("  -r                         treat PATTERN as a regular expression");
    println!("  -b                         only match against the basename");
    println!("  -c                         print count of matches only");
    println!("  -l N                       limit output to N results");
    println!("  -e                         only show paths that still exist on disk");
    println!("  -0                         null-separated output (for xargs -0)");
    println!();
    println!("Patterns:");
    println!("  Plain string  substring match on full path");
    println!("  *  ?          glob wildcards — match full path");
    println!("  -r regex      full regex against full path (or basename with -b)");
    println!();
    println!("Examples:");
    println!("  locate --updatedb                   index all drives");
    println!("  locate --updatedb C:\\               index C: only");
    println!("  locate setup.exe                    find all setup.exe files");
    println!("  locate -i \"*.Log\"                   case-insensitive glob");
    println!("  locate -b config.json               match filename only");
    println!("  locate -r \"\\.rs$\" -e                all .rs files that exist");
    println!("  locate -c \".exe\"                    count indexed .exe files");
    println!("  locate --stats                      show index info");
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        std::process::exit(0);
    }

    let mut a = Args {
        updatedb: false, stats: false,
        pattern: None,
        case_insensitive: false, use_regex: false, basename_only: false,
        count_only: false, limit: None, existing_only: false, null_sep: false,
        scan_paths: Vec::new(),
        db_path: db_default(),
    };

    let mut i = 0;
    while i < raw.len() {
        let nxt = raw.get(i + 1).map(|s| s.as_str()).unwrap_or("");
        match raw[i].as_str() {
            "-u" | "--updatedb" => a.updatedb = true,
            "--stats"           => a.stats = true,
            "-i"                => a.case_insensitive = true,
            "-r"                => a.use_regex = true,
            "-b"                => a.basename_only = true,
            "-c"                => a.count_only = true,
            "-e"                => a.existing_only = true,
            "-0"                => a.null_sep = true,
            "-l"                => { a.limit = nxt.parse().ok(); i += 1; }
            "--db"              => { a.db_path = PathBuf::from(nxt); i += 1; }
            s if s.starts_with('-') && s.len() > 2 && !s.starts_with("--") => {
                for c in s.chars().skip(1) {
                    match c { 'i' => a.case_insensitive = true, 'r' => a.use_regex = true,
                              'b' => a.basename_only = true, 'c' => a.count_only = true,
                              'e' => a.existing_only = true, '0' => a.null_sep = true, _ => {} }
                }
            }
            s => {
                if a.updatedb { a.scan_paths.push(s.to_string()); }
                else { a.pattern = Some(s.to_string()); }
            }
        }
        i += 1;
    }
    a
}

// Scanner

fn local_drives() -> Vec<PathBuf> {
    ('A'..='Z')
        .map(|c| PathBuf::from(format!("{}:\\", c)))
        .filter(|p| p.exists())
        .collect()
}

pub fn scan_dir(dir: &Path, paths: &mut Vec<String>, count: &mut u64, depth: u32) {
    if depth > 64 { return; } // guard against junction loops

    let entries = match std::fs::read_dir(dir) {
        Ok(e)  => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // Strip extended-length prefix \\?\ if present
        let path_str = path.to_string_lossy();
        let path_str = path_str.strip_prefix(r"\\?\").unwrap_or(&path_str);

        paths.push(path_str.to_string());
        *count += 1;
        if *count % 50_000 == 0 {
            eprint!("\r  {:>9} files...", count);
            let _ = io::stderr().flush();
        }

        let ft = match entry.file_type() {
            Ok(t)  => t,
            Err(_) => continue,
        };

        if ft.is_dir() && !ft.is_symlink() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !SKIP_DIRS.contains(&name) {
                scan_dir(&entry.path(), paths, count, depth + 1);
            }
        }
    }
}

pub fn write_db(db_path: &Path, paths: &mut Vec<String>) {
    eprintln!("\r  sorting {} paths...          ", paths.len());
    paths.sort_unstable_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));

    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let f = match std::fs::File::create(db_path) {
        Ok(f)  => f,
        Err(e) => { eprintln!("locate: cannot write {}: {}", db_path.display(), e); std::process::exit(1); }
    };

    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut gz = GzEncoder::new(BufWriter::new(f), Compression::default());
    let _ = writeln!(gz, "# locate-db v1");
    let _ = writeln!(gz, "# timestamp={}", ts);
    let _ = writeln!(gz, "# count={}", paths.len());
    for p in paths.iter() {
        let _ = writeln!(gz, "{}", p);
    }
    let _ = gz.finish();
}

// updatedb

fn do_updatedb(args: &Args) {
    let roots: Vec<PathBuf> = if args.scan_paths.is_empty() {
        let drives = local_drives();
        println!("Scanning {} drive(s): {}",
            drives.len(),
            drives.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join("  "));
        drives
    } else {
        args.scan_paths.iter().map(PathBuf::from).collect()
    };

    let mut paths: Vec<String> = Vec::with_capacity(1_000_000);
    let mut total = 0u64;
    let t0 = Instant::now();

    for root in &roots {
        eprint!("\rScanning {}...            ", root.display());
        let _ = io::stderr().flush();
        scan_dir(root, &mut paths, &mut total, 0);
    }

    eprintln!("\r  {} files found            ", total);
    write_db(&args.db_path, &mut paths);

    let elapsed = t0.elapsed().as_secs_f64();
    let db_size = std::fs::metadata(&args.db_path).map(|m| m.len()).unwrap_or(0);

    println!("{} files indexed in {:.1}s   database: {}  ({})",
        total.to_string().green().bold(),
        elapsed,
        args.db_path.display(),
        human_size(db_size),
    );
}

// stats

fn do_stats(args: &Args) {
    let meta = match std::fs::metadata(&args.db_path) {
        Ok(m)  => m,
        Err(_) => { eprintln!("locate: database not found: {}", args.db_path.display());
                    eprintln!("Run 'locate --updatedb' first."); std::process::exit(1); }
    };

    let f   = std::fs::File::open(&args.db_path).unwrap();
    let gz  = GzDecoder::new(BufReader::new(f));
    let mut rdr = BufReader::new(gz);
    let mut ts: u64 = 0;
    let mut count: u64 = 0;
    let mut line = String::new();
    for _ in 0..5 {
        line.clear();
        if rdr.read_line(&mut line).unwrap_or(0) == 0 { break; }
        if let Some(v) = line.trim().strip_prefix("# timestamp=") { ts    = v.parse().unwrap_or(0); }
        if let Some(v) = line.trim().strip_prefix("# count=")     { count = v.parse().unwrap_or(0); }
    }

    println!("Database : {}", args.db_path.display());
    println!("Size     : {}", human_size(meta.len()));
    println!("Files    : {}", count);
    if ts > 0 {
        let now  = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let ago  = fmt_ago(now.saturating_sub(ts));
        println!("Updated  : {} ({})", fmt_ts(ts), ago);
    }
}

// search

fn do_search(args: &Args, pattern: &str) {
    let f = match std::fs::File::open(&args.db_path) {
        Ok(f)  => f,
        Err(_) => {
            eprintln!("locate: database not found: {}", args.db_path.display());
            eprintln!("Run 'locate --updatedb' to build the index first.");
            std::process::exit(1);
        }
    };

    let re: Option<Regex> = if args.use_regex {
        let pat = if args.case_insensitive { format!("(?i){}", pattern) } else { pattern.into() };
        match Regex::new(&pat) {
            Ok(r)  => Some(r),
            Err(e) => { eprintln!("locate: bad regex: {}", e); std::process::exit(1); }
        }
    } else {
        None
    };

    let pat_lower   = pattern.to_lowercase();
    let use_glob    = re.is_none() && (pattern.contains('*') || pattern.contains('?') || pattern.contains('['));

    let gz  = GzDecoder::new(BufReader::new(f));
    let rdr = BufReader::new(gz);

    let stdout = io::stdout();
    let mut out    = BufWriter::new(stdout.lock());
    let mut count  = 0usize;

    for line in rdr.lines() {
        let line = match line { Ok(l) => l, Err(_) => continue };
        if line.starts_with('#') { continue; }

        let subject = if args.basename_only {
            line.rsplit(|c| c == '\\' || c == '/').next().unwrap_or(&line)
        } else {
            &line
        };

        let hit = if let Some(ref re) = re {
            re.is_match(subject)
        } else if use_glob {
            glob_match_ci(pattern, subject, args.case_insensitive)
        } else if args.case_insensitive {
            subject.to_ascii_lowercase().contains(&pat_lower)
        } else {
            subject.contains(pattern)
        };

        if !hit { continue; }
        if args.existing_only && !Path::new(&line).exists() { continue; }

        count += 1;
        if !args.count_only {
            if args.null_sep { let _ = write!(out, "{}\0", line); }
            else             { let _ = writeln!(out, "{}", line); }
        }
        if args.limit.map_or(false, |lim| count >= lim) { break; }
    }

    let _ = out.flush();
    if args.count_only { println!("{}", count); }
    if count == 0 { std::process::exit(1); }
}

// Glob matching

fn glob_match_ci(pat: &str, text: &str, ci: bool) -> bool {
    let (p, t) = if ci {
        (pat.to_ascii_lowercase(), text.to_ascii_lowercase())
    } else {
        (pat.to_owned(), text.to_owned())
    };
    glob_bytes(p.as_bytes(), t.as_bytes())
}

fn glob_bytes(pat: &[u8], text: &[u8]) -> bool {
    match pat.first() {
        None       => text.is_empty(),
        Some(&b'*') => {
            let rest = &pat[1..];
            if rest.is_empty() { return true; }
            for i in 0..=text.len() {
                if glob_bytes(rest, &text[i..]) { return true; }
            }
            false
        }
        Some(&b'?') => !text.is_empty() && glob_bytes(&pat[1..], &text[1..]),
        Some(&p)    => !text.is_empty() && p == text[0] && glob_bytes(&pat[1..], &text[1..]),
    }
}

// Helpers

fn human_size(b: u64) -> String {
    const K: u64 = 1024; const M: u64 = K*1024; const G: u64 = M*1024;
    if b >= G { format!("{:.1} GB", b as f64/G as f64) }
    else if b >= M { format!("{:.1} MB", b as f64/M as f64) }
    else if b >= K { format!("{:.1} KB", b as f64/K as f64) }
    else { format!("{} B", b) }
}

fn fmt_ago(secs: u64) -> String {
    if secs < 60 { format!("{}s ago", secs) }
    else if secs < 3600 { format!("{}m ago", secs/60) }
    else if secs < 86400 { format!("{}h ago", secs/3600) }
    else { format!("{}d ago", secs/86400) }
}

fn fmt_ts(ts: u64) -> String {
    // Simple Gregorian epoch → YYYY-MM-DD HH:MM
    let s = ts % 86400; let h = s/3600; let m = (s%3600)/60;
    let mut days = ts/86400; let mut y = 1970u32;
    loop {
        let dy = if y%4==0 && (y%100!=0||y%400==0) {366} else {365};
        if days < dy { break; } days -= dy; y += 1;
    }
    let leap = y%4==0 && (y%100!=0||y%400==0);
    let mdays = [31u64,if leap{29}else{28},31,30,31,30,31,31,30,31,30,31];
    let mut mo = 1u32;
    for &d in &mdays { if days < d { break; } days -= d; mo += 1; }
    format!("{}-{:02}-{:02} {:02}:{:02}", y, mo, days+1, h, m)
}

// main

fn main() {
    let args = parse_args();

    if args.updatedb {
        do_updatedb(&args);
    } else if args.stats {
        do_stats(&args);
    } else if let Some(ref pat) = args.pattern.clone() {
        do_search(&args, pat);
    } else {
        eprintln!("locate: no pattern specified");
        eprintln!("Run 'locate --help' for usage.");
        std::process::exit(1);
    }
}
