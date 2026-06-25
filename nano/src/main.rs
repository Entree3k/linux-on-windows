use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const MAX_UNDO:        usize    = 200;
const STATUS_DURATION: Duration = Duration::from_secs(4);
const TAB_WIDTH:       usize    = 8;   // nano default is 8
const FILL_WIDTH:      usize    = 72;  // nano default justify width

// Screen layout
//   row 0        : title bar
//   rows 1..N-4  : content
//   row N-3      : status / prompt
//   row N-2      : shortcut row 1
//   row N-1      : shortcut row 2

fn content_rows(rows: usize) -> usize { rows.saturating_sub(4) }
fn status_y(rows: usize)     -> u16   { rows.saturating_sub(3) as u16 }

// Data

struct Snap { lines: Vec<String>, cy: usize, cx: usize }

struct Editor {
    lines:    Vec<String>,
    cy:       usize,   // cursor row in buffer
    cx:       usize,   // cursor col as char index
    row_off:  usize,
    col_off:  usize,
    cols:     usize,
    rows:     usize,
    filename: Option<PathBuf>,
    modified: bool,
    cut_buf:  Vec<String>,   // accumulates consecutive ^K cuts
    undo:     Vec<Snap>,
    redo:     Vec<Snap>,
    search:   String,
    status:   String,
    sat:      Instant,       // when status was last set
    quit:     bool,
}

impl Editor {
    fn new() -> Self {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        Editor {
            lines:    vec![String::new()],
            cy: 0, cx: 0, row_off: 0, col_off: 0,
            cols: cols as usize,
            rows: rows as usize,
            filename:  None,
            modified:  false,
            cut_buf:   vec![],
            undo:      vec![],
            redo:      vec![],
            search:    String::new(),
            status:    String::new(),
            sat:       Instant::now().checked_sub(STATUS_DURATION * 2).unwrap_or_else(Instant::now),
            quit:      false,
        }
    }

    fn load(&mut self, path: &str) -> io::Result<()> {
        let text = fs::read_to_string(path)?;
        self.lines = text.lines().map(str::to_owned).collect();
        if self.lines.is_empty() { self.lines.push(String::new()); }
        self.filename = Some(PathBuf::from(path));
        Ok(())
    }

    fn msg(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.sat    = Instant::now();
    }

