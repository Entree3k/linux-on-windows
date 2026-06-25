use clap::{Arg, ArgAction, Command};
use colored::*;
use regex::{Regex, RegexBuilder};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, IsTerminal, Read};
use std::path::Path;
use walkdir::WalkDir;

// Output options

struct Opts {
    invert:        bool,
    line_num:      bool,
    count_only:    bool,
    files_only:    bool,   // -l
    files_no:      bool,   // -L (files without match)
    quiet:         bool,   // -q suppress all output, just set exit code
    before:        usize,  // -B
    after:         usize,  // -A
    color:         bool,
    show_filename: bool,   // derived: true when >1 source or recursive
    max_count:     Option<usize>, // -m
}

// Match highlighting

fn highlight(line: &str, re: &Regex) -> String {
    let mut out = String::with_capacity(line.len() + 32);
    let mut last = 0;
    for m in re.find_iter(line) {
        out.push_str(&line[last..m.start()]);
        out.push_str(&line[m.start()..m.end()].red().bold().to_string());
        last = m.end();
    }
    out.push_str(&line[last..]);
    out
}

// Core match logic

fn grep_reader(reader: impl BufRead, re: &Regex, opts: &Opts, label: &str) -> u64 {
    let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

    let mut matched: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| re.is_match(l) ^ opts.invert)
        .map(|(i, _)| i)
        .collect();

    // -m: cap the number of matches
    if let Some(max) = opts.max_count {
        matched.truncate(max);
    }

    let count = matched.len() as u64;

    if opts.quiet { return count; }

    if opts.count_only {
        if opts.show_filename && !label.is_empty() {
            println!("{}:{}", label.magenta(), count);
        } else {
            println!("{}", count);
        }
        return count;
    }

    if opts.files_only {
        if count > 0 { println!("{}", label.magenta()); }
        return count;
    }
    if opts.files_no {
        if count == 0 && !label.is_empty() { println!("{}", label.magenta()); }
        return count;
    }

    if count == 0 { return 0; }

    // Build the set of lines to print, expanding context
    let to_print: Vec<usize> = {
        use std::collections::BTreeSet;
        let mut set: BTreeSet<usize> = BTreeSet::new();
        for &m in &matched {
            let start = m.saturating_sub(opts.before);
            let end   = (m + opts.after + 1).min(lines.len());
            for i in start..end { set.insert(i); }
        }
        set.into_iter().collect()
    };

    let match_set: std::collections::HashSet<usize> = matched.into_iter().collect();
    let mut prev: Option<usize> = None;

    for &i in &to_print {
        if opts.before > 0 || opts.after > 0 {
            if let Some(p) = prev {
                if i > p + 1 { println!("{}", "--".dimmed()); }
            }
        }
        prev = Some(i);

        let line     = &lines[i];
        let is_match = match_set.contains(&i);
        let lnum     = i + 1;
        let sep = if is_match { ":".green().to_string() } else { "-".dimmed().to_string() };

        let mut row = String::new();
        if opts.show_filename && !label.is_empty() {
            row.push_str(&format!("{}{}", label.magenta(), sep));
        }
        if opts.line_num {
            row.push_str(&format!("{}{}", lnum.to_string().green(), sep));
        }
        if is_match && opts.color {
            row.push_str(&highlight(line, re));
        } else {
            row.push_str(line);
        }
        println!("{}", row);
    }

    count
}

// File handling

fn grep_file(path: &Path, re: &Regex, opts: &Opts) -> u64 {
    let f = match File::open(path) {
        Ok(f)  => f,
        Err(e) => {
            eprintln!("{}: {}: {}", "grep".bold(), path.display().to_string().yellow(), e);
            return 0;
        }
    };

    // Skip binary files (null bytes in first 8 KB)
    {
        let mut probe = vec![0u8; 8 * 1024];
        let mut handle = f;
        let n = handle.read(&mut probe).unwrap_or(0);
        if probe[..n].contains(&0u8) {
            if !opts.quiet && !opts.count_only && !opts.files_only {
                let text = String::from_utf8_lossy(&probe[..n]);
                if re.is_match(&text) ^ opts.invert {
                    eprintln!("Binary file {} matches", path.display().to_string().magenta());
                }
            }
            return 0;
        }
        drop(handle);
    }

    let f = match File::open(path) {
        Ok(f)  => f,
        Err(_) => return 0,
    };

    grep_reader(BufReader::new(f), re, opts, &path.display().to_string())
}

