//! sfind search for files in a directory hierarchy.

use std::fs::{self, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

// Predicate AST

#[derive(Clone)]
enum Pred {
    Name   { pat: String, ci: bool },   // -name / -iname (filename only)
    Path   { pat: String, ci: bool },   // -path / -ipath (full path)
    Type   { kind: TypeKind },
    Size   { bytes: u64, cmp: Cmp },
    Mtime  { days: i64, cmp: Cmp },
    Newer  { than: PathBuf },
    Empty,
    Not    (Box<Pred>),
    And    (Vec<Pred>),
    Or     (Vec<Pred>),
    True,
    False,
}

#[derive(Clone, PartialEq)]
enum TypeKind { File, Dir, Link }

#[derive(Clone, PartialEq)]
enum Cmp { Lt, Eq, Gt }

#[derive(Clone)]
enum Action {
    Print,
    Print0,
    Ls,
    Delete,
    Exec(Vec<String>),   // {} is replaced by the found path
}

struct Opts {
    roots:     Vec<PathBuf>,
    pred:      Pred,
    action:    Action,
    maxdepth:  Option<u32>,
    mindepth:  Option<u32>,
    follow:    bool,

    // Index modes
    build_index:  bool,
    locate_pat:   Option<String>,
    locate_ci:    bool,
    locate_db:    PathBuf,
}

// matching

fn glob_match(pat: &[u8], text: &[u8], allow_sep: bool) -> bool {
    match pat.first() {
        None => text.is_empty(),
        Some(&b'*') => {
            let rest = &pat[1..];
            if rest.is_empty() {
                if !allow_sep {
                    return !text.contains(&b'/') && !text.contains(&b'\\');
                }
                return true;
            }
            for i in 0..=text.len() {
                if !allow_sep && i < text.len()
                    && (text[i] == b'/' || text[i] == b'\\')
                {
                    return glob_match(rest, &text[i..], allow_sep);
                }
                if glob_match(rest, &text[i..], allow_sep) {
                    return true;
                }
            }
            false
        }
        Some(&b'?') => {
            if text.is_empty() { return false; }
            let c = text[0];
            if !allow_sep && (c == b'/' || c == b'\\') { return false; }
            glob_match(&pat[1..], &text[1..], allow_sep)
        }
        Some(&b'[') => {
            let close = match pat[1..].iter().position(|&b| b == b']') {
                Some(i) => i + 1,
                None    => return glob_match(&pat[1..], text, allow_sep),
            };
            if text.is_empty() { return false; }
            let c = text[0];
            let cls = &pat[1..close];
            let (negate, cls) = if cls.first() == Some(&b'!') || cls.first() == Some(&b'^') {
                (true, &cls[1..])
            } else {
                (false, cls)
            };
            let matched = class_match(cls, c);
            if matched == negate { return false; }
            glob_match(&pat[close+1..], &text[1..], allow_sep)
        }
        Some(&p) => {
            if text.is_empty() { return false; }
            if p != text[0] { return false; }
            glob_match(&pat[1..], &text[1..], allow_sep)
        }
    }
}

fn class_match(cls: &[u8], c: u8) -> bool {
    let mut i = 0;
    while i < cls.len() {
        if i + 2 < cls.len() && cls[i+1] == b'-' {
            if c >= cls[i] && c <= cls[i+2] { return true; }
            i += 3;
        } else {
            if cls[i] == c { return true; }
            i += 1;
        }
    }
    false
}

fn name_match(pat: &str, name: &str, ci: bool) -> bool {
    let (p, n) = if ci {
        (pat.to_lowercase(), name.to_lowercase())
    } else {
        (pat.to_owned(), name.to_owned())
    };
    glob_match(p.as_bytes(), n.as_bytes(), false)
}

fn path_match(pat: &str, full: &str, ci: bool) -> bool {
    // Normalise separators to /
    let norm_full = full.replace('\\', "/");
    let norm_pat  = pat.replace('\\', "/");
    let (p, n) = if ci {
        (norm_pat.to_lowercase(), norm_full.to_lowercase())
    } else {
        (norm_pat, norm_full)
    };
    glob_match(p.as_bytes(), n.as_bytes(), true)
}

// Size parsing

fn parse_size(s: &str) -> Option<(u64, Cmp)> {
    let (cmp, rest) = if s.starts_with('+') {
        (Cmp::Gt, &s[1..])
    } else if s.starts_with('-') {
        (Cmp::Lt, &s[1..])
    } else {
        (Cmp::Eq, s)
    };
    let (num_str, mult) = if let Some(r) = rest.strip_suffix('c') {
        (r, 1u64)
    } else if let Some(r) = rest.strip_suffix('k') {
        (r, 1024)
    } else if let Some(r) = rest.strip_suffix('M') {
        (r, 1024 * 1024)
    } else if let Some(r) = rest.strip_suffix('G') {
        (r, 1024 * 1024 * 1024)
    } else {
        (rest, 512)
    };
    let n: u64 = num_str.parse().ok()?;
    Some((n * mult, cmp))
}

fn parse_mtime(s: &str) -> Option<(i64, Cmp)> {
    let (cmp, rest) = if s.starts_with('+') {
        (Cmp::Gt, &s[1..])
    } else if s.starts_with('-') {
        (Cmp::Lt, &s[1..])
    } else {
        (Cmp::Eq, s)
    };
    let n: i64 = rest.parse().ok()?;
    Some((n, cmp))
}

// CLI parsing

fn parse_args() -> Result<Opts, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();

    if raw.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        std::process::exit(0);
    }

    let mut roots: Vec<PathBuf> = Vec::new();
    let _preds: Vec<Pred>    = Vec::new();
    let mut action = None::<Action>;
    let mut maxdepth = None::<u32>;
    let mut mindepth = None::<u32>;
    let mut follow   = false;
    let mut build_index = false;
    let mut locate_pat: Option<String> = None;
    let mut locate_ci = false;
    let mut locate_db = db_default();
    let mut scan_paths: Vec<String> = Vec::new();

    let mut i = 0;
    while i < raw.len() {
        let s = &raw[i];
        if s == "!" || s.starts_with('-') { break; }
        roots.push(PathBuf::from(s));
        i += 1;
    }
    if roots.is_empty() { roots.push(PathBuf::from(".")); }

    let mut and_group: Vec<Pred> = Vec::new();
    let mut or_groups: Vec<Vec<Pred>> = Vec::new();

    macro_rules! next_arg {
        ($what:expr) => {{
            i += 1;
            raw.get(i).ok_or_else(|| format!("{}: requires an argument", $what))?
        }};
    }

    while i < raw.len() {
        let s = raw[i].as_str();
        match s {
            "-not" | "!" => {
                i += 1;
                let inner = parse_single_pred(&raw, &mut i)?;
                and_group.push(Pred::Not(Box::new(inner)));
            }
            "-and" | "-a" => {}
            "-or"  | "-o" => {
                or_groups.push(and_group.drain(..).collect());
            }

            // --- global options ---
            "-maxdepth" => {
                let v = next_arg!("-maxdepth");
                maxdepth = Some(v.parse().map_err(|_| "-maxdepth: not a number".to_string())?);
            }
            "-mindepth" => {
                let v = next_arg!("-mindepth");
                mindepth = Some(v.parse().map_err(|_| "-mindepth: not a number".to_string())?);
            }
            "-L" | "--follow" => follow = true,

            "-name"  => { let v = next_arg!("-name");  and_group.push(Pred::Name { pat: v.clone(), ci: false }); }
            "-iname" => { let v = next_arg!("-iname"); and_group.push(Pred::Name { pat: v.clone(), ci: true  }); }
            "-path"  => { let v = next_arg!("-path");  and_group.push(Pred::Path { pat: v.clone(), ci: false }); }
            "-ipath" => { let v = next_arg!("-ipath"); and_group.push(Pred::Path { pat: v.clone(), ci: true  }); }
            "-type"  => {
                let v = next_arg!("-type");
                let kind = match v.as_str() {
                    "f" => TypeKind::File,
                    "d" => TypeKind::Dir,
                    "l" => TypeKind::Link,
                    _   => return Err(format!("-type: unknown type '{}'", v)),
                };
                and_group.push(Pred::Type { kind });
            }
            "-size" => {
                let v = next_arg!("-size");
                let (bytes, cmp) = parse_size(v).ok_or_else(|| format!("-size: bad value '{}'", v))?;
                and_group.push(Pred::Size { bytes, cmp });
            }
            "-mtime" => {
                let v = next_arg!("-mtime");
                let (days, cmp) = parse_mtime(v).ok_or_else(|| format!("-mtime: bad value '{}'", v))?;
                and_group.push(Pred::Mtime { days, cmp });
            }
            "-newer" => {
                let v = next_arg!("-newer");
                and_group.push(Pred::Newer { than: PathBuf::from(v) });
            }
            "-empty" => and_group.push(Pred::Empty),
            "-true"  => and_group.push(Pred::True),
            "-false" => and_group.push(Pred::False),

            "--build-index" => {
                build_index = true;
            }
            "--locate" => {
                let v = next_arg!("--locate");
                locate_pat = Some(v.clone());
            }
            "--locate-i" | "--ilocate" => {
                locate_ci = true;
                let v = next_arg!("--locate-i");
                locate_pat = Some(v.clone());
            }
            "--locate-db" => {
                let v = next_arg!("--locate-db");
                locate_db = PathBuf::from(v);
            }

            "-print"  => action = Some(Action::Print),
            "-print0" => action = Some(Action::Print0),
            "-ls"     => action = Some(Action::Ls),
            "-delete" => action = Some(Action::Delete),
            "-exec"   => {
                i += 1;
                let mut cmd: Vec<String> = Vec::new();
                while i < raw.len() {
                    if raw[i] == ";" || raw[i] == "\\;" { break; }
                    cmd.push(raw[i].clone());
                    i += 1;
                }
                if cmd.is_empty() { return Err("-exec: no command given".into()); }
                action = Some(Action::Exec(cmd));
            }
            other if build_index && !other.starts_with('-') => scan_paths.push(other.to_string()),
            other => return Err(format!("unknown option: {}", other)),
        }
        i += 1;
    }

    or_groups.push(and_group.drain(..).collect());
    let pred = if or_groups.len() == 1 {
        let mut grp = or_groups.remove(0);
        if grp.len() == 1 { grp.remove(0) } else { Pred::And(grp) }
    } else {
        Pred::Or(
            or_groups.into_iter()
                .map(|g| if g.len() == 1 { g.into_iter().next().unwrap() } else { Pred::And(g) })
                .collect()
        )
    };

    Ok(Opts {
        roots,
        pred,
        action: action.unwrap_or(Action::Print),
        maxdepth,
        mindepth,
        follow,
        build_index,
        locate_pat,
        locate_ci,
        locate_db,
    })
}

