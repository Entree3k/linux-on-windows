//! tcpdump for Windows

#![allow(non_snake_case, non_camel_case_types)]

use std::ffi::c_void;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

// Windows imports

#[cfg(windows)]
use windows_sys::Win32::{
    NetworkManagement::IpHelper::{GetAdaptersInfo, IP_ADAPTER_INFO},
    Networking::WinSock::{
        WSACleanup, WSAGetLastError, WSAIoctl, WSAStartup,
        bind, closesocket, recv, setsockopt, socket,
        AF_INET, IPPROTO_IP, SOCK_RAW, SOL_SOCKET, SO_RCVTIMEO,
        SOCKADDR, SOCKADDR_IN, SOCKET, SOCKET_ERROR, INVALID_SOCKET,
        IN_ADDR, IN_ADDR_0, WSADATA,
    },
    System::{
        Console::{
            GetConsoleMode, GetStdHandle, SetConsoleMode,
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE,
        },
        IO::OVERLAPPED,
    },
};

// Constants

const SIO_RCVALL:       u32 = 0x9800_0001;
const RCVALL_ON:        u32 = 1;
const WSAETIMEDOUT:     i32 = 10060;
const RECV_TIMEOUT_MS:  u32 = 500;

const PROTO_ICMP: u8 = 1;
const PROTO_TCP:  u8 = 6;
const PROTO_UDP:  u8 = 17;

const ANSI_CYAN:   &str = "\x1b[36m";
const ANSI_GREEN:  &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_DIM:    &str = "\x1b[2m";
const ANSI_RESET:  &str = "\x1b[0m";

// Data structures

#[derive(Clone)]
struct Adapter {
    index:    usize,
    name:     String,
    ip:       [u8; 4],
}

#[derive(Default)]
struct Filter {
    protocol: Option<u8>,
    host:     Option<[u8; 4]>,
    src_host: Option<[u8; 4]>,
    dst_host: Option<[u8; 4]>,
    port:     Option<u16>,
    src_port: Option<u16>,
    dst_port: Option<u16>,
}

impl Filter {
    fn matches(&self, p: &ParsedPacket) -> bool {
        if let Some(proto) = self.protocol {
            if p.protocol != proto { return false; }
        }
        if let Some(h) = self.host {
            if p.src_ip.octets() != h && p.dst_ip.octets() != h { return false; }
        }
        if let Some(h) = self.src_host { if p.src_ip.octets() != h { return false; } }
        if let Some(h) = self.dst_host { if p.dst_ip.octets() != h { return false; } }
        if let Some(port) = self.port {
            if p.src_port != port && p.dst_port != port { return false; }
        }
        if let Some(port) = self.src_port { if p.src_port != port { return false; } }
        if let Some(port) = self.dst_port { if p.dst_port != port { return false; } }
        true
    }
}

struct ParsedPacket {
    src_ip:    Ipv4Addr,
    dst_ip:    Ipv4Addr,
    protocol:  u8,
    ttl:       u8,
    total_len: u16,
    src_port:  u16,
    dst_port:  u16,
    tcp_flags: u8,
    tcp_seq:   u32,
    tcp_ack:   u32,
    tcp_win:   u16,
    icmp_type: u8,
    icmp_code: u8,
    icmp_id:   u16,
    icmp_seq:  u16,
    data_len:  usize,
    timestamp: SystemTime,
}

struct Args {
    iface:   Option<String>,
    list:    bool,
    count:   Option<u64>,
    write:   Option<String>,
    verbose: bool,
    hex:     bool,
    snaplen: u32,
    filter:  Filter,
}

// Argument parsing

