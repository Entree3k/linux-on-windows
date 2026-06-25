use std::io::{self, BufRead, Write};
use colored::Colorize;

struct Args {
    count:       bool,
    repeated:    bool,
    unique:      bool,
    ignore_case: bool,
    skip_fields: usize,
    skip_chars:  usize,
}

fn parse_args() -> (Args, Vec<String>) {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|a| a == "--help") {
        eprintln!("Usage: uniq [OPTION]... [INPUT [OUTPUT]]");
        eprintln!("Filter adjacent matching lines from INPUT (or stdin).");
        eprintln!("  -c, --count            prefix each line with its repeat count");
        eprintln!("  -d, --repeated         only print duplicate lines");
        eprintln!("  -u, --unique           only print lines that are not repeated");
        eprintln!("  -i, --ignore-case      ignore case when comparing");
        eprintln!("  -f N, --skip-fields=N  avoid comparing the first N fields");
        eprintln!("  -s N, --skip-chars=N   avoid comparing the first N characters");
        std::process::exit(0);
    }

    let mut a = Args { count: false, repeated: false, unique: false, ignore_case: false, skip_fields: 0, skip_chars: 0 };
    let mut files = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let s = raw[i].as_str();
        match s {
            "-c" | "--count"       => a.count       = true,
            "-d" | "--repeated"    => a.repeated    = true,
            "-u" | "--unique"      => a.unique       = true,
            "-i" | "--ignore-case" => a.ignore_case = true,
            "-f" | "--skip-fields" => { i += 1; a.skip_fields = raw.get(i).and_then(|v| v.parse().ok()).unwrap_or(0); }
            "-s" | "--skip-chars"  => { i += 1; a.skip_chars  = raw.get(i).and_then(|v| v.parse().ok()).unwrap_or(0); }
            _ if s.starts_with("--skip-fields=") => { a.skip_fields = s[14..].parse().unwrap_or(0); }
            _ if s.starts_with("--skip-chars=")  => { a.skip_chars  = s[13..].parse().unwrap_or(0); }
            _ if s.starts_with('-') && s.len() > 1 && !s.starts_with("--") => {
                for c in s.chars().skip(1) {
                    match c { 'c' => a.count = true, 'd' => a.repeated = true, 'u' => a.unique = true, 'i' => a.ignore_case = true, _ => {} }
                }
            }
            _ => files.push(s.to_string()),
        }
        i += 1;
    }
    (a, files)
}

fn comparison_key(line: &str, skip_fields: usize, skip_chars: usize, ignore_case: bool) -> String {
    let mut s = line;
    // Skip fields
    for _ in 0..skip_fields {
        s = s.trim_start();
        s = s.trim_start_matches(|c: char| !c.is_whitespace());
    }
    let mut chars = s.chars();
    for _ in 0..skip_chars { chars.next(); }
    let s = chars.as_str();
    if ignore_case { s.to_lowercase() } else { s.to_string() }
}

fn process<R: BufRead, W: Write>(reader: R, mut writer: W, args: &Args) {
    let mut lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

    if lines.is_empty() { return; }

    let mut i = 0;
    while i < lines.len() {
        let key = comparison_key(&lines[i], args.skip_fields, args.skip_chars, args.ignore_case);
        let mut count = 1usize;
        while i + count < lines.len() {
            let next_key = comparison_key(&lines[i + count], args.skip_fields, args.skip_chars, args.ignore_case);
            if key == next_key { count += 1; } else { break; }
        }

        let should_print = if args.repeated { count > 1 }
            else if args.unique { count == 1 }
            else { true };

        if should_print {
            if args.count {
                writeln!(writer, "{:>7} {}", count.to_string().cyan(), lines[i]).ok();
            } else {
                writeln!(writer, "{}", lines[i]).ok();
            }
        }
        i += count;
    }
}

fn main() {
    let (args, files) = parse_args();
    let stdout = io::stdout();
    let out = stdout.lock();

    if files.is_empty() {
        process(io::stdin().lock(), out, &args);
    } else {
        let input_file = &files[0];
        match std::fs::File::open(input_file) {
            Ok(f) => process(io::BufReader::new(f), out, &args),
            Err(e) => { eprintln!("uniq: {}: {}", input_file, e); std::process::exit(1); }
        }
    }
}
