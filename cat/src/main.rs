//! cat - Faithful GNU cat behaviour

use std::io::{self, BufRead, BufReader, Read, Write};

struct Args {
    number:          bool,   // -n: number every line
    number_nonblank: bool,   // -b: number non-blank lines (overrides -n)
    squeeze_blank:   bool,   // -s: suppress runs of blank lines
    show_ends:       bool,   // -E: append $ to each line
    show_tabs:       bool,   // -T: show tabs as ^I
    show_nonprint:   bool,   // -v: show control chars as ^X, high bytes as M-x
    files:           Vec<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        number: false, number_nonblank: false, squeeze_blank: false,
        show_ends: false, show_tabs: false, show_nonprint: false,
        files: Vec::new(),
    };
    let mut iter = std::env::args().skip(1).peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => { print_help(); std::process::exit(0); }
            "-A" | "--show-all"       => { a.show_nonprint = true; a.show_ends = true; a.show_tabs = true; }
            "-e"                      => { a.show_nonprint = true; a.show_ends = true; }
            "-t"                      => { a.show_nonprint = true; a.show_tabs = true; }
            "-E" | "--show-ends"      => a.show_ends = true,
            "-T" | "--show-tabs"      => a.show_tabs = true,
            "-v" | "--show-nonprinting" => a.show_nonprint = true,
            "-n" | "--number"         => a.number = true,
            "-b" | "--number-nonblank"=> a.number_nonblank = true,
            "-s" | "--squeeze-blank"  => a.squeeze_blank = true,
            s if s.starts_with('-') && s.len() > 1 && !s.starts_with("--") => {
                // combined short flags: -ns, -bsE, etc.
                for c in s.chars().skip(1) {
                    match c {
                        'A' => { a.show_nonprint = true; a.show_ends = true; a.show_tabs = true; }
                        'e' => { a.show_nonprint = true; a.show_ends = true; }
                        't' => { a.show_nonprint = true; a.show_tabs = true; }
                        'E' => a.show_ends = true,
                        'T' => a.show_tabs = true,
                        'v' => a.show_nonprint = true,
                        'n' => a.number = true,
                        'b' => a.number_nonblank = true,
                        's' => a.squeeze_blank = true,
                        _   => {}
                    }
                }
            }
            s => a.files.push(s.to_owned()),
        }
    }
    a
}

fn print_help() {
    println!("Usage: cat [OPTION]... [FILE]...");
    println!();
    println!("Concatenate FILE(s) to standard output. With no FILE, or when FILE is -,");
    println!("read standard input.");
    println!();
    println!("Options:");
    println!("  -A, --show-all           equivalent to -vET");
    println!("  -b, --number-nonblank    number nonempty output lines, overrides -n");
    println!("  -e                       equivalent to -vE");
    println!("  -E, --show-ends          display $ at end of each line");
    println!("  -n, --number             number all output lines");
    println!("  -s, --squeeze-blank      suppress repeated empty output lines");
    println!("  -t                       equivalent to -vT");
    println!("  -T, --show-tabs          display TAB characters as ^I");
    println!("  -v, --show-nonprinting   use ^ and M- notation, except for LFD and TAB");
    println!("  -h, --help               show this help");
}

// Per-byte rendering for -v, -E, -T

fn render_byte(b: u8, show_nonprint: bool, show_tabs: bool) -> Option<&'static str> {
    match b {
        b'\t' if show_tabs     => Some("^I"),
        0x00..=0x08 | 0x0B..=0x1F if show_nonprint => {
            // Control chars ^@..^_ (except tab 0x09 and LF 0x0A)
            // We need a static string — build dynamically via thread-local would complicate
            // things, so just handle all 32 directly
            let s: &'static str = match b {
                0x00 => "^@", 0x01 => "^A", 0x02 => "^B", 0x03 => "^C",
                0x04 => "^D", 0x05 => "^E", 0x06 => "^F", 0x07 => "^G",
                0x08 => "^H", 0x0B => "^K", 0x0C => "^L", 0x0D => "^M",
                0x0E => "^N", 0x0F => "^O", 0x10 => "^P", 0x11 => "^Q",
                0x12 => "^R", 0x13 => "^S", 0x14 => "^T", 0x15 => "^U",
                0x16 => "^V", 0x17 => "^W", 0x18 => "^X", 0x19 => "^Y",
                0x1A => "^Z", 0x1B => "^[", 0x1C => "^\\", 0x1D => "^]",
                0x1E => "^^", 0x1F => "^_",
                _ => "",
            };
            Some(s)
        }
        0x7F if show_nonprint  => Some("^?"),
        _   => None,
    }
}

