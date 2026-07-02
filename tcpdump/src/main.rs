#![allow(non_snake_case, non_camel_case_types)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::File;
use std::io::{BufWriter, Read as IoRead, Write};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

const SIO_RCVALL:      u32 = 0x9800_0001;
const RCVALL_ON:       u32 = 1;
const WSAETIMEDOUT:    i32 = 10060;
const RECV_TIMEOUT_MS: u32 = 500;

const PROTO_ICMP: u8 = 1;
const PROTO_TCP:  u8 = 6;
const PROTO_UDP:  u8 = 17;

// TCP flag bits
const TH_FIN:  u8 = 0x01;
const TH_SYN:  u8 = 0x02;
const TH_RST:  u8 = 0x04;
const TH_PUSH: u8 = 0x08;
const TH_ACK:  u8 = 0x10;
const TH_URG:  u8 = 0x20;
const TH_ECE:  u8 = 0x40;
const TH_CWR:  u8 = 0x80;

// Timestamp format

#[derive(Default, PartialEq)]
enum TsFmt {
    #[default]
    Default,   // HH:MM:SS.us  (-no flag-)
    None,      // no timestamp  (-t)
    Epoch,     // seconds.us since epoch  (-tt)
    Delta,     // delta from previous packet  (-ttt)
    DateTime,  // YYYY-MM-DD HH:MM:SS.us  (-tttt)
}

// Filter

#[derive(Default)]
struct Filter {
    protocol: Option<u8>,
    host:     Option<[u8; 4]>,
    src_host: Option<[u8; 4]>,
    dst_host: Option<[u8; 4]>,
    port:     Option<u16>,
    src_port: Option<u16>,
    dst_port: Option<u16>,
    negate:   bool,
}

impl Filter {
    fn matches(&self, p: &ParsedPacket) -> bool {
        let mut m = true;
        if let Some(proto) = self.protocol {
            if p.protocol != proto { m = false; }
        }
        if m {
            if let Some(h) = self.host {
                if p.src_ip.octets() != h && p.dst_ip.octets() != h { m = false; }
            }
        }
        if m { if let Some(h) = self.src_host { if p.src_ip.octets() != h { m = false; } } }
        if m { if let Some(h) = self.dst_host { if p.dst_ip.octets() != h { m = false; } } }
        if m {
            if let Some(port) = self.port {
                if p.src_port != port && p.dst_port != port { m = false; }
            }
        }
        if m { if let Some(p2) = self.src_port { if p.src_port != p2 { m = false; } } }
        if m { if let Some(p2) = self.dst_port { if p.dst_port != p2 { m = false; } } }
        if self.negate { !m } else { m }
    }
}

// Args

struct Args {
    iface:      Option<String>,
    list:       bool,
    count:      Option<u64>,
    write:      Option<String>,
    read:       Option<String>,
    verbose:    u8,        // 0 = normal, 1 = -v, 2 = -vv, 3 = -vvv
    hex:        bool,      // -x  hex only
    hex_ascii:  bool,      // -X  hex + ascii together
    ascii:      bool,      // -A  ascii payload only
    quiet:      bool,      // -q
    no_resolve: bool,      // -n  skip port name resolution
    abs_seq:    bool,      // -S  absolute sequence numbers
    line_buf:   bool,      // -l  flush stdout after each line
    snaplen:    u32,       // -s
    ts_fmt:     TsFmt,
    filter:     Filter,
}

// Adapter

#[derive(Clone)]
struct Adapter {
    index: usize,
    name:  String,
    ip:    [u8; 4],
}

// ParsedPacket

struct ParsedPacket {
    src_ip:    Ipv4Addr,
    dst_ip:    Ipv4Addr,
    protocol:  u8,
    ttl:       u8,
    tos:       u8,
    ip_id:     u16,
    df:        bool,
    total_len: u16,
    src_port:  u16,
    dst_port:  u16,
    tcp_flags: u8,
    tcp_seq:   u32,
    tcp_ack:   u32,
    tcp_win:   u16,
    tcp_urg:   u16,
    icmp_type: u8,
    icmp_code: u8,
    icmp_id:   u16,
    icmp_seq:  u16,
    data_len:  usize,
    ip_hdr_len: usize,
    tcp_hdr_len: usize,
    timestamp: SystemTime,
}

