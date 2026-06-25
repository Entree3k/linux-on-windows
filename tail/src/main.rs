use clap::{Arg, ArgAction, Command};
use colored::*;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

const FOLLOW_INTERVAL: Duration = Duration::from_millis(200);

fn preprocess_args() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for arg in std::env::args() {
        if let Some(digits) = arg.strip_prefix('-') {
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                out.push("-n".to_string());
                out.push(digits.to_string());
                continue;
            }
        }
        out.push(arg);
    }
    out
}

fn last_n_lines(path: &Path, n: usize) -> io::Result<Vec<String>> {
    if n == 0 {
        return Ok(vec![]);
    }

    let mut file = File::open(path)?;
    let size = file.metadata()?.len();

    if size == 0 {
        return Ok(vec![]);
    }

    if size <= 1_024 * 1_024 {
        let mut content = String::new();
        BufReader::new(file).read_to_string(&mut content)?;
        let lines: Vec<String> = content.lines().map(str::to_owned).collect();
        let start = lines.len().saturating_sub(n);
        return Ok(lines[start..].to_owned());
    }

    const BLOCK: u64 = 64 * 1_024;
    let mut newlines = 0usize;
    let mut scan_pos = size;
    let mut start_byte = 0u64;

    'search: loop {
        let block = BLOCK.min(scan_pos);
        scan_pos -= block;
        file.seek(SeekFrom::Start(scan_pos))?;
        let mut buf = vec![0u8; block as usize];
        file.read_exact(&mut buf)?;

        for (i, &b) in buf.iter().enumerate().rev() {
            if b == b'\n' {
                newlines += 1;
                if newlines > n {
                    start_byte = scan_pos + i as u64 + 1;
                    break 'search;
                }
            }
        }

        if scan_pos == 0 {
            break;
        }
    }

    file.seek(SeekFrom::Start(start_byte))?;
    let mut tail = String::new();
    BufReader::new(file).read_to_string(&mut tail)?;
    Ok(tail.lines().map(str::to_owned).collect())
}

fn last_n_stdin(n: usize) -> io::Result<Vec<String>> {
    let mut ring: VecDeque<String> = VecDeque::with_capacity(n + 1);
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if ring.len() == n {
            ring.pop_front();
        }
        ring.push_back(line);
    }
    Ok(ring.into_iter().collect())
}

fn follow(path: &Path, mut offset: u64) -> ! {
    loop {
        thread::sleep(FOLLOW_INTERVAL);

        let size = match std::fs::metadata(path) {
            Ok(m) => m.len(),
            Err(_) => continue,
        };

        if size < offset {
            eprintln!("{}: file truncated, resetting", "tail".yellow());
            offset = 0;
        }

        if size == offset {
            continue;
        }

        let Ok(mut file) = File::open(path) else { continue };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }

        let mut new_bytes = Vec::new();
        if file.read_to_end(&mut new_bytes).is_ok() && !new_bytes.is_empty() {
            let stdout = io::stdout();
            stdout.lock().write_all(&new_bytes).ok();
            offset += new_bytes.len() as u64;
        }
    }
}

fn main() {
    let matches = Command::new("tail")
        .version("1.0.0")
        .about("Output the last part of files")
        .long_about(
            "Print the last N lines of each FILE (default: 10).\n\
             With -f, keep reading and printing as the file grows.\n\
             Shorthand: -50 is equivalent to -n 50.",
        )
        .after_help(
            "EXAMPLES\n\
             \x20 tail file.txt             # last 10 lines\n\
             \x20 tail -50 file.txt         # last 50 lines\n\
             \x20 tail -n 20 file.txt       # last 20 lines\n\
             \x20 tail -f app.log           # follow (live stream)\n\
             \x20 tail -50 -f app.log       # follow from last 50 lines\n\
             \x20 tail -f -n 0 app.log      # follow from end (no history)\n\
             \x20 type file.txt | tail -5   # from stdin",
        )
        .arg(
            Arg::new("lines")
                .short('n')
                .long("lines")
                .value_name("N")
                .default_value("10")
                .help("Number of lines from the end to print"),
        )
        .arg(
            Arg::new("follow")
                .short('f')
                .long("follow")
                .action(ArgAction::SetTrue)
                .help("Keep file open and print new lines as they appear"),
        )
        .arg(
            Arg::new("quiet")
                .short('q')
                .long("quiet")
                .action(ArgAction::SetTrue)
                .help("Never print file headers when multiple files given"),
        )
        .arg(
            Arg::new("files")
                .value_name("FILE")
                .num_args(0..)
                .help("Files to read (default: stdin, follow not supported on stdin)"),
        )
        .get_matches_from(preprocess_args());

    let n: usize = matches
        .get_one::<String>("lines")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let follow_mode = matches.get_flag("follow");
    let quiet       = matches.get_flag("quiet");

    let files: Vec<String> = matches
        .get_many::<String>("files")
        .unwrap_or_default()
        .cloned()
        .collect();

    // stdin
    if files.is_empty() {
        if follow_mode {
            eprintln!("{}: -f not supported on stdin", "tail".yellow());
            std::process::exit(1);
        }
        match last_n_stdin(n) {
            Ok(lines) => {
                for l in &lines { println!("{}", l); }
            }
            Err(e) => { eprintln!("{}: stdin: {}", "tail".bold(), e); std::process::exit(1); }
        }
        return;
    }

    // files
    let show_header = !quiet && files.len() > 1;

    let follow_path = if follow_mode {
        files.last().map(|s| s.as_str())
    } else {
        None
    };

    let stdout = io::stdout();
    let mut out = stdout.lock();

    for (i, path_str) in files.iter().enumerate() {
        if show_header {
            if i > 0 { writeln!(out).ok(); }
            writeln!(out, "{}", format!("==> {} <==", path_str).cyan()).ok();
        }

        let path = Path::new(path_str);
        match last_n_lines(path, n) {
            Err(e) => eprintln!("{}: {}: {}", "tail".bold(), path_str.yellow(), e),
            Ok(lines) => {
                for l in &lines {
                    writeln!(out, "{}", l).ok();
                }
            }
        }
    }

    drop(out);

    if let Some(path_str) = follow_path {
        let path = Path::new(path_str);
        if show_header {
            println!("\n{}", format!("==> {} <==", path_str).cyan());
        }
        let offset = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        eprintln!("{}: following {} — press Ctrl+C to stop", "tail".cyan(), path_str);
        follow(path, offset);
    }
}
