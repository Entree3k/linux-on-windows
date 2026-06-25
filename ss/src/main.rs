//! ss - socket statistics for Windows

#![allow(non_snake_case, non_camel_case_types)]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

// Windows imports

#[cfg(windows)]
use windows_sys::Win32::{
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable,
        MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
        MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID,
        MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID,
        MIB_UDP6ROW_OWNER_PID, MIB_UDP6TABLE_OWNER_PID,
    },
    System::{
        Console::{
            GetConsoleMode, GetStdHandle, SetConsoleMode,
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE,
        },
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
            PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        },
    },
};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};

// ANSI colors

const C_GREEN:  &str = "\x1b[32m";
const C_CYAN:   &str = "\x1b[36m";
const C_YELLOW: &str = "\x1b[33m";
const C_MAGENTA:&str = "\x1b[35m";
const C_DIM:    &str = "\x1b[2m";
const C_BOLD:   &str = "\x1b[1m";
const C_RESET:  &str = "\x1b[0m";

// TCP/UDP table constants

const AF_INET_U32:             u32 = 2;
const AF_INET6_U32:            u32 = 23;
const TCP_TABLE_OWNER_PID_ALL: i32 = 5;
const UDP_TABLE_OWNER_PID:     i32 = 1;

// Data structures

#[derive(Clone)]
struct SockEntry {
    proto:       &'static str,  // "tcp" / "tcp6" / "udp" / "udp6"
    state:       u32,           // TCP state (0 for UDP)
    local_ip:    IpAddr,
    local_port:  u16,
    remote_ip:   IpAddr,
    remote_port: u16,
    pid:         u32,
}

// TCP state

fn tcp_state_str(s: u32) -> &'static str {
    match s {
        1  => "CLOSED",
        2  => "LISTEN",
        3  => "SYN-SENT",
        4  => "SYN-RECV",
        5  => "ESTAB",
        6  => "FIN-WAIT-1",
        7  => "FIN-WAIT-2",
        8  => "CLOSE-WAIT",
        9  => "CLOSING",
        10 => "LAST-ACK",
        11 => "TIME-WAIT",
        12 => "DELETE-TCB",
        _  => "UNKNOWN",
    }
}

fn state_color(s: u32) -> &'static str {
    match s {
        5  => C_GREEN,           // ESTAB
        2  => C_CYAN,            // LISTEN
        11 | 8 => C_YELLOW,      // TIME-WAIT, CLOSE-WAIT
        3  | 4 => C_MAGENTA,     // SYN-*
        _  => C_DIM,
    }
}

// Service name resolution

fn svc(port: u16) -> Option<&'static str> {
    match port {
        20    => Some("ftp-data"),
        21    => Some("ftp"),
        22    => Some("ssh"),
        23    => Some("telnet"),
        25    => Some("smtp"),
        53    => Some("domain"),
        67    => Some("bootps"),
        68    => Some("bootpc"),
        69    => Some("tftp"),
        80    => Some("http"),
        110   => Some("pop3"),
        123   => Some("ntp"),
        135   => Some("msrpc"),
        137   => Some("netbios-ns"),
        138   => Some("netbios-dgm"),
        139   => Some("netbios-ssn"),
        143   => Some("imap"),
        389   => Some("ldap"),
        443   => Some("https"),
        445   => Some("microsoft-ds"),
        465   => Some("smtps"),
        514   => Some("syslog"),
        587   => Some("submission"),
        636   => Some("ldaps"),
        993   => Some("imaps"),
        995   => Some("pop3s"),
        1433  => Some("ms-sql-s"),
        1521  => Some("oracle"),
        3306  => Some("mysql"),
        3389  => Some("rdp"),
        5432  => Some("postgres"),
        5900  => Some("vnc"),
        6379  => Some("redis"),
        6443  => Some("k8s-api"),
        8080  => Some("http-alt"),
        8443  => Some("https-alt"),
        9200  => Some("elasticsearch"),
        27017 => Some("mongodb"),
        _     => None,
    }
}

fn fmt_port(port: u16, numeric: bool) -> String {
    if numeric || port == 0 {
        port.to_string()
    } else {
        svc(port).map(str::to_owned).unwrap_or_else(|| port.to_string())
    }
}