    fn display_name(&self) -> String {
        self.filename.as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "New Buffer".to_owned())
    }

    // Scroll

    fn scroll(&mut self) {
        let ch = content_rows(self.rows);
        if self.cy < self.row_off { self.row_off = self.cy; }
        if ch > 0 && self.cy >= self.row_off + ch { self.row_off = self.cy + 1 - ch; }
        if self.cx < self.col_off { self.col_off = self.cx; }
        if self.cx >= self.col_off + self.cols { self.col_off = self.cx + 1 - self.cols; }
    }

    fn center_scroll(&mut self) {
        let ch = content_rows(self.rows);
        self.row_off = self.cy.saturating_sub(ch / 2);
    }

    // Cursor movement

    fn clen(&self, row: usize) -> usize { self.lines[row].chars().count() }
    fn clamp(&mut self) { let n = self.clen(self.cy); if self.cx > n { self.cx = n; } }

    fn up(&mut self)    { if self.cy > 0             { self.cy -= 1; self.clamp(); } }
    fn down(&mut self)  { if self.cy+1 < self.lines.len() { self.cy += 1; self.clamp(); } }
    fn left(&mut self)  {
        if self.cx > 0 { self.cx -= 1; }
        else if self.cy > 0 { self.cy -= 1; self.cx = self.clen(self.cy); }
    }
    fn right(&mut self) {
        if self.cx < self.clen(self.cy) { self.cx += 1; }
        else if self.cy+1 < self.lines.len() { self.cy += 1; self.cx = 0; }
    }
    fn home(&mut self)  { self.cx = 0; }
    fn end(&mut self)   { self.cx = self.clen(self.cy); }

    fn word_fwd(&mut self) {
        let ch: Vec<char> = self.lines[self.cy].chars().collect();
        let mut i = self.cx;
        while i < ch.len() && !ch[i].is_alphanumeric() { i += 1; }
        while i < ch.len() &&  ch[i].is_alphanumeric() { i += 1; }
        if i < ch.len() { self.cx = i; }
        else if self.cy+1 < self.lines.len() { self.cy += 1; self.cx = 0; }
        else { self.cx = ch.len(); }
    }

    fn word_back(&mut self) {
        if self.cx == 0 && self.cy > 0 { self.cy -= 1; self.cx = self.clen(self.cy); return; }
        let ch: Vec<char> = self.lines[self.cy].chars().collect();
        let mut i = self.cx;
        while i > 0 && !ch[i-1].is_alphanumeric() { i -= 1; }
        while i > 0 &&  ch[i-1].is_alphanumeric() { i -= 1; }
        self.cx = i;
    }

    fn page_up(&mut self) {
        let ch = content_rows(self.rows);
        self.cy      = self.cy.saturating_sub(ch);
        self.row_off = self.row_off.saturating_sub(ch);
        self.clamp();
    }

    fn page_down(&mut self) {
        let ch = content_rows(self.rows);
        self.cy = (self.cy + ch).min(self.lines.len().saturating_sub(1));
        self.clamp();
    }

    fn first_line(&mut self) { self.cy = 0; self.cx = 0; }
    fn last_line(&mut self)  { self.cy = self.lines.len()-1; self.cx = 0; }

    fn go_to_line(&mut self, n: usize) {
        self.cy = n.saturating_sub(1).min(self.lines.len()-1);
        self.cx = 0;
    }

    // Undo / Redo

    fn snap(&mut self) {
        if self.undo.len() >= MAX_UNDO { self.undo.remove(0); }
        self.undo.push(Snap { lines: self.lines.clone(), cy: self.cy, cx: self.cx });
        self.redo.clear();
    }

    fn do_undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(Snap { lines: self.lines.clone(), cy: self.cy, cx: self.cx });
            self.lines = s.lines; self.cy = s.cy; self.cx = s.cx;
            self.modified = true;
            self.msg("Undid action  (Alt+E to redo)");
        } else {
            self.msg("No further undo information.");
        }
    }

    fn do_redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(Snap { lines: self.lines.clone(), cy: self.cy, cx: self.cx });
            self.lines = s.lines; self.cy = s.cy; self.cx = s.cx;
            self.modified = true;
            self.msg("Redid action");
        } else {
            self.msg("Nothing to redo.");
        }
    }

    // Editing

    fn b2c(s: &str, ci: usize) -> usize {
        s.char_indices().nth(ci).map(|(b,_)| b).unwrap_or(s.len())
    }

    fn insert_char(&mut self, c: char) {
        let bi = Self::b2c(&self.lines[self.cy], self.cx);
        self.lines[self.cy].insert(bi, c);
        self.cx += 1;
        self.modified = true;
        self.cut_buf.clear();
    }

    fn insert_tab(&mut self) {
        for _ in 0..TAB_WIDTH { self.insert_char(' '); }
    }

    fn newline(&mut self) {
        self.snap();
        let bi   = Self::b2c(&self.lines[self.cy], self.cx);
        let rest = self.lines[self.cy][bi..].to_owned();
        self.lines[self.cy].truncate(bi);
        self.lines.insert(self.cy+1, rest);
        self.cy += 1; self.cx = 0;
        self.modified = true; self.cut_buf.clear();
    }

    fn backspace(&mut self) {
        if self.cx == 0 && self.cy == 0 { return; }
        self.snap();
        if self.cx == 0 {
            let ln = self.lines.remove(self.cy);
            self.cy -= 1; self.cx = self.clen(self.cy);
            self.lines[self.cy].push_str(&ln);
        } else {
            let bi = Self::b2c(&self.lines[self.cy], self.cx-1);
            self.lines[self.cy].remove(bi);
            self.cx -= 1;
        }
        self.modified = true; self.cut_buf.clear();
    }

    fn delete(&mut self) {
        if self.cx == self.clen(self.cy) && self.cy+1 == self.lines.len() { return; }
        self.snap();
        if self.cx == self.clen(self.cy) {
            let nx = self.lines.remove(self.cy+1);
            self.lines[self.cy].push_str(&nx);
        } else {
            let bi = Self::b2c(&self.lines[self.cy], self.cx);
            self.lines[self.cy].remove(bi);
        }
        self.modified = true; self.cut_buf.clear();
    }

    // ^K: cut line; consecutive ^K appends to cut buffer
    fn cut(&mut self) {
        self.snap();
        let cut = if self.lines.len() > 1 {
            let ln = self.lines.remove(self.cy);
            if self.cy >= self.lines.len() { self.cy = self.lines.len()-1; }
            self.cx = 0;
            ln
        } else {
            let c = self.lines[0].clone();
            self.lines[0].clear(); self.cx = 0; c
        };
        self.cut_buf.push(cut);
        self.modified = true;
        self.msg(format!("Cut {} line(s)  (^U to paste)", self.cut_buf.len()));
    }

    // M-6: copy line to cut buffer without deleting
    fn copy(&mut self) {
        let ln = self.lines[self.cy].clone();
        self.cut_buf = vec![ln];
        self.msg(format!("Copied line to cut buffer  (^U to paste)"));
    }

    // ^U: paste cut buffer above current line
    fn paste(&mut self) {
        if self.cut_buf.is_empty() { self.msg("Nothing in cut buffer."); return; }
        self.snap();
        for (i, ln) in self.cut_buf.iter().enumerate() {
            self.lines.insert(self.cy + i, ln.clone());
        }
        self.cx = 0;
        self.modified = true;
        self.msg(format!("Uncut text  ({} line(s))", self.cut_buf.len()));
    }

    // Search

    // Search from (from_row, from_col) forward, wrapping around.
    fn find(&self, term: &str, from_row: usize, from_col: usize) -> Option<(usize, usize)> {
        if term.is_empty() { return None; }
        let tl = term.to_lowercase();
        let n  = self.lines.len();
        for i in 0..n {
            let row  = (from_row + i) % n;
            let low  = self.lines[row].to_lowercase();
            let skip = if i == 0 { from_col } else { 0 };
            if let Some(bp) = low[skip..].find(&tl) {
                let col = self.lines[row][..skip+bp].chars().count();
                return Some((row, col));
            }
        }
        None
    }

    // Justify

    fn justify(&mut self, width: usize) {
        // Find paragraph boundaries: blank lines mark edges
        let start = (0..=self.cy).rev()
            .find(|&r| r < self.cy && self.lines[r].trim().is_empty())
            .map(|r| r+1)
            .unwrap_or(0);
        let end = ((self.cy+1)..self.lines.len())
            .find(|&r| self.lines[r].trim().is_empty())
            .unwrap_or(self.lines.len());
        if start >= end { self.msg("Nothing to justify."); return; }

        self.snap();
        let words: Vec<String> = self.lines[start..end]
            .iter()
            .flat_map(|l| l.split_whitespace().map(str::to_owned))
            .collect();

        let mut new_lines: Vec<String> = vec![];
        let mut cur = String::new();
        for w in &words {
            if cur.is_empty() {
                cur = w.clone();
            } else if cur.len() + 1 + w.len() <= width {
                cur.push(' ');
                cur.push_str(w);
            } else {
                new_lines.push(cur.clone());
                cur = w.clone();
            }
        }
        if !cur.is_empty() { new_lines.push(cur); }

        self.lines.splice(start..end, new_lines);
        self.cy = start; self.cx = 0;
        self.modified = true;
        self.msg("Justified paragraph.");
    }

    // File I/O

    fn save_to(&mut self, path: &PathBuf) -> io::Result<()> {
        let text = self.lines.join("\n") + "\n";
        fs::write(path, text)?;
        self.modified = false;
        self.msg(format!("Wrote {} lines.", self.lines.len()));
        Ok(())
    }

    fn insert_file(&mut self, path: &str) -> io::Result<()> {
        let text = fs::read_to_string(path)?;
        let new_lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let count = new_lines.len();
        self.snap();
        for (i, ln) in new_lines.into_iter().enumerate() {
            self.lines.insert(self.cy + 1 + i, ln);
        }
        self.cy += 1; self.cx = 0;
        self.modified = true;
        self.msg(format!("Inserted {} lines from \"{}\".", count, path));
        Ok(())
    }
}