fn parse_single_pred(raw: &[String], i: &mut usize) -> Result<Pred, String> {
    let s = raw.get(*i).ok_or("! requires a predicate")?;
    match s.as_str() {
        "-name"  => { *i += 1; Ok(Pred::Name { pat: raw.get(*i).ok_or("-name requires arg")?.clone(), ci: false }) }
        "-iname" => { *i += 1; Ok(Pred::Name { pat: raw.get(*i).ok_or("-iname requires arg")?.clone(), ci: true  }) }
        "-type"  => {
            *i += 1;
            let k = raw.get(*i).ok_or("-type requires arg")?;
            Ok(Pred::Type { kind: match k.as_str() {
                "f" => TypeKind::File, "d" => TypeKind::Dir, "l" => TypeKind::Link,
                _   => return Err(format!("-type: unknown type '{}'", k)),
            }})
        }
        "-empty" => Ok(Pred::Empty),
        "-true"  => Ok(Pred::True),
        "-false" => Ok(Pred::False),
        other => Err(format!("! cannot negate '{}'", other)),
    }
}

fn print_help() {
    println!("Usage: sfind [PATH...] [EXPRESSION]");
    println!();
    println!("Search for files in a directory hierarchy.");
    println!("PATH defaults to the current directory (searches recursively).");
    println!();
    println!("Options:");
    println!("  -maxdepth N        Descend at most N directory levels");
    println!("  -mindepth N        Do not apply tests/actions at levels less than N");
    println!("  -L                 Follow symbolic links");
    println!();
    println!("Tests:");
    println!("  -name PATTERN      File name matches glob (case-sensitive; *, ?, [a-z])");
    println!("  -iname PATTERN     File name matches glob (case-insensitive)");
    println!("  -path PATTERN      Full path matches glob");
    println!("  -ipath PATTERN     Full path matches glob (case-insensitive)");
    println!("  -type f|d|l        File type: f=file, d=directory, l=symlink");
    println!("  -size [+/-]N[ckMG] File size (c=bytes, k=KiB, M=MiB, G=GiB)");
    println!("  -mtime [+/-]N      Modified N days ago (+N=more than, -N=less than)");
    println!("  -newer FILE        Newer than FILE");
    println!("  -empty             Empty file or empty directory");
    println!();
    println!("Operators:");
    println!("  -not / !           Negate next test");
    println!("  -and / -a          AND (implicit between tests)");
    println!("  -or  / -o          OR");
    println!();
    println!("Actions:");
    println!("  -print             Print path (default)");
    println!("  -print0            Print path followed by null byte (for xargs -0)");
    println!("  -ls                Print with size and date");
    println!("  -delete            Delete matching files/empty dirs");
    println!("  -exec CMD {{}} ;    Run CMD with {{}} replaced by the found path");
    println!();
    println!("Examples:");
    println!("  sfind C:\\Users -name \"File*\"");
    println!("  sfind C:\\ -name \"*.log\" -type f");
    println!("  sfind C:\\ -type f -size +100M");
    println!("  sfind C:\\ -mtime -7 -name \"*.tmp\"");
    println!("  sfind C:\\repos -name target -type d -maxdepth 3");
    println!("  sfind C:\\ -iname \"*.TXT\" -exec cat {{}} ;");
    println!("  sfind C:\\logs -type f -mtime +30 -delete");
    println!("  sfind C:\\src -name \"*.rs\" -not -path \"*\\target\\*\"");
}

