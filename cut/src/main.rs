use std::io::{self, BufRead, Write};

struct Args {
    delimiter:      char,
    fields:         Option<Vec<(usize, usize)>>, // 1-based inclusive ranges
    chars:          Option<Vec<(usize, usize)>>,
    bytes:          Option<Vec<(usize, usize)>>,
    complement:     bool,
    only_delimited: bool,
}

fn parse_list(s: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.contains('-') {
            let mut it = part.splitn(2, '-');
            let lo: usize = it.next().and_then(|v| v.parse().ok()).unwrap_or(1);
            let hi: usize = it.next().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
            ranges.push((lo, hi));
        } else if let Ok(n) = part.parse::<usize>() {
            ranges.push((n, n));
        }
    }
    ranges.sort();
    ranges
}

fn in_ranges(pos: usize, ranges: &[(usize, usize)], complement: bool) -> bool {
    let hit = ranges.iter().any(|&(lo, hi)| pos >= lo && pos <= hi);
    if complement { !hit } else { hit }
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|a| a == "--help") {
        eprintln!("Usage: cut OPTION... [FILE]...");
        eprintln!("  -d DELIM   field delimiter (default: tab)");
        eprintln!("  -f LIST    select fields (e.g. 1,3  2-4  1-  -3)");
        eprintln!("  -c LIST    select character positions");
        eprintln!("  -b LIST    select byte positions");
        eprintln!("  --complement   invert selection");
        eprintln!("  -s, --only-delimited   skip lines with no delimiter");
        std::process::exit(0);
    }

    let mut a = Args { delimiter: '\t', fields: None, chars: None, bytes: None, complement: false, only_delimited: false };
    let mut i = 0;
    while i < raw.len() {
        let s = raw[i].as_str();
        match s {
            "--complement"     => a.complement     = true,
            "-s" | "--only-delimited" => a.only_delimited = true,
            "-d" | "--delimiter" => {
                i += 1;
                if let Some(v) = raw.get(i) { a.delimiter = v.chars().next().unwrap_or('\t'); }
            }
            "-f" | "--fields" => {
                i += 1;
                if let Some(v) = raw.get(i) { a.fields = Some(parse_list(v)); }
            }
            "-c" | "--characters" => {
                i += 1;
                if let Some(v) = raw.get(i) { a.chars = Some(parse_list(v)); }
            }
            "-b" | "--bytes" => {
                i += 1;
                if let Some(v) = raw.get(i) { a.bytes = Some(parse_list(v)); }
            }
            _ if s.starts_with("-d") && s.len() > 2 => { a.delimiter = s.chars().nth(2).unwrap_or('\t'); }
            _ if s.starts_with("-f") && s.len() > 2 => { a.fields = Some(parse_list(&s[2..])); }
            _ if s.starts_with("-c") && s.len() > 2 => { a.chars = Some(parse_list(&s[2..])); }
            _ if s.starts_with("-b") && s.len() > 2 => { a.bytes = Some(parse_list(&s[2..])); }
            _ => {}
        }
        i += 1;
    }
    a
}

fn process_line(line: &str, args: &Args, out: &mut dyn Write) {
    if let Some(ref ranges) = args.fields {
        let parts: Vec<&str> = line.split(args.delimiter).collect();
        if parts.len() == 1 && !line.contains(args.delimiter) {
            if args.only_delimited { return; }
            writeln!(out, "{}", line).ok();
            return;
        }
        let selected: Vec<&str> = parts.iter().enumerate()
            .filter(|(i, _)| in_ranges(i + 1, ranges, args.complement))
            .map(|(_, v)| *v)
            .collect();
        writeln!(out, "{}", selected.join(&args.delimiter.to_string())).ok();
    } else if let Some(ref ranges) = args.chars {
        let selected: String = line.chars().enumerate()
            .filter(|(i, _)| in_ranges(i + 1, ranges, args.complement))
            .map(|(_, c)| c)
            .collect();
        writeln!(out, "{}", selected).ok();
    } else if let Some(ref ranges) = args.bytes {
        let bytes = line.as_bytes();
        let selected: Vec<u8> = bytes.iter().enumerate()
            .filter(|(i, _)| in_ranges(i + 1, ranges, args.complement))
            .map(|(_, &b)| b)
            .collect();
        out.write_all(&selected).ok();
        out.write_all(b"\n").ok();
    } else {
        writeln!(out, "{}", line).ok();
    }
}

fn main() {
    let args = parse_args();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.unwrap_or_default();
        process_line(&line, &args, &mut out);
    }
}
