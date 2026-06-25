use colored::Colorize;
use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, TerminateProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const SIGSTOP: i32 = 19;
const SIGCONT: i32 = 18;

fn print_help() {
    println!("Usage: kill [-s SIGNAL | -SIGNAL | -N] PID...");
    println!("       kill -l");
    println!("Send a signal to processes by PID or name.");
    println!();
    println!("  -s SIG, -SIG, -N   signal to send (default: TERM / 15)");
    println!("  -l                 list signal names");
    println!("  --name NAME        kill by process name instead of PID");
    println!("  -f                 force (same as SIGKILL / -9)");
    println!("  -v                 verbose: show what is being killed");
    println!("  -h, --help         show this help");
    println!();
    println!("Note: on Windows all signals terminate the process (SIGSTOP/SIGCONT");
    println!("      are not supported).");
    println!();
    println!("Examples:");
    println!("  kill 1234");
    println!("  kill -9 1234 5678");
    println!("  kill -TERM 1234");
    println!("  kill --name notepad.exe");
    println!("  kill -f --name chrome.exe");
    println!("  kill -l");
}

const SIGNALS: &[(&str, i32)] = &[
    ("HUP",  1), ("INT",  2), ("QUIT", 3), ("ILL",  4), ("TRAP", 5),
    ("ABRT", 6), ("BUS",  7), ("FPE",  8), ("KILL", 9), ("USR1",10),
    ("SEGV",11), ("USR2",12), ("PIPE",13), ("ALRM",14), ("TERM",15),
    ("STKFLT",16),("CHLD",17),("CONT",18), ("STOP",19), ("TSTP",20),
    ("TTIN",21), ("TTOU",22), ("URG", 23), ("XCPU",24), ("XFSZ",25),
    ("VTALRM",26),("PROF",27),("WINCH",28),("POLL",29), ("PWR", 30),
    ("SYS", 31),
];

fn list_signals() {
    for (i, (name, num)) in SIGNALS.iter().enumerate() {
        print!("{:2}) SIG{:<8}", num, name);
        if (i + 1) % 4 == 0 { println!(); }
    }
    if SIGNALS.len() % 4 != 0 { println!(); }
}

fn parse_signal(s: &str) -> Option<i32> {
    // Try numeric
    if let Ok(n) = s.parse::<i32>() { return Some(n); }
    // Try name with or without SIG prefix
    let upper = s.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(&upper);
    SIGNALS.iter().find(|(n, _)| *n == name).map(|(_, num)| *num)
}

struct Args {
    signal:   i32,
    pids:     Vec<u32>,
    names:    Vec<String>,
    verbose:  bool,
    list:     bool,
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        std::process::exit(0);
    }

    let mut signal  = SIGTERM;
    let mut pids:   Vec<u32>   = Vec::new();
    let mut names:  Vec<String> = Vec::new();
    let mut verbose = false;
    let mut list    = false;

    let mut i = 0;
    while i < raw.len() {
        let arg = raw[i].as_str();
        match arg {
            "-l" | "--list"    => list = true,
            "-v" | "--verbose" => verbose = true,
            "-f" | "--force"   => signal = SIGKILL,
            "--name" => {
                i += 1;
                if let Some(n) = raw.get(i) { names.push(n.clone()); }
                else { eprintln!("kill: --name requires an argument"); std::process::exit(1); }
            }
            "-s" => {
                i += 1;
                let s = raw.get(i).map(|s| s.as_str()).unwrap_or("");
                signal = parse_signal(s).unwrap_or_else(|| {
                    eprintln!("kill: invalid signal '{}'", s); std::process::exit(1);
                });
            }
            s if s.starts_with('-') && s.len() > 1 => {
                // -9, -TERM, -SIGKILL, etc.
                let rest = &s[1..];
                if let Some(sig) = parse_signal(rest) {
                    signal = sig;
                } else if let Ok(pid) = rest.parse::<u32>() {
                    // edge case: "-1234" treated as negative pid (ignore)
                    let _ = pid;
                    eprintln!("kill: invalid signal '{}'", rest);
                    std::process::exit(1);
                } else {
                    eprintln!("kill: unknown option '{}'", s);
                    std::process::exit(1);
                }
            }
            s => {
                match s.parse::<u32>() {
                    Ok(pid) => pids.push(pid),
                    Err(_)  => {
                        eprintln!("kill: '{}' is not a valid PID (use --name for names)", s);
                        std::process::exit(1);
                    }
                }
            }
        }
        i += 1;
    }

    Args { signal, pids, names, verbose, list }
}

// Windows process helpers

fn terminate_pid(pid: u32, signal: i32, verbose: bool) -> bool {
    if signal == SIGSTOP || signal == SIGCONT {
        eprintln!("kill: signal {} is not supported on Windows", signal);
        return false;
    }

    unsafe {
        let handle: HANDLE = OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            FALSE,
            pid,
        );
        if handle == 0 {
            eprintln!("kill: ({}) operation not permitted or no such process", pid);
            return false;
        }
        let exit_code: u32 = if signal == SIGKILL { 1 } else { 0 };
        let ok = TerminateProcess(handle, exit_code) != 0;
        CloseHandle(handle);
        if ok {
            if verbose { println!("killed PID {}", pid); }
        } else {
            eprintln!("kill: ({}) failed to terminate process", pid);
        }
        ok
    }
}

fn find_pids_by_name(target: &str) -> Vec<(u32, String)> {
    let target_lower = target.to_ascii_lowercase();
    let mut results = Vec::new();

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == usize::MAX as isize { return results; }

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..std::mem::zeroed()
        };

        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let name_raw: Vec<u16> = entry.szExeFile.iter()
                    .take_while(|&&c| c != 0)
                    .copied()
                    .collect();
                let name = String::from_utf16_lossy(&name_raw);

                if name.to_ascii_lowercase() == target_lower
                    || name.to_ascii_lowercase().strip_suffix(".exe")
                         .map_or(false, |n| n == target_lower.strip_suffix(".exe").unwrap_or(&target_lower))
                {
                    results.push((entry.th32ProcessID, name));
                }

                if Process32NextW(snap, &mut entry) == 0 { break; }
            }
        }
        CloseHandle(snap);
    }
    results
}

// Main

fn main() {
    let args = parse_args();

    if args.list {
        list_signals();
        return;
    }

    if args.pids.is_empty() && args.names.is_empty() {
        eprintln!("kill: no PID or name specified");
        std::process::exit(1);
    }

    let mut any_fail = false;

    // Kill by PID
    for &pid in &args.pids {
        if !terminate_pid(pid, args.signal, args.verbose) {
            any_fail = true;
        }
    }

    // Kill by name
    for name in &args.names {
        let matches = find_pids_by_name(name);
        if matches.is_empty() {
            eprintln!("kill: no process found with name '{}'", name);
            any_fail = true;
            continue;
        }
        for (pid, proc_name) in matches {
            if args.verbose {
                println!("killing {} (PID {})", proc_name.yellow(), pid);
            }
            if !terminate_pid(pid, args.signal, false) {
                any_fail = true;
            }
        }
    }

    if any_fail { std::process::exit(1); }
}