fn fmt_addr(ip: IpAddr, port: u16, numeric: bool) -> String {
    let p = fmt_port(port, numeric);
    match ip {
        IpAddr::V4(v4) if v4.is_unspecified() => format!("*:{}", p),
        IpAddr::V4(v4)  => format!("{}:{}", v4, p),
        IpAddr::V6(v6) if v6.is_unspecified() => format!("[::]*:{}", p),
        // collapse loopback ::1 and common addresses
        IpAddr::V6(v6)  => format!("[{}]:{}", v6, p),
    }
}

fn fmt_peer(ip: IpAddr, port: u16, numeric: bool) -> String {
    match ip {
        IpAddr::V4(v4) if v4.is_unspecified() && port == 0 => "*:*".into(),
        IpAddr::V6(v6) if v6.is_unspecified() && port == 0 => "[::]*:*".into(),
        _ => fmt_addr(ip, port, numeric),
    }
}

// Process name table

fn build_proc_map() -> HashMap<u32, String> {
    let mut map = HashMap::new();
    #[cfg(windows)]
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE { return map; }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let nul = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
                let name = String::from_utf16_lossy(&entry.szExeFile[..nul]);
                map.insert(entry.th32ProcessID, name);
                if Process32NextW(snap, &mut entry) == 0 { break; }
            }
        }
        CloseHandle(snap);
    }
    map
}

// Socket enumeration

fn port_from_dword(dw: u32) -> u16 {
    // dwLocalPort stores port in big-endian in the low 16 bits
    ((dw & 0xFF) << 8 | (dw >> 8 & 0xFF)) as u16
}

fn ipv4_from_dword(dw: u32) -> Ipv4Addr {
    // stored as little-endian on x86 — extract bytes directly
    Ipv4Addr::new(
        (dw & 0xFF) as u8,
        ((dw >> 8) & 0xFF) as u8,
        ((dw >> 16) & 0xFF) as u8,
        ((dw >> 24) & 0xFF) as u8,
    )
}

fn collect_tcp4() -> Vec<SockEntry> {
    let mut out = Vec::new();
    #[cfg(windows)]
    unsafe {
        let mut size: u32 = 0;
        GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET_U32, TCP_TABLE_OWNER_PID_ALL, 0);
        if size == 0 { return out; }
        let mut buf = vec![0u8; size as usize];
        if GetExtendedTcpTable(buf.as_mut_ptr() as *mut _, &mut size, 0, AF_INET_U32, TCP_TABLE_OWNER_PID_ALL, 0) != 0 {
            return out;
        }
        let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
        let rows  = std::slice::from_raw_parts(&table.table[0] as *const MIB_TCPROW_OWNER_PID, table.dwNumEntries as usize);
        for r in rows {
            out.push(SockEntry {
                proto:       "tcp",
                state:       r.dwState,
                local_ip:    IpAddr::V4(ipv4_from_dword(r.dwLocalAddr)),
                local_port:  port_from_dword(r.dwLocalPort),
                remote_ip:   IpAddr::V4(ipv4_from_dword(r.dwRemoteAddr)),
                remote_port: port_from_dword(r.dwRemotePort),
                pid:         r.dwOwningPid,
            });
        }
    }
    out
}

fn collect_tcp6() -> Vec<SockEntry> {
    let mut out = Vec::new();
    #[cfg(windows)]
    unsafe {
        let mut size: u32 = 0;
        GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET6_U32, TCP_TABLE_OWNER_PID_ALL, 0);
        if size == 0 { return out; }
        let mut buf = vec![0u8; size as usize];
        if GetExtendedTcpTable(buf.as_mut_ptr() as *mut _, &mut size, 0, AF_INET6_U32, TCP_TABLE_OWNER_PID_ALL, 0) != 0 {
            return out;
        }
        let table = &*(buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
        let rows  = std::slice::from_raw_parts(&table.table[0] as *const MIB_TCP6ROW_OWNER_PID, table.dwNumEntries as usize);
        for r in rows {
            out.push(SockEntry {
                proto:       "tcp6",
                state:       r.dwState,
                local_ip:    IpAddr::V6(Ipv6Addr::from(r.ucLocalAddr)),
                local_port:  port_from_dword(r.dwLocalPort),
                remote_ip:   IpAddr::V6(Ipv6Addr::from(r.ucRemoteAddr)),
                remote_port: port_from_dword(r.dwRemotePort),
                pid:         r.dwOwningPid,
            });
        }
    }
    out
}