// Sequence number tracker

struct SeqTracker {
    map: HashMap<([u8; 4], [u8; 4], u16, u16), u32>,
}

impl SeqTracker {
    fn new() -> Self { SeqTracker { map: HashMap::new() } }

    fn relativize(&mut self, p: &ParsedPacket) -> (u32, u32) {
        let fwd = (p.src_ip.octets(), p.dst_ip.octets(), p.src_port, p.dst_port);
        let rev = (p.dst_ip.octets(), p.src_ip.octets(), p.dst_port, p.src_port);

        // Record ISN on SYN, or on first sighting of this flow
        if p.tcp_flags & TH_SYN != 0 {
            self.map.insert(fwd, p.tcp_seq);
        } else {
            self.map.entry(fwd).or_insert(p.tcp_seq);
        }

        let fwd_isn = *self.map.get(&fwd).unwrap_or(&p.tcp_seq);
        let rel_seq = p.tcp_seq.wrapping_sub(fwd_isn);

        let rel_ack = if p.tcp_flags & TH_ACK != 0 {
            let peer_isn = self.map.get(&rev).copied().unwrap_or(p.tcp_ack.wrapping_sub(1));
            p.tcp_ack.wrapping_sub(peer_isn)
        } else {
            p.tcp_ack
        };

        (rel_seq, rel_ack)
    }
}

// Argument parsing

