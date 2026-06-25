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
    Foundation::CloseHandle,
    NetworkManagement::{
        IpHelper::{
            FreeMibTable, GetExtendedTcpTable, GetExtendedUdpTable, GetIfTable2Ex,
            MIB_IF_TABLE2, MIB_TCPTABLE_OWNER_PID, MIB_UDPTABLE_OWNER_PID,
            MibIfTableNormal, TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
        },
        Ndis::{IF_OPER_STATUS, IfOperStatusUp},
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
            PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        },
        SystemInformation::GetTickCount64,
    },
};

// ── Data structures ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct IfInfo {
    name:      String,
    in_octets: u64,
    out_octets: u64,
    in_rate:   u64,  // bytes/sec in (computed from delta)
    out_rate:  u64,  // bytes/sec out
    is_up:     bool,
}

#[derive(Clone)]
struct TcpConn {
    local:   String,
    remote:  String,
    state:   &'static str,
    pid:     u32,
    process: String,
}

#[derive(Clone)]
struct UdpConn {
    local:   String,
    pid:     u32,
    process: String,
}

struct App {
    interfaces:  Vec<IfInfo>,
    connections: Vec<TcpConn>,
    udp_ports:   Vec<UdpConn>,
    show_udp:    bool,
    filter:      String,
    filtering:   bool,
    scroll:      usize,
    cols:        usize,
    rows:        usize,
    update_ms:   u64,
}

// ── Windows helpers ───────────────────────────────────────────────────────────

#[cfg(windows)]
fn wall_ms() -> u64 { unsafe { GetTickCount64() } }
#[cfg(not(windows))]
fn wall_ms() -> u64 { 0 }

#[cfg(windows)]
fn process_names() -> HashMap<u32, String> {
    let mut map = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == -1isize { return map; }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut e) == 0 { CloseHandle(snap); return map; }
        loop {
            let name = String::from_utf16_lossy(
                &e.szExeFile[..e.szExeFile.iter().position(|&c| c == 0).unwrap_or(260)],
            );
            map.insert(e.th32ProcessID, name);
            if Process32NextW(snap, &mut e) == 0 { break; }
        }
        CloseHandle(snap);
    }
    map
}
#[cfg(not(windows))]
fn process_names() -> HashMap<u32, String> { HashMap::new() }

fn fmt_ipv4(addr: u32, port: u32) -> String {
    let a = (addr & 0xFF) as u8;
    let b = (addr >> 8  & 0xFF) as u8;
    let c = (addr >> 16 & 0xFF) as u8;
    let d = (addr >> 24 & 0xFF) as u8;
    let p = ((port & 0xFF) << 8 | (port >> 8 & 0xFF)) as u16;
    format!("{}.{}.{}.{}:{}", a, b, c, d, p)
}

fn tcp_state(s: u32) -> &'static str {
    match s {
        1 => "CLOSED",     2 => "LISTEN",     3 => "SYN_SNT",
        4 => "SYN_RCV",    5 => "ESTAB",      6 => "FIN_W1",
        7 => "FIN_W2",     8 => "CLOSE_W",    9 => "CLOSING",
        10 => "LAST_ACK", 11 => "TIME_W",    12 => "DELETE",
        _ => "UNKNWN",
    }
}

#[cfg(windows)]
fn get_interfaces(prev: &[IfInfo], elapsed_ms: u64) -> Vec<IfInfo> {
    let secs = elapsed_ms.max(1) as f64 / 1000.0;
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2Ex(MibIfTableNormal, &mut table) != 0 || table.is_null() {
            return vec![];
        }
        let count = (*table).NumEntries as usize;
        let rows  = std::slice::from_raw_parts((*table).Table.as_ptr(), count);

        let result: Vec<IfInfo> = rows.iter().filter_map(|row| {
            if row.OperStatus != IfOperStatusUp { return None; }
            let name_slice = &row.Alias[..row.Alias.iter().position(|&c| c == 0).unwrap_or(256)];
            let name = String::from_utf16_lossy(name_slice);
            if name.trim().is_empty() { return None; }

            let in_rate  = prev.iter().find(|p| p.name == name)
                .map(|p| ((row.InOctets.saturating_sub(p.in_octets) as f64 / secs) as u64))
                .unwrap_or(0);
            let out_rate = prev.iter().find(|p| p.name == name)
                .map(|p| ((row.OutOctets.saturating_sub(p.out_octets) as f64 / secs) as u64))
                .unwrap_or(0);

            Some(IfInfo {
                name,
                in_octets:  row.InOctets,
                out_octets: row.OutOctets,
                in_rate, out_rate,
                is_up: true,
            })
        }).collect();

        FreeMibTable(table as *const _);
        result
    }
}
#[cfg(not(windows))]
fn get_interfaces(_: &[IfInfo], _: u64) -> Vec<IfInfo> { vec![] }