fn collect_udp4() -> Vec<SockEntry> {
    let mut out = Vec::new();
    #[cfg(windows)]
    unsafe {
        let mut size: u32 = 0;
        GetExtendedUdpTable(std::ptr::null_mut(), &mut size, 0, AF_INET_U32, UDP_TABLE_OWNER_PID, 0);
        if size == 0 { return out; }
        let mut buf = vec![0u8; size as usize];
        if GetExtendedUdpTable(buf.as_mut_ptr() as *mut _, &mut size, 0, AF_INET_U32, UDP_TABLE_OWNER_PID, 0) != 0 {
            return out;
        }
        let table = &*(buf.as_ptr() as *const MIB_UDPTABLE_OWNER_PID);
        let rows  = std::slice::from_raw_parts(&table.table[0] as *const MIB_UDPROW_OWNER_PID, table.dwNumEntries as usize);
        for r in rows {
            out.push(SockEntry {
                proto:       "udp",
                state:       0,
                local_ip:    IpAddr::V4(ipv4_from_dword(r.dwLocalAddr)),
                local_port:  port_from_dword(r.dwLocalPort),
                remote_ip:   IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                remote_port: 0,
                pid:         r.dwOwningPid,
            });
        }
    }
    out
}

fn collect_udp6() -> Vec<SockEntry> {
    let mut out = Vec::new();
    #[cfg(windows)]
    unsafe {
        let mut size: u32 = 0;
        GetExtendedUdpTable(std::ptr::null_mut(), &mut size, 0, AF_INET6_U32, UDP_TABLE_OWNER_PID, 0);
        if size == 0 { return out; }
        let mut buf = vec![0u8; size as usize];
        if GetExtendedUdpTable(buf.as_mut_ptr() as *mut _, &mut size, 0, AF_INET6_U32, UDP_TABLE_OWNER_PID, 0) != 0 {
            return out;
        }
        let table = &*(buf.as_ptr() as *const MIB_UDP6TABLE_OWNER_PID);
        let rows  = std::slice::from_raw_parts(&table.table[0] as *const MIB_UDP6ROW_OWNER_PID, table.dwNumEntries as usize);
        for r in rows {
            out.push(SockEntry {
                proto:       "udp6",
                state:       0,
                local_ip:    IpAddr::V6(Ipv6Addr::from(r.ucLocalAddr)),
                local_port:  port_from_dword(r.dwLocalPort),
                remote_ip:   IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                remote_port: 0,
                pid:         r.dwOwningPid,
            });
        }
    }
    out
}

// Summary statistics

fn print_summary(all: &[SockEntry]) {
    let tcp4_total   = all.iter().filter(|s| s.proto == "tcp").count();
    let tcp6_total   = all.iter().filter(|s| s.proto == "tcp6").count();
    let udp4_total   = all.iter().filter(|s| s.proto == "udp").count();
    let udp6_total   = all.iter().filter(|s| s.proto == "udp6").count();
    let estab        = all.iter().filter(|s| s.state == 5).count();
    let listen       = all.iter().filter(|s| s.state == 2).count();
    let timewait     = all.iter().filter(|s| s.state == 11).count();
    let closewait    = all.iter().filter(|s| s.state == 8).count();

    println!("{}Total: {} sockets{}", C_BOLD, all.len(), C_RESET);
    println!("{}TCP:   {} IPv4 / {} IPv6  (estab={} listen={} timewait={} closewait={}){}",
        C_DIM, tcp4_total, tcp6_total, estab, listen, timewait, closewait, C_RESET);
    println!("{}UDP:   {} IPv4 / {} IPv6{}",
        C_DIM, udp4_total, udp6_total, C_RESET);
    println!();
}

