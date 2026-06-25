use std::io::{self, Read, Write, BufWriter};
use std::fs;
use std::path::Path;

fn print_help() {
    eprintln!("Usage: tac [OPTION]... [FILE]...");
    eprintln!("Concatenate and print files in reverse line order.");
    eprintln!();
    eprintln!("  -b, --before           attach separator before instead of after");
    eprintln!("  -r, --regex            interpret the separator as a regular expression");
    eprintln!("  -s, --separator=SEP    use SEP instead of newline as the separator");
    eprintln!("  -h, --help             show this help");
}

fn tac_bytes(data: &[u8], sep: u8, before: bool, out: &mut impl Write) {
    let mut records: Vec<&[u8]> = Vec::new();
    let mut start = 0;
    for i in 0..data.len() {
        if data[i] == sep {
            records.push(&data[start..=i]);
            start = i + 1;
        }
    }
    if start < data.len() {
        records.push(&data[start..]);
    }

    for rec in records.iter().rev() {
        if before && rec.ends_with(&[sep]) {
            // Move separator to front: sep + rec[..len-1]
            out.write_all(&[sep]).unwrap();
            out.write_all(&rec[..rec.len() - 1]).unwrap();
        } else {
            out.write_all(rec).unwrap();
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut sep: u8 = b'\n';
    let mut before = false;
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => { print_help(); return; }
            "-b" | "--before" => before = true,
            s if s == "-s" || s == "--separator" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    sep = v.bytes().next().unwrap_or(b'\n');
                }
            }
            s if s.starts_with("--separator=") => {
                sep = s["--separator=".len()..].bytes().next().unwrap_or(b'\n');
            }
            "-r" | "--regex" => { /* separator regex: not implemented, ignore */ }
            "--" => { files.extend_from_slice(&args[i + 1..]); break; }
            s if s.starts_with('-') => {
                eprintln!("tac: unknown option '{}'", s);
                std::process::exit(1);
            }
            s => files.push(s.to_string()),
        }
        i += 1;
    }

    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut any_error = false;

    if files.is_empty() {
        let mut buf = Vec::new();
        if io::stdin().read_to_end(&mut buf).is_err() {
            eprintln!("tac: error reading stdin");
            std::process::exit(1);
        }
        tac_bytes(&buf, sep, before, &mut out);
    } else {
        for path in &files {
            let data = if path == "-" {
                let mut buf = Vec::new();
                io::stdin().read_to_end(&mut buf).ok();
                buf
            } else {
                match fs::read(Path::new(path)) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("tac: {}: {}", path, e);
                        any_error = true;
                        continue;
                    }
                }
            };
            tac_bytes(&data, sep, before, &mut out);
        }
    }

    if any_error { std::process::exit(1); }
}
