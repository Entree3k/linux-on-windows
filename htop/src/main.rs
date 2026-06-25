use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::collections::HashMap;
use std::io::{self, Write};
use std::time::{Duration, Instant};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME, HANDLE, SYSTEMTIME},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
            PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        },
        LibraryLoader::{GetProcAddress, LoadLibraryW},
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        SystemInformation::{
            GetLocalTime, GetSystemInfo, GetTickCount64, GlobalMemoryStatusEx,
            MEMORYSTATUSEX, SYSTEM_INFO,
        },
        Threading::{
            GetProcessTimes, OpenProcess, TerminateProcess,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        },
    },
};

// Per-core CPU info

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CorePerfInfo {
    idle_time:       i64,  // 100ns intervals (LARGE_INTEGER)
    kernel_time:     i64,  // includes idle_time
    user_time:       i64,
    dpc_time:        i64,
    interrupt_time:  i64,
    interrupt_count: u32,
    _pad:            u32,
}

#[cfg(windows)]
fn query_core_perf(cpu_count: usize) -> Vec<CorePerfInfo> {
    type NtFn = unsafe extern "system" fn(u32, *mut u8, u32, *mut u32) -> i32;
    unsafe {
        let name: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
        let ntdll = LoadLibraryW(name.as_ptr());
        if ntdll == 0 { return vec![CorePerfInfo::default(); cpu_count]; }

        let fn_ptr = GetProcAddress(ntdll, b"NtQuerySystemInformation\0".as_ptr());
        let Some(f) = fn_ptr else {
            return vec![CorePerfInfo::default(); cpu_count];
        };
        let nt: NtFn = std::mem::transmute(f);

        const CLASS: u32 = 8; // SystemProcessorPerformanceInformation
        let buf_size = cpu_count * std::mem::size_of::<CorePerfInfo>();
        let mut buf  = vec![0u8; buf_size];
        let mut ret  = 0u32;

        if nt(CLASS, buf.as_mut_ptr(), buf_size as u32, &mut ret) != 0 {
            return vec![CorePerfInfo::default(); cpu_count];
        }

        let count = (ret as usize) / std::mem::size_of::<CorePerfInfo>();
        let slice = std::slice::from_raw_parts(
            buf.as_ptr() as *const CorePerfInfo,
            count.min(cpu_count),
        );
        slice.to_vec()
    }
}
#[cfg(not(windows))]
fn query_core_perf(n: usize) -> Vec<CorePerfInfo> { vec![CorePerfInfo::default(); n] }

// Data structures

#[derive(Clone)]
struct ProcEntry {
    pid:      u32,
    ppid:     u32,
    name:     String,
    threads:  u32,
    cpu_ticks: u64,
    cpu_time:  u64,
    mem_ws:    u64,
    cpu_pct:   f64,
}

struct MemInfo {
    total:      u64,  // bytes
    used:       u64,
    swap_total: u64,
    swap_used:  u64,
}

#[derive(Clone, Copy, PartialEq)]
enum SortBy { Cpu, Mem, Pid, Name, Time, Threads }

struct App {
    procs:       Vec<ProcEntry>,
    mem:         MemInfo,
    cpu_pct:     f64,
    core_pcts:   Vec<f64>,
    cpu_count:   u32,
    uptime_sec:  u64,
    sort_by:     SortBy,
    sort_rev:    bool,
    scroll:      usize,
    filter:      String,
    filtering:   bool,
    cols:        usize,
    rows:        usize,
    update_ms:   u64,
}

// Windows helpers

#[cfg(windows)]
fn ft_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

#[cfg(windows)]
fn wall_ms() -> u64 { unsafe { GetTickCount64() } }
#[cfg(not(windows))]
fn wall_ms() -> u64 { 0 }