// Draw

fn draw(e: &Editor, out: &mut impl Write) -> io::Result<()> {
    queue!(out, cursor::Hide)?;

    // Title bar
    {
        let fname = e.display_name();
        let mod_s = if e.modified { " Modified" } else { "" };
        // nano shows:  "  GNU nano x.x         filename         Modified  "
        let left  = "  GNU nano 1.0  ";
        let mid   = format!("{}{}", fname, mod_s);
        let total_side = left.len();
        let pad   = e.cols.saturating_sub(total_side + mid.len() + total_side);
        let lpad  = pad / 2;
        let rpad  = pad - lpad;
        let title = format!("{}{}{}{}", left, " ".repeat(lpad), mid, " ".repeat(rpad + total_side));
        let title = format!("{:<width$}", pad_or_trunc(&title, e.cols), width = e.cols);
        queue!(out,
            cursor::MoveTo(0, 0),
            SetBackgroundColor(Color::White),
            SetForegroundColor(Color::Black),
            SetAttribute(Attribute::Bold),
            Print(title),
            ResetColor,
        )?;
    }

    // Content
    let ch = content_rows(e.rows);
    for sr in 0..ch {
        let br = e.row_off + sr;
        queue!(out,
            cursor::MoveTo(0, (sr+1) as u16),
            terminal::Clear(ClearType::CurrentLine),
        )?;
        if br < e.lines.len() {
            let chars: Vec<char> = e.lines[br].chars().collect();
            // Expand tabs and take visible slice
            let mut disp = String::new();
            let mut col  = 0usize;
            for (i, &c) in chars.iter().enumerate() {
                if i < e.col_off { col += if c == '\t' { TAB_WIDTH } else { 1 }; continue; }
                if disp.chars().count() >= e.cols { break; }
                if c == '\t' { disp.push_str(&" ".repeat(TAB_WIDTH)); }
                else         { disp.push(c); }
                let _ = col;
            }
            let disp = pad_or_trunc(&disp, e.cols);
            queue!(out, Print(disp))?;
        } else {
            queue!(out, SetForegroundColor(Color::DarkBlue), Print("~"), ResetColor)?;
        }
    }

    // Status bar
    {
        let msg = if e.sat.elapsed() < STATUS_DURATION { e.status.as_str() } else { "" };
        let txt = format!("{:<width$}", pad_or_trunc(msg, e.cols), width = e.cols);
        queue!(out,
            cursor::MoveTo(0, status_y(e.rows)),
            SetBackgroundColor(Color::Black),
            SetForegroundColor(Color::White),
            Print(txt),
            ResetColor,
        )?;
    }

    const SHORTCUTS: &[(&str, &str)] = &[
        ("^G", "Help"),       ("^O", "Write Out"),  ("^W", "Where Is"),
        ("^K", "Cut"),        ("^T", "Execute"),     ("^C", "Location"),
        ("^X", "Exit"),       ("^R", "Read File"),   ("^\\","Replace"),
        ("^U", "Paste Text"), ("^J", "Justify"),     ("^/", "Go To Line"),
    ];
    for row in 0..2usize {
        let y = (e.rows - 2 + row) as u16;
        queue!(out, cursor::MoveTo(0, y))?;
        let mut col = 0usize;
        for &(key, label) in SHORTCUTS.iter().skip(row * 6).take(6) {
            // Each item: key in reverse (2 chars) + " " + label padded to 10 = 13 cols
            let label_field = format!(" {:<10}", label);
            let item_w = key.len() + label_field.len();
            if col + item_w > e.cols { break; }
            queue!(out,
                SetAttribute(Attribute::Reverse),
                Print(key),
                SetAttribute(Attribute::Reset),
                Print(&label_field),
            )?;
            col += item_w;
        }
        if col < e.cols {
            queue!(out, Print(" ".repeat(e.cols - col)))?;
        }
    }

    // Reposition cursor
    let scx = e.cx.saturating_sub(e.col_off) as u16;
    let scy = (e.cy.saturating_sub(e.row_off) + 1) as u16;
    queue!(out, cursor::MoveTo(scx, scy), cursor::Show)?;

    out.flush()
}