// Predicate evaluation

fn eval(pred: &Pred, path: &Path, meta: &Metadata, depth: u32) -> bool {
    match pred {
        Pred::True  => true,
        Pred::False => false,
        Pred::Not(p) => !eval(p, path, meta, depth),
        Pred::And(ps) => ps.iter().all(|p| eval(p, path, meta, depth)),
        Pred::Or(ps)  => ps.iter().any(|p| eval(p, path, meta, depth)),

        Pred::Name { pat, ci } => {
            let name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            name_match(pat, name, *ci)
        }
        Pred::Path { pat, ci } => {
            let full = path.to_string_lossy();
            path_match(pat, &full, *ci)
        }
        Pred::Type { kind } => match kind {
            TypeKind::File => meta.is_file(),
            TypeKind::Dir  => meta.is_dir(),
            TypeKind::Link => meta.is_symlink(),
        },
        Pred::Size { bytes, cmp } => {
            let sz = meta.len();
            match cmp {
                Cmp::Lt => sz < *bytes,
                Cmp::Eq => sz == *bytes,
                Cmp::Gt => sz > *bytes,
            }
        }
        Pred::Mtime { days, cmp } => {
            let Ok(modified) = meta.modified() else { return false; };
            let Ok(elapsed)  = SystemTime::now().duration_since(modified) else { return false; };
            let file_days = (elapsed.as_secs() / 86400) as i64;
            match cmp {
                Cmp::Lt => file_days < *days,
                Cmp::Eq => file_days == *days,
                Cmp::Gt => file_days > *days,
            }
        }
        Pred::Newer { than } => {
            let Ok(this_mod)  = meta.modified() else { return false; };
            let Ok(ref_meta)  = fs::metadata(than) else { return false; };
            let Ok(ref_mod)   = ref_meta.modified() else { return false; };
            this_mod > ref_mod
        }
        Pred::Empty => {
            if meta.is_file() {
                meta.len() == 0
            } else if meta.is_dir() {
                fs::read_dir(path).map(|mut d| d.next().is_none()).unwrap_or(false)
            } else {
                false
            }
        }
    }
}

