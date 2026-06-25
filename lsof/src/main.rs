//! lsof — list open files and network connections

#![allow(non_snake_case, non_camel_case_types, dead_code, non_upper_case_globals)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::time::Duration;

// Windows imports

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE},
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable,
        MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
        MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID,
    },
    Storage::FileSystem::QueryDosDeviceW,
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
            PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        },
        LibraryLoader::{GetProcAddress, LoadLibraryA},
        Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE},
    },
};

const SystemExtendedHandleInformation: u32 = 64;
const ObjectNameInformation:           u32 = 1;
const ObjectTypeInformation:           u32 = 2;
const STATUS_INFO_LENGTH_MISMATCH: i32     = -1073741820;
const STATUS_SUCCESS:              i32     = 0;

type NtQuerySystemInfoFn = unsafe extern "system" fn(
    SystemInformationClass: u32,
    SystemInformation:       *mut c_void,
    SystemInformationLength: u32,
    ReturnLength:            *mut u32,
) -> i32;

type NtQueryObjectFn = unsafe extern "system" fn(
    Handle:                    HANDLE,
    ObjectInformationClass:    u32,
    ObjectInformation:         *mut c_void,
    ObjectInformationLength:   u32,
    ReturnLength:              *mut u32,
) -> i32;

#[repr(C)]
struct UnicodeString {
    Length:    u16,
    MaxLength: u16,
    Buffer:    *const u16,
}

#[repr(C)]
struct ObjectNameInfo {
    Name: UnicodeString,
    // followed by string data in same buffer
}

#[repr(C)]
struct ObjectTypeInfo {
    TypeName: UnicodeString,
    // 22 ULONGs + 3 BOOLEANs we don't need
    _rest: [u8; 100],
}

#[repr(C)]
struct SystemHandleEntryEx {
    Object:              *mut c_void,
    UniqueProcessId:     usize,
    HandleValue:         usize,
    GrantedAccess:       u32,
    CreatorBackTrace:    u16,
    ObjectTypeIndex:     u16,
    HandleAttributes:    u32,
    Reserved:            u32,
}

#[repr(C)]
struct SystemHandleInfoEx {
    NumberOfHandles: usize,
    Reserved:        usize,
    // followed by NumberOfHandles SystemHandleEntryEx
}

// NT loader

struct NtFns {
    query_sys:  NtQuerySystemInfoFn,
    query_obj:  NtQueryObjectFn,
}

#[cfg(windows)]
fn load_nt() -> Option<NtFns> {
    unsafe {
        let ntdll = LoadLibraryA(b"ntdll.dll\0".as_ptr());
        if ntdll == 0 { return None; }
        let qs = GetProcAddress(ntdll, b"NtQuerySystemInformation\0".as_ptr())?;
        let qo = GetProcAddress(ntdll, b"NtQueryObject\0".as_ptr())?;
        Some(NtFns {
            query_sys: std::mem::transmute(qs),
            query_obj: std::mem::transmute(qo),
        })
    }
}

#[cfg(not(windows))]
fn load_nt() -> Option<NtFns> { None }

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

// NT path -> DOS path translation

fn build_drive_map() -> HashMap<String, String> {
    let mut map = HashMap::new();
    #[cfg(windows)]
    unsafe {
        for b in b'A'..=b'Z' {
            let letter   = char::from(b);
            let drive_w: Vec<u16> = format!("{}:", letter).encode_utf16().chain([0]).collect();
            let mut buf  = vec![0u16; 512];
            let len      = QueryDosDeviceW(drive_w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32);
            if len > 0 {
                let nt = String::from_utf16_lossy(&buf[..len as usize])
                    .trim_end_matches('\0')
                    .to_owned();
                map.insert(nt, format!("{}:", letter));
            }
        }
    }
    map
}

fn nt_to_dos(path: &str, drives: &HashMap<String, String>) -> String {
    for (nt, dos) in drives {
        if path.starts_with(nt.as_str()) {
            return format!("{}{}", dos, &path[nt.len()..]);
        }
    }
    path.to_owned()
}

// Handle enumeration

#[derive(Debug)]
struct OpenFile {
    pid:  u32,
    path: String,
}