// Prompt reads a line in the status bar

fn prompt(
    label: &str,
    prefill: &str,
    out: &mut impl Write,
    rows: usize,
    cols: usize,
) -> io::Result<Option<String>> {
    let mut val = prefill.to_owned();
    loop {
        let disp   = format!("{}{}", label, val);
        let padded = format!("{:<width$}", pad_or_trunc(&disp, cols), width = cols);
        queue!(out,
            cursor::MoveTo(0, status_y(rows)),
            terminal::Clear(ClearType::CurrentLine),
            SetBackgroundColor(Color::Black),
            SetForegroundColor(Color::White),
            Print(padded),
            ResetColor,
            cursor::MoveTo((label.len() + val.len()).min(cols-1) as u16, status_y(rows)),
            cursor::Show,
        )?;
        out.flush()?;

        if let Event::Key(k) = event::read()? {
            if !matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) { continue; }
            match k.code {
                KeyCode::Enter     => return Ok(Some(val)),
                KeyCode::Esc       => return Ok(None),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(None),
                KeyCode::Backspace => { val.pop(); }
                KeyCode::Char(c)   if k.modifiers.is_empty() || k.modifiers == KeyModifiers::SHIFT
                                   => val.push(c),
                _ => {}
            }
        }
    }
}

// Help screen

