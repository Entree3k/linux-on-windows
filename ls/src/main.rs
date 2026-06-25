use chrono::{DateTime, Local};
use clap::{Arg, ArgAction, Command};
use colored::*;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use terminal_size::{terminal_size, Width};

// Entry model

struct Entry {
    name:        String,
    path:        PathBuf,
    is_dir:      bool,
    is_symlink:  bool,
    is_hidden:   bool,
    is_readonly: bool,
    size:        u64,
    modified:    Option<SystemTime>,
    link_target: Option<String>,
}

impl Entry {
    fn from_path(path: &Path, name: String) -> Option<Self> {
        // symlink_metadata doesn't follow symlinks — we want the link itself.
        let sym_meta = fs::symlink_metadata(path).ok()?;
        let is_symlink = sym_meta.file_type().is_symlink();

        // For size / timestamps, follow the link.
        let meta = if is_symlink {
            fs::metadata(path).unwrap_or(sym_meta.clone())
        } else {
            sym_meta.clone()
        };

        let is_dir      = meta.is_dir();
        let is_readonly = meta.permissions().readonly();
        let modified    = meta.modified().ok();
        let size        = if is_dir { 0 } else { meta.len() };

        #[cfg(target_os = "windows")]
        let is_hidden = {
            use std::os::windows::fs::MetadataExt;
            sym_meta.file_attributes() & 0x02 != 0 // FILE_ATTRIBUTE_HIDDEN
        };
        #[cfg(not(target_os = "windows"))]
        let is_hidden = name.starts_with('.');

        let link_target = if is_symlink {
            fs::read_link(path).ok().map(|t| t.display().to_string())
        } else {
            None
        };

        Some(Entry { name, path: path.to_owned(), is_dir, is_symlink, is_hidden,
                     is_readonly, size, modified, link_target })
    }

    fn display_name(&self) -> String {
        if self.is_symlink { format!("{}@", self.name) }
        else if self.is_dir { format!("{}/", self.name) }
        else { self.name.clone() }
    }

    /// Colorized display name
    fn colored_name(&self) -> ColoredString {
        let dn = self.display_name();
        if self.is_symlink         { dn.bright_cyan() }
        else if self.is_dir        { dn.bright_blue().bold() }
        else if self.is_executable() { dn.bright_green() }
        else { color_by_ext(&dn, &self.name) }
    }

    fn is_executable(&self) -> bool {
        if self.is_dir || self.is_symlink { return false; }
        matches!(
            Path::new(&self.name).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str(),
            "exe" | "bat" | "cmd" | "ps1" | "msi" | "com"
        )
    }

    fn attr_str(&self) -> String {
        let t = if self.is_symlink { 'l' } else if self.is_dir { 'd' } else { '-' };
        let r = if self.is_readonly { 'r' } else { '-' };
        let h = if self.is_hidden  { 'h' } else { '-' };
        format!("{}{}{}", t, r, h)
    }

    fn modified_str(&self) -> String {
        self.modified
            .map(|t| {
                let dt: DateTime<Local> = t.into();
                // Show year if not current year, otherwise show time
                let now: DateTime<Local> = Local::now();
                if dt.format("%Y").to_string() == now.format("%Y").to_string() {
                    dt.format("%b %e %H:%M").to_string()
                } else {
                    dt.format("%b %e  %Y").to_string()
                }
            })
            .unwrap_or_else(|| "?".to_string())
    }
}

// Color by extension

fn color_by_ext<'a>(display: &'a str, name: &str) -> ColoredString {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        // Archives
        "zip" | "rar" | "7z" | "tar" | "gz" | "xz" | "bz2" | "zst" | "cab"
            => display.red(),
        // Images
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "svg" | "ico" | "webp" | "tiff" | "heic"
            => display.bright_magenta(),
        // Audio / video
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a"
        | "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm"
            => display.bright_yellow(),
        // Source code
        "rs" | "py" | "js" | "ts" | "c" | "cpp" | "h" | "go" | "java" | "cs" | "rb" | "php"
            => display.yellow(),
        // Config / data
        "toml" | "yaml" | "yml" | "json" | "xml" | "ini" | "cfg" | "conf" | "env"
            => display.bright_cyan(),
        // Docs
        "md" | "txt" | "pdf" | "doc" | "docx" | "rst"
            => display.white(),
        _   => display.normal(),
    }
}

// Size formatting

fn humanize(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1}G", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1}M", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1}K", b as f64 / (1u64 << 10) as f64),
        b                 => format!("{}B",  b),
    }
}

// Collect entries