fn parse_args() -> Args {
    let mut a = Args {
        iface: None, list: false, count: None, write: None,
        verbose: false, hex: false, snaplen: 65535,
        filter: Filter::default(),
    };
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "-D" | "--list-interfaces" => a.list = true,
            "-i" => { i += 1; if let Some(s) = raw.get(i) { a.iface = Some(s.clone()); } }
            "-c" => { i += 1; if let Some(s) = raw.get(i) { a.count = s.parse().ok(); } }
            "-w" => { i += 1; if let Some(s) = raw.get(i) { a.write = Some(s.clone()); } }
            "-v" => a.verbose = true,
            "-x" | "-X" => a.hex = true,
            "-s" => { i += 1; if let Some(s) = raw.get(i) { a.snaplen = s.parse().unwrap_or(65535); } }
            "-h" | "--help" => { print_help(); std::process::exit(0); }
            "tcp"  => a.filter.protocol = Some(PROTO_TCP),
            "udp"  => a.filter.protocol = Some(PROTO_UDP),
            "icmp" => a.filter.protocol = Some(PROTO_ICMP),
            "host" => { i += 1; if let Some(s) = raw.get(i) { a.filter.host = parse_ip(s); } }
            "src"  => {
                i += 1;
                match raw.get(i).map(|s| s.as_str()) {
                    Some("host") => { i += 1; if let Some(s) = raw.get(i) { a.filter.src_host = parse_ip(s); } }
                    Some("port") => { i += 1; if let Some(s) = raw.get(i) { a.filter.src_port = s.parse().ok(); } }
                    _ => {}
                }
            }
            "dst"  => {
                i += 1;
                match raw.get(i).map(|s| s.as_str()) {
                    Some("host") => { i += 1; if let Some(s) = raw.get(i) { a.filter.dst_host = parse_ip(s); } }
                    Some("port") => { i += 1; if let Some(s) = raw.get(i) { a.filter.dst_port = s.parse().ok(); } }
                    _ => {}
                }
            }
            "port" => { i += 1; if let Some(s) = raw.get(i) { a.filter.port = s.parse().ok(); } }
            "and" | "or" => {}
            _ => {}
        }
        i += 1;
    }
    a
}

fn parse_ip(s: &str) -> Option<[u8; 4]> {
    let ip: Ipv4Addr = s.parse().ok()?;
    Some(ip.octets())
}

fn print_help() {
    println!("Usage: tcpdump [-i <iface>] [-D] [-c <n>] [-w <file>] [-v] [-x] [filter]");
    println!();
    println!("Options:");
    println!("  -i <iface>     Interface IP, description substring, or index (from -D)");
    println!("  -D             List available interfaces and exit");
    println!("  -c <n>         Stop after n packets");
    println!("  -w <file>      Write packets to pcap file (Wireshark-compatible)");
    println!("  -v             Verbose (TTL, window size)");
    println!("  -x             Hex dump of packet data");
    println!("  -s <snaplen>   Snapshot length (default 65535)");
    println!();
    println!("Filter:");
    println!("  tcp / udp / icmp         Protocol filter");
    println!("  host <IP>                Traffic to/from IP");
    println!("  src|dst host <IP>        Directional host filter");
    println!("  port <N>                 Traffic on port N");
    println!("  src|dst port <N>         Directional port filter");
    println!();
    println!("Examples:");
    println!("  tcpdump -D");
    println!("  tcpdump -i 192.168.1.5 tcp and port 443");
    println!("  tcpdump -i 0 -w capture.pcap -c 1000");
    println!("  tcpdump -i 0 host 8.8.8.8");
    println!();
    println!("Requires Administrator privileges (raw socket + promiscuous mode).");
}

// Interface enumeration

