//! watch — execute a command repeatedly, showing output fullscreen.

use std::io::{self, Write};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use colored::Colorize;

fn print_help() {
    eprintln!("Usage: watch [-n SECS] [-t] [-d] [-e] COMMAND [ARGS...]");
    eprintln!();
    eprintln!("  -n SECS   Interval in seconds (default 2.0, decimals ok)");
    eprintln!("  -t        Don't show the header");
    eprintln!("  -d        Highlight differences between updates");
    eprintln!("  -e        Exit if command returns non-zero");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  watch netstat -an");
    eprintln!("  watch -n 1 dir C:\\");
    eprintln!("  watch -n 0.5 -d top --batch");
}

struct Args {
    interval:    f64,
    no_header:   bool,
    differences: bool,
    exit_on_err: bool,
    command:     String,
    cmd_args:    Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        std::process::exit(0);
    }

    let mut interval     = 2.0f64;
    let mut no_header    = false;
    let mut differences  = false;
    let mut exit_on_err  = false;
    let mut i = 0usize;

    while i < raw.len() {
        match raw[i].as_str() {
            "-n" => {
                i += 1;
                interval = raw.get(i).and_then(|s| s.parse().ok())
                    .ok_or("watch: -n requires a number")?;
            }
            "-t"  => no_header   = true,
            "-d"  => differences = true,
            "-e"  => exit_on_err = true,
            "-x"  => {} // accepted, same behavior on Windows
            "--"  => { i += 1; break; }
            s if s.starts_with("-n") && s.len() > 2 => {
                interval = s[2..].parse().map_err(|_| "watch: invalid interval")?;
            }
            _ => break,
        }
        i += 1;
    }

    if i >= raw.len() { return Err("watch: missing command".into()); }

    let rest    = raw[i..].to_vec();
    let command = rest.join(" ");
    let cmd_args = rest;

    Ok(Args { interval, no_header, differences, exit_on_err, command, cmd_args })
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "localhost".into())
}

fn timestamp() -> String {
    // Simple timestamp using system time
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

fn clear_screen() {
    // ANSI escape: clear screen + move cursor to home
    print!("\x1B[2J\x1B[H");
    io::stdout().flush().ok();
}

/// Returns (output_text, exit_ok).
fn run_command(cmd_args: &[String]) -> (String, bool) {
    // Use cmd /C so built-ins (dir, echo, type, etc.) work on Windows.
    match Command::new("cmd").arg("/C").arg(cmd_args.join(" ")).output() {
        Ok(out) => {
            let mut result = String::from_utf8_lossy(&out.stdout).into_owned();
            if !out.stderr.is_empty() {
                result.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            (result, out.status.success())
        }
        Err(e) => (format!("watch: failed to run command: {}", e), false),
    }
}

fn highlight_diff(prev: &str, curr: &str) -> String {
    let prev_lines: Vec<&str> = prev.lines().collect();
    curr.lines().enumerate().map(|(i, line)| {
        if prev_lines.get(i).copied() != Some(line) {
            format!("{}", line.yellow().bold())
        } else {
            line.to_owned()
        }
    }).collect::<Vec<_>>().join("\n")
}

fn main() {
    let args = match parse_args() {
        Ok(a)  => a,
        Err(e) => { eprintln!("{}", e.red()); std::process::exit(1); }
    };

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    }).ok();

    let mut prev_output = String::new();

    while running.load(Ordering::SeqCst) {
        let tick_start = Instant::now();

        clear_screen();

        if !args.no_header {
            let header = format!(
                "Every {:.1}s: {}   {}   {}",
                args.interval,
                args.command.bold(),
                hostname().cyan(),
                timestamp().green()
            );
            println!("{}", header);
            println!("{}", "─".repeat(80).dimmed());
        }

        let (output, ok) = run_command(&args.cmd_args);

        if args.differences {
            print!("{}", highlight_diff(&prev_output, &output));
        } else {
            print!("{}", output);
        }
        prev_output = output;

        if !ok && args.exit_on_err {
            println!("\n{}", "watch: command exited with non-zero status — stopping (-e)".red());
            break;
        }

        io::stdout().flush().ok();

        // Sleep the remaining interval in small chunks so Ctrl+C stays responsive.
        let elapsed = tick_start.elapsed();
        let target  = Duration::from_secs_f64(args.interval);
        if elapsed < target {
            let mut remaining = target - elapsed;
            let chunk = Duration::from_millis(50);
            while remaining > Duration::ZERO && running.load(Ordering::SeqCst) {
                let nap = remaining.min(chunk);
                std::thread::sleep(nap);
                remaining = remaining.saturating_sub(chunk);
            }
        }
    }

    println!();
}
