//! gzip — GNU gzip compatible compression/decompression.
//! Rename or copy to gunzip.exe for decompress-by-default mode.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};

struct Args {
    decompress: bool,
    keep:       bool,
    stdout:     bool,
    force:      bool,
    verbose:    bool,
    quiet:      bool,
    test:       bool,
    list:       bool,
    recursive:  bool,
    level:      u32,
    files:      Vec<PathBuf>,
}

fn parse_args() -> Args {
    // Detect gunzip / zcat invocation from argv[0]
    let argv0 = std::env::args().next().unwrap_or_default();
    let name = std::path::Path::new(&argv0)
        .file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let is_gunzip = name.contains("gunzip");
    let is_zcat   = name.contains("zcat");

    let mut a = Args {
        decompress: is_gunzip || is_zcat,
        keep:    false,
        stdout:  is_zcat,
        force:   false,
        verbose: false,
        quiet:   false,
        test:    false,
        list:    false,
        recursive: false,
        level:   6,
        files:   Vec::new(),
    };

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        let s = raw[i].as_str();
        match s {
            "-h" | "--help"       => { print_help(); std::process::exit(0); }
            "-d" | "--decompress" | "--uncompress" => a.decompress = true,
            "-k" | "--keep"       => a.keep    = true,
            "-c" | "--stdout" | "--to-stdout" => a.stdout   = true,
            "-f" | "--force"      => a.force   = true,
            "-v" | "--verbose"    => a.verbose  = true,
            "-q" | "--quiet"      => a.quiet    = true,
            "-t" | "--test"       => a.test     = true,
            "-l" | "--list"       => a.list     = true,
            "-r" | "--recursive"  => a.recursive = true,
            "-1" | "--fast"       => a.level    = 1,
            "-2"                  => a.level    = 2,
            "-3"                  => a.level    = 3,
            "-4"                  => a.level    = 4,
            "-5"                  => a.level    = 5,
            "-6"                  => a.level    = 6,
            "-7"                  => a.level    = 7,
            "-8"                  => a.level    = 8,
            "-9" | "--best"       => a.level    = 9,
            _ if s.starts_with('-') && s.len() > 1 && !s.starts_with("--") => {
                for c in s.chars().skip(1) {
                    match c {
                        'd' => a.decompress = true,
                        'k' => a.keep       = true,
                        'c' => a.stdout     = true,
                        'f' => a.force      = true,
                        'v' => a.verbose    = true,
                        'q' => a.quiet      = true,
                        't' => a.test       = true,
                        'l' => a.list       = true,
                        'r' => a.recursive  = true,
                        '1'..='9' => a.level = c as u32 - '0' as u32,
                        _ => {}
                    }
                }
            }
            _ => a.files.push(PathBuf::from(s)),
        }
        i += 1;
    }
    a
}

fn print_help() {
    eprintln!("Usage: gzip [OPTION]... [FILE]...");
    eprintln!();
    eprintln!("Compress or uncompress FILEs (by default, compress FILES in-place).");
    eprintln!();
    eprintln!("  -c, --stdout        write on standard output, keep original files unchanged");
    eprintln!("  -d, --decompress    decompress");
    eprintln!("  -f, --force         force overwrite of output file and compress links");
    eprintln!("  -k, --keep          keep (don't delete) input files");
    eprintln!("  -l, --list          list compressed file contents");
    eprintln!("  -q, --quiet         suppress all warnings");
    eprintln!("  -r, --recursive     operate recursively on directories");
    eprintln!("  -t, --test          test compressed file integrity");
    eprintln!("  -v, --verbose       verbose mode");
    eprintln!("  -1, --fast          compress faster");
    eprintln!("  -9, --best          compress better");
    eprintln!("  -h, --help          give this help");
}

// Compress

fn compress_file(src: &Path, args: &Args) -> io::Result<()> {
    let out_path = {
        let name = src.file_name().unwrap().to_string_lossy();
        src.with_file_name(format!("{}.gz", name))
    };

    if out_path.exists() && !args.force && !args.stdout {
        eprintln!("gzip: {}: already exists; not overwritten", out_path.display());
        return Ok(());
    }

    let bytes_in = fs::metadata(src)?.len();
    let inp = BufReader::new(File::open(src)?);

    if args.stdout {
        let mut enc = GzEncoder::new(io::stdout().lock(), Compression::new(args.level));
        io::copy(&mut BufReader::new(inp), &mut enc)?;
        enc.finish()?;
    } else {
        let out = BufWriter::new(File::create(&out_path)?);
        let mut enc = GzEncoder::new(out, Compression::new(args.level));
        io::copy(&mut BufReader::new(inp), &mut enc)?;
        enc.finish()?;

        let bytes_out = fs::metadata(&out_path)?.len();

        if !args.keep {
            fs::remove_file(src)?;
        }

        if args.verbose && !args.quiet {
            let ratio = if bytes_in > 0 {
                100.0 - (bytes_out as f64 / bytes_in as f64 * 100.0)
            } else { 0.0 };
            let action = if args.keep { "created" } else { "replaced with" };
            eprintln!("{}: {:5.1}% -- {} {}", src.display(), ratio, action, out_path.display());
        }
    }
    Ok(())
}

