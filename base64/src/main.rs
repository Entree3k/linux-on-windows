use std::fs::File;
use std::io::{self, Read, Write};

const ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

struct Args {
    decode:        bool,
    wrap:          usize, // 0 = no wrapping
    ignore_garbage: bool,
    file:          Option<String>,
}

fn print_help() {
    println!("Usage: base64 [OPTION]... [FILE]");
    println!();
    println!("Base64 encode or decode FILE, or standard input, to standard output.");
    println!();
    println!("  -d, --decode          decode data");
    println!("  -i, --ignore-garbage  when decoding, ignore non-alphabet characters");
    println!("  -w, --wrap COLS       wrap encoded lines after COLS chars (default 76,");
    println!("                        0 disables line wrapping)");
    println!("  -h, --help            display this help and exit");
    println!();
    println!("With no FILE, or when FILE is -, read standard input.");
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut a = Args { decode: false, wrap: 76, ignore_garbage: false, file: None };

    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        match arg.as_str() {
            "-h" | "--help" => { print_help(); std::process::exit(0); }
            "-d" | "--decode" => a.decode = true,
            "-i" | "--ignore-garbage" => a.ignore_garbage = true,
            "-w" | "--wrap" => {
                i += 1;
                a.wrap = argv.get(i).and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                    eprintln!("base64: invalid wrap size");
                    std::process::exit(1);
                });
            }
            s if s.starts_with("--wrap=") => {
                a.wrap = s["--wrap=".len()..].parse().unwrap_or_else(|_| {
                    eprintln!("base64: invalid wrap size");
                    std::process::exit(1);
                });
            }
            "-" => a.file = Some("-".to_string()),
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("base64: invalid option '{}'", s);
                std::process::exit(1);
            }
            _ => a.file = Some(arg.clone()),
        }
        i += 1;
    }
    a
}

fn read_input(file: &Option<String>) -> Vec<u8> {
    let mut buf = Vec::new();
    match file.as_deref() {
        None | Some("-") => { io::stdin().read_to_end(&mut buf).ok(); }
        Some(path) => {
            File::open(path)
                .and_then(|mut f| f.read_to_end(&mut buf))
                .unwrap_or_else(|e| {
                    eprintln!("base64: {}: {}", path, e);
                    std::process::exit(1);
                });
        }
    }
    buf
}

fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[(n & 63) as usize] as char } else { '=' });
    }
    out
}

fn decode(data: &[u8], ignore_garbage: bool) -> Vec<u8> {
    // Reverse lookup table
    let mut table = [255u8; 256];
    for (i, &c) in ALPHABET.iter().enumerate() {
        table[c as usize] = i as u8;
    }

    let mut out = Vec::with_capacity(data.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in data {
        if c == b'=' { break; }
        if c == b'\n' || c == b'\r' { continue; }
        let v = table[c as usize];
        if v == 255 {
            if ignore_garbage || c == b' ' || c == b'\t' { continue; }
            eprintln!("base64: invalid input");
            std::process::exit(1);
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

fn main() {
    let args = parse_args();
    let input = read_input(&args.file);
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if args.decode {
        let bytes = decode(&input, args.ignore_garbage);
        out.write_all(&bytes).ok();
    } else {
        let encoded = encode(&input);
        if args.wrap == 0 {
            out.write_all(encoded.as_bytes()).ok();
            out.write_all(b"\n").ok();
        } else {
            let bytes = encoded.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let end = (i + args.wrap).min(bytes.len());
                out.write_all(&bytes[i..end]).ok();
                out.write_all(b"\n").ok();
                i = end;
            }
        }
    }
}
