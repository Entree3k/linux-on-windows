use colored::Colorize;
use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    PROCESS_NAME_WIN32,
};

// Process enumeration

struct ProcEntry {
    pid:  u32,
    ppid: u32,
    name: String,
    path: Option<String>,
}

fn list_processes() -> Vec<ProcEntry> {
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == usize::MAX as isize { return out; }
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..std::mem::zeroed()
        };
        if Process32FirstW(snap, &mut e) != 0 {
            loop {
                let name = wstr_to_string(&e.szExeFile);
                out.push(ProcEntry { pid: e.th32ProcessID, ppid: e.th32ParentProcessID, name, path: None });
                if Process32NextW(snap, &mut e) == 0 { break; }
            }
        }
        CloseHandle(snap);
    }
    out
}

fn wstr_to_string(buf: &[u16]) -> String {
    String::from_utf16_lossy(&buf.iter().copied().take_while(|&c| c != 0).collect::<Vec<_>>())
}

fn full_path(pid: u32) -> Option<String> {
    unsafe {
        let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        if handle == 0 { return None; }
        let mut buf = [0u16; 32768];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok != 0 { Some(wstr_to_string(&buf[..len as usize])) } else { None }
    }
}

fn terminate_pid(pid: u32) -> bool {
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        if h == 0 { return false; }
        let ok = TerminateProcess(h, 1) != 0;
        CloseHandle(h);
        ok
    }
}

// Matching

fn matches(pattern: &str, proc: &ProcEntry, exact: bool, full: bool) -> bool {
    let haystack = if full {
        proc.path.as_deref().unwrap_or(&proc.name)
    } else {
        // Strip .exe suffix for friendlier matching
        proc.name.strip_suffix(".exe").unwrap_or(&proc.name)
    };
    let pat = pattern.to_ascii_lowercase();
    let hay = haystack.to_ascii_lowercase();
    if exact { hay == pat } else { hay.contains(&pat) }
}

// Arg parsing

struct Args {
    pattern:  Option<String>,
    exact:    bool,
    full:     bool,
    list:     bool,   // pgrep -l  — also print name
    long:     bool,   // pgrep -a  — full path
    count:    bool,   // pgrep/pkill -c
    newest:   bool,   // -n
    oldest:   bool,   // -o
    delim:    String, // pgrep -d
    parent:   Option<u32>, // -P ppid
    echo:     bool,   // pkill -e
    signal:   u32,    // pkill signal
    verbose:  bool,
}

fn parse_args(is_pkill: bool) -> Args {
    let raw: Vec<String> = std::env::args().skip(1).collect();

    if raw.iter().any(|a| a == "-h" || a == "--help") {
        print_help(is_pkill);
        std::process::exit(0);
    }

    let mut args = Args {
        pattern: None, exact: false, full: false, list: false,
        long: false, count: false, newest: false, oldest: false,
        delim: "\n".to_string(), parent: None, echo: false,
        signal: 15, verbose: false,
    };

    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "-x" | "--exact"    => args.exact   = true,
            "-f" | "--full"     => args.full     = true,
            "-l" | "--list-name"=> args.list     = true,
            "-a" | "--list-full"=> args.long     = true,
            "-c" | "--count"    => args.count    = true,
            "-n" | "--newest"   => args.newest   = true,
            "-o" | "--oldest"   => args.oldest   = true,
            "-v" | "--verbose"  => args.verbose  = true,
            "-e" | "--echo"     => args.echo     = true,
            "-d" => {
                i += 1;
                if let Some(v) = raw.get(i) { args.delim = v.clone(); }
            }
            "-P" => {
                i += 1;
                if let Some(v) = raw.get(i) {
                    args.parent = v.parse::<u32>().ok();
                }
            }
            s if s.starts_with("--delimiter=") => {
                args.delim = s["--delimiter=".len()..].to_string();
            }
            s if s.starts_with('-') && s.len() > 1 => {
                // -9, -TERM, etc. (pkill only, ignored for pgrep)
                let rest = &s[1..];
                if is_pkill {
                    if let Ok(n) = rest.parse::<u32>() { args.signal = n; }
                    // named signals: just treat all as terminate on Windows
                }
            }
            s => {
                args.pattern = Some(s.to_string());
            }
        }
        i += 1;
    }
    args
}