#[cfg(windows)]
fn get_cpu_count() -> u32 {
    unsafe {
        let mut si: SYSTEM_INFO = std::mem::zeroed();
        GetSystemInfo(&mut si);
        si.dwNumberOfProcessors
    }
}
#[cfg(not(windows))]
fn get_cpu_count() -> u32 { 1 }

#[cfg(windows)]
fn get_mem_info() -> MemInfo {
    unsafe {
        let mut m = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            dwMemoryLoad: 0,
            ullTotalPhys: 0, ullAvailPhys: 0,
            ullTotalPageFile: 0, ullAvailPageFile: 0,
            ullTotalVirtual: 0, ullAvailVirtual: 0,
            ullAvailExtendedVirtual: 0,
        };
        GlobalMemoryStatusEx(&mut m);
        MemInfo {
            total:      m.ullTotalPhys,
            used:       m.ullTotalPhys.saturating_sub(m.ullAvailPhys),
            swap_total: m.ullTotalPageFile,
            swap_used:  m.ullTotalPageFile.saturating_sub(m.ullAvailPageFile),
        }
    }
}
#[cfg(not(windows))]
fn get_mem_info() -> MemInfo { MemInfo { total: 1, used: 0, swap_total: 0, swap_used: 0 } }

#[cfg(windows)]
fn proc_stats(pid: u32) -> (u64, u64, u64, u32) {
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 { return (0, 0, 0, 0); }

        let mut cr = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut ex = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut kn = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut us = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        GetProcessTimes(h, &mut cr, &mut ex, &mut kn, &mut us);
        let ticks = ft_to_u64(kn) + ft_to_u64(us);

        let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        GetProcessMemoryInfo(h, &mut pmc, pmc.cb);

        CloseHandle(h);
        (ticks, ticks, pmc.WorkingSetSize as u64, 0)
    }
}
#[cfg(not(windows))]
fn proc_stats(_: u32) -> (u64, u64, u64, u32) { (0, 0, 0, 0) }

#[cfg(windows)]
fn snapshot_procs() -> HashMap<u32, ProcEntry> {
    let mut map = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == -1isize { return map; }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut e) == 0 { CloseHandle(snap); return map; }
        loop {
            let pid  = e.th32ProcessID;
            let ppid = e.th32ParentProcessID;
            let name = String::from_utf16_lossy(
                &e.szExeFile[..e.szExeFile.iter().position(|&c| c == 0).unwrap_or(260)],
            );
            let (cpu_ticks, cpu_time, mem_ws, _) = proc_stats(pid);
            map.insert(pid, ProcEntry {
                pid, ppid, name, threads: e.cntThreads,
                cpu_ticks, cpu_time, mem_ws, cpu_pct: 0.0,
            });
            if Process32NextW(snap, &mut e) == 0 { break; }
        }
        CloseHandle(snap);
    }
    map
}
#[cfg(not(windows))]
fn snapshot_procs() -> HashMap<u32, ProcEntry> { HashMap::new() }

#[cfg(windows)]
fn kill_proc(pid: u32) -> bool {
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h == 0 { return false; }
        let ok = TerminateProcess(h, 1) != 0;
        CloseHandle(h);
        ok
    }
}
#[cfg(not(windows))]
fn kill_proc(_: u32) -> bool { false }

fn compute_cpu(
    prev:      &HashMap<u32, ProcEntry>,
    curr:      &mut HashMap<u32, ProcEntry>,
    wall1:     u64,
    wall2:     u64,
    cpu_count: u32,
) -> f64 {
    let wall_100ns = wall2.saturating_sub(wall1) * 10_000;
    let scale = (wall_100ns * cpu_count as u64) as f64;
    if scale == 0.0 { return 0.0; }
    let mut total_delta = 0u64;
    for (pid, e) in curr.iter_mut() {
        if let Some(old) = prev.get(pid) {
            let d = e.cpu_ticks.saturating_sub(old.cpu_ticks);
            e.cpu_pct = (d as f64 / scale) * 100.0;
            total_delta += d;
        }
    }
    (total_delta as f64 / scale * 100.0).min(100.0)
}

