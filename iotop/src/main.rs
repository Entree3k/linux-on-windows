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
    Foundation::{CloseHandle, HANDLE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
            PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        },
        SystemInformation::GetTickCount64,
        Threading::{
            GetProcessIoCounters, OpenProcess,
            IO_COUNTERS, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

// Data structures

#[derive(Clone)]
struct IoEntry {
    pid:    u32,
    name:   String,
    read:   u64,    // cumulative bytes read
    write:  u64,    // cumulative bytes written
    r_rate: u64,    // bytes/sec read
    w_rate: u64,    // bytes/sec write
    io_pct: f64,    // fraction of total I/O this interval
}

#[derive(Clone, Copy, PartialEq)]
enum SortBy { Total, Read, Write, Pid, Name }

struct App {
    procs:     Vec<IoEntry>,
    total_r:   u64,
    total_w:   u64,
    sort_by:   SortBy,
    sort_rev:  bool,
    scroll:    usize,
    filter:    String,
    filtering: bool,
    cols:      usize,
    rows:      usize,
    update_ms: u64,
}

// Windows helpers

#[cfg(windows)]
fn wall_ms() -> u64 { unsafe { GetTickCount64() } }
#[cfg(not(windows))]
fn wall_ms() -> u64 { 0 }

#[cfg(windows)]
fn proc_io(pid: u32) -> (u64, u64) {
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 { return (0, 0); }
        let mut io: IO_COUNTERS = std::mem::zeroed();
        let ok = GetProcessIoCounters(h, &mut io) != 0;
        CloseHandle(h);
        if ok { (io.ReadTransferCount, io.WriteTransferCount) } else { (0, 0) }
    }
}
#[cfg(not(windows))]
fn proc_io(_: u32) -> (u64, u64) { (0, 0) }

#[cfg(windows)]
fn snapshot() -> HashMap<u32, IoEntry> {
    let mut map = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == -1isize { return map; }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut e) == 0 { CloseHandle(snap); return map; }
        loop {
            let pid  = e.th32ProcessID;
            let name = String::from_utf16_lossy(
                &e.szExeFile[..e.szExeFile.iter().position(|&c| c == 0).unwrap_or(260)],
            );
            let (read, write) = proc_io(pid);
            map.insert(pid, IoEntry { pid, name, read, write, r_rate: 0, w_rate: 0, io_pct: 0.0 });
            if Process32NextW(snap, &mut e) == 0 { break; }
        }
        CloseHandle(snap);
    }
    map
}
#[cfg(not(windows))]
fn snapshot() -> HashMap<u32, IoEntry> { HashMap::new() }

fn compute_rates(
    prev:       &HashMap<u32, IoEntry>,
    curr:       &mut HashMap<u32, IoEntry>,
    elapsed_ms: u64,
) -> (u64, u64) {
    let secs = elapsed_ms.max(1) as f64 / 1000.0;
    let (mut total_r, mut total_w) = (0u64, 0u64);

    for (pid, e) in curr.iter_mut() {
        if let Some(old) = prev.get(pid) {
            e.r_rate = (e.read.saturating_sub(old.read) as f64 / secs) as u64;
            e.w_rate = (e.write.saturating_sub(old.write) as f64 / secs) as u64;
        }
        total_r += e.r_rate;
        total_w += e.w_rate;
    }
    let total = total_r + total_w;
    for e in curr.values_mut() {
        e.io_pct = if total > 0 { (e.r_rate + e.w_rate) as f64 / total as f64 * 100.0 } else { 0.0 };
    }
    (total_r, total_w)
}

// Formatting

fn fmt_bytes(n: u64) -> String {
    const G: u64 = 1 << 30;
    const M: u64 = 1 << 20;
    const K: u64 = 1 << 10;
    if n >= G      { format!("{:.1}G", n as f64 / G as f64) }
    else if n >= M { format!("{:.1}M", n as f64 / M as f64) }
    else if n >= K { format!("{:.1}K", n as f64 / K as f64) }
    else           { format!("{} B", n) }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() }
    else { s.chars().take(n.saturating_sub(1)).collect::<String>() + "…" }
}
fn pad_r(s: &str, n: usize) -> String { format!("{:<width$}", trunc(s, n), width = n) }
fn pad_l(s: &str, n: usize) -> String { format!("{:>width$}", s, width = n) }

// Rendering