fn list_adapters() -> Vec<Adapter> {
    let mut result = Vec::new();
    #[cfg(windows)]
    unsafe {
        let mut size: u32 = 0;
        // First call to get required buffer size
        GetAdaptersInfo(std::ptr::null_mut(), &mut size);
        if size == 0 { return result; }

        let mut buf = vec![0u8; size as usize];
        let ret = GetAdaptersInfo(buf.as_mut_ptr() as *mut IP_ADAPTER_INFO, &mut size);
        if ret != 0 {
            eprintln!("GetAdaptersInfo failed: {}", ret);
            return result;
        }

        let mut ptr = buf.as_ptr() as *const IP_ADAPTER_INFO;
        let mut idx = 0usize;
        while !ptr.is_null() {
            let a = &*ptr;

            let desc_end = a.Description.iter().position(|&b| b == 0).unwrap_or(131);
            let name = String::from_utf8_lossy(&a.Description[..desc_end]).into_owned();

            let ip_end = a.IpAddressList.IpAddress.String.iter()
                .position(|&b| b == 0).unwrap_or(16);
            let ip_str = std::str::from_utf8(&a.IpAddressList.IpAddress.String[..ip_end])
                .unwrap_or("0.0.0.0");

            if let Ok(addr) = ip_str.parse::<Ipv4Addr>() {
                let ip = addr.octets();
                if ip != [0, 0, 0, 0] {
                    result.push(Adapter { index: idx, name, ip });
                    idx += 1;
                }
            }

            ptr = a.Next;
        }
    }
    result
}

fn select_adapter<'a>(adapters: &'a [Adapter], iface: &Option<String>) -> Option<&'a Adapter> {
    match iface {
        None => adapters.iter().find(|a| a.ip[0] != 127),
        Some(s) => {
            // try as numeric index
            if let Ok(idx) = s.parse::<usize>() {
                return adapters.iter().find(|a| a.index == idx);
            }
            // try as exact IP
            if let Ok(ip_addr) = s.parse::<Ipv4Addr>() {
                let oct = ip_addr.octets();
                if let Some(a) = adapters.iter().find(|a| a.ip == oct) {
                    return Some(a);
                }
            }
            let lower = s.to_lowercase();
            adapters.iter().find(|a| a.name.to_lowercase().contains(&lower))
        }
    }
}

// Raw socket capture

fn wsa_error_str(code: i32) -> &'static str {
    match code {
        10013 => "WSAEACCES — access denied (run as Administrator)",
        10022 => "WSAEINVAL — invalid argument",
        10038 => "WSAENOTSOCK — not a socket",
        10047 => "WSAEAFNOSUPPORT — address family not supported",
        10048 => "WSAEADDRINUSE — address already in use",
        10049 => "WSAEADDRNOTAVAIL — address not available on this adapter",
        10065 => "WSAEHOSTUNREACH — no route to host",
        _     => "unknown error",
    }
}

#[cfg(windows)]
fn open_capture(ip: [u8; 4]) -> Result<SOCKET, String> {
    unsafe {
        let mut wsa: WSADATA = std::mem::zeroed();
        if WSAStartup(0x0202, &mut wsa) != 0 {
            return Err("WSAStartup failed".into());
        }

        let sock = socket(AF_INET.into(), SOCK_RAW, IPPROTO_IP as i32);
        if sock == INVALID_SOCKET {
            let err = WSAGetLastError();
            return Err(format!(
                "socket() failed ({}): {}\nMake sure you are running as Administrator.",
                err, wsa_error_str(err)
            ));
        }

        let s_addr = (ip[0] as u32)
            | ((ip[1] as u32) << 8)
            | ((ip[2] as u32) << 16)
            | ((ip[3] as u32) << 24);

        let addr = SOCKADDR_IN {
            sin_family: AF_INET,
            sin_port:   0,
            sin_addr:   IN_ADDR { S_un: IN_ADDR_0 { S_addr: s_addr } },
            sin_zero:   [0; 8],
        };

        let bind_ret = bind(
            sock,
            &addr as *const SOCKADDR_IN as *const SOCKADDR,
            std::mem::size_of::<SOCKADDR_IN>() as i32,
        );
        if bind_ret == SOCKET_ERROR {
            let err = WSAGetLastError();
            closesocket(sock);
            WSACleanup();
            return Err(format!(
                "bind() to {}.{}.{}.{} failed ({}): {}\n\
                 Tip: use -D to list valid interfaces, then -i <index>.",
                ip[0], ip[1], ip[2], ip[3], err, wsa_error_str(err)
            ));
        }

        let rcvall    = RCVALL_ON;
        let mut bytes = 0u32;
        let ioc_ret = WSAIoctl(
            sock, SIO_RCVALL,
            &rcvall as *const u32 as *const c_void, 4,
            std::ptr::null_mut(), 0,
            &mut bytes,
            std::ptr::null_mut() as *mut OVERLAPPED,
            None,
        );
        if ioc_ret == SOCKET_ERROR {
            let err = WSAGetLastError();
            eprintln!(
                "Warning: SIO_RCVALL failed ({}): {} — captured traffic may be incomplete",
                err, wsa_error_str(err)
            );
        }

        setsockopt(
            sock, SOL_SOCKET, SO_RCVTIMEO,
            &RECV_TIMEOUT_MS as *const u32 as *const u8, 4,
        );

        Ok(sock)
    }
}

