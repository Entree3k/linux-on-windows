use std::io::{self, Write};

fn parse_escape(s: &str) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'\\' => { out.push(b'\\'); }
            b'a'  => { out.push(b'\x07'); }
            b'b'  => { out.push(b'\x08'); }
            b'c'  => { return (out, true); } // stop all output
            b'e'  => { out.push(b'\x1B'); }
            b'f'  => { out.push(b'\x0C'); }
            b'n'  => { out.push(b'\n'); }
            b'r'  => { out.push(b'\r'); }
            b't'  => { out.push(b'\t'); }
            b'v'  => { out.push(b'\x0B'); }
            b'0'  => {
                // Octal \0NNN — up to 3 octal digits after the 0
                let mut val = 0u32;
                let mut j = 0;
                while j < 3 && i + 1 + j < bytes.len()
                    && bytes[i + 1 + j] >= b'0' && bytes[i + 1 + j] <= b'7'
                {
                    val = val * 8 + (bytes[i + 1 + j] - b'0') as u32;
                    j += 1;
                }
                i += j;
                out.push(val as u8);
            }
            b'x'  => {
                // Hex \xHH — up to 2 hex digits
                let mut val = 0u32;
                let mut j = 0;
                while j < 2 && i + 1 + j < bytes.len()
                    && bytes[i + 1 + j].is_ascii_hexdigit()
                {
                    let c = bytes[i + 1 + j];
                    val = val * 16 + match c {
                        b'a'..=b'f' => (c - b'a' + 10) as u32,
                        b'A'..=b'F' => (c - b'A' + 10) as u32,
                        _           => (c - b'0') as u32,
                    };
                    j += 1;
                }
                if j > 0 { i += j; out.push(val as u8); }
                else { out.push(b'\\'); out.push(b'x'); }
            }
            c => {
                // Unrecognised escape — pass through literally
                out.push(b'\\');
                out.push(c);
            }
        }
        i += 1;
    }
    (out, false)
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();

    if raw.iter().any(|a| a == "--help") {
        println!("Usage: echo [OPTION]... [STRING]...");
        println!("Echo the STRING(s) to standard output.");
        println!();
        println!("  -n     do not output the trailing newline");
        println!("  -e     enable interpretation of backslash escapes");
        println!("  -E     disable interpretation of backslash escapes (default)");
        println!();
        println!("Escape sequences (with -e):");
        println!("  \\\\   backslash        \\a  alert (bell)     \\b  backspace");
        println!("  \\c   stop output      \\e  escape           \\f  form feed");
        println!("  \\n   newline          \\r  carriage return  \\t  tab");
        println!("  \\v   vertical tab     \\0NNN  octal byte    \\xHH  hex byte");
        return;
    }

    // Parse leading flags (-n, -e, -E, and combinations like -ne).
    // GNU echo: only recognise a leading arg as a flag if every character
    // in it is one of n/e/E. Otherwise treat it as a literal string.
    let mut newline    = true;
    let mut escape     = false;
    let mut arg_start  = 0usize;

    'outer: for (i, arg) in raw.iter().enumerate() {
        if !arg.starts_with('-') || arg.len() < 2 {
            arg_start = i;
            break;
        }
        let chars = &arg[1..];
        if !chars.chars().all(|c| c == 'n' || c == 'e' || c == 'E') {
            arg_start = i;
            break;
        }
        for c in chars.chars() {
            match c {
                'n' => newline = false,
                'e' => escape  = true,
                'E' => escape  = false,
                _   => { arg_start = i; break 'outer; }
            }
        }
        arg_start = i + 1;
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();

    let args = &raw[arg_start..];
    let mut stop = false;

    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            out.write_all(b" ").ok();
        }
        if escape {
            let (bytes, halt) = parse_escape(arg);
            out.write_all(&bytes).ok();
            if halt { stop = true; break; }
        } else {
            out.write_all(arg.as_bytes()).ok();
        }
    }

    if !stop && newline {
        out.write_all(b"\n").ok();
    }
    out.flush().ok();
}
