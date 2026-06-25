use colored::Colorize;
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use std::mem;
use std::time::Duration;
use std::thread;

struct Args {
    human:   bool,
    bytes:   bool,
    kilo:    bool,
    mega:    bool,
    giga:    bool,
    repeat:  Option<f64>,
    count:   Option<u32>,
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|a| a == "--help") {
        println!("Usage: free [OPTION]...");
        println!("Display amount of free and used memory in the system.");
        println!("  -h, --human      show human-readable sizes");
        println!("  -b               show sizes in bytes");
        println!("  -k               show sizes in kibibytes (default)");
        println!("  -m               show sizes in mebibytes");
        println!("  -g               show sizes in gibibytes");
        println!("  -s N             repeat every N seconds");
        println!("  -c N             repeat N times");
        std::process::exit(0);
    }

    let mut a = Args { human: false, bytes: false, kilo: false, mega: false, giga: false, repeat: None, count: None };
    let mut i = 0;
    while i < raw.len() {
        let s = raw[i].as_str();
        match s {
            "-h" | "--human" => a.human = true,
            "-b"             => a.bytes = true,
            "-k"             => a.kilo  = true,
            "-m"             => a.mega  = true,
            "-g"             => a.giga  = true,
            "-s"             => { i += 1; a.repeat = raw.get(i).and_then(|v| v.parse().ok()); }
            "-c"             => { i += 1; a.count  = raw.get(i).and_then(|v| v.parse().ok()); }
            _ if s.starts_with("-s") && s.len() > 2 => { a.repeat = s[2..].parse().ok(); }
            _ if s.starts_with("-c") && s.len() > 2 => { a.count  = s[2..].parse().ok(); }
            _ => {}
        }
        i += 1;
    }
    a
}

fn get_mem() -> (u64, u64, u64, u64) {
    unsafe {
        let mut ms = MEMORYSTATUSEX {
            dwLength: mem::size_of::<MEMORYSTATUSEX>() as u32,
            dwMemoryLoad: 0,
            ullTotalPhys: 0,
            ullAvailPhys: 0,
            ullTotalPageFile: 0,
            ullAvailPageFile: 0,
            ullTotalVirtual: 0,
            ullAvailVirtual: 0,
            ullAvailExtendedVirtual: 0,
        };
        GlobalMemoryStatusEx(&mut ms);
        (ms.ullTotalPhys, ms.ullAvailPhys, ms.ullTotalPageFile, ms.ullAvailPageFile)
    }
}

fn fmt(bytes: u64, args: &Args) -> String {
    if args.human {
        if bytes >= 1 << 40 { return format!("{:.1}T", bytes as f64 / (1u64 << 40) as f64); }
        if bytes >= 1 << 30 { return format!("{:.1}G", bytes as f64 / (1u64 << 30) as f64); }
        if bytes >= 1 << 20 { return format!("{:.1}M", bytes as f64 / (1u64 << 20) as f64); }
        if bytes >= 1 << 10 { return format!("{:.1}K", bytes as f64 / (1u64 << 10) as f64); }
        return format!("{}B", bytes);
    }
    if args.bytes { return format!("{}", bytes); }
    if args.mega  { return format!("{}", bytes / (1 << 20)); }
    if args.giga  { return format!("{}", bytes / (1 << 30)); }
    // default: kibibytes
    format!("{}", bytes / 1024)
}

fn print_mem(args: &Args) {
    let (total_phys, avail_phys, total_page, avail_page) = get_mem();
    let used_phys  = total_phys.saturating_sub(avail_phys);
    let used_page  = total_page.saturating_sub(avail_page);
    // "buffers/cache" concept doesn't map directly on Windows; show available as free
    let col = 14usize;
    println!("{:>col$} {:>col$} {:>col$}", "total".bold(), "used".bold(), "free".bold());
    println!("{:<5}{:>col$} {:>col$} {:>col$}",
        "Mem:".bold(), fmt(total_phys, args), fmt(used_phys, args).yellow(), fmt(avail_phys, args).green());
    println!("{:<5}{:>col$} {:>col$} {:>col$}",
        "Swap:".bold(), fmt(total_page, args), fmt(used_page, args).yellow(), fmt(avail_page, args).green());
}

fn main() {
    let args = parse_args();
    let interval = args.repeat.unwrap_or(0.0);
    let max_count = args.count.unwrap_or(if interval > 0.0 { u32::MAX } else { 1 });

    for iteration in 0..max_count {
        if iteration > 0 { println!(); }
        print_mem(&args);
        if iteration + 1 < max_count && interval > 0.0 {
            thread::sleep(Duration::from_secs_f64(interval));
        }
    }
}