#[cfg(windows)]
fn get_tcp_connections(pnames: &HashMap<u32, String>) -> Vec<TcpConn> {
    unsafe {
        let mut size: u32 = 0;
        GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, 2, TCP_TABLE_OWNER_PID_ALL, 0);
        if size == 0 { return vec![]; }

        let mut buf = vec![0u8; size as usize + 256];
        let ret = GetExtendedTcpTable(buf.as_mut_ptr() as *mut _, &mut size, 1, 2, TCP_TABLE_OWNER_PID_ALL, 0);
        if ret != 0 { return vec![]; }

        let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
        let count = table.dwNumEntries as usize;
        let rows  = std::slice::from_raw_parts(table.table.as_ptr(), count);

        rows.iter().map(|r| {
            let local  = fmt_ipv4(r.dwLocalAddr,  r.dwLocalPort);
            let remote = if r.dwState == 2 { // LISTEN — no remote
                "*:*".to_owned()
            } else {
                fmt_ipv4(r.dwRemoteAddr, r.dwRemotePort)
            };
            let process = pnames.get(&r.dwOwningPid)
                .cloned()
                .unwrap_or_else(|| format!("[{}]", r.dwOwningPid));
            TcpConn {
                local, remote, state: tcp_state(r.dwState),
                pid: r.dwOwningPid, process,
            }
        }).collect()
    }
}
#[cfg(not(windows))]
fn get_tcp_connections(_: &HashMap<u32, String>) -> Vec<TcpConn> { vec![] }

#[cfg(windows)]
fn get_udp_ports(pnames: &HashMap<u32, String>) -> Vec<UdpConn> {
    unsafe {
        let mut size: u32 = 0;
        GetExtendedUdpTable(std::ptr::null_mut(), &mut size, 0, 2, UDP_TABLE_OWNER_PID, 0);
        if size == 0 { return vec![]; }

        let mut buf = vec![0u8; size as usize + 256];
        let ret = GetExtendedUdpTable(buf.as_mut_ptr() as *mut _, &mut size, 1, 2, UDP_TABLE_OWNER_PID, 0);
        if ret != 0 { return vec![]; }

        let table = &*(buf.as_ptr() as *const MIB_UDPTABLE_OWNER_PID);
        let count = table.dwNumEntries as usize;
        let rows  = std::slice::from_raw_parts(table.table.as_ptr(), count);

        rows.iter().map(|r| {
            let local   = fmt_ipv4(r.dwLocalAddr, r.dwLocalPort);
            let process = pnames.get(&r.dwOwningPid)
                .cloned()
                .unwrap_or_else(|| format!("[{}]", r.dwOwningPid));
            UdpConn { local, pid: r.dwOwningPid, process }
        }).collect()
    }
}
#[cfg(not(windows))]
fn get_udp_ports(_: &HashMap<u32, String>) -> Vec<UdpConn> { vec![] }

// ── Formatting ────────────────────────────────────────────────────────────────

fn fmt_bytes(n: u64) -> String {
    const G: u64 = 1 << 30;
    const M: u64 = 1 << 20;
    const K: u64 = 1 << 10;
    if n >= G      { format!("{:.1}GB/s", n as f64 / G as f64) }
    else if n >= M { format!("{:.1}MB/s", n as f64 / M as f64) }
    else if n >= K { format!("{:.1}KB/s", n as f64 / K as f64) }
    else           { format!("{} B/s", n) }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() }
    else { s.chars().take(n.saturating_sub(1)).collect::<String>() + "…" }
}
fn pad_r(s: &str, n: usize) -> String { format!("{:<width$}", trunc(s, n), width = n) }
fn pad_l(s: &str, n: usize) -> String { format!("{:>width$}", s, width = n) }

// ── Rendering ─────────────────────────────────────────────────────────────────