// Actions

fn do_action(action: &Action, path: &Path, meta: &Metadata, out: &mut impl Write) -> bool {
    match action {
        Action::Print => {
            let _ = writeln!(out, "{}", path.display());
        }
        Action::Print0 => {
            let _ = write!(out, "{}\0", path.display());
        }
        Action::Ls => {
            print_ls(path, meta, out);
        }
        Action::Delete => {
            let res = if meta.is_dir() {
                fs::remove_dir(path)
            } else {
                fs::remove_file(path)
            };
            if let Err(e) = res {
                eprintln!("find: {}: {}", path.display(), e);
            }
        }
        Action::Exec(cmd) => {
            let path_str = path.to_string_lossy();
            let args: Vec<String> = cmd[1..].iter()
                .map(|a| if a == "{}" { path_str.as_ref().to_owned() } else { a.clone() })
                .collect();
            let _ = std::process::Command::new(&cmd[0]).args(&args).status();
        }
    }
    true
}

fn print_ls(path: &Path, meta: &Metadata, out: &mut impl Write) {
    let size = meta.len();
    let kind = if meta.is_dir()     { 'd' }
               else if meta.is_symlink() { 'l' }
               else                 { '-' };

    let mtime_str = meta.modified()
        .ok()
        .and_then(|t| {
            let dur = t.duration_since(SystemTime::UNIX_EPOCH).ok()?;
            let secs = dur.as_secs();
            // Format as "Jan  2 15:04" or "Jan  2  2023"
            let days_since_epoch = secs / 86400;
            // Very basic date display using day-of-year approximation
            let year = 1970 + days_since_epoch / 365;
            let day_of_year = days_since_epoch % 365;
            let (month, day) = approx_month_day(day_of_year as u32);
            let h = (secs % 86400) / 3600;
            let m = (secs % 3600) / 60;
            // Show time if within ~6 months, else show year
            let now_secs = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
            if now_secs.saturating_sub(secs) < 180 * 86400 {
                Some(format!("{} {:2} {:02}:{:02}", month, day, h, m))
            } else {
                Some(format!("{} {:2}  {}", month, day, year))
            }
        })
        .unwrap_or_else(|| "--- -- --:--".to_string());

    let _ = writeln!(out, "{}{} {:>12}  {}  {}", kind, "rwxr-xr-x", size, mtime_str, path.display());
}

