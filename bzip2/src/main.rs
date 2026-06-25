//! bzip2 — block-sorting file compression.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use bzip2::{read::BzDecoder, write::BzEncoder, Compression};

struct Args {
    decompress: bool,
    keep:       bool,
    stdout:     bool,
    force:      bool,
    verbose:    bool,
    quiet:      bool,
    test:       bool,
    level:      u32,
    files:      Vec<PathBuf>,
}

fn parse_args() -> Args {
    let argv0 = std::env::args().next().unwrap_or_default();
    let name = std::path::Path::new(&argv0)
        .file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let is_bunzip = name.contains("bunzip") || name.contains("bzcat");
    let is_bzcat  = name.contains("bzcat");

    let mut a = Args {
        decompress: is_bunzip,
        keep: false, stdout: is_bzcat, force: false,
        verbose: false, quiet: false, test: false,
        level: 9, // bzip2 default is level 9 (best)
        files: Vec::new(),
    };

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        let s = raw[i].as_str();
        match s {
            "-h" | "--help"        => { print_help(); std::process::exit(0); }
            "-d" | "--decompress"  => a.decompress = true,
            "-k" | "--keep"        => a.keep       = true,
            "-c" | "--stdout"      => a.stdout     = true,
            "-f" | "--force"       => a.force      = true,
            "-v" | "--verbose"     => a.verbose    = true,
            "-q" | "--quiet"       => a.quiet      = true,
            "-t" | "--test"        => a.test       = true,
            "-z" | "--compress"    => a.decompress = false,
            "-1" | "--fast"        => a.level      = 1,
            "-2" => a.level = 2, "-3" => a.level = 3,
            "-4" => a.level = 4, "-5" => a.level = 5,
            "-6" => a.level = 6, "-7" => a.level = 7,
            "-8" => a.level = 8,
            "-9" | "--best"        => a.level      = 9,
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
                        'z' => a.decompress = false,
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
    eprintln!("Usage: bzip2 [OPTION]... [FILE]...");
    eprintln!();
    eprintln!("  -c, --stdout       write to standard output");
    eprintln!("  -d, --decompress   force decompression");
    eprintln!("  -z, --compress     force compression");
    eprintln!("  -k, --keep         keep input files");
    eprintln!("  -f, --force        overwrite existing output files");
    eprintln!("  -t, --test         test compressed file integrity");
    eprintln!("  -v, --verbose      verbose mode");
    eprintln!("  -q, --quiet        suppress all warnings");
    eprintln!("  -1 .. -9           set block size to 100k .. 900k");
    eprintln!("  -1, --fast         faster compression");
    eprintln!("  -9, --best         better compression (default)");
}

fn compress_file(src: &Path, args: &Args) -> io::Result<()> {
    let out_path = {
        let name = src.file_name().unwrap().to_string_lossy();
        src.with_file_name(format!("{}.bz2", name))
    };

    if out_path.exists() && !args.force && !args.stdout {
        eprintln!("bzip2: {}: already exists; not overwritten", out_path.display());
        return Ok(());
    }

    let bytes_in = fs::metadata(src)?.len();
    let mut inp   = BufReader::new(File::open(src)?);

    let level = Compression::new(args.level);

    if args.stdout {
        let mut enc = BzEncoder::new(io::stdout().lock(), level);
        io::copy(&mut inp, &mut enc)?;
        enc.finish()?;
    } else {
        let out = BufWriter::new(File::create(&out_path)?);
        let mut enc = BzEncoder::new(out, level);
        io::copy(&mut inp, &mut enc)?;
        enc.finish()?;

        if !args.keep { fs::remove_file(src)?; }

        if args.verbose && !args.quiet {
            let bytes_out = fs::metadata(&out_path)?.len();
            let ratio = if bytes_in > 0 {
                100.0 - (bytes_out as f64 / bytes_in as f64 * 100.0)
            } else { 0.0 };
            let action = if args.keep { "created" } else { "replaced with" };
            eprintln!("{}: {:5.1}% -- {} {}", src.display(), ratio, action, out_path.display());
        }
    }
    Ok(())
}

fn decompress_file(src: &Path, args: &Args) -> io::Result<()> {
    let stem = match src.extension().and_then(|e| e.to_str()) {
        Some("bz2") | Some("bz") => src.with_extension(""),
        _ => src.with_file_name(format!("{}.out", src.file_name().unwrap().to_string_lossy())),
    };

    if args.test {
        let mut dec = BzDecoder::new(BufReader::new(File::open(src)?));
        let mut buf = [0u8; 65536];
        loop {
            match dec.read(&mut buf) {
                Ok(0)  => break,
                Ok(_)  => {}
                Err(e) => { eprintln!("bzip2: {}: {}", src.display(), e); return Err(e); }
            }
        }
        if !args.quiet { eprintln!("{}: OK", src.display()); }
        return Ok(());
    }

    if stem.exists() && !args.force && !args.stdout {
        eprintln!("bzip2: {}: already exists; not overwritten", stem.display());
        return Ok(());
    }

    let mut dec = BzDecoder::new(BufReader::new(File::open(src)?));

    if args.stdout {
        io::copy(&mut dec, &mut io::stdout().lock())?;
    } else {
        io::copy(&mut dec, &mut BufWriter::new(File::create(&stem)?))?;
        if !args.keep { fs::remove_file(src)?; }
        if args.verbose && !args.quiet {
            eprintln!("{}: -- replaced with {}", src.display(), stem.display());
        }
    }
    Ok(())
}

fn main() {
    let args = parse_args();

    if args.files.is_empty() {
        if args.decompress || args.test {
            let mut dec = BzDecoder::new(io::stdin().lock());
            io::copy(&mut dec, &mut io::stdout().lock()).ok();
        } else {
            let mut enc = BzEncoder::new(io::stdout().lock(), Compression::new(args.level));
            io::copy(&mut io::stdin().lock(), &mut enc).ok();
        }
        return;
    }

    let mut exit = 0i32;
    for path in &args.files {
        let result = if args.decompress || args.test {
            decompress_file(path, &args)
        } else {
            compress_file(path, &args)
        };
        if let Err(e) = result {
            eprintln!("bzip2: {}: {}", path.display(), e);
            exit = 1;
        }
    }
    std::process::exit(exit);
}