fn collect(dir: &Path, show_all: bool, show_almost_all: bool) -> Vec<Entry> {
    let mut entries = Vec::new();

    if show_all {
        // Add . and ..
        if let Some(e) = Entry::from_path(dir, ".".to_string()) {
            entries.push(e);
        }
        if let Some(parent) = dir.parent() {
            if let Some(e) = Entry::from_path(parent, "..".to_string()) {
                entries.push(e);
            }
        }
    }

    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!("{}: {}: {}", "ls".bold(), dir.display().to_string().yellow(), e);
            return entries;
        }
    };

    for result in read_dir {
        let de = match result {
            Ok(d)  => d,
            Err(_) => continue,
        };
        let name = de.file_name().to_string_lossy().into_owned();
        let path = de.path();

        let entry = match Entry::from_path(&path, name) {
            Some(e) => e,
            None    => continue,
        };

        // Skip hidden unless -a / -A
        if entry.is_hidden && !show_all && !show_almost_all {
            continue;
        }

        entries.push(entry);
    }

    entries
}

fn sort_entries(entries: &mut Vec<Entry>, by_time: bool, by_size: bool, reverse: bool) {
    if by_time {
        entries.sort_by(|a, b| {
            b.modified.cmp(&a.modified) // newest first by default
        });
    } else if by_size {
        entries.sort_by(|a, b| b.size.cmp(&a.size)); // largest first
    } else {
        // Alphabetical, case-insensitive, dirs first
        entries.sort_by(|a, b| {
            match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
        });
    }
    if reverse {
        entries.reverse();
    }
}

// Display: grid

fn print_grid(entries: &[Entry], use_color: bool) {
    if entries.is_empty() { return; }

    let term_width = terminal_size()
        .map(|(Width(w), _)| w as usize)
        .unwrap_or(80);

    // Column width based on longest display name + 2 spaces padding
    let col_w = entries
        .iter()
        .map(|e| e.display_name().len())
        .max()
        .unwrap_or(1)
        + 2;

    let cols = (term_width / col_w).max(1);
    let rows = (entries.len() + cols - 1) / cols;

    for row in 0..rows {
        for col in 0..cols {
            let idx = col * rows + row; // column-major fill (like GNU ls)
            if idx >= entries.len() { break; }
            let e = &entries[idx];
            let name = if use_color {
                format!("{}", e.colored_name())
            } else {
                e.display_name()
            };
            // Pad to col_w using the *display* width (without ANSI codes)
            let display_len = e.display_name().len();
            let pad = col_w.saturating_sub(display_len);
            if col + 1 < cols && idx + rows < entries.len() {
                print!("{}{}", name, " ".repeat(pad));
            } else {
                print!("{}", name);
            }
        }
        println!();
    }
}

// Display: long format

fn print_long(entries: &[Entry], human: bool, use_color: bool) {
    if entries.is_empty() { return; }

    // Column widths: right-align sizes
    let max_size_w = entries
        .iter()
        .map(|e| {
            if e.is_dir { 5 } // "<DIR>"
            else if human { humanize(e.size).len() }
            else { e.size.to_string().len() }
        })
        .max()
        .unwrap_or(4);

    // Total size of all files
    let total_bytes: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
    println!("total {}", if human { humanize(total_bytes) } else { format!("{}B", total_bytes) });

    for e in entries {
        let attrs = e.attr_str();

        let size_str = if e.is_dir {
            format!("{:>width$}", "<DIR>", width = max_size_w)
        } else if human {
            format!("{:>width$}", humanize(e.size), width = max_size_w)
        } else {
            format!("{:>width$}", e.size, width = max_size_w)
        };

        let date = e.modified_str();

        let name = if use_color {
            format!("{}", e.colored_name())
        } else {
            e.display_name()
        };

        let link = e.link_target.as_deref()
            .map(|t| format!(" -> {}", t))
            .unwrap_or_default();

        println!(
            "{}  {}  {}  {}{}",
            attrs.dimmed(),
            size_str.cyan(),
            date.dimmed(),
            name,
            link.dimmed(),
        );
    }
}

// Display: one per line

fn print_one(entries: &[Entry], use_color: bool) {
    for e in entries {
        if use_color {
            println!("{}", e.colored_name());
        } else {
            println!("{}", e.display_name());
        }
    }
}

// Recursive helper

fn list_dir(
    dir: &Path,
    opts: &ListOpts,
    use_color: bool,
    print_header: bool,
    first: bool,
) {
    if print_header {
        if !first { println!(); }
        println!("{}:", dir.display().to_string().cyan().bold());
    }

    let mut entries = collect(dir, opts.all, opts.almost_all);
    sort_entries(&mut entries, opts.by_time, opts.by_size, opts.reverse);

    if opts.long {
        print_long(&entries, opts.human, use_color);
    } else if opts.one {
        print_one(&entries, use_color);
    } else {
        print_grid(&entries, use_color);
    }

    if opts.recursive {
        for e in &entries {
            if e.is_dir && e.name != "." && e.name != ".." {
                list_dir(&e.path, opts, use_color, true, false);
            }
        }
    }
}

struct ListOpts {
    long:        bool,
    all:         bool,
    almost_all:  bool,
    human:       bool,
    recursive:   bool,
    by_time:     bool,
    by_size:     bool,
    reverse:     bool,
    one:         bool,
}

