use clap::{Arg, ArgAction, Command};
use colored::*;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

struct Counts {
    lines: u64,
    words: u64,
    bytes: u64,
    chars: u64,
}

fn count_bytes(data: &[u8]) -> Counts {
    let lines = data.iter().filter(|&&b| b == b'\n').count() as u64;
    let bytes = data.len() as u64;

    // Word count
    let mut words = 0u64;
    let mut in_word = false;
    for &b in data {
        let ws = b == b' ' || b == b'\t' || b == b'\n' || b == b'\r';
        if !ws && !in_word { words += 1; in_word = true; }
        else if ws          { in_word = false; }
    }

    let chars = String::from_utf8_lossy(data).chars().count() as u64;

    Counts { lines, words, bytes, chars }
}

fn print_row(c: &Counts, show_l: bool, show_w: bool, show_c: bool, show_m: bool, width: usize, label: &str) {
    let mut parts = String::new();
    if show_l { parts.push_str(&format!("{:>width$}", c.lines)); }
    if show_w { parts.push_str(&format!("{:>width$}", c.words)); }
    if show_c { parts.push_str(&format!("{:>width$}", c.bytes)); }
    if show_m { parts.push_str(&format!("{:>width$}", c.chars)); }

    if label.is_empty() {
        println!("{}", parts);
    } else {
        println!("{} {}", parts, label.cyan());
    }
}

fn col_width(totals: &Counts, show_l: bool, show_w: bool, show_c: bool, show_m: bool) -> usize {
    let mut max = 1u64;
    if show_l { max = max.max(totals.lines); }
    if show_w { max = max.max(totals.words); }
    if show_c { max = max.max(totals.bytes); }
    if show_m { max = max.max(totals.chars); }
    max.to_string().len().max(4) + 1
}

fn main() {
    let matches = Command::new("wc")
        .version("1.0.0")
        .about("Print line, word, and byte counts for files")
        .after_help(
            "EXAMPLES\n\
             \x20 wc file.txt            # lines  words  bytes\n\
             \x20 wc -l file.txt         # line count only\n\
             \x20 wc -w file.txt         # word count only\n\
             \x20 wc -c file.txt         # byte count only\n\
             \x20 wc -m file.txt         # character count (UTF-8 aware)\n\
             \x20 wc *.txt               # multiple files + total row\n\
             \x20 type file.txt | wc -l  # from stdin",
        )
        .arg(Arg::new("lines").short('l').long("lines").action(ArgAction::SetTrue).help("Print line count"))
        .arg(Arg::new("words").short('w').long("words").action(ArgAction::SetTrue).help("Print word count"))
        .arg(Arg::new("bytes").short('c').long("bytes").action(ArgAction::SetTrue).help("Print byte count"))
        .arg(Arg::new("chars").short('m').long("chars").action(ArgAction::SetTrue).help("Print character count (UTF-8 aware)"))
        .arg(Arg::new("files").value_name("FILE").num_args(0..).help("Files to count (default: stdin)"))
        .get_matches();

    let show_l = matches.get_flag("lines");
    let show_w = matches.get_flag("words");
    let show_c = matches.get_flag("bytes");
    let show_m = matches.get_flag("chars");

    let (show_l, show_w, show_c, show_m) = if !show_l && !show_w && !show_c && !show_m {
        (true, true, true, false)
    } else {
        (show_l, show_w, show_c, show_m)
    };

    let files: Vec<String> = matches
        .get_many::<String>("files")
        .unwrap_or_default()
        .cloned()
        .collect();

    if files.is_empty() {
        let mut data = Vec::new();
        io::stdin().read_to_end(&mut data).ok();
        let c = count_bytes(&data);
        let w = col_width(&c, show_l, show_w, show_c, show_m);
        print_row(&c, show_l, show_w, show_c, show_m, w, "");
        return;
    }

    let mut results: Vec<(&str, Counts)> = Vec::new();
    let mut total = Counts { lines: 0, words: 0, bytes: 0, chars: 0 };

    for path_str in &files {
        match File::open(Path::new(path_str)) {
            Err(e) => eprintln!("{}: {}: {}", "wc".bold(), path_str.yellow(), e),
            Ok(mut f) => {
                let mut data = Vec::new();
                f.read_to_end(&mut data).ok();
                let c = count_bytes(&data);
                total.lines += c.lines;
                total.words += c.words;
                total.bytes += c.bytes;
                total.chars += c.chars;
                results.push((path_str.as_str(), c));
            }
        }
    }

    let w = col_width(&total, show_l, show_w, show_c, show_m);
    for (name, c) in &results {
        print_row(c, show_l, show_w, show_c, show_m, w, name);
    }
    if results.len() > 1 {
        print_row(&total, show_l, show_w, show_c, show_m, w, "total");
    }
}
