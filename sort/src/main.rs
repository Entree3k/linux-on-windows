use std::io::{self, BufRead, Write};
use std::cmp::Ordering;

struct Args {
    numeric:    bool,
    reverse:    bool,
    unique:     bool,
    ignore_case:bool,
    human:      bool,
    stable:     bool,
    check:      bool,
    key_field:  Option<usize>,
    separator:  Option<char>,
    output:     Option<String>,
    files:      Vec<String>,
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|a| a == "--help") {
        eprintln!("Usage: sort [OPTION]... [FILE]...");
        eprintln!("  -n, --numeric-sort        compare as numbers");
        eprintln!("  -r, --reverse             reverse the sort order");
        eprintln!("  -u, --unique              remove duplicate lines");
        eprintln!("  -i, -f, --ignore-case     case-insensitive sort");
        eprintln!("  -h, --human-numeric-sort  compare 1K < 1M < 1G");
        eprintln!("  -s, --stable              stable sort");
        eprintln!("  -c, --check               check if already sorted");
        eprintln!("  -k N                      sort by field N (1-based)");
        eprintln!("  -t SEP                    field separator (default: whitespace)");
        eprintln!("  -o FILE                   write to FILE instead of stdout");
        std::process::exit(0);
    }

    let mut a = Args {
        numeric: false, reverse: false, unique: false, ignore_case: false,
        human: false, stable: false, check: false, key_field: None,
        separator: None, output: None, files: Vec::new(),
    };

    let mut i = 0;
    while i < raw.len() {
        let s = raw[i].as_str();
        match s {
            "-n" | "--numeric-sort"       => a.numeric      = true,
            "-r" | "--reverse"            => a.reverse      = true,
            "-u" | "--unique"             => a.unique       = true,
            "-i" | "-f" | "--ignore-case" | "--fold-case" => a.ignore_case = true,
            "-h" | "--human-numeric-sort" => a.human        = true,
            "-s" | "--stable"             => a.stable       = true,
            "-c" | "--check"              => a.check        = true,
            "-k" | "--key" => { i += 1; a.key_field = raw.get(i).and_then(|v| v.parse::<usize>().ok()); }
            "-t" | "--field-separator" => { i += 1; a.separator = raw.get(i).and_then(|v| v.chars().next()); }
            "-o" | "--output" => { i += 1; a.output = raw.get(i).cloned(); }
            _ if s.starts_with("-k") && s.len() > 2 => { a.key_field = s[2..].parse().ok(); }
            _ if s.starts_with("-t") && s.len() > 2 => { a.separator = s.chars().nth(2); }
            _ if s.starts_with("--key=")           => { a.key_field = s[6..].parse().ok(); }
            _ if s.starts_with("--field-separator=") => { a.separator = s[18..].chars().next(); }
            _ if s.starts_with("--output=")        => { a.output = Some(s[9..].to_string()); }
            _ if s.starts_with('-') && !s.starts_with("--") && s.len() > 1 => {
                for c in s.chars().skip(1) {
                    match c { 'n' => a.numeric = true, 'r' => a.reverse = true, 'u' => a.unique = true, 'i'|'f' => a.ignore_case = true, 'h' => a.human = true, 's' => a.stable = true, 'c' => a.check = true, _ => {} }
                }
            }
            _ => a.files.push(s.to_string()),
        }
        i += 1;
    }
    a
}

fn parse_human(s: &str) -> f64 {
    let s = s.trim();
    let (num, mult) = if s.ends_with('K') || s.ends_with('k') {
        (&s[..s.len()-1], 1024.0f64)
    } else if s.ends_with('M') || s.ends_with('m') {
        (&s[..s.len()-1], 1024.0 * 1024.0)
    } else if s.ends_with('G') || s.ends_with('g') {
        (&s[..s.len()-1], 1024.0 * 1024.0 * 1024.0)
    } else if s.ends_with('T') || s.ends_with('t') {
        (&s[..s.len()-1], 1024.0 * 1024.0 * 1024.0 * 1024.0)
    } else {
        (s, 1.0)
    };
    num.parse::<f64>().unwrap_or(0.0) * mult
}

fn get_key<'a>(line: &'a str, args: &Args) -> &'a str {
    if let Some(field) = args.key_field {
        let parts: Vec<&str> = if let Some(sep) = args.separator {
            line.split(sep).collect()
        } else {
            line.split_whitespace().collect()
        };
        parts.get(field.saturating_sub(1)).copied().unwrap_or("")
    } else {
        line
    }
}

fn compare_lines(a: &str, b: &str, args: &Args) -> Ordering {
    let ka = get_key(a, args);
    let kb = get_key(b, args);

    let ord = if args.numeric {
        let na: f64 = ka.trim().parse().unwrap_or(0.0);
        let nb: f64 = kb.trim().parse().unwrap_or(0.0);
        na.partial_cmp(&nb).unwrap_or(Ordering::Equal)
    } else if args.human {
        let na = parse_human(ka.trim());
        let nb = parse_human(kb.trim());
        na.partial_cmp(&nb).unwrap_or(Ordering::Equal)
    } else if args.ignore_case {
        ka.to_lowercase().cmp(&kb.to_lowercase())
    } else {
        ka.cmp(kb)
    };

    if args.reverse { ord.reverse() } else { ord }
}

fn read_lines(files: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    if files.is_empty() {
        for line in io::stdin().lock().lines() {
            lines.push(line.unwrap_or_default());
        }
    } else {
        for fname in files {
            match std::fs::File::open(fname) {
                Ok(f) => {
                    for line in io::BufReader::new(f).lines() {
                        lines.push(line.unwrap_or_default());
                    }
                }
                Err(e) => eprintln!("sort: {}: {}", fname, e),
            }
        }
    }
    lines
}

fn main() {
    let args = parse_args();
    let mut lines = read_lines(&args.files);

    if args.check {
        for i in 1..lines.len() {
            if compare_lines(&lines[i-1], &lines[i], &args) == Ordering::Greater {
                eprintln!("sort: -:{}:{}: disorder: {}", i+1, 0, lines[i]);
                std::process::exit(1);
            }
        }
        return;
    }

    if args.stable {
        lines.sort_by(|a, b| compare_lines(a, b, &args));
    } else {
        lines.sort_unstable_by(|a, b| compare_lines(a, b, &args));
    }

    if args.unique {
        lines.dedup_by(|a, b| {
            let ka = get_key(a, &args);
            let kb = get_key(b, &args);
            if args.ignore_case { ka.to_lowercase() == kb.to_lowercase() } else { ka == kb }
        });
    }

    let output: Box<dyn Write> = if let Some(ref fname) = args.output {
        match std::fs::File::create(fname) {
            Ok(f) => Box::new(f),
            Err(e) => { eprintln!("sort: {}: {}", fname, e); std::process::exit(1); }
        }
    } else {
        Box::new(io::stdout())
    };

    let mut out = io::BufWriter::new(output);
    for line in &lines {
        writeln!(out, "{}", line).ok();
    }
}