fn print_help(is_pkill: bool) {
    if is_pkill {
        println!("Usage: pkill [OPTION]... PATTERN");
        println!("Kill processes matching PATTERN.");
        println!();
        println!("  -e, --echo         display what is killed");
        println!("  -c, --count        count of matching processes");
        println!("  -x, --exact        match process name exactly");
        println!("  -f, --full         match against full executable path");
        println!("  -n, --newest       kill only the most recently started match");
        println!("  -o, --oldest       kill only the oldest match");
        println!("  -v, --verbose      verbose output");
        println!("  -P PPID            match only children of PPID");
        println!("  -SIGNAL            signal to send (all terminate on Windows)");
    } else {
        println!("Usage: pgrep [OPTION]... PATTERN");
        println!("List process IDs matching PATTERN.");
        println!();
        println!("  -l, --list-name    list PID and process name");
        println!("  -a, --list-full    list PID and full executable path");
        println!("  -c, --count        count of matching processes");
        println!("  -x, --exact        match process name exactly");
        println!("  -f, --full         match against full executable path");
        println!("  -n, --newest       print only the most recently started match");
        println!("  -o, --oldest       print only the oldest match");
        println!("  -d SEP             delimiter between PIDs (default: newline)");
        println!("  -P PPID            match only children of PPID");
        println!("  -v, --verbose      verbose output");
    }
    println!("  -h, --help         show this help");
}

// Main

fn main() {
    let bin = std::env::args()
        .next()
        .map(|p| std::path::Path::new(&p).file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default())
        .unwrap_or_default();
    let is_pkill = bin.contains("pkill");

    let args = parse_args(is_pkill);

    let pattern = match &args.pattern {
        Some(p) => p.clone(),
        None => {
            eprintln!("{}: no pattern specified", if is_pkill { "pkill" } else { "pgrep" });
            std::process::exit(1);
        }
    };

    // Fetch process list
    let mut procs = list_processes();
    if args.full || args.long {
        for p in &mut procs {
            p.path = full_path(p.pid);
        }
    }

    // Filter
    let mut matched: Vec<&ProcEntry> = procs.iter()
        .filter(|p| {
            let name_ok = matches(&pattern, p, args.exact, args.full);
            let ppid_ok = args.parent.map_or(true, |pp| p.ppid == pp);
            name_ok && ppid_ok
        })
        .collect();

    if matched.is_empty() {
        if args.count { println!("0"); }
        std::process::exit(1);
    }

    if args.newest {
        let target = matched.iter().map(|x| x.pid).max().unwrap_or(0);
        matched.retain(|p| p.pid == target);
    } else if args.oldest {
        let target = matched.iter().map(|x| x.pid).min().unwrap_or(0);
        matched.retain(|p| p.pid == target);
    }

    if args.count {
        println!("{}", matched.len());
        return;
    }

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    use std::io::Write;

    if is_pkill {
        let mut killed = 0u32;
        let mut failed = 0u32;
        for p in &matched {
            if args.echo || args.verbose {
                println!("killing {} (PID {})", p.name.yellow(), p.pid);
            }
            if terminate_pid(p.pid) { killed += 1; } else { failed += 1; }
        }
        if args.verbose {
            println!("killed {}, failed {}", killed, failed);
        }
        if failed > 0 { std::process::exit(1); }
    } else {
        let delim = args.delim.replace("\\n", "\n").replace("\\t", "\t");
        let mut first = true;
        for p in &matched {
            if !first { write!(out, "{}", delim).unwrap(); }
            first = false;
            if args.long {
                let path = p.path.as_deref().unwrap_or(&p.name);
                writeln!(out, "{} {}", p.pid, path).unwrap();
            } else if args.list {
                writeln!(out, "{} {}", p.pid, p.name).unwrap();
            } else {
                write!(out, "{}", p.pid).unwrap();
            }
        }
        if delim == "\n" || delim.is_empty() {
            writeln!(out).unwrap();
        } else {
            writeln!(out).unwrap();
        }
    }
}
