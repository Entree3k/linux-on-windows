//! stat — display detailed file or file system status.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use colored::Colorize;
use windows_sys::Win32::Storage::FileSystem::{
    GetFileAttributesW,
    FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_DEVICE,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_TEMPORARY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_ENCRYPTED,
};

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn fmt_time(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs();
    // Basic UTC breakdown (no external crate)
    let mut s = secs;
    let sec = s % 60; s /= 60;
    let min = s % 60; s /= 60;
    let hour = s % 24; s /= 24;
    // Gregorian calendar approximation
    let mut year = 1970u64;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let days = if leap { 366 } else { 365 };
        if s < days { break; }
        s -= days;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = [31u64, if leap {29} else {28}, 31,30,31,30,31,31,30,31,30,31];
    let mut month = 0usize;
    for &d in &month_days {
        if s < d { break; }
        s -= d;
        month += 1;
    }
    let day = s + 1;
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC", year, month+1, day, hour, min, sec)
}

fn attr_string(attrs: u32) -> String {
    let mut parts = Vec::new();
    if attrs & FILE_ATTRIBUTE_READONLY    != 0 { parts.push("ReadOnly"); }
    if attrs & FILE_ATTRIBUTE_HIDDEN      != 0 { parts.push("Hidden"); }
    if attrs & FILE_ATTRIBUTE_SYSTEM      != 0 { parts.push("System"); }
    if attrs & FILE_ATTRIBUTE_DIRECTORY   != 0 { parts.push("Directory"); }
    if attrs & FILE_ATTRIBUTE_ARCHIVE     != 0 { parts.push("Archive"); }
    if attrs & FILE_ATTRIBUTE_DEVICE      != 0 { parts.push("Device"); }
    if attrs & FILE_ATTRIBUTE_NORMAL      != 0 { parts.push("Normal"); }
    if attrs & FILE_ATTRIBUTE_TEMPORARY   != 0 { parts.push("Temporary"); }
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 { parts.push("ReparsePoint"); }
    if attrs & FILE_ATTRIBUTE_COMPRESSED  != 0 { parts.push("Compressed"); }
    if attrs & FILE_ATTRIBUTE_ENCRYPTED   != 0 { parts.push("Encrypted"); }
    if parts.is_empty() { parts.push("Normal"); }
    format!("{} (0x{:04X})", parts.join(", "), attrs)
}

fn stat_path(path: &Path) {
    let meta = match fs::metadata(path) {
        Ok(m)  => m,
        Err(e) => { eprintln!("{}: {}: {}", "stat".red(), path.display(), e); return; }
    };

    let file_type = if meta.is_dir() {
        "directory"
    } else if meta.is_symlink() {
        "symbolic link"
    } else {
        "regular file"
    };

    let size    = meta.len();
    let blocks  = (size + 511) / 512;

    let accessed = meta.accessed().ok();
    let modified = meta.modified().ok();
    let created  = meta.created().ok();

    // Get Win32 attributes
    let wide = to_wide(&path.to_string_lossy());
    let attrs = unsafe { GetFileAttributesW(wide.as_ptr()) };
    let attr_str = if attrs == u32::MAX { "unknown".into() } else { attr_string(attrs) };

    println!("  {}: {}", "File".bold(), path.display().to_string().cyan());
    println!("  {}: {:<14} {}: {:<6} {}: {}",
        "Size".bold(),  size,
        "Blocks".bold(), blocks,
        "Type".bold(),  file_type);
    println!("{}: {}",
        "Attributes".bold(), attr_str.yellow());

    if let Some(t) = accessed { println!("{}: {}", "Access".bold(), fmt_time(t).green()); }
    if let Some(t) = modified { println!("{}: {}", "Modify".bold(), fmt_time(t).green()); }
    if let Some(t) = created  { println!("{}: {}", "Create".bold(), fmt_time(t).green()); }

    // For symlinks, show target
    if meta.is_symlink() {
        if let Ok(target) = fs::read_link(path) {
            println!("{}: {}", "Target".bold(), target.display().to_string().cyan());
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!("Usage: stat [FILE]...");
        eprintln!("Display file or file system status.");
        std::process::exit(0);
    }

    let mut first = true;
    for path_str in &args {
        let path = Path::new(path_str);
        if !first { println!(); }
        first = false;
        if !path.exists() {
            eprintln!("{}: {}: No such file or directory", "stat".red(), path_str);
            continue;
        }
        stat_path(path);
    }
}
