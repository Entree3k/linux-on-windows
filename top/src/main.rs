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

// Windows API imports

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME, HANDLE, SYSTEMTIME},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
            PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        },
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        SystemInformation::{
            GetSystemInfo, GetTickCount64, GlobalMemoryStatusEx,
            MEMORYSTATUSEX, SYSTEM_INFO,
        },
        Threading::{GetProcessTimes, OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE},
    },
};

// Data structures

#[derive(Clone)]
struct ProcEntry {
    pid:       u32,
    name:      String,
    threads:   u32,
    cpu_ticks: u64,
    mem_ws:    u64,
    cpu_pct:   f64,
    cpu_time:  u64,
}

struct MemInfo {
    total:      u64,  // MiB
    used:       u64,
    swap_total: u64,
    swap_used:  u64,
}

#[derive(Clone, Copy, PartialEq)]
enum SortBy { Cpu, Mem, Pid, Name, Time }

struct App {
    procs:      Vec<ProcEntry>,
    mem:        MemInfo,
    cpu_pct:    f64,
    cpu_count:  u32,
    uptime_sec: u64,
    sort_by:    SortBy,
    sort_rev:   bool,
    scroll:     usize,
    filter:     String,
    filtering:  bool,
    cols:       usize,
    rows:       usize,
    update_ms:  u64,
}

// Windows API helpers

#[cfg(windows)]
fn ft_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

#[cfg(windows)]
fn wall_ms() -> u64 { unsafe { GetTickCount64() } }

#[cfg(not(windows))]
fn wall_ms() -> u64 { 0 }

#[cfg(windows)]
fn snapshot_procs() -> HashMap<u32, ProcEntry> {
    let mut map = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == -1isize { return map; }

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snap, &mut entry) == 0 {
            CloseHandle(snap);
            return map;
        }

        loop {
            let pid  = entry.th32ProcessID;
            let name = String::from_utf16_lossy(
                &entry.szExeFile[..entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(260)],
            );

            let (cpu_ticks, cpu_time, mem_ws) = proc_stats(pid);

            map.insert(pid, ProcEntry {
                pid,
                name,
                threads: entry.cntThreads,
                cpu_ticks,
                cpu_time,
                mem_ws,
                cpu_pct: 0.0,
            });

            if Process32NextW(snap, &mut entry) == 0 { break; }
        }
        CloseHandle(snap);
    }
    map
}

#[cfg(not(windows))]
fn snapshot_procs() -> HashMap<u32, ProcEntry> { HashMap::new() }

#[cfg(windows)]
fn proc_stats(pid: u32) -> (u64, u64, u64) {
    unsafe {
        let flags  = PROCESS_QUERY_LIMITED_INFORMATION;
        let handle: HANDLE = OpenProcess(flags, 0, pid);
        if handle == 0 { return (0, 0, 0); }

        // CPU time
        let mut creation = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut exit     = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut kernel   = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut user     = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        let cpu_ticks = ft_to_u64(kernel) + ft_to_u64(user);

        // Memory — needs a separate handle with VM_READ; try just query info
        let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        GetProcessMemoryInfo(handle, &mut pmc, pmc.cb);
        let mem_ws = pmc.WorkingSetSize as u64;

        CloseHandle(handle);
        (cpu_ticks, cpu_ticks, mem_ws)
    }
}

#[cfg(not(windows))]
fn proc_stats(_pid: u32) -> (u64, u64, u64) { (0, 0, 0) }

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
        let total = m.ullTotalPhys / (1024 * 1024);
        let avail = m.ullAvailPhys  / (1024 * 1024);
        let swap_total = m.ullTotalPageFile / (1024 * 1024);
        let swap_avail = m.ullAvailPageFile / (1024 * 1024);
        MemInfo {
            total,
            used:       total.saturating_sub(avail),
            swap_total,
            swap_used:  swap_total.saturating_sub(swap_avail),
        }
    }
}

