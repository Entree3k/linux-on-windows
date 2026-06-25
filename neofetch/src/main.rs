use colored::*;
use std::env;
use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};

// Windows ASCII art

const WIN10_ART: &[&str] = &[
    r"                                ..,",
    r"                    ....,,:;+ccllll",
    r"      ...,,+:;  cllllllllllllllllll",
    r",cclllllllllll  lllllllllllllllllll",
    r"llllllllllllll  lllllllllllllllllll",
    r"llllllllllllll  lllllllllllllllllll",
    r"llllllllllllll  lllllllllllllllllll",
    r"llllllllllllll  lllllllllllllllllll",
    r"llllllllllllll  lllllllllllllllllll",
    r"                                   ",
    r"llllllllllllll  lllllllllllllllllll",
    r"llllllllllllll  lllllllllllllllllll",
    r"llllllllllllll  lllllllllllllllllll",
    r"llllllllllllll  lllllllllllllllllll",
    r"llllllllllllll  lllllllllllllllllll",
    r"llllllllllllll  lllllllllllllllllll",
    r"`'ccllllllllll  lllllllllllllllllll",
    r"      `' \*::  :ccllllllllllllllll",
    r"                       ````''*::cll",
    r"                                 ``",
];

const WIN11_ART: &[&str] = &[
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"                                   ",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"llllllllllllllll   llllllllllllllll",
    r"                                   ",
];

// System information

struct SysInfo {
    username:   String,
    hostname:   String,
    os:         String,
    build:      String,   // e.g. "23H2"
    build_num:  u32,      // e.g. 26200 used to pick art
    uptime_sec: u64,
    resolution: String,
    terminal:   String,
    cpu:        String,
    gpu:        String,
    mem_used:   u64,      // MiB
    mem_total:  u64,      // MiB
    mem_pct:    u32,
    disk_total: f64,      // GiB
    disk_free:  f64,      // GiB
}

// Info collection

fn collect_info() -> SysInfo {
    let username = env::var("USERNAME").unwrap_or_else(|_| "user".into());
    let hostname = env::var("COMPUTERNAME").unwrap_or_else(|_| "host".into());

    let (os, build, build_num) = get_windows_version();
    let uptime_sec            = get_uptime();
    let resolution            = get_resolution();
    let terminal              = detect_terminal();
    let cpu                   = get_cpu();
    let gpu                   = get_gpu();
    let (mem_used, mem_total, mem_pct) = get_memory();
    let (disk_total, disk_free)        = get_disk("C:\\");

    SysInfo { username, hostname, os, build, build_num,
              uptime_sec, resolution, terminal, cpu, gpu,
              mem_used, mem_total, mem_pct, disk_total, disk_free }
}

fn get_windows_version() -> (String, String, u32) {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    let (product, display, build_s) =
        if let Ok(key) = hklm.open_subkey("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion") {
            (
                key.get_value::<String, _>("ProductName").unwrap_or_else(|_| "Windows".into()),
                key.get_value::<String, _>("DisplayVersion").unwrap_or_else(|_| "".into()),
                key.get_value::<String, _>("CurrentBuildNumber").unwrap_or_else(|_| "0".into()),
            )
        } else {
            ("Windows".into(), "".into(), "0".into())
        };

    let build_n: u32 = build_s.parse().unwrap_or(0);

    let os = if build_n >= 22000 {
        "Windows 11".to_string()
    } else if product.contains("10") || build_n >= 10000 {
        "Windows 10".to_string()
    } else {
        product
    };

    (os, display, build_n)
}

fn get_cpu() -> String {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    hklm.open_subkey("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0")
        .and_then(|k| k.get_value::<String, _>("ProcessorNameString"))
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| "Unknown CPU".into())
}