// processing 

fn process<R: Read>(reader: R, args: &Args, line_num: &mut u64, out: &mut impl Write) -> io::Result<()> {
    let needs_line_proc = args.number || args.number_nonblank || args.squeeze_blank
        || args.show_ends || args.show_tabs || args.show_nonprint;

    if !needs_line_proc {
        // Fast path: raw byte copy
        let mut r = BufReader::new(reader);
        let mut buf = [0u8; 65536];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 { break; }
            out.write_all(&buf[..n])?;
        }
        return Ok(());
    }

    let mut prev_blank = false;
    let mut r = BufReader::new(reader);
    let mut raw = Vec::new();

    loop {
        raw.clear();
        let n = r.read_until(b'\n', &mut raw)?;
        if n == 0 { break; }

        // Strip trailing \r\n or \n so we can inspect the line content
        let has_newline = raw.ends_with(b"\n");
        let line: &[u8] = if has_newline {
            let trimmed = if raw.len() >= 2 && raw[raw.len()-2] == b'\r' {
                &raw[..raw.len()-2]
            } else {
                &raw[..raw.len()-1]
            };
            trimmed
        } else {
            &raw[..]
        };

        let is_blank = line.is_empty();

        // -s: squeeze consecutive blank lines
        if args.squeeze_blank && is_blank {
            if prev_blank {
                continue;
            }
            prev_blank = true;
        } else {
            prev_blank = false;
        }

        // Line number prefix
        if args.number_nonblank {
            if !is_blank {
                *line_num += 1;
                write!(out, "{:6}\t", line_num)?;
            }
        } else if args.number {
            *line_num += 1;
            write!(out, "{:6}\t", line_num)?;
        }

        // Line content with optional -v / -T transformations
        if args.show_nonprint || args.show_tabs {
            for &b in line {
                if let Some(esc) = render_byte(b, args.show_nonprint, args.show_tabs) {
                    out.write_all(esc.as_bytes())?;
                } else if b >= 0x80 && args.show_nonprint {
                    // M- notation for high bytes
                    if b >= 0x80 + 0x20 && b < 0x80 + 0x7F {
                        write!(out, "M-{}", (b - 0x80) as char)?;
                    } else if b == 0x80 + 0x7F {
                        out.write_all(b"M-^?")?;
                    } else {
                        write!(out, "M-^{}", (b - 0x80 + b'@') as char)?;
                    }
                } else {
                    out.write_all(&[b])?;
                }
            }
        } else {
            out.write_all(line)?;
        }

        // -E: show $ before newline
        if args.show_ends && has_newline {
            out.write_all(b"$")?;
        }

        if has_newline {
            out.write_all(b"\n")?;
        }
    }
    Ok(())
}

fn main() {
    let args  = parse_args();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut line_num: u64 = 0;

    if args.files.is_empty() {
        // Read stdin
        if let Err(e) = process(io::stdin().lock(), &args, &mut line_num, &mut out) {
            eprintln!("cat: stdin: {}", e);
            std::process::exit(1);
        }
    } else {
        let mut exit_code = 0;
        for name in &args.files {
            let result = if name == "-" {
                process(io::stdin().lock(), &args, &mut line_num, &mut out)
            } else {
                match std::fs::File::open(name) {
                    Ok(f)  => process(f, &args, &mut line_num, &mut out),
                    Err(e) => {
                        eprintln!("cat: {}: {}", name, e);
                        exit_code = 1;
                        continue;
                    }
                }
            };
            if let Err(e) = result {
                // stdout broken pipe is silent exit
                if e.kind() == io::ErrorKind::BrokenPipe {
                    break;
                }
                eprintln!("cat: {}: {}", name, e);
                exit_code = 1;
            }
        }
        out.flush().ok();
        std::process::exit(exit_code);
    }
}