#[cfg(not(windows))]
fn get_mem_info() -> MemInfo { MemInfo { total: 0, used: 0, swap_total: 0, swap_used: 0 } }

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
fn get_uptime() -> u64 {
    unsafe { GetTickCount64() / 1000 }
}

#[cfg(not(windows))]
fn get_uptime() -> u64 { 0 }

#[cfg(windows)]
fn kill_process(pid: u32) -> bool {
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h == 0 { return false; }
        let ok = TerminateProcess(h, 1) != 0;
        CloseHandle(h);
        ok
    }
}

#[cfg(not(windows))]
fn kill_process(_: u32) -> bool { false }

fn compute_proc_cpu(
    prev:      &HashMap<u32, ProcEntry>,
    curr:      &mut HashMap<u32, ProcEntry>,
    wall1:     u64,
    wall2:     u64,
    cpu_count: u32,
) -> f64 {
    let wall_100ns = wall2.saturating_sub(wall1) * 10_000; // ms → 100ns
    let scale      = (wall_100ns * cpu_count as u64) as f64;
    if scale == 0.0 { return 0.0; }

    let mut total_delta: u64 = 0;
    for (pid, entry) in curr.iter_mut() {
        if let Some(old) = prev.get(pid) {
            let delta = entry.cpu_ticks.saturating_sub(old.cpu_ticks);
            entry.cpu_pct = (delta as f64 / scale) * 100.0;
            total_delta  += delta;
        }
    }
    let sys_scale = (wall_100ns * cpu_count as u64) as f64;
    (total_delta as f64 / sys_scale * 100.0).min(100.0)
}

// Formatting helpers

fn fmt_uptime(s: u64) -> String {
    let d = s / 86_400;
    let h = (s % 86_400) / 3600;
    let m = (s % 3600) / 60;
    if d > 0 { format!("{d} day{}, {h:02}:{m:02}", if d == 1 { "" } else { "s" }) }
    else      { format!("{h:02}:{m:02}") }
}

fn fmt_time(ticks_100ns: u64) -> String {
    let total_cs = ticks_100ns / 100_000;
    let mins  = total_cs / 6000;
    let secs  = (total_cs % 6000) / 100;
    let cs    = total_cs % 100;
    format!("{mins}:{secs:02}.{cs:02}")
}

fn fmt_mib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() }
    else { s.chars().take(n.saturating_sub(1)).collect::<String>() + "…" }
}

fn pad_right(s: &str, n: usize) -> String {
    format!("{:<width$}", trunc(s, n), width = n)
}

fn pad_left(s: &str, n: usize) -> String {
    format!("{:>width$}", s, width = n)
}