#[cfg(windows)]
fn get_handle_type(fns: &NtFns, handle: HANDLE) -> String {
    let mut buf = vec![0u8; 1024];
    let mut ret = 0u32;
    let st = unsafe {
        (fns.query_obj)(
            handle,
            ObjectTypeInformation,
            buf.as_mut_ptr() as *mut c_void,
            buf.len() as u32,
            &mut ret,
        )
    };
    if st != STATUS_SUCCESS { return String::new(); }
    let info = unsafe { &*(buf.as_ptr() as *const ObjectTypeInfo) };
    let len  = info.TypeName.Length as usize / 2;
    if info.TypeName.Buffer.is_null() || len == 0 { return String::new(); }
    let slice = unsafe { std::slice::from_raw_parts(info.TypeName.Buffer, len) };
    String::from_utf16_lossy(slice)
}

#[cfg(windows)]
fn get_handle_name_in_thread(
    fns_ptr: usize,
    handle:  HANDLE,
) -> Option<String> {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::sync_channel::<Option<String>>(1);

    std::thread::spawn(move || {
        let fns_ref: NtQueryObjectFn = unsafe { std::mem::transmute(fns_ptr) };
        let mut buf  = vec![0u8; 65536];
        let mut ret  = 0u32;
        let st = unsafe {
            fns_ref(
                handle,
                ObjectNameInformation,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut ret,
            )
        };
        let result = if st == STATUS_SUCCESS {
            let info = unsafe { &*(buf.as_ptr() as *const ObjectNameInfo) };
            let len  = info.Name.Length as usize / 2;
            if !info.Name.Buffer.is_null() && len > 0 {
                let slice = unsafe { std::slice::from_raw_parts(info.Name.Buffer, len) };
                Some(String::from_utf16_lossy(slice).to_owned())
            } else {
                None
            }
        } else {
            None
        };
        let _ = tx.send(result);
    });

    match rx.recv_timeout(Duration::from_millis(80)) {
        Ok(r)  => r,
        Err(_) => None,
    }
}

fn enumerate_files(
    fns:    &NtFns,
    drives: &HashMap<String, String>,
    pids:   Option<&[u32]>,
) -> Vec<OpenFile> {
    let mut result = Vec::new();
    #[cfg(windows)]
    unsafe {
        let mut buf_size = 1u32 << 20; // start at 1 MiB
        let mut buf;
        loop {
            buf = vec![0u8; buf_size as usize];
            let mut ret = 0u32;
            let st = (fns.query_sys)(
                SystemExtendedHandleInformation,
                buf.as_mut_ptr() as *mut c_void,
                buf_size,
                &mut ret,
            );
            if st == STATUS_SUCCESS            { break; }
            if st == STATUS_INFO_LENGTH_MISMATCH {
                buf_size = ret + (1 << 16); // grow buffer
                continue;
            }
            return result;
        }

        let info     = &*(buf.as_ptr() as *const SystemHandleInfoEx);
        let count    = info.NumberOfHandles;
        let base_ptr = buf.as_ptr().add(std::mem::size_of::<SystemHandleInfoEx>())
                           as *const SystemHandleEntryEx;
        let handles  = std::slice::from_raw_parts(base_ptr, count);

        let self_proc = GetCurrentProcess();

        for h in handles {
            let pid = h.UniqueProcessId as u32;
            if pid == 0 || pid == 4 { continue; } // skip Idle and System
            if let Some(filter) = pids {
                if !filter.contains(&pid) { continue; }
            }

            let proc_handle = OpenProcess(PROCESS_DUP_HANDLE, 0, pid);
            if proc_handle == 0 { continue; }

            let mut dup: HANDLE = 0;
            let ok = DuplicateHandle(
                proc_handle,
                h.HandleValue as HANDLE,
                self_proc,
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            );
            CloseHandle(proc_handle);
            if ok == 0 || dup == 0 { continue; }

            // Check type
            let ty = get_handle_type(fns, dup);
            if ty != "File" {
                CloseHandle(dup);
                continue;
            }

            let fns_ptr: usize = std::mem::transmute(fns.query_obj);
            if let Some(nt_path) = get_handle_name_in_thread(fns_ptr, dup) {
                if !nt_path.is_empty() {
                    let path = nt_to_dos(&nt_path, drives);
                    result.push(OpenFile { pid, path });
                }
            }

            CloseHandle(dup);
        }
    }
    result
}