fn draw(app: &App, out: &mut impl Write) -> io::Result<()> {
    let cols = app.cols;
    let mut y = 0u16;

    // ── Title ─────────────────────────────────────────────────────────────────
    let title = format!(
        "iptop — network monitor   {} connections{}  interval: {:.1}s",
        app.connections.len(),
        if app.show_udp { format!("  {} udp ports", app.udp_ports.len()) } else { String::new() },
        app.update_ms as f64 / 1000.0,
    );
    queue!(out,
        cursor::MoveTo(0, y),
        SetBackgroundColor(Color::DarkBlue), SetForegroundColor(Color::White),
        SetAttribute(Attribute::Bold),
        Print(format!("{:<width$}", trunc(&title, cols), width = cols)),
        ResetColor,
    )?;
    y += 1;

    // ── Interface section ─────────────────────────────────────────────────────
    let iface_label = "Interfaces:";
    queue!(out,
        cursor::MoveTo(0, y),
        SetForegroundColor(Color::Cyan), SetBackgroundColor(Color::Black),
        SetAttribute(Attribute::Bold),
        Print(format!("{:<width$}", iface_label, width = cols)),
        ResetColor,
    )?;
    y += 1;

    let max_iface_rows = 6usize;
    let iface_count = app.interfaces.len().min(max_iface_rows);

    if iface_count == 0 {
        queue!(out, cursor::MoveTo(0, y), terminal::Clear(ClearType::CurrentLine),
            SetForegroundColor(Color::DarkGrey),
            Print("  (no active interfaces)"),
            ResetColor,
        )?;
        y += 1;
    } else {
        for iface in app.interfaces.iter().take(iface_count) {
            let in_color  = if iface.in_rate  > 10 << 20 { Color::Red }
                            else if iface.in_rate  > 1 << 20  { Color::Yellow }
                            else { Color::Green };
            let out_color = if iface.out_rate > 10 << 20 { Color::Red }
                            else if iface.out_rate > 1 << 20  { Color::Yellow }
                            else { Color::Green };

            let name = pad_r(&iface.name, 20);
            queue!(out,
                cursor::MoveTo(0, y), terminal::Clear(ClearType::CurrentLine),
                SetForegroundColor(Color::White), Print(format!("  {} ", name)), ResetColor,
                SetForegroundColor(Color::DarkGrey), Print("↑ "), ResetColor,
                SetForegroundColor(out_color), SetAttribute(Attribute::Bold),
                Print(format!("{:<12}", fmt_bytes(iface.out_rate))), ResetColor,
                SetForegroundColor(Color::DarkGrey), Print("↓ "), ResetColor,
                SetForegroundColor(in_color), SetAttribute(Attribute::Bold),
                Print(fmt_bytes(iface.in_rate)), ResetColor,
            )?;
            y += 1;
        }
    }

    // Blank separator
    queue!(out, cursor::MoveTo(0, y), terminal::Clear(ClearType::CurrentLine))?;
    y += 1;

    // ── Connection section ────────────────────────────────────────────────────
    let conn_label = if app.show_udp { "Connections (TCP + UDP ports):" } else { "Connections (TCP):" };
    queue!(out,
        cursor::MoveTo(0, y),
        SetForegroundColor(Color::Cyan), SetBackgroundColor(Color::Black),
        SetAttribute(Attribute::Bold),
        Print(format!("{:<width$}", conn_label, width = cols)),
        ResetColor,
    )?;
    y += 1;

    // Column header
    let proc_w = cols.saturating_sub(60);
    let hdr = format!(
        "{}  {}  {}  {}  {}",
        pad_r("Local",   21),
        pad_r("Remote",  21),
        pad_l("State",    8),
        pad_l("PID",      6),
        pad_r("Process", proc_w.max(4)),
    );
    queue!(out,
        cursor::MoveTo(0, y),
        SetBackgroundColor(Color::Cyan), SetForegroundColor(Color::Black),
        SetAttribute(Attribute::Bold),
        Print(format!("{:<width$}", trunc(&hdr, cols), width = cols)),
        ResetColor,
    )?;
    y += 1;

    // Filtered + scrolled connections
    let filter = app.filter.to_lowercase();
    let mut conns: Vec<&TcpConn> = app.connections.iter()
        .filter(|c| filter.is_empty()
            || c.local.contains(&filter)
            || c.remote.contains(&filter)
            || c.process.to_lowercase().contains(&filter)
            || c.pid.to_string().contains(&filter))
        .collect();

    // Sort: ESTAB first, then LISTEN, then others, then by process name
    conns.sort_by(|a, b| {
        fn state_ord(s: &str) -> u8 {
            match s { "ESTAB" => 0, "LISTEN" => 1, "CLOSE_W" | "TIME_W" => 3, _ => 2 }
        }
        state_ord(a.state).cmp(&state_ord(b.state))
            .then(a.process.cmp(&b.process))
    });

    let footer_rows = 2u16;
    let content = (app.rows as u16).saturating_sub(y + footer_rows) as usize;
    let scroll  = app.scroll.min(conns.len().saturating_sub(1));

    for row in 0..content {
        let sy = y + row as u16;
        queue!(out, cursor::MoveTo(0, sy), terminal::Clear(ClearType::CurrentLine))?;

        if let Some(c) = conns.get(scroll + row) {
            let state_color = match c.state {
                "ESTAB"            => Color::Green,
                "LISTEN"           => Color::Cyan,
                "TIME_W" | "CLOSE_W" => Color::Yellow,
                "SYN_SNT" | "SYN_RCV" => Color::Blue,
                _                  => Color::DarkGrey,
            };
            let line = format!(
                "{}  {}  {}  {}  {}",
                pad_r(&c.local,   21),
                pad_r(&c.remote,  21),
                pad_l(c.state,     8),
                pad_l(&c.pid.to_string(), 6),
                pad_r(&c.process, proc_w.max(4)),
            );
            let (first, rest) = line.split_at(44.min(line.len()));
            queue!(out,
                SetForegroundColor(Color::White), Print(trunc(first, 44)), ResetColor,
                SetForegroundColor(state_color), SetAttribute(Attribute::Bold),
                Print(&rest[..rest.len().min(8)]),
                ResetColor,
                Print(trunc(&rest[rest.len().min(8)..], cols.saturating_sub(52))),
            )?;
        }
    }

    // ── Footer ────────────────────────────────────────────────────────────────
    let hy = (app.rows as u16).saturating_sub(2);
    if app.filtering {
        let fl = format!("Filter: {}█", app.filter);
        queue!(out,
            cursor::MoveTo(0, hy),
            SetBackgroundColor(Color::DarkBlue), SetForegroundColor(Color::White),
            Print(format!("{:<width$}", trunc(&fl, cols), width = cols)),
            ResetColor,
        )?;
    } else {
        let help = format!(
            "q=Quit  /=Filter  u=Toggle-UDP  Up/Down=Scroll  [{}/{}]",
            scroll + 1, conns.len().max(1),
        );
        queue!(out,
            cursor::MoveTo(0, hy),
            SetBackgroundColor(Color::Black), SetForegroundColor(Color::Cyan),
            Print(format!("{:<width$}", trunc(&help, cols), width = cols)),
            ResetColor,
        )?;
    }
    let hint_y = (app.rows as u16).saturating_sub(1);
    let hint = if app.filter.is_empty() {
        format!("{} TCP connections shown (ESTAB first)", conns.len())
    } else {
        format!("Filtered by \"{}\" — {} match(es)", app.filter, conns.len())
    };
    queue!(out,
        cursor::MoveTo(0, hint_y),
        SetBackgroundColor(Color::Black), SetForegroundColor(Color::DarkGrey),
        Print(format!("{:<width$}", trunc(&hint, cols), width = cols)),
        ResetColor,
    )?;

    out.flush()
}

