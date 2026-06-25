//! file — determine file type by magic bytes.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use colored::Colorize;

fn detect(path: &Path) -> String {
    // Directories
    if path.is_dir() { return "directory".into(); }
    if path.is_symlink() { return format!("symbolic link to {}", std::fs::read_link(path).map(|p| p.display().to_string()).unwrap_or_default()); }

    let mut buf = [0u8; 512];
    let mut f = match File::open(path) {
        Ok(f)  => f,
        Err(e) => return format!("cannot open: {}", e),
    };
    let n = f.read(&mut buf).unwrap_or(0);
    let b = &buf[..n];

    // Magic byte signatures

    // Executables
    if b.starts_with(b"MZ")         { return pe_description(b); }
    if b.starts_with(b"\x7FELF")    { return "ELF executable".into(); }

    // Archives / compressed
    if b.starts_with(b"\x1F\x8B")   { return "gzip compressed data".into(); }
    if b.starts_with(b"BZh")        { return "bzip2 compressed data".into(); }
    if b.starts_with(b"PK\x03\x04") { return zip_description(path, b); }
    if b.starts_with(b"PK\x05\x06") { return "Zip archive (empty)".into(); }
    if b.starts_with(b"7z\xBC\xAF\x27\x1C") { return "7-zip archive".into(); }
    if b.starts_with(b"Rar!\x1A\x07") { return "RAR archive".into(); }
    if b.starts_with(b"\xFD7zXZ\x00") { return "XZ compressed data".into(); }
    if b.starts_with(b"\x1F\x9D")   { return "compress'd data".into(); }
    if n >= 257 && &b[257..262] == b"ustar" { return "POSIX tar archive".into(); }

    // Images
    if b.starts_with(b"\x89PNG\r\n\x1A\n") { return "PNG image".into(); }
    if b.starts_with(b"\xFF\xD8\xFF") { return "JPEG image".into(); }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") { return "GIF image".into(); }
    if b.starts_with(b"BM")         { return "BMP image".into(); }
    if b.starts_with(b"II\x2A\x00") || b.starts_with(b"MM\x00\x2A") { return "TIFF image".into(); }
    if b.starts_with(b"RIFF") && n >= 12 && &b[8..12] == b"WEBP" { return "WebP image".into(); }
    if b.starts_with(b"\x00\x00\x01\x00") { return "Windows ICO icon".into(); }
    if b.starts_with(b"\x00\x00\x02\x00") { return "Windows CUR cursor".into(); }

    // Documents
    if b.starts_with(b"%PDF")       { return format!("PDF document, version {}", pdf_version(b)); }
    if b.starts_with(b"\xD0\xCF\x11\xE0") { return "Microsoft Office document (OLE2)".into(); }
    if b.starts_with(b"<?xml") || b.starts_with(b"<xml") { return "XML document".into(); }
    if b.starts_with(b"<!DOCTYPE") || b.starts_with(b"<html") || b.starts_with(b"<HTML") { return "HTML document".into(); }

    // Audio / video
    if b.starts_with(b"ID3")        { return "MP3 audio (ID3 tag)".into(); }
    if b.starts_with(b"\xFF\xFB") || b.starts_with(b"\xFF\xF3") || b.starts_with(b"\xFF\xF2") { return "MP3 audio".into(); }
    if b.starts_with(b"fLaC")       { return "FLAC audio".into(); }
    if b.starts_with(b"OggS")       { return "Ogg data".into(); }
    if b.starts_with(b"RIFF") && n >= 12 && &b[8..12] == b"WAVE" { return "WAV audio".into(); }
    if b.starts_with(b"RIFF") && n >= 12 && &b[8..12] == b"AVI " { return "AVI video".into(); }
    if n >= 8 && is_mp4(b)          { return "MP4/MOV video".into(); }
    if b.starts_with(b"\x1A\x45\xDF\xA3") { return "WebM/MKV video".into(); }

    // Text / scripts
    if b.starts_with(b"\xEF\xBB\xBF") { return "UTF-8 text (with BOM)".into(); }
    if b.starts_with(b"\xFF\xFE")   { return "UTF-16 LE text".into(); }
    if b.starts_with(b"\xFE\xFF")   { return "UTF-16 BE text".into(); }

    if is_text(b) {
        return text_description(path, b);
    }

    // Fallback: extension hint + "data"
    match path.extension().and_then(|e| e.to_str()).map(|s| s.to_lowercase()).as_deref() {
        Some("dll")  => "PE32 DLL (dynamic library)".into(),
        Some("sys")  => "Windows kernel driver".into(),
        Some("db") | Some("sqlite") => "SQLite database (or similar)".into(),
        _ => format!("data ({} bytes)", n),
    }
}

