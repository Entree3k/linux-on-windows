use colored::Colorize;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ};

struct ProcInfo {
    pid:     u32,
    ppid:    u32,
    name:    String,
    threads: u32,
    mem_kb:  u64,
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..end]).to_string_lossy().into_owned()
}

fn get_mem_kb(pid: u32) -> u64 {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle == 0 || handle == INVALID_HANDLE_VALUE {
            return 0;
        }
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            PageFaultCount: 0,
            PeakWorkingSetSize: 0,
            WorkingSetSize: 0,
            QuotaPeakPagedPoolUsage: 0,
            QuotaPagedPoolUsage: 0,
            QuotaPeakNonPagedPoolUsage: 0,
            QuotaNonPagedPoolUsage: 0,
            PagefileUsage: 0,
            PeakPagefileUsage: 0,
        };
        GetProcessMemoryInfo(handle, &mut counters, counters.cb);
        CloseHandle(handle);
        (counters.WorkingSetSize / 1024) as u64
    }
}

fn list_processes() -> Vec<ProcInfo> {
    let mut procs = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE { return procs; }

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            cntUsage: 0,
            th32ProcessID: 0,
            th32DefaultHeapID: 0,
            th32ModuleID: 0,
            cntThreads: 0,
            th32ParentProcessID: 0,
            pcPriClassBase: 0,
            dwFlags: 0,
            szExeFile: [0u16; 260],
        };

        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let name = wide_to_string(&entry.szExeFile);
                let mem_kb = get_mem_kb(entry.th32ProcessID);
                procs.push(ProcInfo {
                    pid:     entry.th32ProcessID,
                    ppid:    entry.th32ParentProcessID,
                    name,
                    threads: entry.cntThreads,
                    mem_kb,
                });
                entry.szExeFile = [0u16; 260];
                if Process32NextW(snap, &mut entry) == 0 { break; }
            }
        }
        CloseHandle(snap);
    }
    procs
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help") {
        println!("Usage: ps [OPTION]...");
        println!("Report a snapshot of current processes.");
        println!("  -e, -A          show all processes (default)");
        println!("  -p PID          show only the given PID");
        println!("  --filter NAME   filter by name (case-insensitive substring)");
        println!("  --sort FIELD    sort by: pid, ppid, mem, threads, name");
        return;
    }

    let filter_pid: Option<u32> = args.windows(2)
        .find(|w| w[0] == "-p")
        .and_then(|w| w[1].parse().ok());

    let filter_name: Option<String> = args.windows(2)
        .find(|w| w[0] == "--filter")
        .map(|w| w[1].to_lowercase());

    let sort_field: &str = args.windows(2)
        .find(|w| w[0] == "--sort")
        .map(|w| w[1].as_str())
        .unwrap_or("pid");

    let mut procs = list_processes();

    // Apply filters
    if let Some(pid) = filter_pid {
        procs.retain(|p| p.pid == pid);
    }
    if let Some(ref name) = filter_name {
        procs.retain(|p| p.name.to_lowercase().contains(name.as_str()));
    }

    // Sort
    match sort_field {
        "mem" | "memory" => procs.sort_by(|a, b| b.mem_kb.cmp(&a.mem_kb)),
        "threads"        => procs.sort_by(|a, b| b.threads.cmp(&a.threads)),
        "name"           => procs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
        "ppid"           => procs.sort_by(|a, b| a.ppid.cmp(&b.ppid)),
        _                => procs.sort_by(|a, b| a.pid.cmp(&b.pid)),
    }

    println!("{:>7}  {:>7}  {:>8}  {:>8}  {}",
        "PID".bold(), "PPID".bold(), "Mem(KB)".bold(), "Threads".bold(), "Name".bold());
    println!("{}", "─".repeat(60).dimmed());

    for p in &procs {
        let mem_str = if p.mem_kb > 0 {
            let mb = p.mem_kb as f64 / 1024.0;
            if mb >= 1000.0 { format!("{:.1}M", mb / 1024.0).yellow().to_string() }
            else if mb >= 1.0 { format!("{:.1}M", mb) }
            else { format!("{}K", p.mem_kb) }
        } else {
            "-".dimmed().to_string()
        };
        println!("{:>7}  {:>7}  {:>8}  {:>8}  {}",
            p.pid.to_string().cyan(),
            p.ppid,
            mem_str,
            p.threads,
            p.name);
    }

    println!("{}", "─".repeat(60).dimmed());
    println!("{} processes", procs.len().to_string().bold());
}