// Packet parsing

fn parse_packet(buf: &[u8]) -> Option<ParsedPacket> {
    if buf.len() < 20 { return None; }
    if buf[0] >> 4 != 4 { return None; } // IPv4 only

    let ihl       = (buf[0] & 0x0F) as usize * 4;
    let total_len = u16::from_be_bytes([buf[2], buf[3]]);
    let ttl       = buf[8];
    let protocol  = buf[9];
    let src_ip    = Ipv4Addr::new(buf[12], buf[13], buf[14], buf[15]);
    let dst_ip    = Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]);

    if ihl > buf.len() { return None; }

    let mut p = ParsedPacket {
        src_ip, dst_ip, protocol, ttl, total_len,
        src_port: 0, dst_port: 0,
        tcp_flags: 0, tcp_seq: 0, tcp_ack: 0, tcp_win: 0,
        icmp_type: 0, icmp_code: 0, icmp_id: 0, icmp_seq: 0,
        data_len: 0,
        timestamp: SystemTime::now(),
    };

    let transport = &buf[ihl..];
    if transport.len() < 4 { return Some(p); }

    match protocol {
        PROTO_TCP if transport.len() >= 20 => {
            p.src_port  = u16::from_be_bytes([transport[0], transport[1]]);
            p.dst_port  = u16::from_be_bytes([transport[2], transport[3]]);
            p.tcp_seq   = u32::from_be_bytes([transport[4], transport[5], transport[6], transport[7]]);
            p.tcp_ack   = u32::from_be_bytes([transport[8], transport[9], transport[10], transport[11]]);
            p.tcp_flags = transport[13];
            p.tcp_win   = u16::from_be_bytes([transport[14], transport[15]]);
            let doff    = (transport[12] >> 4) as usize * 4;
            p.data_len  = transport.len().saturating_sub(doff);
        }
        PROTO_UDP if transport.len() >= 8 => {
            p.src_port = u16::from_be_bytes([transport[0], transport[1]]);
            p.dst_port = u16::from_be_bytes([transport[2], transport[3]]);
            p.data_len = transport.len().saturating_sub(8);
        }
        PROTO_ICMP if transport.len() >= 8 => {
            p.icmp_type = transport[0];
            p.icmp_code = transport[1];
            p.icmp_id   = u16::from_be_bytes([transport[4], transport[5]]);
            p.icmp_seq  = u16::from_be_bytes([transport[6], transport[7]]);
            p.data_len  = transport.len().saturating_sub(8);
        }
        _ => {}
    }
    Some(p)
}

// Display

fn fmt_flags(f: u8) -> String {
    let mut s = String::new();
    if f & 0x02 != 0 { s.push('S'); }
    if f & 0x01 != 0 { s.push('F'); }
    if f & 0x04 != 0 { s.push('R'); }
    if f & 0x08 != 0 { s.push('P'); }
    if f & 0x10 != 0 { s.push('.'); }
    if f & 0x20 != 0 { s.push('U'); }
    if s.is_empty()  { s.push('0'); }
    format!("[{}]", s)
}

