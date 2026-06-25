use colored::Colorize;
use std::net::{UdpSocket, SocketAddr};
use std::time::{Duration, Instant};

// DNS constants

const QTYPE_A:     u16 = 1;
const QTYPE_NS:    u16 = 2;
const QTYPE_CNAME: u16 = 5;
const QTYPE_SOA:   u16 = 6;
const QTYPE_PTR:   u16 = 12;
const QTYPE_MX:    u16 = 15;
const QTYPE_TXT:   u16 = 16;
const QTYPE_AAAA:  u16 = 28;
const QTYPE_SRV:   u16 = 33;
const QTYPE_ANY:   u16 = 255;

const QCLASS_IN: u16 = 1;

fn qtype_name(t: u16) -> &'static str {
    match t {
        QTYPE_A     => "A",
        QTYPE_NS    => "NS",
        QTYPE_CNAME => "CNAME",
        QTYPE_SOA   => "SOA",
        QTYPE_PTR   => "PTR",
        QTYPE_MX    => "MX",
        QTYPE_TXT   => "TXT",
        QTYPE_AAAA  => "AAAA",
        QTYPE_SRV   => "SRV",
        QTYPE_ANY   => "ANY",
        _           => "UNKNOWN",
    }
}

fn parse_qtype(s: &str) -> Option<u16> {
    Some(match s.to_ascii_uppercase().as_str() {
        "A"     => QTYPE_A,
        "NS"    => QTYPE_NS,
        "CNAME" => QTYPE_CNAME,
        "SOA"   => QTYPE_SOA,
        "PTR"   => QTYPE_PTR,
        "MX"    => QTYPE_MX,
        "TXT"   => QTYPE_TXT,
        "AAAA"  => QTYPE_AAAA,
        "SRV"   => QTYPE_SRV,
        "ANY"   => QTYPE_ANY,
        _       => return None,
    })
}

fn rcode_str(rcode: u8) -> &'static str {
    match rcode {
        0 => "NOERROR",
        1 => "FORMERR",
        2 => "SERVFAIL",
        3 => "NXDOMAIN",
        4 => "NOTIMP",
        5 => "REFUSED",
        _ => "UNKNOWN",
    }
}

// DNS packet builder

fn build_query(id: u16, name: &str, qtype: u16, rec_desired: bool) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&id.to_be_bytes());
    let flags: u16 = if rec_desired { 0x0100 } else { 0x0000 };
    buf.extend_from_slice(&flags.to_be_bytes());
    buf.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    buf.extend_from_slice(&0u16.to_be_bytes()); // ancount
    buf.extend_from_slice(&0u16.to_be_bytes()); // nscount
    buf.extend_from_slice(&0u16.to_be_bytes()); // arcount

    // Encode name as DNS labels
    let clean = name.trim_end_matches('.');
    for label in clean.split('.') {
        buf.push(label.len() as u8);
        buf.extend_from_slice(label.as_bytes());
    }
    buf.push(0); // root label

    buf.extend_from_slice(&qtype.to_be_bytes());
    buf.extend_from_slice(&QCLASS_IN.to_be_bytes());
    buf
}

// DNS response parser