fn parse_args() -> Args {
    let mut a = Args {
        iface: None, list: false, count: None, write: None, read: None,
        verbose: 0, hex: false, hex_ascii: false, ascii: false,
        quiet: false, no_resolve: false, abs_seq: false, line_buf: false,
        snaplen: 65535, ts_fmt: TsFmt::Default,
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
            "-r" => { i += 1; if let Some(s) = raw.get(i) { a.read = Some(s.clone()); } }
            "-s" => { i += 1; if let Some(s) = raw.get(i) { a.snaplen = s.parse().unwrap_or(65535); } }
            "-v"   => a.verbose = a.verbose.max(1),
            "-vv"  => a.verbose = a.verbose.max(2),
            "-vvv" => a.verbose = a.verbose.max(3),
            "-x"   => a.hex = true,
            "-X"   => a.hex_ascii = true,
            "-A"   => a.ascii = true,
            "-q"   => a.quiet = true,
            "-n" | "-nn" => a.no_resolve = true,
            "-S"   => a.abs_seq = true,
            "-l"   => a.line_buf = true,
            "-t"   => a.ts_fmt = TsFmt::None,
            "-tt"  => a.ts_fmt = TsFmt::Epoch,
            "-ttt" => a.ts_fmt = TsFmt::Delta,
            "-tttt"=> a.ts_fmt = TsFmt::DateTime,
            "-h" | "--help" => { print_help(); std::process::exit(0); }
            "tcp"  => a.filter.protocol = Some(PROTO_TCP),
            "udp"  => a.filter.protocol = Some(PROTO_UDP),
            "icmp" => a.filter.protocol = Some(PROTO_ICMP),
            "not"  => a.filter.negate = !a.filter.negate,
            "host" => { i += 1; if let Some(s) = raw.get(i) { a.filter.host = parse_ip(s); } }
            "src"  => {
                i += 1;
                match raw.get(i).map(|s| s.as_str()) {
                    Some("host") => { i += 1; if let Some(s) = raw.get(i) { a.filter.src_host = parse_ip(s); } }
                    Some("port") => { i += 1; if let Some(s) = raw.get(i) { a.filter.src_port = s.parse().ok(); } }
                    _ => {}
                }
            }
            "dst" => {
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
    Some(s.parse::<Ipv4Addr>().ok()?.octets())
}

fn print_help() {
    println!("Usage: tcpdump [-i iface] [-D] [-c n] [-w file] [-r file] [-v] [-A] [-x] [-X] [-q] [-n] [-S] [filter]");
    println!();
    println!("Options:");
    println!("  -i <iface>     Interface IP, description substring, or index (from -D)");
    println!("  -D             List available interfaces and exit");
    println!("  -c <n>         Stop after n packets");
    println!("  -w <file>      Write packets to pcap file (Wireshark-compatible)");
    println!("  -r <file>      Read packets from pcap file instead of capturing");
    println!("  -s <len>       Snapshot length (default 65535)");
    println!("  -v / -vv       Verbose: show IP header fields");
    println!("  -A             Print packet payload as ASCII");
    println!("  -x             Hex dump of packet");
    println!("  -X             Hex + ASCII dump of packet");
    println!("  -q             Quick (quiet) output");
    println!("  -n             Don't resolve port names to service names");
    println!("  -S             Print absolute TCP sequence numbers");
    println!("  -l             Line-buffered output");
    println!("  -t             Don't print timestamp");
    println!("  -tt            Print seconds since epoch");
    println!("  -ttt           Print delta time between packets");
    println!("  -tttt          Print date and time");
    println!();
    println!("Filter expressions:");
    println!("  tcp / udp / icmp           Protocol filter");
    println!("  host <IP>                  Traffic to/from IP");
    println!("  src host <IP>              Traffic from IP");
    println!("  dst host <IP>              Traffic to IP");
    println!("  port <N>                   Traffic on port N");
    println!("  src port <N>               Traffic from port N");
    println!("  dst port <N>               Traffic to port N");
    println!("  not <expr>                 Negate filter");
    println!("  and / or                   Combine filters");
    println!();
    println!("Examples:");
    println!("  tcpdump -D");
    println!("  tcpdump -i 0 tcp and port 443");
    println!("  tcpdump -i 192.168.1.5 host 8.8.8.8 -w capture.pcap");
    println!("  tcpdump -r capture.pcap -n");
    println!("  tcpdump -i 0 -c 100 not icmp");
    println!();
    println!("Requires Administrator privileges for live capture.");
}

// Interface enumeration

fn list_adapters() -> Vec<Adapter> {
    let mut result = Vec::new();
    #[cfg(windows)]
    unsafe {
        let mut size: u32 = 0;
        GetAdaptersInfo(std::ptr::null_mut(), &mut size);
        if size == 0 { return result; }

        let mut buf = vec![0u8; size as usize];
        let ret = GetAdaptersInfo(buf.as_mut_ptr() as *mut IP_ADAPTER_INFO, &mut size);
        if ret != 0 { eprintln!("GetAdaptersInfo failed: {}", ret); return result; }

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
            if let Ok(idx) = s.parse::<usize>() {
                return adapters.iter().find(|a| a.index == idx);
            }
            if let Ok(ip_addr) = s.parse::<Ipv4Addr>() {
                let oct = ip_addr.octets();
                if let Some(a) = adapters.iter().find(|a| a.ip == oct) { return Some(a); }
            }
            let lower = s.to_lowercase();
            adapters.iter().find(|a| a.name.to_lowercase().contains(&lower))
        }
    }
}

// Windows raw socket

fn wsa_error_str(code: i32) -> &'static str {
    match code {
        10013 => "WSAEACCES — run as Administrator",
        10022 => "WSAEINVAL — invalid argument",
        10038 => "WSAENOTSOCK",
        10047 => "WSAEAFNOSUPPORT",
        10048 => "WSAEADDRINUSE",
        10049 => "WSAEADDRNOTAVAIL — address not available on this adapter",
        10065 => "WSAEHOSTUNREACH",
        _     => "unknown WSA error",
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
                "socket() failed ({}): {}\nRun as Administrator.",
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

        if bind(sock, &addr as *const SOCKADDR_IN as *const SOCKADDR,
                std::mem::size_of::<SOCKADDR_IN>() as i32) == SOCKET_ERROR {
            let err = WSAGetLastError();
            closesocket(sock);
            WSACleanup();
            return Err(format!(
                "bind() to {}.{}.{}.{} failed ({}): {}\nUse -D to list interfaces.",
                ip[0], ip[1], ip[2], ip[3], err, wsa_error_str(err)
            ));
        }

        let rcvall = RCVALL_ON;
        let mut bytes = 0u32;
        let ioc_ret = WSAIoctl(
            sock, SIO_RCVALL,
            &rcvall as *const u32 as *const c_void, 4,
            std::ptr::null_mut(), 0, &mut bytes,
            std::ptr::null_mut() as *mut OVERLAPPED, None,
        );
        if ioc_ret == SOCKET_ERROR {
            let err = WSAGetLastError();
            eprintln!(
                "Warning: SIO_RCVALL failed ({}): {} — capture may be incomplete",
                err, wsa_error_str(err)
            );
        }

        setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO,
            &RECV_TIMEOUT_MS as *const u32 as *const u8, 4);

        Ok(sock)
    }
}