fn icmp_name(ty: u8, code: u8) -> String {
    match ty {
        0  => "echo-reply".into(),
        3  => match code {
            0 => "net-unreachable".into(),
            1 => "host-unreachable".into(),
            3 => "port-unreachable".into(),
            _ => format!("dest-unreachable(code={})", code),
        },
        8  => "echo-request".into(),
        11 => "time-exceeded".into(),
        _  => format!("type={} code={}", ty, code),
    }
}

fn print_packet(p: &ParsedPacket, verbose: bool, hex: bool, raw: &[u8]) {
    let dur   = p.timestamp.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs  = dur.as_secs();
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let ts = format!("{:02}:{:02}:{:02}.{:06}", h, m, s, dur.subsec_micros());

    let (color, line) = match p.protocol {
        PROTO_TCP => {
            let flags = fmt_flags(p.tcp_flags);
            let ack   = if p.tcp_flags & 0x10 != 0 { format!(" ack={}", p.tcp_ack) } else { String::new() };
            let verb  = if verbose { format!(" win={} ttl={}", p.tcp_win, p.ttl) } else { String::new() };
            let info  = format!("{} {}.{} > {}.{}: TCP {} seq={}{} len={}{}",
                ts, p.src_ip, p.src_port, p.dst_ip, p.dst_port,
                flags, p.tcp_seq, ack, p.data_len, verb);
            (ANSI_CYAN, info)
        }
        PROTO_UDP => {
            let verb = if verbose { format!(" ttl={}", p.ttl) } else { String::new() };
            let info = format!("{} {}.{} > {}.{}: UDP len={}{}",
                ts, p.src_ip, p.src_port, p.dst_ip, p.dst_port, p.data_len, verb);
            (ANSI_GREEN, info)
        }
        PROTO_ICMP => {
            let name = icmp_name(p.icmp_type, p.icmp_code);
            let id   = if p.icmp_type == 0 || p.icmp_type == 8 {
                format!(" id={} seq={} len={}", p.icmp_id, p.icmp_seq, p.data_len)
            } else { String::new() };
            let verb = if verbose { format!(" ttl={}", p.ttl) } else { String::new() };
            let info = format!("{} {} > {}: ICMP {}{}{}", ts, p.src_ip, p.dst_ip, name, id, verb);
            (ANSI_YELLOW, info)
        }
        proto => {
            let info = format!("{} {} > {}: proto={} len={}", ts, p.src_ip, p.dst_ip, proto, p.total_len);
            (ANSI_DIM, info)
        }
    };

    println!("{}{}{}", color, line, ANSI_RESET);

    if hex && !raw.is_empty() {
        let snap = raw.len().min(64);
        for (i, chunk) in raw[..snap].chunks(16).enumerate() {
            print!("  {:04x}:  ", i * 16);
            for b in chunk { print!("{:02x} ", b); }
            let pad = 16 - chunk.len();
            for _ in 0..pad { print!("   "); }
            print!(" ");
            for b in chunk { print!("{}", if *b >= 0x20 && *b < 0x7f { *b as char } else { '.' }); }
            println!();
        }
    }
}

// pcap file writer

struct PcapWriter { w: BufWriter<File>, snaplen: u32 }

impl PcapWriter {
    fn new(path: &str, snaplen: u32) -> std::io::Result<Self> {
        let mut w = BufWriter::new(File::create(path)?);
        w.write_all(&0xa1b2c3d4u32.to_le_bytes())?;
        w.write_all(&2u16.to_le_bytes())?;
        w.write_all(&4u16.to_le_bytes())?;
        w.write_all(&0i32.to_le_bytes())?;
        w.write_all(&0u32.to_le_bytes())?;
        w.write_all(&snaplen.to_le_bytes())?;
        w.write_all(&101u32.to_le_bytes())?;
        Ok(Self { w, snaplen })
    }