// Network connections

const AF_INET:                 u32 = 2;
const TCP_TABLE_OWNER_PID_ALL: i32 = 5;
const UDP_TABLE_OWNER_PID:     i32 = 1;

fn fmt_ip(addr: u32) -> String {
    format!("{}.{}.{}.{}", addr & 0xFF, (addr >> 8) & 0xFF, (addr >> 16) & 0xFF, (addr >> 24) & 0xFF)
}

fn fmt_port(port: u32) -> u16 {
    ((port & 0xFF) << 8 | (port & 0xFF00) >> 8) as u16
}

fn tcp_state(s: u32) -> &'static str {
    match s {
        1  => "CLOSED",
        2  => "LISTEN",
        3  => "SYN_SENT",
        4  => "SYN_RCVD",
        5  => "ESTABLISHED",
        6  => "FIN_WAIT1",
        7  => "FIN_WAIT2",
        8  => "CLOSE_WAIT",
        9  => "CLOSING",
        10 => "LAST_ACK",
        11 => "TIME_WAIT",
        12 => "DELETE_TCB",
        _  => "UNKNOWN",
    }
}

#[derive(Debug)]
struct NetConn {
    pid:     u32,
    proto:   &'static str,
    local:   String,
    remote:  String,
    state:   String,
}

fn enumerate_network(pids: Option<&[u32]>) -> Vec<NetConn> {
    let mut result = Vec::new();
    #[cfg(windows)]
    unsafe {
        // TCP
        let mut size: u32 = 0;
        GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET, TCP_TABLE_OWNER_PID_ALL, 0);
        if size > 0 {
            let mut buf = vec![0u8; size as usize];
            if GetExtendedTcpTable(buf.as_mut_ptr() as *mut c_void, &mut size, 0, AF_INET, TCP_TABLE_OWNER_PID_ALL, 0) == 0 {
                let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
                let count = table.dwNumEntries as usize;
                let rows_ptr = &table.table[0] as *const MIB_TCPROW_OWNER_PID;
                let rows = std::slice::from_raw_parts(rows_ptr, count);
                for row in rows {
                    let pid = row.dwOwningPid;
                    if let Some(filter) = pids {
                        if !filter.contains(&pid) { continue; }
                    }
                    let local  = format!("{}:{}", fmt_ip(row.dwLocalAddr),  fmt_port(row.dwLocalPort));
                    let remote = if row.dwRemoteAddr != 0 {
                        format!("{}:{}", fmt_ip(row.dwRemoteAddr), fmt_port(row.dwRemotePort))
                    } else {
                        "*".to_owned()
                    };
                    result.push(NetConn {
                        pid,
                        proto:  "TCP",
                        local,
                        remote,
                        state:  tcp_state(row.dwState).to_owned(),
                    });
                }
            }
        }

        // UDP
        let mut size: u32 = 0;
        GetExtendedUdpTable(std::ptr::null_mut(), &mut size, 0, AF_INET, UDP_TABLE_OWNER_PID, 0);
        if size > 0 {
            let mut buf = vec![0u8; size as usize];
            if GetExtendedUdpTable(buf.as_mut_ptr() as *mut c_void, &mut size, 0, AF_INET, UDP_TABLE_OWNER_PID, 0) == 0 {
                let table = &*(buf.as_ptr() as *const MIB_UDPTABLE_OWNER_PID);
                let count = table.dwNumEntries as usize;
                let rows_ptr = &table.table[0] as *const MIB_UDPROW_OWNER_PID;
                let rows = std::slice::from_raw_parts(rows_ptr, count);
                for row in rows {
                    let pid = row.dwOwningPid;
                    if let Some(filter) = pids {
                        if !filter.contains(&pid) { continue; }
                    }
                    let local = format!("{}:{}", fmt_ip(row.dwLocalAddr), fmt_port(row.dwLocalPort));
                    result.push(NetConn {
                        pid,
                        proto: "UDP",
                        local,
                        remote: "*".to_owned(),
                        state:  String::new(),
                    });
                }
            }
        }
    }
    result
}

// Output formatting