/// Compute per-core CPU% from two snapshots of CorePerfInfo.
fn compute_core_pcts(prev: &[CorePerfInfo], curr: &[CorePerfInfo]) -> Vec<f64> {
    prev.iter().zip(curr.iter()).map(|(p, c)| {
        let total = (c.kernel_time + c.user_time).saturating_sub(p.kernel_time + p.user_time);
        let idle  = c.idle_time.saturating_sub(p.idle_time);
        if total <= 0 { 0.0 } else {
            ((total - idle).max(0) as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
        }
    }).collect()
}

// Formatting helpers

fn fmt_bytes_short(b: u64) -> String {
    const G: u64 = 1 << 30;
    const M: u64 = 1 << 20;
    const K: u64 = 1 << 10;
    if b >= G      { format!("{:.1}G", b as f64 / G as f64) }
    else if b >= M { format!("{:.1}M", b as f64 / M as f64) }
    else if b >= K { format!("{:.1}K", b as f64 / K as f64) }
    else           { format!("{} B", b) }
}

fn fmt_uptime(s: u64) -> String {
    let d = s / 86_400;
    let h = (s % 86_400) / 3600;
    let m = (s % 3600) / 60;
    if d > 0 { format!("{d}d {h:02}h {m:02}m") }
    else      { format!("{h:02}h {m:02}m") }
}

fn fmt_time(ticks: u64) -> String {
    let cs   = ticks / 100_000;
    let mins = cs / 6000;
    let secs = (cs % 6000) / 100;
    let c    = cs % 100;
    format!("{mins}:{secs:02}.{c:02}")
}

fn now_time() -> String {
    #[cfg(windows)]
    unsafe {
        let mut st: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut st);
        return format!("{:02}:{:02}:{:02}", st.wHour, st.wMinute, st.wSecond);
    }
    #[cfg(not(windows))]
    "00:00:00".to_string()
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() }
    else { s.chars().take(n.saturating_sub(1)).collect::<String>() + "…" }
}
fn pad_r(s: &str, n: usize) -> String { format!("{:<width$}", trunc(s, n), width = n) }
fn pad_l(s: &str, n: usize) -> String { format!("{:>width$}", s, width = n) }

// CPU bar rendering

fn cpu_bar_line(
    core_pcts: &[f64],
    cols:      usize,
    per_row:   usize,
    start:     usize,   // which core to start from
) -> String {
    let count = per_row.min(core_pcts.len().saturating_sub(start));
    let col_w = cols / per_row;
    let bar_inner = col_w.saturating_sub(10); // "CPU10 [" + "%] " = ~10

    let mut line = String::new();
    for i in 0..count {
        let idx = start + i;
        let pct = core_pcts[idx];
        let filled = ((pct / 100.0) * bar_inner as f64).round() as usize;
        let filled = filled.min(bar_inner);
        let empty  = bar_inner - filled;
        let bar    = format!(
            "CPU{} [{}{}{:3.0}%]",
            idx + 1,
            "|".repeat(filled),
            " ".repeat(empty),
            pct,
        );
        line.push_str(&format!("{:<width$}", bar, width = col_w));
    }
    trunc(&line, cols)
}

// Memory bar rendering

fn mem_bar(label: &str, used: u64, total: u64, cols: usize) -> String {
    let pct = if total > 0 { used as f64 / total as f64 } else { 0.0 };
    let suffix = format!("{}/{}", fmt_bytes_short(used), fmt_bytes_short(total));
    // bar inner width = cols - label(5) - "[" - "]" - suffix - spaces
    let bar_inner = cols.saturating_sub(label.len() + 3 + suffix.len());
    let filled = ((pct * bar_inner as f64).round() as usize).min(bar_inner);
    let empty  = bar_inner - filled;
    format!("{} [{}{}{}]", label, "|".repeat(filled), " ".repeat(empty), suffix)
}