fn approx_month_day(day_of_year: u32) -> (&'static str, u32) {
    const MONTHS: [(&str, u32); 12] = [
        ("Jan", 31), ("Feb", 28), ("Mar", 31), ("Apr", 30),
        ("May", 31), ("Jun", 30), ("Jul", 31), ("Aug", 31),
        ("Sep", 30), ("Oct", 31), ("Nov", 30), ("Dec", 31),
    ];
    let mut rem = day_of_year;
    for (name, days) in &MONTHS {
        if rem < *days { return (name, rem + 1); }
        rem -= days;
    }
    ("Dec", 31)
}

// Directory walking

fn walk(
    path:  &Path,
    depth: u32,
    opts:  &Opts,
    out:   &mut impl Write,
) {
    let meta = if opts.follow {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    };
    let meta = match meta {
        Ok(m)  => m,
        Err(e) => {
            if e.kind() != io::ErrorKind::PermissionDenied {
                eprintln!("sfind: {}: {}", path.display(), e);
            }
            return;
        }
    };

    // Apply mindepth/maxdepth to tests and actions
    let apply = opts.mindepth.map_or(true, |min| depth >= min);

    if apply && eval(&opts.pred, path, &meta, depth) {
        do_action(&opts.action, path, &meta, out);
    }

    if meta.is_dir() && !meta.is_symlink() {
        if let Some(max) = opts.maxdepth {
            if depth >= max { return; }
        }
        let entries = match fs::read_dir(path) {
            Ok(e)  => e,
            Err(e) => {
                if e.kind() != io::ErrorKind::PermissionDenied {
                    eprintln!("sfind: {}: {}", path.display(), e);
                }
                return;
            }
        };
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            walk(&entry.path(), depth + 1, opts, out);
        }
    }
}