fn pe_description(b: &[u8]) -> String {
    // Check PE offset at bytes 0x3C
    if b.len() > 0x40 {
        let pe_off = u32::from_le_bytes([b[0x3C], b[0x3D], b[0x3E], b[0x3F]]) as usize;
        if pe_off + 6 < b.len() && &b[pe_off..pe_off+4] == b"PE\0\0" {
            let machine = u16::from_le_bytes([b[pe_off+4], b[pe_off+5]]);
            let arch = match machine {
                0x8664 => "x86-64",
                0x014C => "x86",
                0xAA64 => "ARM64",
                _      => "unknown arch",
            };
            // Check if it's a DLL (characteristics bit 13)
            let chars_off = pe_off + 22;
            if chars_off + 2 <= b.len() {
                let chars = u16::from_le_bytes([b[chars_off], b[chars_off+1]]);
                if chars & 0x2000 != 0 {
                    return format!("PE32 DLL ({})", arch);
                }
            }
            return format!("PE32+ executable ({})", arch);
        }
    }
    "MS-DOS executable".into()
}

fn zip_description(path: &Path, _b: &[u8]) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "docx" => "Microsoft Word document (OOXML)".into(),
        "xlsx" => "Microsoft Excel spreadsheet (OOXML)".into(),
        "pptx" => "Microsoft PowerPoint presentation (OOXML)".into(),
        "jar"  => "Java JAR archive".into(),
        "apk"  => "Android APK package".into(),
        "epub" => "EPUB ebook".into(),
        _      => "Zip archive".into(),
    }
}

fn pdf_version(b: &[u8]) -> String {
    // %PDF-1.7 → "1.7"
    if b.len() > 8 { String::from_utf8_lossy(&b[5..8]).trim().to_owned() } else { "?".into() }
}

fn is_mp4(b: &[u8]) -> bool {
    if b.len() < 12 { return false; }
    let brands: &[&[u8]] = &[b"ftyp", b"moov", b"mdat"];
    brands.iter().any(|br| &b[4..8] == *br)
}

fn is_text(b: &[u8]) -> bool {
    if b.is_empty() { return true; }
    // Text if <10% non-printable non-whitespace bytes
    let non_text = b.iter().filter(|&&c| c < 0x09 || (c > 0x0D && c < 0x20 && c != 0x1B) || c == 0x7F).count();
    non_text * 10 < b.len()
}

fn text_description(path: &Path, b: &[u8]) -> String {
    // Check shebang
    if b.starts_with(b"#!/") || b.starts_with(b"#! /") {
        let line: String = b.iter().take(80).take_while(|&&c| c != b'\n').map(|&c| c as char).collect();
        return format!("script, {}", line);
    }

    // Guess from extension
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "rs"   => "Rust source code".into(),
        "py"   => "Python script".into(),
        "js"   => "JavaScript source".into(),
        "ts"   => "TypeScript source".into(),
        "json" => "JSON data".into(),
        "toml" => "TOML configuration".into(),
        "yaml" | "yml" => "YAML data".into(),
        "xml"  => "XML document".into(),
        "html" | "htm" => "HTML document".into(),
        "css"  => "CSS stylesheet".into(),
        "c"    => "C source code".into(),
        "cpp" | "cxx" | "cc" => "C++ source code".into(),
        "h" | "hpp" => "C/C++ header".into(),
        "java" => "Java source code".into(),
        "go"   => "Go source code".into(),
        "sh" | "bash" => "shell script".into(),
        "ps1"  => "PowerShell script".into(),
        "bat" | "cmd" => "DOS batch file".into(),
        "md"   => "Markdown document".into(),
        "csv"  => "CSV data".into(),
        "log"  => "log file".into(),
        "ini" | "cfg" | "conf" => "configuration file".into(),
        "sql"  => "SQL script".into(),
        _      => "ASCII text".into(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!("Usage: file [file...]");
        eprintln!("Determine the type of each file.");
        std::process::exit(0);
    }

    for path_str in &args {
        let path = Path::new(path_str);
        if !path.exists() {
            println!("{}: {}: No such file or directory", path_str, path_str);
            continue;
        }
        let kind = detect(path);
        println!("{}: {}", path_str.cyan(), kind);
    }
}