fn draw(app: &App, out: &mut impl Write) -> io::Result<()> {
    queue!(out, cursor::Hide)?;

    let cols = app.cols;

    let now    = chrono_time();
    let uptime = fmt_uptime(app.uptime_sec);
    let h0 = format!(
        "top - {}   up {}   tasks: {}   cpus: {}",
        now, uptime, app.procs.len(), app.cpu_count
    );
    header_row(out, &h0, cols, 0)?;

    let cpu_bar = bar_str(app.cpu_pct, 20);
    let h1 = format!("%Cpu(s): {:>5.1}%  {}", app.cpu_pct, cpu_bar);
    header_row(out, &h1, cols, 1)?;

    let mem_bar  = bar_str(
        if app.mem.total > 0 { app.mem.used as f64 / app.mem.total as f64 * 100.0 } else { 0.0 },
        20,
    );
    let h2 = format!(
        "MiB Mem: {:>8} total  {:>8} free  {:>8} used  {}",
        app.mem.total, app.mem.total.saturating_sub(app.mem.used), app.mem.used, mem_bar
    );
    header_row(out, &h2, cols, 2)?;

    let swp_bar = bar_str(
        if app.mem.swap_total > 0 { app.mem.swap_used as f64 / app.mem.swap_total as f64 * 100.0 } else { 0.0 },
        20,
    );
    let h3 = format!(
        "MiB Swp: {:>8} total  {:>8} free  {:>8} used  {}",
        app.mem.swap_total, app.mem.swap_total.saturating_sub(app.mem.swap_used), app.mem.swap_used, swp_bar
    );
    header_row(out, &h3, cols, 3)?;

    queue!(out,
        cursor::MoveTo(0, 4),
        terminal::Clear(ClearType::CurrentLine),
    )?;

    let name_w = cols.saturating_sub(52);
    let col_hdr = format!(
        "{} {} {} {} {} {} {}",
        pad_left("PID",    7),
        pad_left("CPU%",   6),
        pad_left("MEM%",   5),
        pad_left("RSS MiB",8),
        pad_left("THR",    4),
        pad_left("TIME+",  9),
        pad_right("NAME",  name_w.max(4)),
    );
    let col_hdr = trunc(&col_hdr, cols);
    queue!(out,
        cursor::MoveTo(0, 5),
        SetBackgroundColor(Color::Cyan),
        SetForegroundColor(Color::Black),
        SetAttribute(Attribute::Bold),
        Print(format!("{:<width$}", col_hdr, width = cols)),
        ResetColor,
    )?;

    // Process rows
    let content_rows = app.rows.saturating_sub(8);
    let procs = filtered_sorted(app);
    let scroll = app.scroll.min(procs.len().saturating_sub(1));

    for row in 0..content_rows {
        let screen_y = (row + 6) as u16;
        queue!(out, cursor::MoveTo(0, screen_y), terminal::Clear(ClearType::CurrentLine))?;

        if let Some(p) = procs.get(scroll + row) {
            let mem_pct = if app.mem.total > 0 {
                (p.mem_ws as f64 / (app.mem.total as f64 * 1024.0 * 1024.0)) * 100.0
            } else { 0.0 };

            let line = format!(
                "{} {} {} {} {} {} {}",
                pad_left(&p.pid.to_string(),         7),
                pad_left(&format!("{:>5.1}", p.cpu_pct),  6),
                pad_left(&format!("{:>4.1}", mem_pct),    5),
                pad_left(&fmt_mib(p.mem_ws),              8),
                pad_left(&p.threads.to_string(),          4),
                pad_left(&fmt_time(p.cpu_time),           9),
                pad_right(&p.name,                        name_w.max(4)),
            );

            if p.cpu_pct >= 50.0 {
                queue!(out, SetForegroundColor(Color::Red), SetAttribute(Attribute::Bold))?;
            } else if p.cpu_pct >= 10.0 {
                queue!(out, SetForegroundColor(Color::Yellow))?;
            }
            queue!(out, Print(trunc(&line, cols)), ResetColor)?;
        }
    }

    let help_y = (app.rows - 2) as u16;

    if app.filtering {
        let filter_line = format!("Filter: {}█", app.filter);
        queue!(out,
            cursor::MoveTo(0, help_y),
            SetBackgroundColor(Color::DarkBlue),
            SetForegroundColor(Color::White),
            Print(format!("{:<width$}", trunc(&filter_line, cols), width = cols)),
            ResetColor,
        )?;
    } else {
        let scroll_info = if procs.len() > content_rows {
            format!("  [{}/{}]", scroll + 1, procs.len())
        } else { String::new() };
        let help = format!(
            "q:Quit  P:CPU  M:Mem  N:PID  T:Time  A:Name  R:Rev  /:Filter  k:Kill{}",
            scroll_info
        );
        queue!(out,
            cursor::MoveTo(0, help_y),
            SetBackgroundColor(Color::Black),
            SetForegroundColor(Color::Cyan),
            Print(format!("{:<width$}", trunc(&help, cols), width = cols)),
            ResetColor,
        )?;
    }

    let hint_y = (app.rows - 1) as u16;
    let hint = format!(
        "Sorted by: {:?}{}  |  Processes: {}  |  interval: {}s",
        app.sort_by,
        if app.sort_rev { " ▲" } else { " ▼" },
        procs.len(),
        app.update_ms / 1000,
    );
    queue!(out,
        cursor::MoveTo(0, hint_y),
        SetBackgroundColor(Color::Black),
        SetForegroundColor(Color::DarkGrey),
        Print(format!("{:<width$}", trunc(&hint, cols), width = cols)),
        ResetColor,
    )?;

    out.flush()
}