fn db_default() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("USERPROFILE").map(|h| format!("{}\\AppData\\Local", h)))
        .unwrap_or_else(|_| "C:\\Users\\Default\\AppData\\Local".into());
    PathBuf::from(base).join("stools").join("locate.db")
}

// Index building

const SKIP_DIRS: &[&str] = &[
    "$Recycle.Bin", "System Volume Information", "Recovery",
    "$WinREAgent", "WpSystem", "MSOCache", "Config.Msi",
];

fn local_drives() -> Vec<PathBuf> {
    ('A'..='Z').map(|c| PathBuf::from(format!("{}:\\", c))).filter(|p| p.exists()).collect()
}

fn index_scan(dir: &Path, paths: &mut Vec<String>, count: &mut u64, depth: u32) {
    if depth > 64 { return; }
    let entries = match std::fs::read_dir(dir) { Ok(e) => e, Err(_) => return };
    for entry in entries.flatten() {
        let path = entry.path();
        let s = path.to_string_lossy();
        let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
        paths.push(s);
        *count += 1;
        if *count % 50_000 == 0 {
            eprint!("\r  {:>9} files...", count);
            let _ = io::stderr().flush();
        }
        let ft = match entry.file_type() { Ok(t) => t, Err(_) => continue };
        if ft.is_dir() && !ft.is_symlink() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !SKIP_DIRS.contains(&name) { index_scan(&entry.path(), paths, count, depth + 1); }
        }
    }
}

fn do_build_index(db_path: &Path, scan_paths: &[String]) {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::BufWriter;
    use std::time::{Instant, UNIX_EPOCH};

    let roots: Vec<PathBuf> = if scan_paths.is_empty() {
        let drives = local_drives();
        println!("Scanning {} drive(s): {}",
            drives.len(),
            drives.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join("  "));
        drives
    } else {
        scan_paths.iter().map(PathBuf::from).collect()
    };

    let mut paths: Vec<String> = Vec::with_capacity(1_000_000);
    let mut total = 0u64;
    let t0 = Instant::now();

    for root in &roots {
        eprint!("\rScanning {}...            ", root.display());
        let _ = io::stderr().flush();
        index_scan(root, &mut paths, &mut total, 0);
    }
    eprintln!("\r  {} files found, sorting...          ", total);
    paths.sort_unstable_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));

    if let Some(parent) = db_path.parent() { let _ = std::fs::create_dir_all(parent); }

    let f = match std::fs::File::create(db_path) {
        Ok(f)  => f,
        Err(e) => { eprintln!("sfind: cannot write {}: {}", db_path.display(), e); std::process::exit(1); }
    };
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut gz = GzEncoder::new(BufWriter::new(f), Compression::default());
    let _ = writeln!(gz, "# locate-db v1");
    let _ = writeln!(gz, "# timestamp={}", ts);
    let _ = writeln!(gz, "# count={}", paths.len());
    for p in &paths { let _ = writeln!(gz, "{}", p); }
    let _ = gz.finish();

    let elapsed = t0.elapsed().as_secs_f64();
    let db_size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    println!("{} files indexed in {:.1}s  (db: {}  {})", total, elapsed,
        db_path.display(), human_size(db_size));
}