fn get_gpu() -> String {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    if let Ok(k) = hklm.open_subkey("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\WinSAT") {
        if let Ok(v) = k.get_value::<String, _>("PrimaryAdapterString") {
            if !v.trim().is_empty() { return v.trim().to_owned(); }
        }
    }

    let class = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    if let Ok(class_key) = hklm.open_subkey(class) {
        for i in 0..8u32 {
            let sub = format!("{:04}", i);
            if let Ok(sk) = class_key.open_subkey(&sub) {
                if let Ok(name) = sk.get_value::<String, _>("DriverDesc") {
                    let name = name.trim().to_owned();
                    if !name.is_empty() { return name; }
                }
            }
        }
    }

    "Unknown GPU".into()
}

#[cfg(windows)]
fn get_memory() -> (u64, u64, u32) {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    unsafe {
        let mut m = MEMORYSTATUSEX {
            dwLength:                std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            dwMemoryLoad:            0,
            ullTotalPhys:            0,
            ullAvailPhys:            0,
            ullTotalPageFile:        0,
            ullAvailPageFile:        0,
            ullTotalVirtual:         0,
            ullAvailVirtual:         0,
            ullAvailExtendedVirtual: 0,
        };
        GlobalMemoryStatusEx(&mut m);
        let total = m.ullTotalPhys / 1_048_576;
        let avail = m.ullAvailPhys / 1_048_576;
        let used  = total.saturating_sub(avail);
        (used, total, m.dwMemoryLoad)
    }
}

#[cfg(not(windows))]
fn get_memory() -> (u64, u64, u32) { (0, 0, 0) }

#[cfg(windows)]
fn get_disk(path: &str) -> (f64, f64) {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut free_caller: u64 = 0;
    let mut total:       u64 = 0;
    let mut free_total:  u64 = 0;
    unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut free_caller, &mut total, &mut free_total);
    }
    let total_gb = total       as f64 / 1_073_741_824.0;
    let free_gb  = free_total  as f64 / 1_073_741_824.0;
    (total_gb, free_gb)
}

#[cfg(not(windows))]
fn get_disk(_path: &str) -> (f64, f64) { (0.0, 0.0) }

#[cfg(windows)]
fn get_uptime() -> u64 {
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    unsafe { GetTickCount64() / 1000 }
}

#[cfg(not(windows))]
fn get_uptime() -> u64 { 0 }

#[cfg(windows)]
fn get_resolution() -> String {
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS,
    };
    unsafe {
        let mut dm: DEVMODEW = std::mem::zeroed();
        dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        if EnumDisplaySettingsW(std::ptr::null(), ENUM_CURRENT_SETTINGS, &mut dm) != 0 {
            let w  = dm.dmPelsWidth;
            let h  = dm.dmPelsHeight;
            let hz = dm.dmDisplayFrequency;
            return format!("{}x{} @{}Hz", w, h, hz);
        }
    }
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe {
        format!("{}x{}", GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
    }
}

#[cfg(not(windows))]
fn get_resolution() -> String { "Unknown".into() }

fn detect_terminal() -> String {
    if env::var("WT_SESSION").is_ok()     { return "Windows Terminal".into(); }
    if env::var("ConEmuPID").is_ok()      { return "ConEmu".into(); }
    if env::var("VSCODE_PID").is_ok() || env::var("VSCODE_CWD").is_ok() {
        return "VS Code".into();
    }
    if env::var("HYPER_BIN_FOLDER").is_ok() { return "Hyper".into(); }
    if let Ok(tp) = env::var("TERM_PROGRAM") {
        if !tp.is_empty() { return tp; }
    }
    "Windows Console".into()
}

// Rendering

fn fmt_uptime(secs: u64) -> String {
    let days  = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let mins  = (secs % 3600)   / 60;
    let d = if days  == 1 { "day"    } else { "days"    };
    let h = if hours == 1 { "hour"   } else { "hours"   };
    let m = if mins  == 1 { "minute" } else { "minutes" };
    match (days, hours, mins) {
        (0, 0, _) => format!("{} {}", mins, m),
        (0, _, _) => format!("{} {}, {} {}", hours, h, mins, m),
        _         => format!("{} {}, {} {}, {} {}", days, d, hours, h, mins, m),
    }
}