// Entry point

fn main() {
    let matches = Command::new("grep")
        .version("1.1.0")
        .about("Search files for lines matching a pattern")
        .after_help(
            "EXAMPLES\n\
             \x20 grep error app.log                   # basic search\n\
             \x20 grep -i warning app.log               # case-insensitive\n\
             \x20 grep -n \"fn main\" src/main.rs         # with line numbers\n\
             \x20 grep -r \"TODO\" src/                   # recursive\n\
             \x20 grep -rn \"fn \" src/ --include *.rs    # recursive with filter\n\
             \x20 grep -r \"TODO\" src/ --exclude *.min.js\n\
             \x20 grep -F \"1.2.3.4\" log.txt             # literal, no regex\n\
             \x20 grep -w \"log\" src/ -r                 # whole word (not 'logger')\n\
             \x20 grep -x \"exact line\" file.txt         # whole line match\n\
             \x20 grep -m 5 error app.log               # stop after 5 matches\n\
             \x20 grep -v \"^#\" config.ini               # invert match\n\
             \x20 grep -c error app.log                 # count only\n\
             \x20 grep -A 3 -B 2 panic app.log          # context lines\n\
             \n\
             EXIT CODES:  0=match found  1=no match  2=error",
        )
        .arg(Arg::new("pattern").required(true).index(1).help("Pattern to search for"))
        .arg(Arg::new("files").value_name("FILE").index(2).num_args(0..).help("Files or directories to search"))
        .arg(Arg::new("ignore_case").short('i').long("ignore-case").action(ArgAction::SetTrue).help("Case-insensitive match"))
        .arg(Arg::new("invert").short('v').long("invert-match").action(ArgAction::SetTrue).help("Print lines that do NOT match"))
        .arg(Arg::new("line_num").short('n').long("line-number").action(ArgAction::SetTrue).help("Prefix each line with its line number"))
        .arg(Arg::new("count").short('c').long("count").action(ArgAction::SetTrue).help("Print only a count of matching lines"))
        .arg(Arg::new("files_only").short('l').long("files-with-matches").action(ArgAction::SetTrue).help("Print only names of files with matches"))
        .arg(Arg::new("files_no").short('L').long("files-without-match").action(ArgAction::SetTrue).help("Print only names of files with no matches"))
        .arg(Arg::new("recursive").short('r').long("recursive").action(ArgAction::SetTrue).help("Recurse into directories"))
        .arg(Arg::new("quiet").short('q').long("quiet").action(ArgAction::SetTrue).help("Suppress output; exit 0 if any match"))
        .arg(Arg::new("no_color").long("no-color").action(ArgAction::SetTrue).help("Disable color output"))
        .arg(Arg::new("fixed").short('F').long("fixed-strings").action(ArgAction::SetTrue).help("Treat pattern as literal string, not regex"))
        .arg(Arg::new("word").short('w').long("word-regexp").action(ArgAction::SetTrue).help("Match whole words only"))
        .arg(Arg::new("line_match").short('x').long("line-regexp").action(ArgAction::SetTrue).help("Match whole lines only"))
        .arg(Arg::new("max_count").short('m').long("max-count").value_name("N").help("Stop after N matching lines per file"))
        .arg(Arg::new("after").short('A').long("after-context").value_name("N").default_value("0").help("Print N lines after each match"))
        .arg(Arg::new("before").short('B').long("before-context").value_name("N").default_value("0").help("Print N lines before each match"))
        .arg(Arg::new("context").short('C').long("context").value_name("N").help("Print N lines before AND after each match"))
        .arg(Arg::new("include").long("include").value_name("GLOB").help("Only search files matching GLOB (e.g. *.rs)"))
        .arg(Arg::new("exclude").long("exclude").value_name("GLOB").help("Skip files matching GLOB (e.g. *.min.js)"))
        .get_matches();

    let raw_pattern  = matches.get_one::<String>("pattern").unwrap().clone();
    let ignore_case  = matches.get_flag("ignore_case");
    let recursive    = matches.get_flag("recursive");
    let fixed        = matches.get_flag("fixed");
    let word         = matches.get_flag("word");
    let line_match   = matches.get_flag("line_match");

    // Build the final regex pattern from flags
    let built_pattern = {
        let base = if fixed { regex::escape(&raw_pattern) } else { raw_pattern.clone() };
        let worded = if word { format!(r"\b{}\b", base) } else { base };
        if line_match { format!(r"^{}$", worded) } else { worded }
    };

    let re = match RegexBuilder::new(&built_pattern)
        .case_insensitive(ignore_case)
        .build()
    {
        Ok(r)  => r,
        Err(e) => {
            eprintln!("{}: invalid pattern: {}", "grep".bold(), e);
            std::process::exit(2);
        }
    };

    let context_n: Option<usize> = matches.get_one::<String>("context").and_then(|s| s.parse().ok());
    let after:  usize = context_n.unwrap_or_else(|| matches.get_one::<String>("after").and_then(|s| s.parse().ok()).unwrap_or(0));
    let before: usize = context_n.unwrap_or_else(|| matches.get_one::<String>("before").and_then(|s| s.parse().ok()).unwrap_or(0));
    let max_count: Option<usize> = matches.get_one::<String>("max_count").and_then(|s| s.parse().ok());

    let include_glob: Option<String> = matches.get_one::<String>("include").cloned();
    let exclude_glob: Option<String> = matches.get_one::<String>("exclude").cloned();

    let files: Vec<String> = matches.get_many::<String>("files").unwrap_or_default().cloned().collect();
    let use_color = !matches.get_flag("no_color") && io::stdout().is_terminal();

    let multi_source = recursive
        || files.len() > 1
        || files.iter().any(|f| Path::new(f).is_dir());

    let opts = Opts {
        invert:        matches.get_flag("invert"),
        line_num:      matches.get_flag("line_num"),
        count_only:    matches.get_flag("count"),
        files_only:    matches.get_flag("files_only"),
        files_no:      matches.get_flag("files_no"),
        quiet:         matches.get_flag("quiet"),
        before, after, color: use_color,
        show_filename: multi_source,
        max_count,
    };

    let mut total_matches: u64 = 0;

    if files.is_empty() {
        total_matches += grep_reader(io::stdin().lock(), &re, &opts, "");
    } else {
        for path_str in &files {
            let path = Path::new(path_str);

            if recursive && path.is_dir() {
                for entry in WalkDir::new(path)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                {
                    let name = entry.file_name().to_string_lossy();
                    if let Some(ref glob) = include_glob {
                        if !glob_match(glob, &name) { continue; }
                    }
                    if let Some(ref glob) = exclude_glob {
                        if glob_match(glob, &name) { continue; }
                    }
                    total_matches += grep_file(entry.path(), &re, &opts);
                }
            } else {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if let Some(ref glob) = include_glob {
                    if !glob_match(glob, &name) { continue; }
                }
                if let Some(ref glob) = exclude_glob {
                    if glob_match(glob, &name) { continue; }
                }
                total_matches += grep_file(path, &re, &opts);
            }
        }
    }

    std::process::exit(if total_matches > 0 { 0 } else { 1 });
}

/// Glob matcher: supports `*` (any chars) and `?` (single char)
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_rec(&p, &t, 0, 0)
}

fn glob_rec(p: &[char], t: &[char], pi: usize, ti: usize) -> bool {
    if pi == p.len() { return ti == t.len(); }
    if p[pi] == '*' {
        for skip in ti..=t.len() {
            if glob_rec(p, t, pi + 1, skip) { return true; }
        }
        return false;
    }
    if ti == t.len() { return false; }
    if p[pi] == '?' || p[pi] == t[ti] { return glob_rec(p, t, pi + 1, ti + 1); }
    false
}