// ── Event loop ────────────────────────────────────────────────────────────────

fn run() -> io::Result<()> {
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide)?;

    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let mut app = App {
        interfaces: vec![], connections: vec![], udp_ports: vec![],
        show_udp: false,
        filter: String::new(), filtering: false,
        scroll: 0, cols: cols as usize, rows: rows as usize, update_ms: 2000,
    };
    draw(&app, &mut out)?;

    let mut prev_wall  = wall_ms();
    let mut prev_ifaces: Vec<IfInfo> = vec![];
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
                            KeyCode::Char('/') => { app.filtering = true; app.filter.clear(); }
                            KeyCode::Char('u') | KeyCode::Char('U') => {
                                app.show_udp = !app.show_udp;
                                app.scroll = 0;
                            }
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
            let curr_wall = wall_ms();
            let elapsed   = curr_wall.saturating_sub(prev_wall);

            let pnames   = process_names();
            app.interfaces  = get_interfaces(&prev_ifaces, elapsed);
            app.connections = get_tcp_connections(&pnames);
            app.udp_ports   = if app.show_udp { get_udp_ports(&pnames) } else { vec![] };

            prev_ifaces = app.interfaces.clone();
            prev_wall   = curr_wall;
            last_update = Instant::now();
            draw(&app, &mut out)?;
        }
    }

    execute!(out, terminal::LeaveAlternateScreen, cursor::Show)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("Usage: iptop");
        println!("  Network monitor: interface bandwidth + TCP connection table.");
        println!();
        println!("  Keys:");
        println!("    q        Quit");
        println!("    u        Toggle UDP port display");
        println!("    /        Filter by address, process, or PID");
        println!("    Up/Down  Scroll connection list");
        return;
    }
    if let Err(e) = run() {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
        eprintln!("iptop: {}", e);
        std::process::exit(1);
    }
}