fn header_row(out: &mut impl Write, text: &str, cols: usize, y: u16) -> io::Result<()> {
    let padded = format!("{:<width$}", trunc(text, cols), width = cols);
    queue!(out,
        cursor::MoveTo(0, y),
        SetBackgroundColor(Color::Black),
        SetForegroundColor(Color::Green),
        Print(padded),
        ResetColor,
    )
}

fn bar_str(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let empty  = width - filled;
    format!("[{}{}]", "#".repeat(filled), " ".repeat(empty))
}

fn chrono_time() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        unsafe {
            let mut st: SYSTEMTIME = std::mem::zeroed();
            GetLocalTime(&mut st);
            return format!("{:02}:{:02}:{:02}", st.wHour, st.wMinute, st.wSecond);
        }
    }
    #[cfg(not(windows))]
    "00:00:00".to_string()
}

fn filtered_sorted(app: &App) -> Vec<ProcEntry> {
    let filter = app.filter.to_lowercase();
    let mut list: Vec<ProcEntry> = app.procs.iter()
        .filter(|p| filter.is_empty() || p.name.to_lowercase().contains(&filter)
                                       || p.pid.to_string().contains(&filter))
        .cloned()
        .collect();

    list.sort_by(|a, b| {
        let ord = match app.sort_by {
            SortBy::Cpu  => a.cpu_pct.partial_cmp(&b.cpu_pct).unwrap_or(std::cmp::Ordering::Equal),
            SortBy::Mem  => a.mem_ws.cmp(&b.mem_ws),
            SortBy::Pid  => a.pid.cmp(&b.pid),
            SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortBy::Time => a.cpu_time.cmp(&b.cpu_time),
        };
        if app.sort_rev { ord } else { ord.reverse() }
    });
    list
}

// Required for the Debug format in the hint bar
impl std::fmt::Debug for SortBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", match self {
            SortBy::Cpu  => "CPU%",
            SortBy::Mem  => "MEM",
            SortBy::Pid  => "PID",
            SortBy::Name => "NAME",
            SortBy::Time => "TIME",
        })
    }
}

fn prompt_line(label: &str, out: &mut impl Write, app: &App) -> io::Result<Option<String>> {
    let mut val = String::new();
    loop {
        let text = format!("{}{}_", label, val);
        queue!(out,
            cursor::MoveTo(0, (app.rows - 2) as u16),
            terminal::Clear(ClearType::CurrentLine),
            SetBackgroundColor(Color::DarkBlue),
            SetForegroundColor(Color::White),
            Print(format!("{:<width$}", &text[..text.len().min(app.cols)], width = app.cols)),
            ResetColor,
        )?;
        out.flush()?;

        if let Ok(Event::Key(k)) = event::read() {
            if !matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) { continue; }
            match k.code {
                KeyCode::Enter     => return Ok(Some(val)),
                KeyCode::Esc       => return Ok(None),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(None),
                KeyCode::Backspace => { val.pop(); }
                KeyCode::Char(c)   if k.modifiers.is_empty() || k.modifiers == KeyModifiers::SHIFT
                                   => val.push(c),
                _ => {}
            }
        }
    }
}