// Packet parsing

fn parse_packet(buf: &[u8]) -> Option<ParsedPacket> {
    if buf.len() < 20 { return None; }
    if buf[0] >> 4 != 4 { return None; } // IPv4 only

    let ihl        = (buf[0] & 0x0F) as usize * 4;
    let tos        = buf[1];
    let total_len  = u16::from_be_bytes([buf[2], buf[3]]);
    let ip_id      = u16::from_be_bytes([buf[4], buf[5]]);
    let flags_frag = u16::from_be_bytes([buf[6], buf[7]]);
    let df         = (flags_frag & 0x4000) != 0;
    let ttl        = buf[8];
    let protocol   = buf[9];
    let src_ip     = Ipv4Addr::new(buf[12], buf[13], buf[14], buf[15]);
    let dst_ip     = Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]);

    if ihl > buf.len() { return None; }

    let mut p = ParsedPacket {
        src_ip, dst_ip, protocol, ttl, tos, ip_id, df, total_len,
        src_port: 0, dst_port: 0,
        tcp_flags: 0, tcp_seq: 0, tcp_ack: 0, tcp_win: 0, tcp_urg: 0,
        icmp_type: 0, icmp_code: 0, icmp_id: 0, icmp_seq: 0,
        data_len: 0, ip_hdr_len: ihl, tcp_hdr_len: 0,
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
            p.tcp_urg   = u16::from_be_bytes([transport[18], transport[19]]);
            let doff    = (transport[12] >> 4) as usize * 4;
            p.tcp_hdr_len = doff;
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

// Display helpers

fn fmt_flags(f: u8) -> String {
    // Order matches tcpdump: F S R P . U E W
    let mut s = String::new();
    if f & TH_FIN  != 0 { s.push('F'); }
    if f & TH_SYN  != 0 { s.push('S'); }
    if f & TH_RST  != 0 { s.push('R'); }
    if f & TH_PUSH != 0 { s.push('P'); }
    if f & TH_ACK  != 0 { s.push('.'); }
    if f & TH_URG  != 0 { s.push('U'); }
    if f & TH_ECE  != 0 { s.push('E'); }
    if f & TH_CWR  != 0 { s.push('W'); }
    if s.is_empty() { "none".to_string() } else { s }
}

fn port_name(port: u16, no_resolve: bool) -> String {
    if no_resolve { return port.to_string(); }
    let name = match port {
        7    => "echo",
        13   => "daytime",
        20   => "ftp-data",
        21   => "ftp",
        22   => "ssh",
        23   => "telnet",
        25   => "smtp",
        37   => "time",
        43   => "whois",
        53   => "domain",
        67   => "bootps",
        68   => "bootpc",
        69   => "tftp",
        80   => "http",
        88   => "kerberos",
        110  => "pop3",
        111  => "sunrpc",
        119  => "nntp",
        123  => "ntp",
        135  => "epmap",
        137  => "netbios-ns",
        138  => "netbios-dgm",
        139  => "netbios-ssn",
        143  => "imap",
        161  => "snmp",
        162  => "snmptrap",
        179  => "bgp",
        389  => "ldap",
        443  => "https",
        445  => "microsoft-ds",
        465  => "smtps",
        500  => "isakmp",
        514  => "syslog",
        520  => "router",
        587  => "submission",
        636  => "ldaps",
        993  => "imaps",
        995  => "pop3s",
        1080 => "socks",
        1194 => "openvpn",
        1433 => "ms-sql-s",
        1723 => "pptp",
        3306 => "mysql",
        3389 => "ms-wbt-server",
        4500 => "ipsec-nat-t",
        5432 => "postgresql",
        5900 => "rfb",
        6379 => "redis",
        8080 => "http-alt",
        8443 => "https-alt",
        9200 => "wap-wsp",
        _    => return port.to_string(),
    };
    name.to_string()
}

fn proto_name(p: u8) -> &'static str {
    match p {
        1   => "ICMP",
        2   => "IGMP",
        6   => "TCP",
        17  => "UDP",
        41  => "IPv6",
        47  => "GRE",
        50  => "ESP",
        51  => "AH",
        58  => "IPv6-ICMP",
        89  => "OSPF",
        132 => "SCTP",
        _   => "unknown",
    }
}

