use std::path::{Path, PathBuf};
use std::fs;
use colored::Colorize;
use similar::{ChangeTag, TextDiff};

struct Args {
    context_lines: usize,
    ignore_case:   bool,
    ignore_all_ws: bool,
    ignore_ws:     bool,
    brief:         bool,
    recursive:     bool,
    files:         Vec<PathBuf>,
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|a| a == "--help") {
        eprintln!("Usage: diff [OPTION]... FILE1 FILE2");
        eprintln!("Compare files line by line.");
        eprintln!("  -u N           unified diff with N context lines (default 3)");
        eprintln!("  -i, --ignore-case          ignore case differences");
        eprintln!("  -w, --ignore-all-space     ignore all whitespace");
        eprintln!("  -b, --ignore-space-change  ignore changes in whitespace");
        eprintln!("  -q, --brief                report only whether files differ");
        eprintln!("  -r, --recursive            compare directories recursively");
        eprintln!("Exit: 0=same  1=differ  2=error");
        std::process::exit(0);
    }

    let mut a = Args {
        context_lines: 3, ignore_case: false, ignore_all_ws: false,
        ignore_ws: false, brief: false, recursive: false, files: Vec::new(),
    };

    let mut i = 0;
    while i < raw.len() {
        let s = raw[i].as_str();
        match s {
            "-u" | "--unified" => {
                i += 1;
                if let Some(n) = raw.get(i).and_then(|v| v.parse().ok()) {
                    a.context_lines = n;
                }
            }
            "-i" | "--ignore-case"        => a.ignore_case   = true,
            "-w" | "--ignore-all-space"   => a.ignore_all_ws = true,
            "-b" | "--ignore-space-change"=> a.ignore_ws     = true,
            "-q" | "--brief"              => a.brief         = true,
            "-r" | "--recursive"          => a.recursive     = true,
            _ if s.starts_with("-u") && s.len() > 2 => {
                a.context_lines = s[2..].parse().unwrap_or(3);
            }
            _ if s.starts_with('-') => {}
            _ => a.files.push(PathBuf::from(s)),
        }
        i += 1;
    }
    a
}

fn normalize(line: &str, ignore_case: bool, ignore_all_ws: bool, ignore_ws: bool) -> String {
    let s = if ignore_all_ws {
        line.chars().filter(|c| !c.is_whitespace()).collect::<String>()
    } else if ignore_ws {
        let mut out = String::new();
        let mut prev_ws = false;
        for c in line.trim_end().chars() {
            if c.is_whitespace() {
                if !prev_ws && !out.is_empty() { out.push(' '); }
                prev_ws = true;
            } else {
                out.push(c);
                prev_ws = false;
            }
        }
        out
    } else {
        line.to_string()
    };
    if ignore_case { s.to_lowercase() } else { s }
}

fn diff_files(old: &str, new: &str, old_name: &str, new_name: &str, args: &Args) -> bool {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let (cmp_old, cmp_new) = if args.ignore_case || args.ignore_all_ws || args.ignore_ws {
        let no: Vec<String> = old_lines.iter().map(|l| normalize(l, args.ignore_case, args.ignore_all_ws, args.ignore_ws)).collect();
        let nn: Vec<String> = new_lines.iter().map(|l| normalize(l, args.ignore_case, args.ignore_all_ws, args.ignore_ws)).collect();
        (no.join("\n") + if old.ends_with('\n') || old.is_empty() { "\n" } else { "" },
         nn.join("\n") + if new.ends_with('\n') || new.is_empty() { "\n" } else { "" })
    } else {
        (old.to_string(), new.to_string())
    };

    let diff = TextDiff::from_lines(&cmp_old, &cmp_new);
    let groups = diff.grouped_ops(args.context_lines);

    if groups.is_empty() { return false; }
    if args.brief { return true; }

    println!("{}", format!("--- {}", old_name).cyan());
    println!("{}", format!("+++ {}", new_name).cyan());

    for group in &groups {
        let first = &group[0];
        let last  = &group[group.len() - 1];
        let os = first.old_range().start;
        let oe = last.old_range().end;
        let ns = first.new_range().start;
        let ne = last.new_range().end;
        println!("{}", format!("@@ -{},{} +{},{} @@", os + 1, oe - os, ns + 1, ne - ns).cyan());

        for op in group {
            for change in diff.iter_changes(op) {
                let orig = match change.tag() {
                    ChangeTag::Delete => old_lines.get(change.old_index().unwrap_or(0)).copied().unwrap_or(""),
                    ChangeTag::Insert => new_lines.get(change.new_index().unwrap_or(0)).copied().unwrap_or(""),
                    ChangeTag::Equal  => old_lines.get(change.old_index().unwrap_or(0)).copied().unwrap_or(""),
                };
                let nl = if change.missing_newline() { "" } else { "\n" };
                match change.tag() {
                    ChangeTag::Equal  => print!(" {}{}", orig, nl),
                    ChangeTag::Delete => print!("{}", format!("-{}{}", orig, nl).red()),
                    ChangeTag::Insert => print!("{}", format!("+{}{}", orig, nl).green()),
                }
            }
        }
    }
    true
}

fn diff_pair(a: &Path, b: &Path, args: &Args) -> i32 {
    let read_a = fs::read_to_string(a);
    let read_b = fs::read_to_string(b);

    match (read_a, read_b) {
        (Ok(ca), Ok(cb)) => {
            let differ = diff_files(&ca, &cb, &a.display().to_string(), &b.display().to_string(), args);
            if differ {
                if args.brief { println!("Files {} and {} differ", a.display(), b.display()); }
                1
            } else { 0 }
        }
        (Err(e), _) => { eprintln!("diff: {}: {}", a.display(), e); 2 }
        (_, Err(e)) => { eprintln!("diff: {}: {}", b.display(), e); 2 }
    }
}

fn diff_dirs(a: &Path, b: &Path, args: &Args) -> i32 {
    let mut exit = 0;
    let mut entries_a: Vec<_> = fs::read_dir(a).into_iter().flatten().flatten()
        .map(|e| e.file_name()).collect();
    entries_a.sort();

    for name in entries_a {
        let pa = a.join(&name);
        let pb = b.join(&name);
        if !pb.exists() {
            println!("Only in {}: {}", a.display(), name.to_string_lossy());
            exit = 1;
            continue;
        }
        if pa.is_dir() && pb.is_dir() && args.recursive {
            let r = diff_dirs(&pa, &pb, args);
            if r != 0 { exit = r; }
        } else if pa.is_file() && pb.is_file() {
            let r = diff_pair(&pa, &pb, args);
            if r != 0 { exit = r; }
        }
    }
    exit
}

fn main() {
    let args = parse_args();

    if args.files.len() < 2 {
        eprintln!("diff: missing operand");
        eprintln!("Usage: diff FILE1 FILE2");
        std::process::exit(2);
    }

    let a = &args.files[0];
    let b = &args.files[1];

    let code = if a.is_dir() && b.is_dir() {
        diff_dirs(a, b, &args)
    } else {
        diff_pair(a, b, &args)
    };

    std::process::exit(code);
}
