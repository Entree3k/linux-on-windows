use image::GenericImageView;
use std::io::{self, Write};
use std::path::Path;

struct Args {
    files:     Vec<String>,
    width:     Option<u32>,
    height:    Option<u32>,
    no_scale:  bool,
    use_block: bool,
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw.iter().any(|a| a == "--help") {
        eprintln!("Usage: icat [OPTION]... IMAGE...");
        eprintln!("Render images in the terminal using Unicode block characters.");
        eprintln!("  --width N    force output width in columns");
        eprintln!("  --height N   force output height in rows");
        eprintln!("  --no-scale   don't scale; render pixel-for-pixel");
        eprintln!("  --block      use full block █ (coarser, single color per cell)");
        eprintln!("Formats: PNG, JPEG, GIF (first frame), BMP, WebP, ICO, TIFF");
        std::process::exit(if raw.is_empty() { 1 } else { 0 });
    }

    let mut a = Args { files: Vec::new(), width: None, height: None, no_scale: false, use_block: false };
    let mut i = 0;
    while i < raw.len() {
        let s = raw[i].as_str();
        match s {
            "--no-scale" => a.no_scale   = true,
            "--block"    => a.use_block  = true,
            "--width"    => { i += 1; a.width  = raw.get(i).and_then(|v| v.parse().ok()); }
            "--height"   => { i += 1; a.height = raw.get(i).and_then(|v| v.parse().ok()); }
            _ if s.starts_with("--width=")  => { a.width  = s[8..].parse().ok(); }
            _ if s.starts_with("--height=") => { a.height = s[9..].parse().ok(); }
            _ if !s.starts_with('-') => a.files.push(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    a
}

fn terminal_width() -> u32 {
    std::env::var("COLUMNS").ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(80u32)
}

fn render(path: &Path, args: &Args) {
    let img = match image::open(path) {
        Ok(i)  => i,
        Err(e) => { eprintln!("icat: {}: {}", path.display(), e); return; }
    };

    let (orig_w, orig_h) = img.dimensions();

    let (out_w, out_h) = if args.no_scale {
        (orig_w, orig_h)
    } else {
        let term_w = args.width.unwrap_or_else(terminal_width);
        // Each character cell is approximately 2:1 height:width ratio in pixels
        // Using half-blocks: each cell covers 1 col × 2 rows of image pixels
        let scale_w = term_w.min(orig_w);
        let scale_h = (orig_h as f64 * scale_w as f64 / orig_w as f64 / 2.0).round() as u32;
        let out_h = args.height.unwrap_or(scale_h).max(1);
        (args.width.unwrap_or(scale_w), out_h)
    };

    // Scale image to target dimensions (width × height*2 pixels)
    let pixel_h = out_h * 2;
    let scaled = img.resize_exact(out_w, pixel_h, image::imageops::FilterType::Triangle);
    let rgb = scaled.to_rgb8();

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    if args.use_block {
        // Full block mode: one color per character
        for row in 0..out_h {
            let y = row * 2;
            for x in 0..out_w {
                let p = rgb.get_pixel(x, y);
                write!(out, "\x1b[38;2;{};{};{}m█", p[0], p[1], p[2]).ok();
            }
            write!(out, "\x1b[0m\n").ok();
        }
    } else {
        // Half-block mode: ▄ with top=background, bottom=foreground
        // Each terminal row covers 2 pixel rows
        for row in 0..out_h {
            let y_top = row * 2;
            let y_bot = y_top + 1;
            for x in 0..out_w {
                let top = rgb.get_pixel(x, y_top);
                let bot = if y_bot < pixel_h {
                    *rgb.get_pixel(x, y_bot)
                } else {
                    image::Rgb([0u8, 0, 0])
                };
                // Background = top pixel, foreground = bottom pixel, char = ▄ (lower half block)
                write!(out,
                    "\x1b[48;2;{};{};{}m\x1b[38;2;{};{};{}m\u{2584}",
                    top[0], top[1], top[2],
                    bot[0], bot[1], bot[2]
                ).ok();
            }
            write!(out, "\x1b[0m\n").ok();
        }
    }
    out.flush().ok();
}

fn main() {
    let args = parse_args();
    let files = args.files.clone();

    let multi = files.len() > 1;
    for (i, f) in files.iter().enumerate() {
        if multi {
            println!("==> {} <==", f);
        }
        render(Path::new(f), &args);
        if multi && i + 1 < files.len() { println!(); }
    }
}