fn icmp_type_name(ty: u8, code: u8) -> String {
    match ty {
        0  => "echo reply".into(),
        3  => match code {
            0  => "net unreachable".into(),
            1  => "host unreachable".into(),
            2  => "protocol unreachable".into(),
            3  => "port unreachable".into(),
            4  => "frag needed and DF set".into(),
            5  => "source route failed".into(),
            6  => "dest net unknown".into(),
            7  => "dest host unknown".into(),
            9  => "dest net prohibited".into(),
            10 => "dest host prohibited".into(),
            13 => "packet filtered".into(),
            _  => format!("dest unreachable (code {})", code),
        },
        4  => "source quench".into(),
        5  => match code {
            0 => "redirect network".into(),
            1 => "redirect host".into(),
            2 => "redirect tos and net".into(),
            3 => "redirect tos and host".into(),
            _ => format!("redirect (code {})", code),
        },
        8  => "echo request".into(),
        9  => "router advertisement".into(),
        10 => "router solicitation".into(),
        11 => match code {
            0 => "time exceeded in-transit".into(),
            1 => "time exceeded reassembly".into(),
            _ => format!("time exceeded (code {})", code),
        },
        12 => "parameter problem".into(),
        13 => "time stamp request".into(),
        14 => "time stamp reply".into(),
        17 => "address mask request".into(),
        18 => "address mask reply".into(),
        _  => format!("type-{} code-{}", ty, code),
    }
}