fn human_size(b: u64) -> String {
    const K: u64 = 1024; const M: u64 = K*1024; const G: u64 = M*1024;
    if b >= G { format!("{:.1}GB", b as f64/G as f64) }
    else if b >= M { format!("{:.1}MB", b as f64/M as f64) }
    else if b >= K { format!("{:.1}KB", b as f64/K as f64) }
    else { format!("{}B", b) }
}

// Locate search

fn do_locate_search(opts: &Opts, pattern: &str) {
    use flate2::read::GzDecoder;
    use regex::Regex;
    use std::io::{BufRead, BufReader};

    let f = match std::fs::File::open(&opts.locate_db) {
        Ok(f)  => f,
        Err(_) => {
            eprintln!("sfind: index not found: {}", opts.locate_db.display());
            eprintln!("Run 'sfind --build-index' or 'locate --updatedb' first.");
            std::process::exit(1);
        }
    };

    let use_glob = pattern.contains('*') || pattern.contains('?') || pattern.contains('[');
    let re: Option<Regex> = if use_glob {
        None
    } else {
        None
    };
    let pat_lower = pattern.to_ascii_lowercase();

    let rdr = BufReader::new(GzDecoder::new(BufReader::new(f)));
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let _ = re;

    for line in rdr.lines() {
        let line = match line { Ok(l) => l, Err(_) => continue };
        if line.starts_with('#') { continue; }

        let subject = line.rsplit(|c| c == '\\' || c == '/').next().unwrap_or(&line);

        let hit = if use_glob {
            let (p, t) = if opts.locate_ci {
                (pattern.to_ascii_lowercase(), subject.to_ascii_lowercase())
            } else {
                (pattern.to_owned(), subject.to_owned())
            };
            locate_glob(p.as_bytes(), t.as_bytes())
        } else if opts.locate_ci {
            subject.to_ascii_lowercase().contains(&pat_lower)
        } else {
            subject.contains(pattern)
        };

        if !hit { continue; }

        let path = Path::new(&line);
        let meta = match fs::symlink_metadata(path) {
            Ok(m)  => m,
            Err(_) => continue,
        };

        if !eval(&opts.pred, path, &meta, 0) { continue; }

        do_action(&opts.action, path, &meta, &mut out);
    }
    let _ = out.flush();
}

fn locate_glob(pat: &[u8], text: &[u8]) -> bool {
    match pat.first() {
        None        => text.is_empty(),
        Some(&b'*') => {
            let rest = &pat[1..];
            if rest.is_empty() { return true; }
            for i in 0..=text.len() { if locate_glob(rest, &text[i..]) { return true; } }
            false
        }
        Some(&b'?') => !text.is_empty() && locate_glob(&pat[1..], &text[1..]),
        Some(&p)    => !text.is_empty() && p == text[0] && locate_glob(&pat[1..], &text[1..]),
    }
}

// Main

fn main() {
    let opts = match parse_args() {
        Ok(o)  => o,
        Err(e) => {
            eprintln!("sfind: {}", e);
            eprintln!("Try 'sfind --help' for usage.");
            std::process::exit(1);
        }
    };

    if opts.build_index {
        let scan_paths: Vec<String> = opts.roots.iter()
            .filter(|p| *p != &PathBuf::from("."))
            .map(|p| p.display().to_string())
            .collect();
        do_build_index(&opts.locate_db, &scan_paths);
        return;
    }

    if let Some(ref pat) = opts.locate_pat.clone() {
        do_locate_search(&opts, pat);
        return;
    }

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    for root in &opts.roots {
        walk(root, 0, &opts, &mut out);
    }
}