fn mem_color(used: u64, total: u64) -> Color {
    if total == 0 { return Color::Green; }
    let pct = used as f64 / total as f64;
    if pct >= 0.8 { Color::Red } else if pct >= 0.6 { Color::Yellow } else { Color::Green }
}

// Rendering

fn filtered_sorted(app: &App) -> Vec<ProcEntry> {
    let f = app.filter.to_lowercase();
    let mut list: Vec<ProcEntry> = app.procs.iter()
        .filter(|p| f.is_empty()
            || p.name.to_lowercase().contains(&f)
            || p.pid.to_string().contains(&f))
        .cloned()
        .collect();
    list.sort_by(|a, b| {
        let ord = match app.sort_by {
            SortBy::Cpu     => a.cpu_pct.partial_cmp(&b.cpu_pct).unwrap_or(std::cmp::Ordering::Equal),
            SortBy::Mem     => a.mem_ws.cmp(&b.mem_ws),
            SortBy::Pid     => a.pid.cmp(&b.pid),
            SortBy::Name    => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortBy::Time    => a.cpu_time.cmp(&b.cpu_time),
            SortBy::Threads => a.threads.cmp(&b.threads),
        };
        if app.sort_rev { ord } else { ord.reverse() }
    });
    list
}

fn draw(app: &App, out: &mut impl Write) -> io::Result<()> {
    let cols = app.cols;
    let cpu_count = app.cpu_count as usize;
    let per_row = if cpu_count <= 4 { 2 } else { 4 };
    let cpu_bar_rows = cpu_count.div_ceil(per_row);

    let mut y = 0u16;

    // Title row
    let username = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    let hostname = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "pc".into());
    let title = format!(
        "htop - {}@{}   up {}   tasks: {}   cpus: {}   {}",
        username, hostname,
        fmt_uptime(app.uptime_sec),
        app.procs.len(), cpu_count, now_time()
    );
    queue!(out,
        cursor::MoveTo(0, y),
        SetBackgroundColor(Color::DarkBlue), SetForegroundColor(Color::White),
        SetAttribute(Attribute::Bold),
        Print(format!("{:<width$}", trunc(&title, cols), width = cols)),
        ResetColor,
    )?;
    y += 1;

    // Per-core CPU bars
    for bar_row in 0..cpu_bar_rows {
        let start = bar_row * per_row;
        let line  = cpu_bar_line(&app.core_pcts, cols, per_row, start);

        // Color by max usage in this row
        let max_pct = (start..start + per_row)
            .filter_map(|i| app.core_pcts.get(i).copied())
            .fold(0.0f64, f64::max);
        let color = if max_pct >= 80.0 { Color::Red }
                    else if max_pct >= 50.0 { Color::Yellow }
                    else { Color::Green };

        queue!(out,
            cursor::MoveTo(0, y),
            SetForegroundColor(color), SetBackgroundColor(Color::Black),
            Print(format!("{:<width$}", line, width = cols)),
            ResetColor,
        )?;
        y += 1;
    }

    // Memory bars
    let mem_line  = mem_bar("Mem ", app.mem.used, app.mem.total, cols);
    let swap_line = mem_bar("Swap", app.mem.swap_used, app.mem.swap_total, cols);

    queue!(out,
        cursor::MoveTo(0, y),
        SetForegroundColor(mem_color(app.mem.used, app.mem.total)),
        SetBackgroundColor(Color::Black),
        Print(format!("{:<width$}", trunc(&mem_line, cols), width = cols)),
        ResetColor,
    )?;
    y += 1;

    let swap_color = if app.mem.swap_total > 0 {
        mem_color(app.mem.swap_used, app.mem.swap_total)
    } else {
        Color::Green
    };
    queue!(out,
        cursor::MoveTo(0, y),
        SetForegroundColor(swap_color), SetBackgroundColor(Color::Black),
        Print(format!("{:<width$}", trunc(&swap_line, cols), width = cols)),
        ResetColor,
    )?;
    y += 1;

    // Blank separator
    queue!(out, cursor::MoveTo(0, y), terminal::Clear(ClearType::CurrentLine))?;
    y += 1;

    // Column headers
    let name_w = cols.saturating_sub(52);
    let hdr = format!(
        "{} {} {} {} {} {} {}",
        pad_l("PID",     7),
        pad_l("CPU%",    6),
        pad_l("MEM%",    5),
        pad_l("RSS",     8),
        pad_l("THR",     4),
        pad_l("TIME+",   9),
        pad_r("COMMAND", name_w.max(4)),
    );
    queue!(out,
        cursor::MoveTo(0, y),
        SetBackgroundColor(Color::Cyan), SetForegroundColor(Color::Black),
        SetAttribute(Attribute::Bold),
        Print(format!("{:<width$}", trunc(&hdr, cols), width = cols)),
        ResetColor,
    )?;
    let hdr_y = y;
    y += 1;

    // Process rows
    let footer_rows = 2u16;
    let content = (app.rows as u16).saturating_sub(hdr_y + 1 + footer_rows) as usize;
    let procs  = filtered_sorted(app);
    let scroll = app.scroll.min(procs.len().saturating_sub(1));

    for row in 0..content {
        let sy = y + row as u16;
        queue!(out, cursor::MoveTo(0, sy), terminal::Clear(ClearType::CurrentLine))?;

        if let Some(p) = procs.get(scroll + row) {
            let mem_pct = if app.mem.total > 0 {
                p.mem_ws as f64 / app.mem.total as f64 * 100.0
            } else { 0.0 };

            let line = format!(
                "{} {} {} {} {} {} {}",
                pad_l(&p.pid.to_string(),          7),
                pad_l(&format!("{:>5.1}", p.cpu_pct),  6),
                pad_l(&format!("{:>4.1}", mem_pct),    5),
                pad_l(&fmt_bytes_short(p.mem_ws),       8),
                pad_l(&p.threads.to_string(),           4),
                pad_l(&fmt_time(p.cpu_time),            9),
                pad_r(&p.name,                          name_w.max(4)),
            );
            if p.cpu_pct >= 50.0 {
                queue!(out, SetForegroundColor(Color::Red), SetAttribute(Attribute::Bold))?;
            } else if p.cpu_pct >= 10.0 {
                queue!(out, SetForegroundColor(Color::Yellow))?;
            }
            queue!(out, Print(trunc(&line, cols)), ResetColor)?;
        }
    }

    // F-key footer
    let help_y = (app.rows as u16).saturating_sub(2);
    if app.filtering {
        let fl = format!("Search: {}█", app.filter);
        queue!(out,
            cursor::MoveTo(0, help_y),
            SetBackgroundColor(Color::DarkBlue), SetForegroundColor(Color::White),
            Print(format!("{:<width$}", trunc(&fl, cols), width = cols)),
            ResetColor,
        )?;
    } else {
        let f_keys = format!(
            "{}{}{}{}{}{}",
            fkey("F1", "Help"),
            fkey("F3", "Search"),
            fkey("F6", "Sort"),
            fkey("F9", "Kill"),
            fkey("F10", "Quit"),
            fkey("r", "Reverse"),
        );
        queue!(out,
            cursor::MoveTo(0, help_y),
            Print(format!("{:<width$}", trunc(&f_keys, cols), width = cols)),
        )?;
    }

    let hint_y = (app.rows as u16).saturating_sub(1);
    let sort_name = match app.sort_by {
        SortBy::Cpu     => "CPU%",
        SortBy::Mem     => "MEM",
        SortBy::Pid     => "PID",
        SortBy::Name    => "NAME",
        SortBy::Time    => "TIME",
        SortBy::Threads => "THR",
    };
    let hint = format!(
        "Sort: {}{} | Procs: {} | Filter: \"{}\" | Scroll: {}/{}",
        sort_name,
        if app.sort_rev { "▲" } else { "▼" },
        procs.len(), app.filter,
        scroll + 1, procs.len().max(1),
    );
    queue!(out,
        cursor::MoveTo(0, hint_y),
        SetBackgroundColor(Color::Black), SetForegroundColor(Color::DarkGrey),
        Print(format!("{:<width$}", trunc(&hint, cols), width = cols)),
        ResetColor,
    )?;

    out.flush()
}