// Main

fn main() {
    let matches = Command::new("ls")
        .version("1.0.0")
        .about("List directory contents")
        .after_help(
            "COLORS\n\
             \x20 bright blue   directories\n\
             \x20 bright green  executables  (.exe .bat .cmd .ps1)\n\
             \x20 bright cyan   symlinks\n\
             \x20 red           archives     (.zip .rar .7z .tar .gz …)\n\
             \x20 magenta       images       (.jpg .png .gif …)\n\
             \x20 yellow        audio/video  (.mp3 .mp4 .mkv …)\n\
             \x20 yellow        source code  (.rs .py .js …)\n\
             \x20 cyan          config/data  (.toml .json .yaml …)\n\
             \n\
             ATTRIBUTE STRING  (first column of -l)\n\
             \x20 d/-/l   directory / file / symlink\n\
             \x20 r/-     readonly\n\
             \x20 h/-     hidden\n\
             \n\
             EXAMPLES\n\
             \x20 ls                      # list current directory\n\
             \x20 ls C:\\Windows\\System32  # list a specific directory\n\
             \x20 ls -la                  # long format, show hidden files\n\
             \x20 ls -lh                  # long format, human-readable sizes\n\
             \x20 ls -lt                  # long format, sorted by time\n\
             \x20 ls -lS                  # long format, sorted by size\n\
             \x20 ls -R src/              # recursive listing\n\
             \x20 ls -1                   # one file per line",
        )
        .arg(Arg::new("long").short('l').action(ArgAction::SetTrue).help("Long format: show attrs, size, date"))
        .arg(Arg::new("all").short('a').long("all").action(ArgAction::SetTrue).help("Show hidden files including . and .."))
        .arg(Arg::new("almost_all").short('A').long("almost-all").action(ArgAction::SetTrue).help("Show hidden files, but not . and .."))
        .arg(Arg::new("human").short('h').long("human-readable").action(ArgAction::SetTrue).help("Human-readable sizes (1.2K, 3.4M) — use with -l"))
        .arg(Arg::new("recursive").short('R').long("recursive").action(ArgAction::SetTrue).help("List subdirectories recursively"))
        .arg(Arg::new("by_time").short('t').action(ArgAction::SetTrue).help("Sort by modification time, newest first"))
        .arg(Arg::new("by_size").short('S').action(ArgAction::SetTrue).help("Sort by file size, largest first"))
        .arg(Arg::new("reverse").short('r').long("reverse").action(ArgAction::SetTrue).help("Reverse sort order"))
        .arg(Arg::new("one").short('1').action(ArgAction::SetTrue).help("One file per line"))
        .arg(Arg::new("no_color").long("no-color").action(ArgAction::SetTrue).help("Disable colors"))
        .arg(Arg::new("paths").value_name("PATH").num_args(0..).help("Directories or files to list (default: current directory)"))
        .get_matches();

    let opts = ListOpts {
        long:       matches.get_flag("long"),
        all:        matches.get_flag("all"),
        almost_all: matches.get_flag("almost_all"),
        human:      matches.get_flag("human"),
        recursive:  matches.get_flag("recursive"),
        by_time:    matches.get_flag("by_time"),
        by_size:    matches.get_flag("by_size"),
        reverse:    matches.get_flag("reverse"),
        one:        matches.get_flag("one"),
    };

    let use_color = !matches.get_flag("no_color") && io::stdout().is_terminal();

    let paths: Vec<PathBuf> = matches
        .get_many::<String>("paths")
        .map(|vals| vals.map(PathBuf::from).collect())
        .unwrap_or_else(|| vec![PathBuf::from(".")]);

    // Separate file args from directory args
    let mut files: Vec<PathBuf> = Vec::new();
    let mut dirs:  Vec<PathBuf> = Vec::new();

    for p in &paths {
        if !p.exists() {
            eprintln!("{}: {}: No such file or directory", "ls".bold(), p.display().to_string().yellow());
            continue;
        }
        if p.is_file() || p.is_symlink() {
            files.push(p.clone());
        } else {
            dirs.push(p.clone());
        }
    }

    // Files listed individually
    if !files.is_empty() {
        let mut entries: Vec<Entry> = files
            .iter()
            .filter_map(|p| {
                let name = p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string());
                Entry::from_path(p, name)
            })
            .collect();
        sort_entries(&mut entries, opts.by_time, opts.by_size, opts.reverse);

        if opts.long {
            print_long(&entries, opts.human, use_color);
        } else if opts.one {
            print_one(&entries, use_color);
        } else {
            print_grid(&entries, use_color);
        }
    }

    // Directories
    let print_header = dirs.len() > 1 || (dirs.len() == 1 && !files.is_empty());
    for (i, dir) in dirs.iter().enumerate() {
        list_dir(dir, &opts, use_color, print_header, i == 0 && files.is_empty());
    }
}