fn unix_to_ymd(secs: u64) -> (u32, u32, u32) {
    let days = (secs / 86400) as u32;
    let mut y = 1970u32;
    let mut d = days;
    loop {
        let yd = if is_leap(y) { 366u32 } else { 365 };
        if d < yd { break; }
        d -= yd;
        y += 1;
    }
    let mdays: [u32; 12] = [31, if is_leap(y) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut mo = 1u32;
    for &md in &mdays {
        if d < md { break; }
        d -= md;
        mo += 1;
    }
    (y, mo, d + 1)
}

fn is_leap(y: u32) -> bool { (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 }

fn fmt_timestamp(ts: &SystemTime, fmt: &TsFmt, prev: Option<&SystemTime>) -> String {
    let dur      = ts.duration_since(UNIX_EPOCH).unwrap_or_default();
    let total_s  = dur.as_secs();
    let us       = dur.subsec_micros();

    match fmt {
        TsFmt::None     => String::new(),
        TsFmt::Epoch    => format!("{}.{:06}", total_s, us),
        TsFmt::Delta    => {
            let delta = if let Some(p) = prev {
                let pd = p.duration_since(UNIX_EPOCH).unwrap_or_default();
                dur.checked_sub(pd).unwrap_or_default()
            } else {
                Duration::ZERO
            };
            format!("{}.{:06}", delta.as_secs(), delta.subsec_micros())
        }
        TsFmt::DateTime => {
            let (y, mo, d) = unix_to_ymd(total_s);
            let h = (total_s % 86400) / 3600;
            let m = (total_s % 3600) / 60;
            let s = total_s % 60;
            format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:06}", y, mo, d, h, m, s, us)
        }
        TsFmt::Default  => {
            let h = (total_s % 86400) / 3600;
            let m = (total_s % 3600) / 60;
            let s = total_s % 60;
            format!("{:02}:{:02}:{:02}.{:06}", h, m, s, us)
        }
    }
}

fn dump_hex_ascii(raw: &[u8], hex: bool, ascii: bool, snaplen: u32) {
    let snap = raw.len().min(snaplen as usize);
    if snap == 0 { return; }

    if hex || ascii {
        for (i, chunk) in raw[..snap].chunks(16).enumerate() {
            print!("\t0x{:04x}:  ", i * 16);

            if hex {
                let mut j = 0;
                while j < 16 {
                    if j < chunk.len() {
                        let hi = chunk[j];
                        let lo = if j + 1 < chunk.len() { chunk[j + 1] } else { 0 };
                        if j + 1 < chunk.len() {
                            print!("{:02x}{:02x} ", hi, lo);
                        } else {
                            print!("{:02x}   ", hi);
                        }
                    } else {
                        print!("     ");
                    }
                    j += 2;
                }
            }

            if ascii {
                print!(" ");
                for &b in chunk {
                    print!("{}", if b >= 0x20 && b < 0x7f { b as char } else { '.' });
                }
            }
            println!();
        }
    }
}

fn dump_ascii_only(raw: &[u8], payload_offset: usize, snaplen: u32) {
    let snap = raw.len().min(snaplen as usize);
    if payload_offset >= snap { return; }
    let payload = &raw[payload_offset..snap];
    for chunk in payload.chunks(64) {
        print!("\t");
        for &b in chunk {
            print!("{}", if b >= 0x20 && b < 0x7f { b as char } else { '.' });
        }
        println!();
    }
}

fn print_packet(
    p:           &ParsedPacket,
    args:        &Args,
    raw:         &[u8],
    seq_tracker: &mut SeqTracker,
    prev_ts:     Option<&SystemTime>,
) {
    let ts_str = fmt_timestamp(&p.timestamp, &args.ts_fmt, prev_ts);
    let ts_pfx = if ts_str.is_empty() { String::new() } else { format!("{} ", ts_str) };

    let ip_verbose = if args.verbose >= 1 {
        format!(" (tos 0x{:x}, ttl {}, id {}, offset 0, flags [{}], proto {} ({}), length {})",
            p.tos, p.ttl, p.ip_id,
            if p.df { "DF" } else { "none" },
            proto_name(p.protocol), p.protocol,
            p.total_len)
    } else {
        String::new()
    };

    match p.protocol {
        PROTO_TCP => {
            let sp = port_name(p.src_port, args.no_resolve);
            let dp = port_name(p.dst_port, args.no_resolve);

            if args.quiet {
                println!("{}IP{} {}.{} > {}.{}: tcp {}",
                    ts_pfx, ip_verbose, p.src_ip, sp, p.dst_ip, dp, p.data_len);
            } else {
                let flags = fmt_flags(p.tcp_flags);

                let (rel_seq, rel_ack) = if args.abs_seq {
                    (p.tcp_seq, p.tcp_ack)
                } else {
                    seq_tracker.relativize(p)
                };

                let seq_part = if args.verbose >= 2
                    || p.data_len > 0
                    || p.tcp_flags & (TH_SYN | TH_FIN | TH_RST) != 0
                {
                    if p.data_len > 0 {
                        format!(", seq {}:{}", rel_seq, rel_seq.wrapping_add(p.data_len as u32))
                    } else {
                        format!(", seq {}", rel_seq)
                    }
                } else {
                    String::new()
                };

                let ack_part = if p.tcp_flags & TH_ACK != 0 {
                    format!(", ack {}", rel_ack)
                } else {
                    String::new()
                };

                let urg_part = if p.tcp_flags & TH_URG != 0 {
                    format!(", urg {}", p.tcp_urg)
                } else {
                    String::new()
                };

                println!("{}IP{} {}.{} > {}.{}: Flags [{}]{}, win {}{}{}, length {}",
                    ts_pfx, ip_verbose,
                    p.src_ip, sp, p.dst_ip, dp,
                    flags, seq_part, p.tcp_win, ack_part, urg_part, p.data_len);
            }
        }
        PROTO_UDP => {
            let sp = port_name(p.src_port, args.no_resolve);
            let dp = port_name(p.dst_port, args.no_resolve);

            if args.quiet {
                println!("{}IP{} {}.{} > {}.{}: udp {}",
                    ts_pfx, ip_verbose, p.src_ip, sp, p.dst_ip, dp, p.data_len);
            } else {
                println!("{}IP{} {}.{} > {}.{}: UDP, length {}",
                    ts_pfx, ip_verbose, p.src_ip, sp, p.dst_ip, dp, p.data_len);
            }
        }
        PROTO_ICMP => {
            if args.quiet {
                println!("{}IP{} {} > {}: ICMP, length {}",
                    ts_pfx, ip_verbose, p.src_ip, p.dst_ip, p.total_len);
            } else {
                let name   = icmp_type_name(p.icmp_type, p.icmp_code);
                let detail = if p.icmp_type == 0 || p.icmp_type == 8 {
                    format!(", id {}, seq {}, length {}", p.icmp_id, p.icmp_seq, p.data_len + 8)
                } else {
                    format!(", length {}", p.total_len)
                };
                println!("{}IP{} {} > {}: ICMP {}{}", ts_pfx, ip_verbose, p.src_ip, p.dst_ip, name, detail);
            }
        }
        proto => {
            println!("{}IP{} {} > {}: {} ({}), length {}",
                ts_pfx, ip_verbose, p.src_ip, p.dst_ip,
                proto_name(proto), proto, p.total_len);
        }
    }

    let payload_offset = p.ip_hdr_len + p.tcp_hdr_len
        + match p.protocol { PROTO_UDP => 8, PROTO_ICMP => 8, _ => 0 };

    if args.hex_ascii {
        dump_hex_ascii(raw, true, true, args.snaplen);
    } else if args.hex {
        dump_hex_ascii(raw, true, false, args.snaplen);
    } else if args.ascii {
        dump_ascii_only(raw, payload_offset, args.snaplen);
    }

    if args.line_buf { let _ = std::io::stdout().flush(); }
}

// pcap writer

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
        w.write_all(&101u32.to_le_bytes())?; // raw IP
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

// pcap reader

fn r32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off+1], b[off+2], b[off+3]])
}
fn r32be(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off+1], b[off+2], b[off+3]])
}