fn show_help(out: &mut impl Write, rows: usize, cols: usize) -> io::Result<()> {
    const LINES: &[&str] = &[
        "",
        "  nano 1.0  —  Keyboard Reference",
        "  ──────────────────────────────────────────────────────",
        "",
        "  FILE",
        "  ^S               Save current file (no prompt)",
        "  ^O  Write Out    Save as (prompts for filename)",
        "  ^R  Read File    Insert another file at cursor",
        "  ^X  Exit         Exit nano",
        "",
        "  EDITING",
        "  ^K  Cut          Cut current line to cut buffer",
        "  M-6 Copy         Copy current line to cut buffer",
        "  ^U  Paste        Paste cut buffer before cursor",
        "  ^J  Justify      Reflow current paragraph",
        "  M-U Undo         Undo last action",
        "  M-E Redo         Redo last undone action",
        "",
        "  SEARCH / REPLACE",
        "  ^W  Where Is     Search forward",
        "  ^\\  Replace       Search and replace",
        "  ^C  Location     Show cursor position",
        "  ^T  Execute      Run a command",
        "",
        "  NAVIGATION",
        "  Arrow keys       Move cursor",
        "  ^A / Home        Beginning of line",
        "  ^E / End         End of line",
        "  ^P / ^N          Previous / next line",
        "  ^Y / ^V / PgUp/Dn  Page up / page down",
        "  Ctrl+Left/Right  Jump one word",
        "  ^Home / ^End     First / last line",
        "  ^/  Go To Line   Jump to line number",
        "  ^L               Center cursor on screen",
        "  ^G               This help screen",
        "",
        "  Press any key to return.",
    ];

    execute!(out, terminal::Clear(ClearType::All))?;
    for (i, line) in LINES.iter().enumerate() {
        if i >= rows { break; }
        let padded = format!("{:<width$}", pad_or_trunc(line, cols), width = cols);
        queue!(out, cursor::MoveTo(0, i as u16), Print(padded))?;
    }
    out.flush()?;
    loop {
        if let Event::Key(k) = event::read()? {
            if matches!(k.kind, KeyEventKind::Press) { break; }
        }
    }
    Ok(())
}

// Event loop

fn run(mut e: Editor) -> io::Result<()> {
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide)?;

    if e.filename.is_none() {
        e.msg("New Buffer  [^G for help]");
    } else {
        let n = e.lines.len();
        let f = e.display_name();
        e.msg(format!("Read {} line(s)  \"{}\"", n, f));
    }

    e.scroll();
    draw(&e, &mut out)?;

    while !e.quit {
        let had_event = event::poll(Duration::from_secs(1))?;

        if had_event {
            match event::read()? {
                Event::Resize(c, r) => {
                    e.cols = c as usize;
                    e.rows = r as usize;
                }
                Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    handle_key(&mut e, k, &mut out)?;
                }
                _ => {}
            }
        }

        // Sync terminal size in case of resize event or OS notification
        if let Ok((c, r)) = terminal::size() {
            e.cols = c as usize; e.rows = r as usize;
        }
        e.scroll();
        draw(&e, &mut out)?;
    }

    execute!(out, terminal::LeaveAlternateScreen, cursor::Show)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

// Key dispatch