// Decompress

fn decompress_file(src: &Path, args: &Args) -> io::Result<()> {
    // Strip .gz extension for output name
    let stem = match src.extension().and_then(|e| e.to_str()) {
        Some("gz") | Some("z") | Some("Z") => src.with_extension(""),
        _ => src.with_file_name(format!("{}.out", src.file_name().unwrap().to_string_lossy())),
    };

    if args.test {
        let mut dec = GzDecoder::new(BufReader::new(File::open(src)?));
        let mut buf = [0u8; 65536];
        loop {
            let n = dec.read(&mut buf).map_err(|e| {
                eprintln!("gzip: {}: {}", src.display(), e);
                e
            })?;
            if n == 0 { break; }
        }
        if !args.quiet {
            eprintln!("{}: OK", src.display());
        }
        return Ok(());
    }

    if stem.exists() && !args.force && !args.stdout {
        eprintln!("gzip: {}: already exists; not overwritten", stem.display());
        return Ok(());
    }

    let inp = BufReader::new(File::open(src)?);
    let mut dec = GzDecoder::new(inp);

    if args.stdout {
        io::copy(&mut dec, &mut io::stdout().lock())?;
    } else {
        io::copy(&mut dec, &mut BufWriter::new(File::create(&stem)?))?;
        if !args.keep {
            fs::remove_file(src)?;
        }
        if args.verbose && !args.quiet {
            eprintln!("{}: -- replaced with {}", src.display(), stem.display());
        }
    }
    Ok(())
}

// List

fn list_file(src: &Path) -> io::Result<()> {
    let meta = fs::metadata(src)?;
    let compressed = meta.len();

    // Read ISIZE from last 4 bytes of gzip stream (uncompressed size mod 2^32)
    let mut f = File::open(src)?;
    f.seek(SeekFrom::End(-4))?;
    let mut isize_buf = [0u8; 4];
    f.read_exact(&mut isize_buf)?;
    let uncompressed = u32::from_le_bytes(isize_buf) as u64;

    let ratio = if uncompressed > 0 {
        100.0 - (compressed as f64 / uncompressed as f64 * 100.0)
    } else { 0.0 };

    let name = src.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
    println!("{:>18} {:>18}  {:5.1}%  {}", compressed, uncompressed, ratio, name);
    Ok(())
}

// Walk directory

fn collect_files(paths: &[PathBuf], recursive: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() && recursive {
            for e in walkdir_iter(p) {
                if e.is_file() { out.push(e); }
            }
        } else {
            out.push(p.clone());
        }
    }
    out
}

fn walkdir_iter(dir: &Path) -> impl Iterator<Item = PathBuf> {
    let mut stack = vec![dir.to_owned()];
    let mut files = Vec::new();
    while let Some(d) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() { stack.push(p); }
                else { files.push(p); }
            }
        }
    }
    files.into_iter()
}

fn main() {
    let args = parse_args();

    if args.files.is_empty() {
        // Read from stdin
        if args.decompress || args.test {
            let mut dec = GzDecoder::new(io::stdin().lock());
            io::copy(&mut dec, &mut io::stdout().lock()).ok();
        } else {
            let mut enc = GzEncoder::new(io::stdout().lock(), Compression::new(args.level));
            io::copy(&mut io::stdin().lock(), &mut enc).ok();
        }
        return;
    }

    if args.list && !args.decompress {
        println!("{:>18} {:>18}  ratio  name", "compressed", "uncompressed");
        for f in &args.files {
            if let Err(e) = list_file(f) {
                eprintln!("gzip: {}: {}", f.display(), e);
            }
        }
        return;
    }

    let files = collect_files(&args.files, args.recursive);
    let mut exit = 0i32;

    for path in &files {
        let result = if args.decompress || args.test {
            decompress_file(path, &args)
        } else {
            compress_file(path, &args)
        };
        if let Err(e) = result {
            eprintln!("gzip: {}: {}", path.display(), e);
            exit = 1;
        }
    }
    std::process::exit(exit);
}