fn read_pcap(path: &str, args: &Args) -> (u64, u64) {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) => { eprintln!("tcpdump: {}: {}", path, e); std::process::exit(2); }
    };

    let mut ghdr = [0u8; 24];
    if f.read_exact(&mut ghdr).is_err() {
        eprintln!("tcpdump: {}: truncated pcap header", path);
        std::process::exit(2);
    }

    let magic = u32::from_le_bytes([ghdr[0], ghdr[1], ghdr[2], ghdr[3]]);
    let (le, nano) = match magic {
        0xa1b2c3d4 => (true,  false),
        0xd4c3b2a1 => (false, false),
        0xa1b23c4d => (true,  true),
        0x4d3cb2a1 => (false, true),
        _ => {
            eprintln!("tcpdump: {}: bad pcap magic 0x{:08x}", path, magic);
            std::process::exit(2);
        }
    };

    let network = if le { r32le(&ghdr, 20) } else { r32be(&ghdr, 20) };

    // Ethernet = 1
    // Raw IP   = 101
    // Null/loopback = 0
    let link_skip: usize = match network {
        0   => 4,
        1   => 14,
        101 => 0,
        _   => 0,
    };

    let mut seq_tracker = SeqTracker::new();
    let mut prev_ts: Option<SystemTime> = None;
    let mut captured = 0u64;
    let mut received = 0u64;
    let mut rec_hdr  = [0u8; 16];
    let mut buf      = vec![0u8; 65536];

    eprintln!("reading from file {}, link-type {} ({})",
        path, network, match network { 1 => "EN10MB", 101 => "RAW", _ => "unknown" });

    loop {
        match f.read_exact(&mut rec_hdr) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => { eprintln!("tcpdump: read error: {}", e); break; }
        }

        let ts_sec  = if le { r32le(&rec_hdr, 0) } else { r32be(&rec_hdr, 0) };
        let ts_frac = if le { r32le(&rec_hdr, 4) } else { r32be(&rec_hdr, 4) };
        let incl    = if le { r32le(&rec_hdr, 8) } else { r32be(&rec_hdr, 8) } as usize;

        if incl > buf.len() { buf.resize(incl + 256, 0); }
        if f.read_exact(&mut buf[..incl]).is_err() { break; }

        let ts_us = if nano { ts_frac / 1000 } else { ts_frac };
        let ts    = UNIX_EPOCH + Duration::new(ts_sec as u64, ts_us * 1000);

        if incl <= link_skip { received += 1; continue; }
        let raw = &buf[link_skip..incl];

        received += 1;
        let Some(mut pkt) = parse_packet(raw) else { continue; };
        pkt.timestamp = ts;

        if !args.filter.matches(&pkt) { continue; }
        if let Some(max) = args.count { if captured >= max { break; } }

        print_packet(&pkt, args, raw, &mut seq_tracker, prev_ts.as_ref());
        prev_ts = Some(ts);
        captured += 1;
    }

    (captured, received)
}