fn fkey(key: &str, label: &str) -> String {
    format!("\x1b[0;30;46m{}\x1b[0;37;40m{} ", key, label)
}

// Prompt helper

fn prompt_line(label: &str, out: &mut impl Write, app: &App) -> io::Result<Option<String>> {
    let mut val = String::new();
    loop {
        let text = format!("{}{}_", label, val);
        queue!(out,
            cursor::MoveTo(0, (app.rows as u16).saturating_sub(2)),
            terminal::Clear(ClearType::CurrentLine),
            SetBackgroundColor(Color::DarkBlue), SetForegroundColor(Color::White),
            Print(format!("{:<width$}", trunc(&text, app.cols), width = app.cols)),
            ResetColor,
        )?;
        out.flush()?;

        if let Ok(Event::Key(k)) = event::read() {
            if !matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) { continue; }
            match k.code {
                KeyCode::Enter => return Ok(Some(val)),
                KeyCode::Esc   => return Ok(None),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(None),
                KeyCode::Backspace => { val.pop(); }
                KeyCode::Char(c) if k.modifiers.is_empty() || k.modifiers == KeyModifiers::SHIFT
                    => val.push(c),
                _ => {}
            }
        }
    }
}

// Main event loop

fn run() -> io::Result<()> {
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide)?;

    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let cpu_count    = get_cpu_count();

    let mut app = App {
        procs: vec![], mem: get_mem_info(),
        cpu_pct: 0.0, core_pcts: vec![0.0; cpu_count as usize],
        cpu_count, uptime_sec: unsafe { GetTickCount64() } / 1000,
        sort_by: SortBy::Cpu, sort_rev: false,
        scroll: 0, filter: String::new(), filtering: false,
        cols: cols as usize, rows: rows as usize, update_ms: 2000,
    };

    let mut prev_wall  = wall_ms();
    let mut prev_procs = snapshot_procs();
    let mut prev_cores = query_core_perf(cpu_count as usize);
    app.procs = prev_procs.values().cloned().collect();
    draw(&app, &mut out)?;

    let mut last_update = Instant::now();

    loop {
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Resize(c, r) => {
                    app.cols = c as usize; app.rows = r as usize;
                    draw(&app, &mut out)?;
                }
                Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    if app.filtering {
                        match k.code {
                            KeyCode::Esc | KeyCode::Enter | KeyCode::F(3) => { app.filtering = false; }
                            KeyCode::Backspace => { app.filter.pop(); }
                            KeyCode::Char(c) if k.modifiers.is_empty() || k.modifiers == KeyModifiers::SHIFT
                                => app.filter.push(c),
                            _ => {}
                        }
                    } else {
                        match k.code {
                            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::F(10) => break,
                            KeyCode::F(9) => {
                                if let Some(inp) = prompt_line("Kill PID: ", &mut out, &app)? {
                                    if let Ok(pid) = inp.trim().parse::<u32>() {
                                        let ok = kill_proc(pid);
                                        let msg = if ok { format!("Sent SIGKILL to {pid}") }
                                                  else { format!("Cannot kill {pid} (access denied?)") };
                                        queue!(out,
                                            cursor::MoveTo(0, (app.rows as u16).saturating_sub(2)),
                                            terminal::Clear(ClearType::CurrentLine),
                                            SetForegroundColor(Color::Red),
                                            Print(format!("{:<width$}", msg, width = app.cols)),
                                            ResetColor,
                                        )?;
                                        out.flush()?;
                                        std::thread::sleep(Duration::from_millis(800));
                                    }
                                }
                            }
                            KeyCode::Char('k') | KeyCode::Char('K') => {
                                if let Some(inp) = prompt_line("Kill PID: ", &mut out, &app)? {
                                    if let Ok(pid) = inp.trim().parse::<u32>() {
                                        kill_proc(pid);
                                    }
                                }
                            }
                            KeyCode::F(3) | KeyCode::Char('/') => { app.filtering = true; app.filter.clear(); }
                            KeyCode::Char('P') | KeyCode::Char('p') => { app.sort_by = SortBy::Cpu;     app.scroll = 0; }
                            KeyCode::Char('M') | KeyCode::Char('m') => { app.sort_by = SortBy::Mem;     app.scroll = 0; }
                            KeyCode::Char('N') | KeyCode::Char('n') => { app.sort_by = SortBy::Pid;     app.scroll = 0; }
                            KeyCode::Char('T') | KeyCode::Char('t') => { app.sort_by = SortBy::Time;    app.scroll = 0; }
                            KeyCode::Char('A') | KeyCode::Char('a') => { app.sort_by = SortBy::Name;    app.scroll = 0; }
                            KeyCode::Char('H') | KeyCode::Char('h') => { app.sort_by = SortBy::Threads; app.scroll = 0; }
                            KeyCode::Char('r') | KeyCode::Char('R') => app.sort_rev = !app.sort_rev,
                            KeyCode::Up       => { app.scroll = app.scroll.saturating_sub(1); }
                            KeyCode::Down     => { app.scroll += 1; }
                            KeyCode::PageUp   => { app.scroll = app.scroll.saturating_sub(app.rows / 2); }
                            KeyCode::PageDown => { app.scroll += app.rows / 2; }
                            KeyCode::Home     => { app.scroll = 0; }
                            _ => {}
                        }
                    }
                    draw(&app, &mut out)?;
                }
                _ => {}
            }
        }

        if last_update.elapsed().as_millis() as u64 >= app.update_ms {
            let curr_wall  = wall_ms();
            let curr_cores = query_core_perf(cpu_count as usize);
            let mut curr   = snapshot_procs();

            app.cpu_pct   = compute_cpu(&prev_procs, &mut curr, prev_wall, curr_wall, cpu_count);
            app.core_pcts = compute_core_pcts(&prev_cores, &curr_cores);
            app.mem        = get_mem_info();
            app.uptime_sec = curr_wall / 1000;
            app.procs      = curr.values().cloned().collect();

            let max_scroll = filtered_sorted(&app).len().saturating_sub(1);
            app.scroll = app.scroll.min(max_scroll);

            prev_wall  = curr_wall;
            prev_procs = curr.into_iter().collect();
            prev_cores = curr_cores;
            last_update = Instant::now();
            draw(&app, &mut out)?;
        }
    }

    execute!(out, terminal::LeaveAlternateScreen, cursor::Show)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

// Entry point

fn main() {
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("Usage: htop");
        println!("  Interactive process viewer with per-core CPU bars.");
        println!();
        println!("  Keys:");
        println!("    q / F10    Quit");
        println!("    P          Sort by CPU%");
        println!("    M          Sort by memory");
        println!("    N          Sort by PID");
        println!("    T          Sort by CPU time");
        println!("    A          Sort by name");
        println!("    H          Sort by thread count");
        println!("    r / R      Reverse sort order");
        println!("    F3 / /     Search / filter");
        println!("    k / F9     Kill process (enter PID)");
        println!("    Up/Down    Scroll");
        return;
    }
    if let Err(e) = run() {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
        eprintln!("htop: {}", e);
        std::process::exit(1);
    }
}