struct Parser<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(buf: &'a [u8]) -> Self { Self { buf, pos: 0 } }

    fn u8(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn u16(&mut self) -> Option<u16> {
        let hi = self.u8()? as u16;
        let lo = self.u8()? as u16;
        Some((hi << 8) | lo)
    }

    fn u32(&mut self) -> Option<u32> {
        let hi = self.u16()? as u32;
        let lo = self.u16()? as u32;
        Some((hi << 16) | lo)
    }

    fn skip(&mut self, n: usize) { self.pos += n; }

    fn slice(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.buf.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }

    // DNS name with pointer compression
    fn name(&mut self) -> Option<String> {
        self.read_name_at(self.pos).map(|(s, end)| { self.pos = end; s })
    }

    fn read_name_at(&self, mut pos: usize) -> Option<(String, usize)> {
        let mut labels: Vec<String> = Vec::new();
        let mut end_pos = None;
        let mut jumps = 0;
        loop {
            if jumps > 10 { break; } // pointer loop guard
            let b = *self.buf.get(pos)?;
            if b == 0 {
                end_pos.get_or_insert(pos + 1);
                break;
            } else if b & 0xC0 == 0xC0 {
                let offset = (((b & 0x3F) as usize) << 8) | (*self.buf.get(pos + 1)? as usize);
                end_pos.get_or_insert(pos + 2);
                pos = offset;
                jumps += 1;
            } else {
                let len = b as usize;
                pos += 1;
                let label = std::str::from_utf8(self.buf.get(pos..pos + len)?).ok()?;
                labels.push(label.to_string());
                pos += len;
            }
        }
        Some((labels.join("."), end_pos.unwrap_or(pos + 1)))
    }
}

// Record types

struct RR {
    name:  String,
    rtype: u16,
    class: u16,
    ttl:   u32,
    rdata: String,
}

fn parse_rdata(p: &mut Parser, rtype: u16, rdlen: u16) -> String {
    let start = p.pos;
    let end   = start + rdlen as usize;
    let result = match rtype {
        QTYPE_A if rdlen == 4 => {
            let a = p.u8().unwrap_or(0);
            let b = p.u8().unwrap_or(0);
            let c = p.u8().unwrap_or(0);
            let d = p.u8().unwrap_or(0);
            format!("{}.{}.{}.{}", a, b, c, d)
        }
        QTYPE_AAAA if rdlen == 16 => {
            let mut groups = [0u16; 8];
            for g in &mut groups {
                *g = p.u16().unwrap_or(0);
            }
            let s: Vec<String> = groups.iter().map(|x| format!("{:x}", x)).collect();
            s.join(":")
        }
        QTYPE_NS | QTYPE_CNAME | QTYPE_PTR => {
            p.name().unwrap_or_default()
        }
        QTYPE_MX => {
            let pref = p.u16().unwrap_or(0);
            let exch = p.name().unwrap_or_default();
            format!("{} {}.", pref, exch)
        }
        QTYPE_TXT => {
            let mut parts = Vec::new();
            while p.pos < end {
                let len = p.u8().unwrap_or(0) as usize;
                if let Some(bytes) = p.slice(len) {
                    parts.push(String::from_utf8_lossy(bytes).into_owned());
                }
            }
            format!("\"{}\"", parts.join(""))
        }
        QTYPE_SOA => {
            let mname  = p.name().unwrap_or_default();
            let rname  = p.name().unwrap_or_default();
            let serial  = p.u32().unwrap_or(0);
            let refresh = p.u32().unwrap_or(0);
            let retry   = p.u32().unwrap_or(0);
            let expire  = p.u32().unwrap_or(0);
            let minimum = p.u32().unwrap_or(0);
            format!("{}. {}. {} {} {} {} {}", mname, rname, serial, refresh, retry, expire, minimum)
        }
        QTYPE_SRV => {
            let priority = p.u16().unwrap_or(0);
            let weight   = p.u16().unwrap_or(0);
            let port     = p.u16().unwrap_or(0);
            let target   = p.name().unwrap_or_default();
            format!("{} {} {} {}.", priority, weight, port, target)
        }
        _ => {
            // Hex dump for unknown types
            let bytes = p.slice(rdlen as usize).unwrap_or(&[]);
            bytes.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ")
        }
    };
    p.pos = end; // ensure we consumed exactly rdlen bytes
    result
}

fn parse_response(buf: &[u8]) -> Option<(u8, u8, u16, Vec<RR>, Vec<RR>, Vec<RR>)> {
    let mut p = Parser::new(buf);

    let _id     = p.u16()?;
    let flags   = p.u16()?;
    let qr      = ((flags >> 15) & 1) as u8;
    let _opcode = ((flags >> 11) & 0xF) as u8;
    let _aa     = ((flags >> 10) & 1) as u8;
    let _tc     = ((flags >> 9)  & 1) as u8;
    let _rd     = ((flags >> 8)  & 1) as u8;
    let _ra     = ((flags >> 7)  & 1) as u8;
    let rcode   = (flags & 0xF) as u8;

    let qdcount = p.u16()? as usize;
    let ancount = p.u16()? as usize;
    let nscount = p.u16()? as usize;
    let arcount = p.u16()? as usize;

    // Skip question section
    for _ in 0..qdcount {
        p.name()?;
        p.skip(4); // qtype + qclass
    }

    let parse_rrs = |p: &mut Parser, count: usize| -> Vec<RR> {
        let mut rrs = Vec::new();
        for _ in 0..count {
            let name  = p.name().unwrap_or_default();
            let rtype = p.u16().unwrap_or(0);
            let class = p.u16().unwrap_or(0);
            let ttl   = p.u32().unwrap_or(0);
            let rdlen = p.u16().unwrap_or(0);
            let rdata = parse_rdata(p, rtype, rdlen);
            rrs.push(RR { name, rtype, class, ttl, rdata });
        }
        rrs
    };

    let answers     = parse_rrs(&mut p, ancount);
    let authority   = parse_rrs(&mut p, nscount);
    let additional  = parse_rrs(&mut p, arcount);

    Some((qr, rcode, flags, answers, authority, additional))
}

// Main

fn print_help() {
    println!("Usage: dig [@server] name [type] [options]");
    println!();
    println!("  @server            DNS server to query (default: 8.8.8.8)");
    println!("  name               hostname or IP (for PTR) to look up");
    println!("  type               record type: A AAAA MX NS TXT CNAME SOA SRV PTR ANY (default: A)");
    println!();
    println!("Options:");
    println!("  +short             print only the answer values");
    println!("  +norecurse         disable recursion desired flag");
    println!("  -p PORT            DNS server port (default: 53)");
    println!("  -4                 use IPv4 only");
    println!("  -x                 reverse lookup (PTR) — auto-reverses the IP");
    println!("  -h, --help         show this help");
}

fn reverse_ip(addr: &str) -> String {
    // 1.2.3.4 → 4.3.2.1.in-addr.arpa
    let parts: Vec<&str> = addr.split('.').collect();
    if parts.len() == 4 {
        format!("{}.{}.{}.{}.in-addr.arpa", parts[3], parts[2], parts[1], parts[0])
    } else {
        format!("{}.in-addr.arpa", addr)
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }

    let mut server    = "8.8.8.8".to_string();
    let mut port: u16 = 53;
    let mut qtype     = QTYPE_A;
    let mut name: Option<String> = None;
    let mut short     = false;
    let mut recurse   = true;
    let mut reverse   = false;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        if arg.starts_with('@') {
            server = arg[1..].to_string();
        } else if arg == "+short" {
            short = true;
        } else if arg == "+norecurse" || arg == "+norecurse" {
            recurse = false;
        } else if arg == "-x" {
            reverse = true;
        } else if arg == "-p" {
            i += 1;
            if let Some(v) = argv.get(i) { port = v.parse().unwrap_or(53); }
        } else if arg == "-4" {
            // already using IPv4 by default
        } else if let Some(t) = parse_qtype(arg) {
            qtype = t;
        } else if !arg.starts_with('-') {
            name = Some(arg.to_string());
        }
        i += 1;
    }

    let mut name = match name {
        Some(n) => n,
        None => { eprintln!("dig: no name specified"); std::process::exit(1); }
    };

    if reverse {
        name = reverse_ip(&name);
        qtype = QTYPE_PTR;
    }

    // Ensure fully-qualified
    if !name.ends_with('.') { name.push('.'); }

    let server_addr: SocketAddr = format!("{}:{}", server, port)
        .parse()
        .unwrap_or_else(|_| {
            // Try resolving the server name
            use std::net::ToSocketAddrs;
            format!("{}:{}", server, port)
                .to_socket_addrs()
                .ok()
                .and_then(|mut it| it.next())
                .unwrap_or_else(|| { eprintln!("dig: cannot resolve server '{}'", server); std::process::exit(1); })
        });

    let id: u16 = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() & 0xFFFF) as u16;

    let query = build_query(id, &name, qtype, recurse);

    let sock = UdpSocket::bind("0.0.0.0:0").expect("failed to bind UDP socket");
    sock.set_read_timeout(Some(Duration::from_secs(5))).ok();

    let t0 = Instant::now();
    sock.send_to(&query, server_addr).expect("failed to send query");

    let mut resp_buf = [0u8; 4096];
    let resp_len = match sock.recv_from(&mut resp_buf) {
        Ok((n, _)) => n,
        Err(e) => { eprintln!("dig: no response from {}: {}", server, e); std::process::exit(1); }
    };
    let elapsed = t0.elapsed();

    let buf = &resp_buf[..resp_len];
    let (_, rcode, _flags, answers, authority, additional) =
        match parse_response(buf) {
            Some(r) => r,
            None    => { eprintln!("dig: failed to parse response"); std::process::exit(1); }
        };

    if short {
        for rr in &answers {
            println!("{}", rr.rdata);
        }
        if rcode != 0 { std::process::exit(1); }
        return;
    }

    // Full dig-style output
    println!();
    println!("{}", format!("; <<>> dig-rs 1.0 <<>> {} {}", name.trim_end_matches('.'), qtype_name(qtype)).cyan());
    println!("{} {}", ";; ->>HEADER<<- opcode: QUERY, status:".cyan(), rcode_str(rcode).yellow());
    println!("{} answers: {}, authority: {}, additional: {}",
        ";; flags: qr rd ra;".cyan(), answers.len(), authority.len(), additional.len());

    if !answers.is_empty() {
        println!();
        println!("{}", ";; ANSWER SECTION:".cyan());
        for rr in &answers {
            println!("{:<24} {:<8} {:<4} {:<8} {}",
                format!("{}.", rr.name),
                rr.ttl,
                if rr.class == 1 { "IN" } else { "??" },
                qtype_name(rr.rtype).green(),
                rr.rdata);
        }
    }

    if !authority.is_empty() {
        println!();
        println!("{}", ";; AUTHORITY SECTION:".cyan());
        for rr in &authority {
            println!("{:<24} {:<8} {:<4} {:<8} {}",
                format!("{}.", rr.name),
                rr.ttl,
                if rr.class == 1 { "IN" } else { "??" },
                qtype_name(rr.rtype).green(),
                rr.rdata);
        }
    }

    if !additional.is_empty() {
        println!();
        println!("{}", ";; ADDITIONAL SECTION:".cyan());
        for rr in &additional {
            println!("{:<24} {:<8} {:<4} {:<8} {}",
                format!("{}.", rr.name),
                rr.ttl,
                if rr.class == 1 { "IN" } else { "??" },
                qtype_name(rr.rtype).green(),
                rr.rdata);
        }
    }

    println!();
    println!("{} {} ms", ";; Query time:".cyan(), elapsed.as_millis());
    println!("{} {}#{}", ";; SERVER:".cyan(), server, port);
    println!("{} {} bytes", ";; MSG SIZE rcvd:".cyan(), resp_len);
    println!();

    if rcode != 0 { std::process::exit(1); }
}