fn filtered_sorted(app: &App) -> Vec<IoEntry> {
    let f = app.filter.to_lowercase();
    let mut list: Vec<IoEntry> = app.procs.iter()
        .filter(|p| f.is_empty()
            || p.name.to_lowercase().contains(&f)
            || p.pid.to_string().contains(&f))
        .cloned()
        .collect();

    list.sort_by(|a, b| {
        let ord = match app.sort_by {
            SortBy::Total => (a.r_rate + a.w_rate).cmp(&(b.r_rate + b.w_rate)),
            SortBy::Read  => a.r_rate.cmp(&b.r_rate),
            SortBy::Write => a.w_rate.cmp(&b.w_rate),
            SortBy::Pid   => a.pid.cmp(&b.pid),
            SortBy::Name  => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        };
        if app.sort_rev { ord } else { ord.reverse() }
    });
    list
}

fn draw(app: &App, out: &mut impl Write) -> io::Result<()> {
    let cols = app.cols;

    // Header
    let h = format!(
        "iotop  Total:  Read {}/s   Write {}/s    Interval: {:.1}s",
        fmt_bytes(app.total_r), fmt_bytes(app.total_w),
        app.update_ms as f64 / 1000.0,
    );
    queue!(out,
        cursor::MoveTo(0, 0),
        SetBackgroundColor(Color::Black), SetForegroundColor(Color::Green),
        Print(format!("{:<width$}", trunc(&h, cols), width = cols)),
        ResetColor,
        cursor::MoveTo(0, 1), terminal::Clear(ClearType::CurrentLine),
    )?;

    // Column headers
    let name_w = cols.saturating_sub(44);
    let hdr = format!(
        "{}  {}  {}  {}  {}",
        pad_l("PID", 7),
        pad_l("IO%", 6),
        pad_l("READ/s", 10),
        pad_l("WRITE/s", 10),
        pad_r("COMMAND", name_w.max(4)),
    );
    queue!(out,
        cursor::MoveTo(0, 2),
        SetBackgroundColor(Color::Cyan), SetForegroundColor(Color::Black),
        SetAttribute(Attribute::Bold),
        Print(format!("{:<width$}", trunc(&hdr, cols), width = cols)),
        ResetColor,
    )?;

    // Process rows
    let content = app.rows.saturating_sub(5);
    let procs   = filtered_sorted(app);
    let scroll  = app.scroll.min(procs.len().saturating_sub(1));

    for row in 0..content {
        let y = (row + 3) as u16;
        queue!(out, cursor::MoveTo(0, y), terminal::Clear(ClearType::CurrentLine))?;

        if let Some(p) = procs.get(scroll + row) {
            let total_rate = p.r_rate + p.w_rate;
            let line = format!(
                "{}  {}  {}  {}  {}",
                pad_l(&p.pid.to_string(), 7),
                pad_l(&format!("{:>5.1}", p.io_pct), 6),
                pad_l(&format!("{}/s", fmt_bytes(p.r_rate)), 10),
                pad_l(&format!("{}/s", fmt_bytes(p.w_rate)), 10),
                pad_r(&p.name, name_w.max(4)),
            );
            if total_rate >= 100 << 20 {
                queue!(out, SetForegroundColor(Color::Red), SetAttribute(Attribute::Bold))?;
            } else if total_rate >= 1 << 20 {
                queue!(out, SetForegroundColor(Color::Yellow))?;
            } else if total_rate > 0 {
                queue!(out, SetForegroundColor(Color::White))?;
            } else {
                queue!(out, SetForegroundColor(Color::DarkGrey))?;
            }
            queue!(out, Print(trunc(&line, cols)), ResetColor)?;
        }
    }

    // Footer
    let hy = (app.rows - 2) as u16;
    if app.filtering {
        let fl = format!("Filter: {}█", app.filter);
        queue!(out,
            cursor::MoveTo(0, hy),
            SetBackgroundColor(Color::DarkBlue), SetForegroundColor(Color::White),
            Print(format!("{:<width$}", trunc(&fl, cols), width = cols)),
            ResetColor,
        )?;
    } else {
        let sort_label = match app.sort_by {
            SortBy::Total => "Total",
            SortBy::Read  => "Read",
            SortBy::Write => "Write",
            SortBy::Pid   => "PID",
            SortBy::Name  => "Name",
        };
        let help = format!(
            "q=Quit  I=Total  R=Read  W=Write  N=PID  A=Name  r=Reverse  /=Filter  \
             Sort:{}{} [{}/{}]",
            sort_label,
            if app.sort_rev { "▲" } else { "▼" },
            scroll + 1, procs.len().max(1),
        );
        queue!(out,
            cursor::MoveTo(0, hy),
            SetBackgroundColor(Color::Black), SetForegroundColor(Color::Cyan),
            Print(format!("{:<width$}", trunc(&help, cols), width = cols)),
            ResetColor,
        )?;
    }

    let hint_y = (app.rows - 1) as u16;
    let hint = if app.filter.is_empty() {
        format!("Showing {} processes", procs.len())
    } else {
        format!("Filtered by \"{}\" — {} match(es)", app.filter, procs.len())
    };
    queue!(out,
        cursor::MoveTo(0, hint_y),
        SetBackgroundColor(Color::Black), SetForegroundColor(Color::DarkGrey),
        Print(format!("{:<width$}", trunc(&hint, cols), width = cols)),
        ResetColor,
    )?;

    out.flush()
}