fn handle_key(e: &mut Editor, k: KeyEvent, out: &mut impl Write) -> io::Result<()> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt  = k.modifiers.contains(KeyModifiers::ALT);

    // Alt-key bindings
    if alt && !ctrl {
        match k.code {
            KeyCode::Char('u') | KeyCode::Char('U') => { e.do_undo(); return Ok(()); }
            KeyCode::Char('e') | KeyCode::Char('E') => { e.do_redo(); return Ok(()); }
            KeyCode::Char('6') | KeyCode::Char('^') => { e.copy();    return Ok(()); }
            // Ctrl+Home / Ctrl+End
            KeyCode::Char('<') | KeyCode::Char(',') => { e.first_line(); return Ok(()); }
            KeyCode::Char('>') | KeyCode::Char('.') => { e.last_line();  return Ok(()); }
            _ => {}
        }
    }

    if ctrl {
        match k.code {
            KeyCode::Home => { e.first_line(); return Ok(()); }
            KeyCode::End  => { e.last_line();  return Ok(()); }
            _ => {}
        }
    }

    match k.code {
        KeyCode::Up       => {
            if ctrl { /* to_prev_block: skip for now, just move up */ e.up(); }
            else    { e.up(); }
        }
        KeyCode::Down     => { e.down(); }
        KeyCode::Left     => {
            if ctrl { e.word_back(); } else { e.left(); }
        }
        KeyCode::Right    => {
            if ctrl { e.word_fwd(); } else { e.right(); }
        }
        KeyCode::Home     => e.home(),
        KeyCode::End      => e.end(),
        KeyCode::PageUp   => e.page_up(),
        KeyCode::PageDown => e.page_down(),

        KeyCode::Char('a') if ctrl => e.home(),   // ^A  beginning of line
        KeyCode::Char('b') if ctrl => e.home(),   // ^B  beginning of line
        KeyCode::Char('e') if ctrl => e.end(),    // ^E  end of line
        KeyCode::Char('p') if ctrl => e.up(),     // ^P  previous line
        KeyCode::Char('n') if ctrl => e.down(),   // ^N  next line
        KeyCode::Char('y') if ctrl => e.page_up(),
        KeyCode::Char('v') if ctrl => e.page_down(),
        KeyCode::Char('f') if ctrl => e.right(),  // ^F  forward one char

        // ^L — center cursor and refresh
        KeyCode::Char('l') if ctrl => {
            e.center_scroll();
            e.msg(format!("line {}, col {}", e.cy+1, e.cx+1));
        }

        // Editing
        KeyCode::Enter                    => e.newline(),
        KeyCode::Tab                      => e.insert_tab(),
        KeyCode::Backspace                => e.backspace(),
        KeyCode::Delete                   => e.delete(),
        KeyCode::Char('h') if ctrl        => e.backspace(),
        KeyCode::Char('d') if ctrl        => e.delete(),
        KeyCode::Char('k') if ctrl        => e.cut(),
        KeyCode::Char('u') if ctrl        => e.paste(),
        KeyCode::Char('j') if ctrl        => {
            let w = e.cols.min(FILL_WIDTH);
            e.justify(w);
        }

        // File operations
        KeyCode::Char('s') if ctrl => {
            if let Some(path) = e.filename.clone() {
                if let Err(err) = e.save_to(&path) {
                    e.msg(format!("Error saving: {}", err));
                }
            } else {
                write_out(e, out)?;
            }
        }

        // ^O - Write Out (save with filename prompt)
        KeyCode::Char('o') if ctrl => { write_out(e, out)?; }

        // ^R - Read File (insert file)
        KeyCode::Char('r') if ctrl => {
            let rows  = e.rows;
            let cols  = e.cols;
            if let Some(name) = prompt("Insert file: ", "", out, rows, cols)? {
                if name.is_empty() {
                    e.msg("Cancelled.");
                } else {
                    match e.insert_file(&name) {
                        Ok(_)    => {}
                        Err(err) => e.msg(format!("Error: {}", err)),
                    }
                }
            } else {
                e.msg("Cancelled.");
            }
        }

        // ^W - Where Is
        KeyCode::Char('w') if ctrl => { do_search(e, out)?; }

        // ^\ - Replace
        KeyCode::Char('\\') if ctrl => { do_replace(e, out)?; }

        // ^T - Execute
        KeyCode::Char('t') if ctrl => {
            let rows = e.rows;
            let cols = e.cols;
            if let Some(cmd) = prompt("Command to execute: ", "", out, rows, cols)? {
                if cmd.is_empty() { e.msg("Cancelled."); }
                else { run_command(e, &cmd); }
            }
        }

        // ^C - Location
        KeyCode::Char('c') if ctrl => {
            let total = e.lines.len();
            let pct   = if total == 0 { 0 } else { ((e.cy+1) * 100) / total };
            let bytes: usize = e.lines[..e.cy].iter().map(|l| l.len()+1).sum::<usize>()
                + Editor::b2c(&e.lines[e.cy], e.cx);
            e.msg(format!(
                "line {}/{} ({}%), col {}/{}, char {}",
                e.cy+1, total, pct,
                e.cx+1, e.clen(e.cy)+1,
                bytes
            ));
        }

        // ^G - Help
        KeyCode::Char('g') if ctrl => {
            show_help(out, e.rows, e.cols)?;
        }

        // ^/ or ^_ - Go To Line
        KeyCode::Char('/') if ctrl | ctrl => {
            let rows = e.rows;
            let cols = e.cols;
            if let Some(inp) = prompt("Enter line number, column number: ", "", out, rows, cols)? {
                let parts: Vec<&str> = inp.splitn(2, ',').collect();
                if let Ok(n) = parts[0].trim().parse::<usize>() {
                    e.go_to_line(n);
                    if let Some(col_s) = parts.get(1) {
                        if let Ok(c) = col_s.trim().parse::<usize>() {
                            e.cx = (c.saturating_sub(1)).min(e.clen(e.cy));
                        }
                    }
                    e.msg(format!("Moved to line {}.", n));
                } else {
                    e.msg("Invalid line number.");
                }
            }
        }
        KeyCode::Char('_') if ctrl => {
            let rows = e.rows;
            let cols = e.cols;
            if let Some(inp) = prompt("Enter line number, column number: ", "", out, rows, cols)? {
                let parts: Vec<&str> = inp.splitn(2, ',').collect();
                if let Ok(n) = parts[0].trim().parse::<usize>() {
                    e.go_to_line(n);
                    if let Some(col_s) = parts.get(1) {
                        if let Ok(c) = col_s.trim().parse::<usize>() {
                            e.cx = (c.saturating_sub(1)).min(e.clen(e.cy));
                        }
                    }
                    e.msg(format!("Moved to line {}.", n));
                } else {
                    e.msg("Invalid line number.");
                }
            }
        }

        // ^X - Exit
        KeyCode::Char('x') if ctrl => { do_exit(e, out)?; }

        // Regular characters
        KeyCode::Char(c) if !ctrl && !alt => { e.insert_char(c); }

        _ => {}
    }
    Ok(())
}