    fn write_packet(&mut self, raw: &[u8], ts: &SystemTime) -> std::io::Result<()> {
        let dur    = ts.duration_since(UNIX_EPOCH).unwrap_or_default();
        let caplen = raw.len().min(self.snaplen as usize) as u32;
        self.w.write_all(&(dur.as_secs() as u32).to_le_bytes())?;
        self.w.write_all(&dur.subsec_micros().to_le_bytes())?;
        self.w.write_all(&caplen.to_le_bytes())?;
        self.w.write_all(&(raw.len() as u32).to_le_bytes())?;
        self.w.write_all(&raw[..caplen as usize])
    }
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
    let adapters = list_adapters();

    if args.list {
        if adapters.is_empty() {
            eprintln!("No adapters found. Try running as Administrator.");
            return;
        }
        println!("Available interfaces:");
        for a in &adapters {
            println!("  {:2}: {:<18}  {}",
                a.index,
                format!("{}.{}.{}.{}", a.ip[0], a.ip[1], a.ip[2], a.ip[3]),
                a.name,
            );
        }
        return;
    }

    let adapter = match select_adapter(&adapters, &args.iface) {
        Some(a) => a.clone(),
        None => {
            if adapters.is_empty() {
                eprintln!("No network adapters found. Are you running as Administrator?");
            } else {
                eprintln!("Interface not found. Available interfaces:");
                for a in &adapters {
                    eprintln!("  {:2}: {}.{}.{}.{}  {}",
                        a.index, a.ip[0], a.ip[1], a.ip[2], a.ip[3], a.name);
                }
                eprintln!("Use -i <index> or -i <IP> to select one.");
            }
            std::process::exit(1);
        }
    };

    eprintln!(
        "Capturing on {} ({}.{}.{}.{}) — Ctrl+C to stop",
        adapter.name, adapter.ip[0], adapter.ip[1], adapter.ip[2], adapter.ip[3]
    );

    #[cfg(not(windows))]
    { eprintln!("Packet capture requires Windows."); std::process::exit(1); }

    #[cfg(windows)]
    let sock = match open_capture(adapter.ip) {
        Ok(s)  => s,
        Err(e) => { eprintln!("{}", e); std::process::exit(1); }
    };

    let mut pcap = args.write.as_deref().map(|path| {
        match PcapWriter::new(path, args.snaplen) {
            Ok(w)  => { eprintln!("Writing pcap to: {}", path); w }
            Err(e) => { eprintln!("Cannot open pcap file: {}", e); std::process::exit(1); }
        }
    });

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::Relaxed))
        .expect("Error setting Ctrl+C handler");

    let mut buf      = vec![0u8; 65535];
    let mut captured: u64 = 0;
    let mut non_ip:   u64 = 0;

    #[cfg(windows)]
    loop {
        if !running.load(Ordering::Relaxed) { break; }
        if let Some(max) = args.count { if captured >= max { break; } }

        let n = unsafe { recv(sock, buf.as_mut_ptr(), buf.len() as i32, 0) };

        if n == SOCKET_ERROR {
            let err = unsafe { WSAGetLastError() };
            if err == WSAETIMEDOUT { continue; }
            eprintln!("recv() error {}: {}", err, wsa_error_str(err));
            break;
        }
        if n <= 0 { continue; }

        let raw = &buf[..n as usize];
        let Some(pkt) = parse_packet(raw) else { non_ip += 1; continue; };
        if !args.filter.matches(&pkt) { continue; }

        print_packet(&pkt, args.verbose, args.hex, raw);

        if let Some(ref mut w) = pcap {
            if let Err(e) = w.write_packet(raw, &pkt.timestamp) {
                eprintln!("pcap write error: {}", e);
            }
        }
        captured += 1;
    }

    eprintln!("\n{}{} packets captured, {} non-IP skipped{}", ANSI_DIM, captured, non_ip, ANSI_RESET);

    #[cfg(windows)]
    unsafe {
        closesocket(sock);
        WSACleanup();
    }
}