fn print_header() {
    println!("{:<20} {:>6}  {:<6}  {:<25}  {:<25}  {}",
        "COMMAND", "PID", "TYPE", "LOCAL / PATH", "REMOTE", "STATE");
    println!("{}", "-".repeat(100));
}

fn print_file(row: &OpenFile, procs: &HashMap<u32, String>) {
    let cmd = procs.get(&row.pid).map(|s| s.as_str()).unwrap_or("?");
    println!("{:<20} {:>6}  {:<6}  {}",
        trunc(cmd, 20), row.pid, "FILE", row.path);
}

fn print_net(row: &NetConn, procs: &HashMap<u32, String>) {
    let cmd = procs.get(&row.pid).map(|s| s.as_str()).unwrap_or("?");
    println!("{:<20} {:>6}  {:<6}  {:<25}  {:<25}  {}",
        trunc(cmd, 20), row.pid, row.proto, row.local, row.remote, row.state);
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() }
    else { s.chars().take(n - 1).collect::<String>() + "…" }
}

// CLI arguments

struct Args {
    pids:       Vec<u32>,
    names:      Vec<String>,
    inet_only:  bool,
    files_only: bool,
    file_match: Option<String>,
}

fn parse_args() -> Args {
    let mut args = Args {
        pids:       vec![],
        names:      vec![],
        inet_only:  false,
        files_only: false,
        file_match: None,
    };
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "-p" => {
                i += 1;
                if let Some(s) = raw.get(i) {
                    if let Ok(pid) = s.parse() { args.pids.push(pid); }
                }
            }
            "-c" => {
                i += 1;
                if let Some(s) = raw.get(i) {
                    args.names.push(s.to_lowercase());
                }
            }
            "-i" => args.inet_only = true,
            "-f" => args.files_only = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            s if !s.starts_with('-') => {
                args.file_match = Some(s.to_owned());
            }
            _ => {}
        }
        i += 1;
    }
    args
}

fn print_help() {
    println!("Usage: lsof [OPTIONS] [FILE]");
    println!();
    println!("List open files and network connections.");
    println!();
    println!("Options:");
    println!("  -p <PID>    Filter by process ID (can repeat)");
    println!("  -c <NAME>   Filter by process name substring (can repeat)");
    println!("  -i          Show network connections only");
    println!("  -f          Show file handles only (skip network)");
    println!("  FILE        Show only processes with this file open");
    println!("  -h          Show this help");
    println!();
    println!("Examples:");
    println!("  lsof              List everything");
    println!("  lsof -i           Network connections only");
    println!("  lsof -p 1234      Files for PID 1234");
    println!("  lsof -c chrome    Files for processes matching 'chrome'");
    println!("  lsof C:\\pagefile.sys   Who has pagefile.sys open");
}

// Main

fn main() {
    let args = parse_args();

    let procs   = build_proc_map();
    let drives  = build_drive_map();

    // Resolve -c name filters to PIDs
    let mut pid_filter: Vec<u32> = args.pids.clone();
    if !args.names.is_empty() {
        for (&pid, name) in &procs {
            let lower = name.to_lowercase();
            if args.names.iter().any(|n| lower.contains(n.as_str())) {
                pid_filter.push(pid);
            }
        }
    }
    let pid_opt: Option<&[u32]> = if pid_filter.is_empty() { None } else { Some(&pid_filter) };

    print_header();

    // Network connections
    if !args.files_only {
        let conns = enumerate_network(pid_opt);
        let mut rows: Vec<&NetConn> = conns.iter().collect();
        rows.sort_by_key(|r| (r.pid, r.proto, r.local.clone()));
        for row in rows {
            print_net(row, &procs);
        }
    }

    // File handles
    if !args.inet_only {
        let nt = match load_nt() {
            Some(f) => f,
            None    => {
                eprintln!("lsof: failed to load ntdll");
                return;
            }
        };

        let mut files = enumerate_files(&nt, &drives, pid_opt);

        if let Some(ref pat) = args.file_match {
            let pat_lower = pat.to_lowercase();
            files.retain(|f| f.path.to_lowercase().contains(&pat_lower));
        }

        files.sort_by_key(|f| (f.pid, f.path.clone()));
        files.dedup_by_key(|f| (f.pid, f.path.clone()));

        for row in &files {
            print_file(row, &procs);
        }
    }
}