// CLI

struct Args {
    tcp:      bool,
    udp:      bool,
    listen:   bool,
    all:      bool,
    proc:     bool,
    numeric:  bool,
    ipv4:     bool,
    ipv6:     bool,
    summary:  bool,
    filter_port: Option<u16>,
    filter_state: Option<u32>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            tcp: false, udp: false, listen: false, all: false,
            proc: false, numeric: false, ipv4: false, ipv6: false,
            summary: false, filter_port: None, filter_state: None,
        }
    }
}

fn parse_args() -> Args {
    let mut a = Args::default();
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        let s = raw[i].as_str();
        // support combined flags like -tulpn or -tlp
        if s.starts_with('-') && !s.starts_with("--") && s.len() > 2 {
            for c in s.chars().skip(1) {
                match c {
                    't' => a.tcp = true,
                    'u' => a.udp = true,
                    'l' => a.listen = true,
                    'a' => a.all = true,
                    'p' => a.proc = true,
                    'n' => a.numeric = true,
                    '4' => a.ipv4 = true,
                    '6' => a.ipv6 = true,
                    's' => a.summary = true,
                    _ => {}
                }
            }
            i += 1;
            continue;
        }
        match s {
            "-t" | "--tcp"       => a.tcp = true,
            "-u" | "--udp"       => a.udp = true,
            "-l" | "--listening" => a.listen = true,
            "-a" | "--all"       => a.all = true,
            "-p" | "--processes" => a.proc = true,
            "-n" | "--numeric"   => a.numeric = true,
            "-4"                 => a.ipv4 = true,
            "-6"                 => a.ipv6 = true,
            "-s" | "--summary"   => a.summary = true,
            "-h" | "--help"      => { print_help(); std::process::exit(0); }
            // filter shortcuts: "sport :80", "dport :443", "state established"
            "state" => {
                i += 1;
                if let Some(st) = raw.get(i) {
                    a.filter_state = parse_state(st);
                }
            }
            s if s.starts_with("dport") || s.starts_with("sport") => {
                // "sport :80" or "sport 80"
                let port_part = s.split(':').last().unwrap_or("").trim();
                if let Ok(p) = port_part.parse::<u16>() { a.filter_port = Some(p); }
            }
            _ => {}
        }
        i += 1;
    }
    a
}

fn parse_state(s: &str) -> Option<u32> {
    match s.to_lowercase().as_str() {
        "established" | "estab" => Some(5),
        "listen"      | "listening" => Some(2),
        "syn-sent"    | "syn_sent"  => Some(3),
        "syn-recv"    | "syn_recv"  => Some(4),
        "fin-wait-1"  | "fin_wait_1" => Some(6),
        "fin-wait-2"  | "fin_wait_2" => Some(7),
        "close-wait"  | "close_wait" => Some(8),
        "closing"                    => Some(9),
        "last-ack"    | "last_ack"   => Some(10),
        "time-wait"   | "time_wait"  => Some(11),
        _ => None,
    }
}

fn print_help() {
    println!("Usage: ss [OPTIONS] [FILTER]");
    println!();
    println!("Show socket statistics (like Linux ss / netstat).");
    println!();
    println!("Options (can be combined: -tulpn):");
    println!("  -t, --tcp         TCP sockets");
    println!("  -u, --udp         UDP sockets");
    println!("  -l, --listening   Listening sockets only");
    println!("  -a, --all         All sockets (listening + connected)");
    println!("  -p, --processes   Show owning process");
    println!("  -n, --numeric     Don't resolve port names");
    println!("  -4                IPv4 only");
    println!("  -6                IPv6 only");
    println!("  -s, --summary     Print summary statistics");
    println!("  -h, --help        This help");
    println!();
    println!("Filter:");
    println!("  state <STATE>     e.g. state established, state listen");
    println!();
    println!("Examples:");
    println!("  ss -tulpn         Classic: TCP+UDP, listening, processes, numeric");
    println!("  ss -t             Established TCP connections");
    println!("  ss -tlp           Listening TCP with processes");
    println!("  ss -ua            All UDP");
    println!("  ss -4 state listen  IPv4 listening sockets");
    println!("  ss -s             Summary statistics");
}

