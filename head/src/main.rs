use clap::{Arg, ArgAction, Command};
use colored::*;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;

fn print_lines(reader: &mut dyn BufRead, n: usize, out: &mut dyn Write) -> io::Result<()> {
    let mut line = String::new();
    for _ in 0..n {
        line.clear();
        if reader.read_line(&mut line)? == 0 { break; }
        out.write_all(line.as_bytes())?;
        // Ensure we always end with a newline for display consistency
        if !line.ends_with('\n') {
            out.write_all(b"\n")?;
        }
    }
    Ok(())
}

fn print_bytes(reader: &mut dyn Read, n: usize, out: &mut dyn Write) -> io::Result<()> {
    let mut remaining = n;
    let mut buf = vec![0u8; 8 * 1024];
    while remaining > 0 {
        let want = buf.len().min(remaining);
        let got = reader.read(&mut buf[..want])?;
        if got == 0 { break; }
        out.write_all(&buf[..got])?;
        remaining -= got;
    }
    Ok(())
}

fn main() {
    let matches = Command::new("head")
        .version("1.0.0")
        .about("Output the first part of files")
        .after_help(
            "EXAMPLES\n\
             \x20 head file.txt             # first 10 lines (default)\n\
             \x20 head -n 20 file.txt       # first 20 lines\n\
             \x20 head -c 512 file.txt      # first 512 bytes\n\
             \x20 head -n 5 a.txt b.txt     # first 5 lines of each, with headers\n\
             \x20 type file.txt | head -3   # from stdin",
        )
        .arg(
            Arg::new("lines")
                .short('n')
                .long("lines")
                .value_name("N")
                .default_value("10")
                .conflicts_with("bytes")
                .help("Number of lines to print (default: 10)"),
        )
        .arg(
            Arg::new("bytes")
                .short('c')
                .long("bytes")
                .value_name("N")
                .help("Print first N bytes instead of lines"),
        )
        .arg(
            Arg::new("quiet")
                .short('q')
                .long("quiet")
                .action(ArgAction::SetTrue)
                .help("Never print file headers"),
        )
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .action(ArgAction::SetTrue)
                .help("Always print file headers"),
        )
        .arg(
            Arg::new("files")
                .value_name("FILE")
                .num_args(0..)
                .help("Files to read (default: stdin)"),
        )
        .get_matches();

    let quiet   = matches.get_flag("quiet");
    let verbose = matches.get_flag("verbose");

    let byte_count: Option<usize> = matches
        .get_one::<String>("bytes")
        .and_then(|s| s.parse().ok());

    let line_count: usize = matches
        .get_one::<String>("lines")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let files: Vec<String> = matches
        .get_many::<String>("files")
        .unwrap_or_default()
        .cloned()
        .collect();

    let show_header = verbose || (!quiet && files.len() > 1);

    let stdout = io::stdout();
    let mut out = stdout.lock();

    if files.is_empty() {
        let stdin = io::stdin();
        let mut lock = stdin.lock();
        if let Some(n) = byte_count {
            print_bytes(&mut lock, n, &mut out).ok();
        } else {
            print_lines(&mut lock, line_count, &mut out).ok();
        }
        return;
    }

    for (i, path_str) in files.iter().enumerate() {
        if show_header {
            if i > 0 { writeln!(out).ok(); }
            writeln!(out, "{}", format!("==> {} <==", path_str).cyan()).ok();
        }

        match File::open(Path::new(path_str)) {
            Err(e) => eprintln!("{}: {}: {}", "head".bold(), path_str.yellow(), e),
            Ok(f) => {
                let mut reader = BufReader::new(f);
                if let Some(n) = byte_count {
                    print_bytes(&mut reader, n, &mut out).ok();
                } else {
                    print_lines(&mut reader, line_count, &mut out).ok();
                }
            }
        }
    }
}