// ANSI terminal

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

// Main

fn main() {
    enable_ansi();
    let args = parse_args();

    // Read from file
    if let Some(ref path) = args.read {
        let (captured, received) = read_pcap(path, &args);
        eprintln!("\n{} packets captured", captured);
        eprintln!("{} packets received by filter", received);
        eprintln!("0 packets dropped by kernel");
        return;
    }

    // Live capture
    let adapters = list_adapters();

    if args.list {
        if adapters.is_empty() {
            eprintln!("tcpdump: no interfaces found — run as Administrator");
            return;
        }
        for a in &adapters {
            let flags = if a.ip[0] == 127 { "[Up, Running, Loopback]" } else { "[Up, Running]" };
            println!("{}.{}.{}.{}.{} ({}) {}",
                a.index, a.ip[0], a.ip[1], a.ip[2], a.ip[3],
                a.name, flags);
        }
        return;
    }

    let adapter = match select_adapter(&adapters, &args.iface) {
        Some(a) => a.clone(),
        None => {
            if adapters.is_empty() {
                eprintln!("tcpdump: no network adapters found — run as Administrator");
            } else {
                eprintln!("tcpdump: interface not found. Use -D to list interfaces, then -i <index>.");
            }
            std::process::exit(1);
        }
    };

    eprintln!("tcpdump: listening on {}, link-type RAW (Raw IP), snapshot length {} bytes",
        adapter.name, args.snaplen);

    #[cfg(not(windows))]
    { eprintln!("tcpdump: live capture requires Windows."); std::process::exit(1); }

    #[cfg(windows)]
    let sock = match open_capture(adapter.ip) {
        Ok(s)  => s,
        Err(e) => { eprintln!("tcpdump: {}", e); std::process::exit(1); }
    };

    let mut pcap = args.write.as_deref().map(|path| {
        match PcapWriter::new(path, args.snaplen) {
            Ok(w)  => { eprintln!("tcpdump: writing to {}", path); w }
            Err(e) => { eprintln!("tcpdump: cannot open {}: {}", path, e); std::process::exit(1); }
        }
    });

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::Relaxed))
        .expect("Error setting Ctrl+C handler");

    let mut buf      = vec![0u8; 65535];
    let mut captured: u64 = 0;
    let mut received: u64 = 0;
    let mut seq_tracker   = SeqTracker::new();
    let mut prev_ts: Option<SystemTime> = None;

    #[cfg(windows)]
    loop {
        if !running.load(Ordering::Relaxed) { break; }
        if let Some(max) = args.count { if captured >= max { break; } }

        let n = unsafe { recv(sock, buf.as_mut_ptr(), buf.len() as i32, 0) };

        if n == SOCKET_ERROR {
            let err = unsafe { WSAGetLastError() };
            if err == WSAETIMEDOUT { continue; }
            eprintln!("tcpdump: recv() error {}: {}", err, wsa_error_str(err));
            break;
        }
        if n <= 0 { continue; }

        let raw = &buf[..n as usize];
        received += 1;

        let Some(pkt) = parse_packet(raw) else { continue; };
        if !args.filter.matches(&pkt) { continue; }

        print_packet(&pkt, &args, raw, &mut seq_tracker, prev_ts.as_ref());
        prev_ts = Some(pkt.timestamp);

        if let Some(ref mut w) = pcap {
            if let Err(e) = w.write_packet(raw, &pkt.timestamp) {
                eprintln!("tcpdump: pcap write error: {}", e);
            }
        }
        captured += 1;
    }

    // Stats — matching real tcpdump format
    eprintln!("\n{} packets captured", captured);
    eprintln!("{} packets received by filter", received);
    eprintln!("0 packets dropped by kernel");

    #[cfg(windows)]
    unsafe {
        closesocket(sock);
        WSACleanup();
    }
}