// Event loop

fn run() -> io::Result<()> {
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide)?;

    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let mut app = App {
        procs: vec![], total_r: 0, total_w: 0,
        sort_by: SortBy::Total, sort_rev: false,
        scroll: 0, filter: String::new(), filtering: false,
        cols: cols as usize, rows: rows as usize, update_ms: 2000,
    };

    let mut prev_wall  = wall_ms();
    let mut prev_procs = snapshot();
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
                            KeyCode::Esc | KeyCode::Enter => { app.filtering = false; }
                            KeyCode::Backspace => { app.filter.pop(); }
                            KeyCode::Char(c) if k.modifiers.is_empty() || k.modifiers == KeyModifiers::SHIFT
                                => app.filter.push(c),
                            _ => {}
                        }
                    } else {
                        match k.code {
                            KeyCode::Char('q') | KeyCode::Char('Q') => break,
                            KeyCode::Char('I') | KeyCode::Char('i') => { app.sort_by = SortBy::Total; app.scroll = 0; }
                            KeyCode::Char('R') => { app.sort_by = SortBy::Read;  app.scroll = 0; }
                            KeyCode::Char('W') | KeyCode::Char('w') => { app.sort_by = SortBy::Write; app.scroll = 0; }
                            KeyCode::Char('N') | KeyCode::Char('n') => { app.sort_by = SortBy::Pid;   app.scroll = 0; }
                            KeyCode::Char('A') | KeyCode::Char('a') => { app.sort_by = SortBy::Name;  app.scroll = 0; }
                            KeyCode::Char('r') => app.sort_rev = !app.sort_rev,
                            KeyCode::Char('/') => { app.filtering = true; app.filter.clear(); }
                            KeyCode::Up        => { app.scroll = app.scroll.saturating_sub(1); }
                            KeyCode::Down      => { app.scroll += 1; }
                            KeyCode::PageUp    => { app.scroll = app.scroll.saturating_sub(app.rows / 2); }
                            KeyCode::PageDown  => { app.scroll += app.rows / 2; }
                            KeyCode::Home      => { app.scroll = 0; }
                            _ => {}
                        }
                    }
                    draw(&app, &mut out)?;
                }
                _ => {}
            }
        }

        if last_update.elapsed().as_millis() as u64 >= app.update_ms {
            let curr_wall = wall_ms();
            let mut curr  = snapshot();
            let (tr, tw)  = compute_rates(&prev_procs, &mut curr, curr_wall.saturating_sub(prev_wall));
            app.total_r   = tr;
            app.total_w   = tw;
            app.procs     = curr.values().cloned().collect();

            let max_scroll = filtered_sorted(&app).len().saturating_sub(1);
            app.scroll = app.scroll.min(max_scroll);

            prev_wall  = curr_wall;
            prev_procs = curr;
            last_update = Instant::now();
            draw(&app, &mut out)?;
        }
    }

    execute!(out, terminal::LeaveAlternateScreen, cursor::Show)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

fn main() {
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("Usage: iotop");
        println!("  Real-time disk I/O monitor per process.");
        println!();
        println!("  Keys:");
        println!("    q        Quit");
        println!("    I        Sort by total I/O (default)");
        println!("    R        Sort by read rate");
        println!("    W        Sort by write rate");
        println!("    N        Sort by PID");
        println!("    A        Sort by name");
        println!("    r        Reverse sort order");
        println!("    /        Filter by name or PID");
        println!("    Up/Down  Scroll");
        return;
    }
    if let Err(e) = run() {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
        eprintln!("iotop: {}", e);
        std::process::exit(1);
    }
}