fn run() -> io::Result<()> {
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide)?;

    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let cpu_count    = get_cpu_count();

    let mut app = App {
        procs:     vec![],
        mem:       get_mem_info(),
        cpu_pct:   0.0,
        cpu_count,
        uptime_sec: get_uptime(),
        sort_by:   SortBy::Cpu,
        sort_rev:  false,
        scroll:    0,
        filter:    String::new(),
        filtering: false,
        cols:      cols as usize,
        rows:      rows as usize,
        update_ms: 2000,
    };

    let mut prev_wall  = wall_ms();
    let mut prev_procs = snapshot_procs();
    app.procs = prev_procs.values().cloned().collect();
    draw(&app, &mut out)?;

    let mut last_update = Instant::now();

    loop {
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Resize(c, r) => {
                    app.cols = c as usize;
                    app.rows = r as usize;
                    draw(&app, &mut out)?;
                }
                Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    if app.filtering {
                        match k.code {
                            KeyCode::Esc | KeyCode::Enter => { app.filtering = false; }
                            KeyCode::Backspace => { app.filter.pop(); }
                            KeyCode::Char(c) if k.modifiers.is_empty() || k.modifiers == KeyModifiers::SHIFT
                                => app.filter.push(c),
                            _ => {}
                        }
                    } else {
                        match k.code {
                            KeyCode::Char('q') | KeyCode::Char('Q') => break,
                            KeyCode::Char('P') | KeyCode::Char('p') => { app.sort_by = SortBy::Cpu;  app.scroll = 0; }
                            KeyCode::Char('M') | KeyCode::Char('m') => { app.sort_by = SortBy::Mem;  app.scroll = 0; }
                            KeyCode::Char('N') | KeyCode::Char('n') => { app.sort_by = SortBy::Pid;  app.scroll = 0; }
                            KeyCode::Char('T') | KeyCode::Char('t') => { app.sort_by = SortBy::Time; app.scroll = 0; }
                            KeyCode::Char('A') | KeyCode::Char('a') => { app.sort_by = SortBy::Name; app.scroll = 0; }
                            KeyCode::Char('R') | KeyCode::Char('r') => { app.sort_rev = !app.sort_rev; }
                            KeyCode::Char('/') => { app.filtering = true; app.filter.clear(); }
                            KeyCode::Char('k') | KeyCode::Char('K') => {
                                if let Some(inp) = prompt_line("Kill PID: ", &mut out, &app)? {
                                    if let Ok(pid) = inp.trim().parse::<u32>() {
                                        let ok = kill_process(pid);
                                        // Brief status — will be overwritten on next draw
                                        let msg = if ok { format!("Sent SIGKILL to {pid}") }
                                                  else   { format!("Failed to kill {pid} (access denied?)") };
                                        queue!(out,
                                            cursor::MoveTo(0, (app.rows - 2) as u16),
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
                            KeyCode::Up   => { app.scroll = app.scroll.saturating_sub(1); }
                            KeyCode::Down => { app.scroll = app.scroll.saturating_add(1); }
                            KeyCode::PageUp   => { app.scroll = app.scroll.saturating_sub(app.rows / 2); }
                            KeyCode::PageDown => { app.scroll = app.scroll.saturating_add(app.rows / 2); }
                            KeyCode::Home => { app.scroll = 0; }
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
            let mut curr_procs = snapshot_procs();

            app.cpu_pct    = compute_proc_cpu(&prev_procs, &mut curr_procs, prev_wall, curr_wall, cpu_count);
            app.mem        = get_mem_info();
            app.uptime_sec = get_uptime();
            app.procs      = curr_procs.values().cloned().collect();

            let visible = filtered_sorted(&app).len().saturating_sub(1);
            if app.scroll > visible { app.scroll = visible; }

            prev_wall  = curr_wall;
            prev_procs = curr_procs.iter().map(|(&k, v)| (k, v.clone())).collect();
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
        println!("Usage: top [options]");
        println!("  Real-time process monitor for Windows.");
        println!();
        println!("  Keys:");
        println!("    q        Quit");
        println!("    P        Sort by CPU%");
        println!("    M        Sort by Memory");
        println!("    N        Sort by PID");
        println!("    T        Sort by CPU Time");
        println!("    R        Reverse sort order");
        println!("    /        Filter by name or PID");
        println!("    k        Kill process (by PID)");
        println!("    Up/Down  Scroll process list");
        return;
    }

    if let Err(e) = run() {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
        eprintln!("top: {}", e);
        std::process::exit(1);
    }
}
