use std::collections::HashSet;
use std::io::{self, Read, Write};

struct Opts {
    delete:     bool,
    squeeze:    bool,
    complement: bool,
}

enum Token {
    Char(char),
    Class(Vec<char>),
}

fn parse_class(name: &str) -> Vec<char> {
    match name {
        "alpha" => ('a'..='z').chain('A'..='Z').collect(),
        "digit" => ('0'..='9').collect(),
        "alnum" => ('a'..='z').chain('A'..='Z').chain('0'..='9').collect(),
        "upper" => ('A'..='Z').collect(),
        "lower" => ('a'..='z').collect(),
        "space" => vec![' ', '\t', '\n', '\r', '\u{0b}', '\u{0c}'],
        "blank" => vec![' ', '\t'],
        "punct" => "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~".chars().collect(),
        _       => Vec::new(),
    }
}

fn tokenize(s: &str) -> Vec<Token> {
    let chars: Vec<char> = s.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '[' && i + 1 < chars.len() && chars[i + 1] == ':' {
            // [:class:]
            if let Some(end) = (i + 2..chars.len()).find(|&j| chars[j] == ':' && j + 1 < chars.len() && chars[j + 1] == ']') {
                let name: String = chars[i + 2..end].iter().collect();
                tokens.push(Token::Class(parse_class(&name)));
                i = end + 2;
                continue;
            }
        }
        if c == '\\' && i + 1 < chars.len() {
            let n = chars[i + 1];
            let ec = match n {
                'n'  => '\n',
                't'  => '\t',
                'r'  => '\r',
                '\\' => '\\',
                '0'  => '\0',
                _    => n,
            };
            tokens.push(Token::Char(ec));
            i += 2;
            continue;
        }
        tokens.push(Token::Char(c));
        i += 1;
    }
    tokens
}

fn expand_set(s: &str) -> Vec<char> {
    let tokens = tokenize(s);
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if let Token::Char(a) = tokens[i] {
            if i + 2 < tokens.len() {
                if let (Token::Char('-'), Token::Char(b)) = (&tokens[i + 1], &tokens[i + 2]) {
                    if a <= *b {
                        for ch in a..=*b {
                            out.push(ch);
                        }
                        i += 3;
                        continue;
                    }
                }
            }
            out.push(a);
            i += 1;
        } else if let Token::Class(ref v) = tokens[i] {
            out.extend(v.iter().copied());
            i += 1;
        }
    }
    out
}

fn complement(set: &[char]) -> Vec<char> {
    let present: HashSet<char> = set.iter().copied().collect();
    (0u8..=255).map(|b| b as char).filter(|c| !present.contains(c)).collect()
}

fn squeeze(input: Vec<char>, members: &HashSet<char>) -> Vec<char> {
    let mut out = Vec::with_capacity(input.len());
    let mut prev: Option<char> = None;
    for c in input {
        if members.contains(&c) && prev == Some(c) {
            continue;
        }
        prev = Some(c);
        out.push(c);
    }
    out
}

fn print_help() {
    eprintln!("Usage: tr [OPTION]... SET1 [SET2]");
    eprintln!("Translate, squeeze, and/or delete characters from stdin, writing to stdout.");
    eprintln!("  -d, --delete       delete characters in SET1");
    eprintln!("  -s, --squeeze-repeats  squeeze repeated characters");
    eprintln!("  -c, -C, --complement   use the complement of SET1");
    eprintln!("Ranges (a-z), classes ([:alpha:] [:digit:] [:space:] ...), and escapes (\\n \\t \\r \\\\) are supported.");
}

fn parse_args() -> (Opts, Vec<String>) {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|a| a == "--help") {
        print_help();
        std::process::exit(0);
    }
    let mut o = Opts { delete: false, squeeze: false, complement: false };
    let mut sets = Vec::new();
    for s in raw {
        match s.as_str() {
            "-d" | "--delete"          => o.delete = true,
            "-s" | "--squeeze-repeats" => o.squeeze = true,
            "-c" | "-C" | "--complement" => o.complement = true,
            _ if s.starts_with('-') && s.len() > 1 && !s.starts_with("--") => {
                for c in s.chars().skip(1) {
                    match c {
                        'd' => o.delete = true,
                        's' => o.squeeze = true,
                        'c' | 'C' => o.complement = true,
                        _ => {}
                    }
                }
            }
            _ => sets.push(s),
        }
    }
    (o, sets)
}

fn main() {
    let (opts, sets) = parse_args();
    if sets.is_empty() {
        eprintln!("tr: missing operand");
        eprintln!("Try 'tr --help' for more information.");
        std::process::exit(1);
    }

    let set1 = expand_set(&sets[0]);
    let set1 = if opts.complement { complement(&set1) } else { set1 };

    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        // Fall back to lossy bytes for non-UTF8 input.
        let mut bytes = Vec::new();
        io::stdin().read_to_end(&mut bytes).ok();
        input = String::from_utf8_lossy(&bytes).into_owned();
    }
    let chars: Vec<char> = input.chars().collect();

    let result: Vec<char> = if opts.delete {
        let del: HashSet<char> = set1.iter().copied().collect();
        let kept: Vec<char> = chars.into_iter().filter(|c| !del.contains(c)).collect();
        if opts.squeeze && sets.len() > 1 {
            let sq: HashSet<char> = expand_set(&sets[1]).into_iter().collect();
            squeeze(kept, &sq)
        } else {
            kept
        }
    } else if sets.len() > 1 {
        // Translate SET1 -> SET2.
        let set2 = expand_set(&sets[1]);
        let last = *set2.last().unwrap_or(&'\0');
        let mut map = std::collections::HashMap::new();
        for (i, &c) in set1.iter().enumerate() {
            map.insert(c, *set2.get(i).unwrap_or(&last));
        }
        let translated: Vec<char> = chars.into_iter().map(|c| *map.get(&c).unwrap_or(&c)).collect();
        if opts.squeeze {
            let sq: HashSet<char> = set2.into_iter().collect();
            squeeze(translated, &sq)
        } else {
            translated
        }
    } else if opts.squeeze {
        let sq: HashSet<char> = set1.into_iter().collect();
        squeeze(chars, &sq)
    } else {
        chars
    };

    let out_str: String = result.into_iter().collect();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    out.write_all(out_str.as_bytes()).ok();
}