fn draw_bar(pct: u32) -> String {
    let filled = (pct as usize * 20 / 100).min(20);
    let empty  = 20 - filled;
    let f: String = "/".repeat(filled);
    let e: String = "/".repeat(empty);
    format!("-=[ {}{} ]=-", f.bright_blue(), e.dimmed())
}

fn color_row(bright: bool) -> String {
    let colors: &[(&str, fn(ColoredString) -> ColoredString)] = &[
        ("   ", |s| s.on_black()),
        ("   ", |s| s.on_red()),
        ("   ", |s| s.on_green()),
        ("   ", |s| s.on_yellow()),
        ("   ", |s| s.on_blue()),
        ("   ", |s| s.on_magenta()),
        ("   ", |s| s.on_cyan()),
        ("   ", |s| s.on_white()),
    ];
    let bright_colors: &[(&str, fn(ColoredString) -> ColoredString)] = &[
        ("   ", |s| s.on_bright_black()),
        ("   ", |s| s.on_bright_red()),
        ("   ", |s| s.on_bright_green()),
        ("   ", |s| s.on_bright_yellow()),
        ("   ", |s| s.on_bright_blue()),
        ("   ", |s| s.on_bright_magenta()),
        ("   ", |s| s.on_bright_cyan()),
        ("   ", |s| s.on_bright_white()),
    ];
    let list = if bright { bright_colors } else { colors };
    list.iter().map(|(s, f)| f(s.normal()).to_string()).collect()
}

fn print_neofetch(info: &SysInfo) {
    colored::control::set_override(true);

    let art = if info.build_num >= 22000 { WIN11_ART } else { WIN10_ART };

    let lbl = |s: &str| s.bright_cyan().bold().to_string();

    let user_at_host = format!("{}@{}", info.username.bright_white().bold(), info.hostname.bright_white().bold());
    let divider      = "─".repeat(info.username.len() + 1 + info.hostname.len());

    let disk_pct = if info.disk_total > 0.0 {
        ((1.0 - info.disk_free / info.disk_total) * 100.0) as u32
    } else { 0 };

    let info_lines: Vec<String> = vec![
        user_at_host,
        divider.bright_black().to_string(),
        format!("{} {}", lbl("         OS:"), info.os),
        format!("{} {} ({})", lbl("      Build:"), info.build, info.build_num),
        format!("{} {}", lbl("     Uptime:"), fmt_uptime(info.uptime_sec)),
        format!("{} {}", lbl(" Resolution:"), info.resolution),
        format!("{} {}", lbl("   Terminal:"), info.terminal),
        format!("{} {}", lbl("        CPU:"), info.cpu),
        format!("{} {}", lbl("        GPU:"), info.gpu),
        format!("{} {} MiB / {} MiB ({}% in use)",
            lbl("     Memory:"), info.mem_used, info.mem_total, info.mem_pct),
        format!("{} C:\\ {:.2} GB ({:.2} GB free)",
            lbl("       Disk:"), info.disk_total, info.disk_free),
        String::new(),
        format!("{} {}", lbl("      Mem%:"), draw_bar(info.mem_pct)),
        String::new(),
        format!("{} {}", lbl("     Disk%:"), draw_bar(disk_pct)),
        String::new(),
        color_row(false),
        color_row(true),
        String::new(),
        String::new(),
    ];

    let art_width = art.iter().map(|l| l.len()).max().unwrap_or(35);
    let n = art.len().max(info_lines.len());

    for i in 0..n {
        let art_line  = art.get(i).copied().unwrap_or("");
        let info_line = info_lines.get(i).map(|s| s.as_str()).unwrap_or("");

        let colored_art = if art_line.trim().is_empty() {
            format!("{:<width$}", "", width = art_width)
        } else {
            format!("{:<width$}", art_line, width = art_width)
                .bright_blue()
                .to_string()
        };

        println!("{}   {}", colored_art, info_line);
    }
}

// Entry point

fn main() {
    if env::args().any(|a| a == "--help" || a == "-h") {
        println!("Usage: neofetch");
        println!("  Displays system information alongside a Windows logo.");
        return;
    }

    let info = collect_info();
    print_neofetch(&info);
}