// Rendering

const W_NETID: usize  = 6;
const W_STATE: usize  = 12;
const W_QS:   usize   = 7;
const W_ADDR: usize   = 30;

fn print_header(show_proc: bool) {
    let proc_col = if show_proc { "  Process" } else { "" };
    println!("{}{}  {}  {}  {}  {}{}{}",
        C_BOLD,
        pad("Netid", W_NETID),
        pad("State", W_STATE),
        pad("Recv-Q", W_QS),
        pad("Local Address:Port", W_ADDR),
        pad("Peer Address:Port", W_ADDR),
        proc_col,
        C_RESET,
    );
}

fn print_entry(e: &SockEntry, procs: &HashMap<u32, String>, args: &Args) {
    let state_s  = if e.proto.starts_with("tcp") { tcp_state_str(e.state) } else { "UNCONN" };
    let color    = if e.proto.starts_with("tcp") { state_color(e.state) } else { C_DIM };

    let local    = fmt_addr(e.local_ip, e.local_port, args.numeric);
    let peer     = fmt_peer(e.remote_ip, e.remote_port, args.numeric);

    let proc_s = if args.proc {
        let name = procs.get(&e.pid).map(|s| s.as_str()).unwrap_or("-");
        format!("  {}{}({}){}", C_DIM, name, e.pid, C_RESET)
    } else {
        String::new()
    };

    println!("{}{}  {}{}  {}  {}  {}{}{}",
        color,
        pad(e.proto, W_NETID),
        pad(state_s, W_STATE),
        C_RESET,
        pad("0", W_QS),
        pad(&local, W_ADDR),
        pad(&peer,  W_ADDR),
        proc_s,
        if proc_s.is_empty() { C_RESET } else { "" },
    );
}

fn pad(s: &str, n: usize) -> String {
    if s.len() >= n { s[..n].to_owned() }
    else            { format!("{:<width$}", s, width = n) }
}

// Main

fn enable_ansi() {
    #[cfg(windows)]
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(h, &mut mode) != 0 {
            SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

fn main() {
    enable_ansi();
    let args = parse_args();

    let show_tcp = args.tcp || args.udp || args.all || args.listen || args.summary
        || (args.filter_state.is_some())
        || (!args.tcp && !args.udp); // default: show tcp
    let show_udp = args.udp || args.all;

    let mut all: Vec<SockEntry> = Vec::new();

    if show_tcp {
        if !args.ipv6 { all.extend(collect_tcp4()); }
        if !args.ipv4 { all.extend(collect_tcp6()); }
    }
    if show_udp {
        if !args.ipv6 { all.extend(collect_udp4()); }
        if !args.ipv4 { all.extend(collect_udp6()); }
    }

    if args.summary {
        let mut summary_all = collect_tcp4();
        summary_all.extend(collect_tcp6());
        summary_all.extend(collect_udp4());
        summary_all.extend(collect_udp6());
        print_summary(&summary_all);
        if !args.tcp && !args.udp && !args.all && !args.listen {
            return;
        }
    }

    all.retain(|e| {
        if args.listen && !args.all {
            if e.proto.starts_with("tcp") && e.state != 2 { return false; }
        } else if !args.all && !args.listen {
            if e.proto.starts_with("tcp") && (e.state == 2 || e.state == 1 || e.state == 12) {
                return false;
            }
        }
        if let Some(st) = args.filter_state {
            if e.proto.starts_with("tcp") && e.state != st { return false; }
        }
        // port filter
        if let Some(p) = args.filter_port {
            if e.local_port != p && e.remote_port != p { return false; }
        }
        true
    });

    all.sort_by(|a, b| {
        a.proto.cmp(b.proto)
            .then(a.state.cmp(&b.state))
            .then(a.local_port.cmp(&b.local_port))
    });

    let procs = if args.proc { build_proc_map() } else { HashMap::new() };

    print_header(args.proc);
    for e in &all {
        print_entry(e, &procs, &args);
    }

    if all.is_empty() {
        println!("{}(no matching sockets){}", C_DIM, C_RESET);
    }
}