// Command helpers

fn write_out(e: &mut Editor, out: &mut impl Write) -> io::Result<()> {
    let rows  = e.rows;
    let cols  = e.cols;
    let fname = e.filename.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
    if let Some(name) = prompt("File Name to Write: ", &fname, out, rows, cols)? {
        if name.is_empty() {
            e.msg("Cancelled.");
        } else {
            let path = PathBuf::from(&name);
            e.filename = Some(path.clone());
            match e.save_to(&path) {
                Ok(_)    => {}
                Err(err) => e.msg(format!("Error writing file: {}", err)),
            }
        }
    } else {
        e.msg("Cancelled.");
    }
    Ok(())
}

fn do_search(e: &mut Editor, out: &mut impl Write) -> io::Result<()> {
    let rows = e.rows;
    let cols = e.cols;
    let prev = e.search.clone();
    let lbl  = if prev.is_empty() {
        "Search: ".to_owned()
    } else {
        format!("Search [{}]: ", prev)
    };
    let term = match prompt(&lbl, "", out, rows, cols)? {
        Some(s) if !s.is_empty() => { e.search = s.clone(); s }
        Some(_)  => { if prev.is_empty() { e.msg("Cancelled."); return Ok(()); } prev }
        None     => { e.msg("Cancelled."); return Ok(()); }
    };

    let sc = e.cx + 1;
    let (sr, sc) = if sc <= e.clen(e.cy) { (e.cy, sc) }
                   else { ((e.cy+1) % e.lines.len(), 0) };

    if let Some((ry, rx)) = e.find(&term, sr, sc) {
        e.cy = ry; e.cx = rx;
        e.msg(format!("\"{}\"  (^W to find next)", term));
    } else {
        e.msg(format!("\"{}\"  not found", term));
    }
    Ok(())
}

fn do_replace(e: &mut Editor, out: &mut impl Write) -> io::Result<()> {
    let rows = e.rows;
    let cols = e.cols;

    let prev = e.search.clone();
    let lbl  = if prev.is_empty() { "Search: ".to_owned() }
               else { format!("Search [{}]: ", prev) };
    let term = match prompt(&lbl, "", out, rows, cols)? {
        Some(s) if !s.is_empty() => { e.search = s.clone(); s }
        Some(_)  => { if prev.is_empty() { e.msg("Cancelled."); return Ok(()); } prev }
        None     => { e.msg("Cancelled."); return Ok(()); }
    };

    let repl = match prompt("Replace with: ", "", out, rows, cols)? {
        Some(s) => s,
        None    => { e.msg("Cancelled."); return Ok(()); }
    };

    let mut count   = 0usize;
    let mut row     = 0;
    let mut col     = 0;
    let mut replace_all = false;

    loop {
        let found = e.find(&term, row, col);
        let Some((fy, fx)) = found else { break; };

        if !replace_all {
            e.cy = fy; e.cx = fx;
            e.scroll();
            draw(e, out)?;
            let ans = match prompt(
                &format!("Replace this instance? [Y/N/A=All/^C=cancel] "),
                "", out, rows, cols,
            )? {
                Some(s) => s.to_lowercase(),
                None    => { e.msg(format!("Replaced {} instance(s).", count)); return Ok(()); }
            };
            match ans.as_str() {
                "y" => {}
                "n" => { col = fx + 1; row = fy; if col > e.clen(fy) { row += 1; col = 0; } continue; }
                "a" => { replace_all = true; }
                _   => { e.msg(format!("Replaced {} instance(s).", count)); return Ok(()); }
            }
        }

        // Do the replacement
        e.snap();
        let line  = &e.lines[fy];
        let chars: Vec<char> = line.chars().collect();
        let tlen  = term.chars().count();
        let before: String = chars[..fx].iter().collect();
        let after:  String = chars[fx+tlen..].iter().collect();
        e.lines[fy] = format!("{}{}{}", before, repl, after);
        e.modified  = true;

        count += 1;
        col    = fx + repl.chars().count();
        row    = fy;
        if col > e.clen(fy) { row += 1; col = 0; }
    }

    e.msg(format!("Replaced {} instance(s).", count));
    Ok(())
}

fn run_command(e: &mut Editor, cmd: &str) {
    match std::process::Command::new("cmd")
        .args(["/C", cmd])
        .output()
    {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let new_lines: Vec<String> = text.lines().map(str::to_owned).collect();
            let count = new_lines.len();
            e.snap();
            for (i, ln) in new_lines.into_iter().enumerate() {
                e.lines.insert(e.cy + 1 + i, ln);
            }
            e.cy += 1; e.cx = 0;
            e.modified = true;
            e.msg(format!("Inserted {} line(s) from command.", count));
        }
        Err(err) => e.msg(format!("Command failed: {}", err)),
    }
}

fn do_exit(e: &mut Editor, out: &mut impl Write) -> io::Result<()> {
    if !e.modified { e.quit = true; return Ok(()); }

    let rows = e.rows;
    let cols = e.cols;
    let ans  = prompt(
        "Save modified buffer? (Y=Yes, N=No, ^C=Cancel) ",
        "", out, rows, cols,
    )?;
    match ans.as_deref() {
        Some(a) if a.eq_ignore_ascii_case("y") => {
            write_out(e, out)?;
            if !e.modified { e.quit = true; }
        }
        Some(a) if a.eq_ignore_ascii_case("n") => { e.quit = true; }
        _ => { e.msg("Cancelled."); }
    }
    Ok(())
}

// Utilities

fn pad_or_trunc(s: &str, w: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= w { chars[..w].iter().collect() }
    else { format!("{}{}", s, " ".repeat(w - chars.len())) }
}

// Entry point

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: nano [file]");
        println!("  ^G  Help (inside editor)  ^X  Exit  ^O  Write Out  ^W  Search");
        return;
    }

    let mut ed = Editor::new();
    if let Some(path) = args.get(1) {
        if let Err(err) = ed.load(path) {
            if err.kind() == io::ErrorKind::NotFound {
                ed.filename = Some(PathBuf::from(path));
                ed.msg(format!("New File"));
            } else {
                eprintln!("nano: {}: {}", path, err);
                std::process::exit(1);
            }
        }
    }

    if let Err(err) = run(ed) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
        eprintln!("nano: {}", err);
        std::process::exit(1);
    }
}

