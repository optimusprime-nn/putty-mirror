//! The terminal: alacritty's emulator, plus what the deck needs from it.

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, CursorStyle, NamedColor, Processor};
use crate::filter::{Action, Filter, FilterConfig};
use encoding_rs::{Decoder, Encoder, Encoding, UTF_8};
use crate::settings::Settings;
use std::sync::{Arc, Mutex};

pub type Rgb = (u8, u8, u8);

/// PuTTY's 22 colours, in the order of its `Colour0` to `Colour21` settings.
/// The longest title shown, in characters.
const MAX_TITLE: usize = 512;

const DEFAULT_COLOURS: [Rgb; 22] = [
    (187, 187, 187), (255, 255, 255), (0, 0, 0), (85, 85, 85), (0, 0, 0), (0, 255, 0),
    (0, 0, 0), (85, 85, 85), (187, 0, 0), (255, 85, 85), (0, 187, 0), (85, 255, 85),
    (187, 187, 0), (255, 255, 85), (0, 0, 187), (85, 85, 255), (187, 0, 187), (255, 85, 255),
    (0, 187, 187), (85, 255, 255), (187, 187, 187), (255, 255, 255),
];

/// A session's colours, and how bold is shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    fg: Rgb,
    bold_fg: Rgb,
    bg: Rgb,
    pub cursor: Rgb,
    ansi: [Rgb; 16],
    bold_as_colour: bool,
    /// Whether bold text also uses the bold font (`BoldAsColour` 0 or 2).
    pub bold_font: bool,
}

impl Palette {
    pub fn from_settings(s: &Settings) -> Palette {
        let mut c = DEFAULT_COLOURS;
        for (i, slot) in c.iter_mut().enumerate() {
            let text = s.s(&format!("Colour{i}"), "");
            let parts: Vec<u8> = text.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            if let [r, g, b] = parts[..] {
                *slot = (r, g, b);
            }
        }
        // PuTTY keeps each normal colour next to its bold form
        let mut ansi = [(0, 0, 0); 16];
        for i in 0..8 {
            ansi[i] = c[6 + i * 2];
            ansi[i + 8] = c[7 + i * 2];
        }
        Palette { fg: c[0], bold_fg: c[1], bg: c[2], cursor: c[5], ansi, bold_as_colour: s.i("BoldAsColour", 1) != 0, bold_font: s.i("BoldAsColour", 1) != 1 }
    }

    /// `UseSystemColours`: Windows's own text and window colours.
    pub fn use_system(&mut self, fg: Rgb, bg: Rgb) {
        self.fg = fg;
        self.bold_fg = fg;
        self.bg = bg;
    }

    pub fn background(&self) -> Rgb {
        self.bg
    }

    fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi[i as usize],
            _ => cube_or_grey(i),
        }
    }

    fn resolve(&self, c: Color, bold: bool, default: Rgb) -> Rgb {
        let brighten = bold && self.bold_as_colour;
        match c {
            Color::Spec(c) => (c.r, c.g, c.b),
            Color::Indexed(i) if brighten && i < 8 => self.ansi[i as usize + 8],
            Color::Indexed(i) => self.indexed(i),
            Color::Named(n) => {
                let i = n as usize;
                if i < 16 {
                    self.ansi[if brighten && i < 8 { i + 8 } else { i }]
                } else if n == NamedColor::Background {
                    self.bg
                } else {
                    default
                }
            }
        }
    }
}

fn cube_or_grey(i: u8) -> Rgb {
    match i {
        16..=231 => {
            let i = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (step(i / 36), step((i / 6) % 6), step(i % 6))
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            (g, g, g)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderCell {
    pub c: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub underline: bool,
    pub wide: bool,
}

pub struct Screen {
    pub rows: Vec<Vec<RenderCell>>,
    pub cursor: Option<(usize, usize)>,
}

/// What the emulator wants done, collected for the owner to act on.
#[derive(Debug, PartialEq)]
pub enum TermEvent {
    /// Bytes to send back to the host, such as a reply to a query.
    Reply(Vec<u8>),
    Title(String),
    Bell,
    Print(Vec<u8>),
    PrintEnd,
}

/// `PasteRTF`: text as rich text in a font, for pasting into a word processor.
pub fn rtf(text: &str, font: &str, points: u32) -> String {
    let safe_font: String = font.chars().filter(|c| c.is_ascii_alphanumeric() || *c == ' ').collect();
    let mut out = format!("{{\\rtf1\\ansi\\deff0{{\\fonttbl{{\\f0\\fmodern {safe_font};}}}}\\f0\\fs{} ", points * 2);
    for c in text.chars() {
        match c {
            '\\' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out += "\\par\n",
            '\r' => {}
            c if (c as u32) < 0x80 => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out += &format!("\\u{}?", *u as i16);
                }
            }
        }
    }
    out.push('}');
    out
}

#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<TermEvent>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let e = match event {
            Event::PtyWrite(s) => TermEvent::Reply(s.into_bytes()),
            Event::Title(t) => TermEvent::Title(clean_title(&t)),
            Event::Bell => TermEvent::Bell,
            _ => return,
        };
        self.0.lock().unwrap().push(e);
    }
}

pub struct Terminal {
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
    anchor: Option<Point>,
    dragged: bool,
    kind: SelectionType,
    palette: Palette,
    mouse_reports: bool,
    bracketed_paste: bool,
    filter: Filter,
    decoder: Option<Decoder>,
    encoder: Option<Encoder>,
    title: String,
    scroll_on_output: bool,
    scroll_on_key: bool,
    paste_controls: bool,
    rect_select: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CursorKind {
    Block,
    Underline,
    Bar,
}

fn cursor_style(s: &Settings) -> CursorStyle {
    let shape = match s.i("CurType", 0) {
        1 => CursorShape::Underline,
        2 => CursorShape::Beam,
        _ => CursorShape::Block,
    };
    CursorStyle { shape, blinking: s.b("BlinkCur", false) }
}

/// A title can't be allowed to end the report it's sent back in, or to reorder
/// itself into something else in the session list.
fn clean_title(t: &str) -> String {
    t.chars().filter(|c| !c.is_control() && !crate::session::is_direction_mark(*c)).take(MAX_TITLE).collect()
}

/// `LineCodePage` is PuTTY's description of a character set, such as
/// "ISO-8859-1:1998 (Latin-1, West Europe)" or "Win1252 (Western)". Anything
/// not recognised is treated as UTF-8.
fn line_encoding(name: &str) -> &'static Encoding {
    let label = name.split([':', ' ']).next().unwrap_or("").to_lowercase();
    let label = match label.strip_prefix("win") {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => format!("windows-{rest}"),
        _ => label,
    };
    Encoding::for_label(label.as_bytes()).unwrap_or(UTF_8)
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Terminal {
        Terminal::with_settings(cols, rows, &Settings::default())
    }

    pub fn with_settings(cols: usize, rows: usize, s: &Settings) -> Terminal {
        let events = Listener::default();
        let config = Config {
            scrolling_history: s.i("ScrollbackLines", 10_000).min(1_000_000) as usize,
            // A remote host must not be able to touch the clipboard.
            osc52: Osc52::Disabled,
            ..Config::default()
        };
        let config = Config { default_cursor_style: cursor_style(s), ..config };
        let term = Term::new(config, &TermSize::new(cols.max(1), rows.max(1)), events.clone());
        let encoding = line_encoding(&s.s("LineCodePage", ""));
        let mut t = Terminal {
            term,
            parser: Processor::new(),
            filter: Filter::new(FilterConfig::new(s)),
            decoder: (encoding != UTF_8).then(|| encoding.new_decoder_without_bom_handling()),
            encoder: (encoding != UTF_8).then(|| encoding.new_encoder()),
            title: String::new(),
            scroll_on_output: s.b("ScrollOnDisp", true),
            scroll_on_key: s.b("ScrollOnKey", false),
            paste_controls: s.b("PasteControls", false),
            rect_select: s.b("RectSelect", false),
            events,
            anchor: None,
            dragged: false,
            kind: SelectionType::Simple,
            palette: Palette::from_settings(s),
            mouse_reports: !s.b("NoMouseReporting", false),
            bracketed_paste: !s.b("DisableBracketedPaste", false),
        };
        // the settings that are modes a host could also set later
        let mut start = Vec::new();
        for (key, default, sequence) in [
            ("AutoWrapMode", true, &b"\x1b[?7l"[..]),
            ("DECOriginMode", false, &b"\x1b[?6l"[..]),
            ("ApplicationCursorKeys", false, &b"\x1b[?1l"[..]),
            ("ApplicationKeypad", false, &b"\x1b>"[..]),
        ] {
            let on = s.b(key, default);
            if on != default {
                // a mode turned on differs only in its last byte
                let mut seq = sequence.to_vec();
                match seq.last_mut() {
                    Some(b @ b'l') if on => *b = b'h',
                    Some(b @ b'>') if on => *b = b'=',
                    _ => {}
                }
                start.extend(seq);
            }
        }
        t.parser.advance(&mut t.term, &start);
        t
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let decoded;
        let bytes = match &mut self.decoder {
            Some(d) => {
                let mut text = String::with_capacity(bytes.len() * 3 + 16);
                let _ = d.decode_to_string(bytes, &mut text, false);
                decoded = text;
                decoded.as_bytes()
            }
            None => bytes,
        };
        let mut actions = Vec::new();
        let filtered = self.filter.feed(bytes, &mut actions);
        self.parser.advance(&mut self.term, &filtered);
        for a in actions {
            let reply = match a {
                Action::Reply(b) => b,
                Action::ReportTitle => format!("\x1b]l{}\x1b\\", clean_title(&self.title)).into_bytes(),
                Action::Print(b) => {
                    self.events.0.lock().unwrap().push(TermEvent::Print(b));
                    continue;
                }
                Action::PrintEnd => {
                    self.events.0.lock().unwrap().push(TermEvent::PrintEnd);
                    continue;
                }
            };
            self.events.0.lock().unwrap().push(TermEvent::Reply(reply));
        }
        if self.scroll_on_output {
            self.scroll_to_bottom();
        }
    }

    /// Text typed or pasted, in the session's character set.
    pub fn encode_text(&mut self, text: &str) -> Vec<u8> {
        let Some(e) = &mut self.encoder else { return text.as_bytes().to_vec() };
        let mut out = Vec::with_capacity(text.len() * 2 + 8);
        let _ = e.encode_from_utf8_to_vec(text, &mut out, false);
        out
    }

    pub fn scroll_on_key(&self) -> bool {
        self.scroll_on_key
    }

    pub fn cursor_style(&self) -> (CursorKind, bool) {
        let style = self.term.cursor_style();
        let kind = match style.shape {
            CursorShape::Underline => CursorKind::Underline,
            CursorShape::Beam => CursorKind::Bar,
            _ => CursorKind::Block,
        };
        (kind, style.blinking)
    }

    /// Where the view is in the scrollback, for a scrollbar: lines of history,
    /// and how far up from the bottom the view is.
    pub fn scroll_position(&self) -> (usize, usize) {
        let g = self.term.grid();
        (g.history_size(), g.display_offset())
    }

    pub fn scroll_to(&mut self, from_bottom: usize) {
        let now = self.term.grid().display_offset() as i32;
        self.term.scroll_display(Scroll::Delta(from_bottom as i32 - now));
    }

    pub fn take_events(&mut self) -> Vec<TermEvent> {
        let events = std::mem::take(&mut *self.events.0.lock().unwrap());
        for e in &events {
            if let TermEvent::Title(t) = e {
                self.title = t.clone();
            }
        }
        events
    }

    pub fn size(&self) -> (usize, usize) {
        (self.term.columns(), self.term.screen_lines())
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.term.resize(TermSize::new(cols.max(1), rows.max(1)));
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn palette_mut(&mut self) -> &mut Palette {
        &mut self.palette
    }

    /// What a mouse event should be sent as, if the program asked for them.
    /// `button` is 0 left, 1 middle, 2 right, 64 wheel up, 65 wheel down.
    pub fn mouse_report(&self, button: u8, col: usize, row: usize, pressed: bool, motion: bool) -> Option<Vec<u8>> {
        let mode = *self.term.mode();
        if !self.mouse_reports {
            return None;
        }
        let wanted = if motion {
            mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
        } else {
            mode.intersects(TermMode::MOUSE_MODE)
        };
        if !wanted {
            return None;
        }
        let code = button + if motion { 32 } else { 0 };
        let (x, y) = (col + 1, row + 1);
        if mode.contains(TermMode::SGR_MOUSE) {
            let end = if pressed { 'M' } else { 'm' };
            return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
        }
        // the old form has no release of a particular button, and tops out at 223
        let code = if pressed { code } else { 3 };
        let small = |v: usize| (v.min(223) + 32) as u8;
        Some(vec![0x1b, b'[', b'M', code + 32, small(x), small(y)])
    }

    pub fn alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn app_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    fn point(&self, col: usize, row: usize) -> Point {
        let offset = self.term.grid().display_offset() as i32;
        let col = col.min(self.term.columns() - 1);
        Point::new(Line(row as i32 - offset), Column(col))
    }

    pub fn select_start(&mut self, col: usize, row: usize) {
        self.select_start_with(col, row, false);
    }

    /// `flip` is Alt, which swaps between line and rectangular selection.
    pub fn select_start_with(&mut self, col: usize, row: usize, flip: bool) {
        let p = self.point(col, row);
        self.anchor = Some(p);
        self.dragged = false;
        self.kind = if self.rect_select != flip { SelectionType::Block } else { SelectionType::Simple };
        self.term.selection = Some(Selection::new(self.kind, p, Side::Left));
    }

    /// Extends the selection from whichever end is nearer, for the mouse button
    /// PuTTY calls "extend".
    pub fn select_extend(&mut self, col: usize, row: usize) {
        if self.anchor.is_none() {
            self.select_start(col, row);
        }
        self.dragged = true;
        self.select_to(col, row);
    }

    /// Extends a selection. Staying in the first cell selects nothing, so a
    /// click with a slight wobble can't overwrite the clipboard.
    pub fn select_to(&mut self, col: usize, row: usize) {
        let Some(anchor) = self.anchor else { return };
        let p = self.point(col, row);
        self.dragged |= p != anchor;
        if !self.dragged {
            return;
        }
        // both end cells count, whichever way the drag goes
        let (from, to) = if p < anchor { (Side::Right, Side::Left) } else { (Side::Left, Side::Right) };
        let mut sel = Selection::new(self.kind, anchor, from);
        sel.update(p, to);
        self.term.selection = Some(sel);
    }

    pub fn select_word(&mut self, col: usize, row: usize) {
        let p = self.point(col, row);
        self.term.selection = Some(Selection::new(SelectionType::Semantic, p, Side::Left));
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn selected_text(&self) -> Option<String> {
        if !self.has_selection() {
            return None;
        }
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// The whole scrollback and screen as text, keeping any selection.
    pub fn all_text(&mut self) -> String {
        let saved = self.term.selection.take();
        let top = self.term.grid().topmost_line();
        let bottom = self.term.grid().bottommost_line();
        let mut sel = Selection::new(SelectionType::Lines, Point::new(top, Column(0)), Side::Left);
        sel.update(Point::new(bottom, Column(0)), Side::Right);
        self.term.selection = Some(sel);
        let text = self.term.selection_to_string().unwrap_or_default();
        self.term.selection = saved;
        text.trim_end().to_string() + "\n"
    }

    /// The bytes to send for pasted text. Line ends become Enter, and in
    /// bracketed paste an escape is removed so that pasted text can't end the
    /// bracket and run as commands.
    pub fn paste_bytes(&mut self, text: &str) -> Vec<u8> {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        // `PasteControls` off: as in PuTTY, only tab and line ends get through
        let text: String = if self.paste_controls {
            text
        } else {
            text.chars().filter(|&c| !c.is_control() || c == '\t' || c == '\r').collect()
        };
        if self.bracketed_paste && self.term.mode().contains(TermMode::BRACKETED_PASTE) {
            let clean: String = text.chars().filter(|&c| c != '\x1b').collect();
            let mut out = b"\x1b[200~".to_vec();
            out.extend(self.encode_text(&clean));
            out.extend(b"\x1b[201~");
            out
        } else {
            self.encode_text(&text)
        }
    }

    /// Whether pasted line ends would run as commands, not go in as one paste.
    pub fn paste_runs_lines(&self, text: &str) -> bool {
        let bracketed = self.bracketed_paste && self.term.mode().contains(TermMode::BRACKETED_PASTE);
        !bracketed && text.trim_end().contains(['\r', '\n'])
    }

    /// The session is over, so nothing more goes to a printer.
    pub fn stop_printing(&mut self) {
        self.filter.stop_printing();
    }

    pub fn app_keypad(&self) -> bool {
        self.term.mode().contains(TermMode::APP_KEYPAD)
    }

    pub fn screen(&self) -> Screen {
        let content = self.term.renderable_content();
        let (cols, lines) = self.size();
        let p = &self.palette;
        let blank = RenderCell { c: ' ', fg: p.fg, bg: p.bg, bold: false, underline: false, wide: false };
        let mut rows = vec![vec![blank; cols]; lines];
        let offset = content.display_offset as i32;
        let selection = content.selection;
        for item in content.display_iter {
            let cell = item.cell;
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let bold = cell.flags.contains(Flags::BOLD);
            let mut fg = p.resolve(cell.fg, bold, if bold { p.bold_fg } else { p.fg });
            let mut bg = p.resolve(cell.bg, false, p.bg);
            if cell.flags.contains(Flags::DIM) {
                fg = (fg.0 / 3 * 2, fg.1 / 3 * 2, fg.2 / 3 * 2);
            }
            let selected = selection.is_some_and(|s| s.contains(item.point));
            if cell.flags.contains(Flags::INVERSE) != selected {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            let row = item.point.line.0 + offset;
            if (0..lines as i32).contains(&row) {
                rows[row as usize][item.point.column.0] = RenderCell {
                    c: cell.c,
                    fg,
                    bg,
                    bold,
                    underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                    wide: cell.flags.contains(Flags::WIDE_CHAR),
                };
            }
        }
        let c = content.cursor.point;
        let row = c.line.0 + offset;
        let visible = self.term.mode().contains(TermMode::SHOW_CURSOR) && (0..lines as i32).contains(&row);
        Screen { rows, cursor: visible.then_some((c.column.0, row as usize)) }
    }

    /// One screen row as plain text, for tests.
    pub fn row_text(&self, row: usize) -> String {
        let s = self.screen();
        s.rows[row].iter().map(|c| c.c).collect::<String>().trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Value;

    #[test]
    fn text_reaches_the_screen() {
        let mut t = Terminal::new(40, 10);
        t.feed(b"hello\r\nworld");
        assert_eq!(t.row_text(0), "hello");
        assert_eq!(t.row_text(1), "world");
        assert_eq!(t.screen().cursor, Some((5, 1)));
    }

    #[test]
    fn colours_and_inverse() {
        let mut t = Terminal::new(10, 2);
        t.feed(b"\x1b[31mr\x1b[0;7mi\x1b[0;1;32mb");
        let row = &t.screen().rows[0];
        assert_eq!(row[0].fg, (187, 0, 0));
        assert_eq!((row[1].fg, row[1].bg), ((0, 0, 0), (187, 187, 187)));
        assert_eq!(row[2].fg, (85, 255, 85), "bold brightens");
    }

    #[test]
    fn selection_copies_and_clears() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"copy me please");
        t.select_start(5, 0);
        t.select_to(6, 0);
        assert_eq!(t.selected_text().as_deref(), Some("me"));
        assert_ne!(t.screen().rows[0][5].bg, (0, 0, 0), "selection is highlighted");
        t.clear_selection();
        assert!(!t.has_selection());
    }

    #[test]
    fn a_click_is_not_a_selection() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"text");
        t.select_start(1, 0);
        assert!(!t.has_selection(), "Ctrl+C after a bare click must still interrupt");
    }

    #[test]
    fn a_wobble_within_one_cell_is_still_a_click() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"text");
        t.select_start(1, 0);
        t.select_to(1, 0);
        assert!(!t.has_selection());
        t.select_to(2, 0);
        assert_eq!(t.selected_text().as_deref(), Some("ex"));
        t.select_to(1, 0);
        assert_eq!(t.selected_text().as_deref(), Some("e"), "after a real drag, one cell can be selected");
    }

    #[test]
    fn dragging_backwards_keeps_both_ends() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"copy me please");
        t.select_start(6, 0);
        t.select_to(5, 0);
        assert_eq!(t.selected_text().as_deref(), Some("me"));
        t.feed(b"\r\nsecond");
        t.select_start(2, 1);
        t.select_to(5, 0);
        assert_eq!(t.selected_text().as_deref(), Some("me please\nsec"));
    }

    #[test]
    fn alt_screen_and_cursor_modes() {
        let mut t = Terminal::new(20, 5);
        assert!(!t.alt_screen());
        t.feed(b"\x1b[?1049h\x1b[?1h");
        assert!(t.alt_screen() && t.app_cursor());
        t.feed(b"\x1b[?1049l\x1b[?1l");
        assert!(!t.alt_screen() && !t.app_cursor());
    }

    #[test]
    fn plain_paste_turns_line_ends_into_enter() {
        let mut t = Terminal::new(20, 5);
        assert_eq!(t.paste_bytes("a\nb\r\nc"), b"a\rb\rc");
    }

    #[test]
    fn bracketed_paste_cant_escape_its_bracket() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[?2004h");
        let got = t.paste_bytes("x\x1b[201~rm -rf /\n");
        assert_eq!(got, b"\x1b[200~x[201~rm -rf /\r\x1b[201~".to_vec());
    }

    #[test]
    fn title_bell_and_replies_are_reported() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]0;my title\x07\x07\x1b[6n");
        let e = t.take_events();
        assert!(e.contains(&TermEvent::Title("my title".into())));
        assert!(e.contains(&TermEvent::Bell));
        assert!(e.contains(&TermEvent::Reply(b"\x1b[1;1R".to_vec())));
        assert!(t.take_events().is_empty());
    }

    #[test]
    fn host_cant_write_the_clipboard() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;c;aGVsbG8=\x07");
        assert!(t.take_events().is_empty());
    }

    #[test]
    fn copy_all_includes_scrollback() {
        let mut t = Terminal::new(20, 3);
        for i in 0..10 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        let all = t.all_text();
        assert!(all.starts_with("line0\n"), "{all:?}");
        assert!(all.contains("line9"));
    }

    #[test]
    fn resize_rewraps() {
        let mut t = Terminal::new(10, 3);
        t.feed(b"abcdefghijkl");
        assert_eq!(t.row_text(0), "abcdefghij");
        t.resize(20, 3);
        assert_eq!(t.size(), (20, 3));
        assert_eq!(t.row_text(0), "abcdefghijkl");
    }

    #[test]
    fn selection_follows_scrollback() {
        let mut t = Terminal::new(20, 3);
        for i in 0..10 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        t.scroll(5);
        let top = t.row_text(0);
        t.select_start(0, 0);
        t.select_to(4, 0);
        assert_eq!(t.selected_text().unwrap(), top);
        t.scroll_to_bottom();
    }

    #[test]
    fn colours_come_from_the_session() {
        let mut s = Settings::default();
        s.set("Colour2", crate::settings::Value::Str("10,20,30".into()));
        s.set("Colour8", crate::settings::Value::Str("1,2,3".into()));
        let mut t = Terminal::with_settings(10, 2, &s);
        t.feed(b"\x1b[31mx");
        let cell = t.screen().rows[0][0];
        assert_eq!((cell.fg, cell.bg), ((1, 2, 3), (10, 20, 30)));
    }

    #[test]
    fn mouse_is_only_reported_when_asked_for() {
        let mut t = Terminal::new(40, 10);
        assert_eq!(t.mouse_report(0, 1, 2, true, false), None);
        t.feed(b"\x1b[?1000h");
        assert_eq!(t.mouse_report(0, 1, 2, true, false).unwrap(), vec![0x1b, b'[', b'M', 32, 34, 35]);
        assert_eq!(t.mouse_report(0, 1, 2, true, true), None, "plain clicks mode has no motion");
        t.feed(b"\x1b[?1006h");
        assert_eq!(t.mouse_report(0, 1, 2, false, false).unwrap(), b"\x1b[<0;2;3m");
    }

    fn with(pairs: &[(&str, Value)]) -> Terminal {
        let mut s = Settings::default();
        for (k, v) in pairs {
            s.set(k, v.clone());
        }
        Terminal::with_settings(20, 4, &s)
    }

    #[test]
    fn wrap_can_start_off() {
        let mut t = with(&[("AutoWrapMode", Value::Int(0))]);
        t.feed(&[b'x'; 25]);
        assert_eq!(t.row_text(1), "");
        let mut t = with(&[]);
        t.feed(&[b'x'; 25]);
        assert_eq!(t.row_text(1), "xxxxx");
    }

    #[test]
    fn keypad_and_cursor_modes_can_start_on() {
        let t = with(&[("ApplicationKeypad", Value::Int(1)), ("ApplicationCursorKeys", Value::Int(1))]);
        assert!(t.app_keypad() && t.app_cursor());
        let t = with(&[]);
        assert!(!t.app_keypad() && !t.app_cursor());
    }

    #[test]
    fn a_legacy_character_set_both_ways() {
        let mut t = with(&[("LineCodePage", Value::Str("ISO-8859-1:1998 (Latin-1, West Europe)".into()))]);
        t.feed(&[0xe9]);
        assert_eq!(t.row_text(0), "é");
        assert_eq!(t.encode_text("é"), [0xe9]);
        let mut t = with(&[("LineCodePage", Value::Str("Win1251 (Cyrillic)".into()))]);
        t.feed(&[0xc0]);
        assert_eq!(t.row_text(0), "А");
        assert_eq!(with(&[("LineCodePage", Value::Str("UTF-8".into()))]).encode_text("é"), "é".as_bytes());
    }

    #[test]
    fn output_scrolls_to_the_bottom_unless_told_not_to() {
        let mut t = with(&[]);
        t.feed(&b"l\r\n".repeat(20));
        t.scroll(3);
        t.feed(b"x");
        assert_eq!(t.scroll_position().1, 0);
        let mut t = with(&[("ScrollOnDisp", Value::Int(0))]);
        t.feed(&b"l\r\n".repeat(20));
        t.scroll(3);
        t.feed(b"x");
        assert_eq!(t.scroll_position().1, 3);
    }

    #[test]
    fn pasted_control_characters_are_dropped_by_default() {
        assert_eq!(with(&[]).paste_bytes("a\x1b[1mb\tc\n\x07"), b"a[1mb\tc\r");
        assert_eq!(with(&[("PasteControls", Value::Int(1))]).paste_bytes("a\x07"), b"a\x07");
    }

    #[test]
    fn rectangular_selection() {
        let mut t = with(&[("RectSelect", Value::Int(1))]);
        t.feed(b"abcd\r\nefgh");
        t.select_start_with(1, 0, false);
        t.select_to(2, 1);
        assert_eq!(t.selected_text().unwrap().trim(), "bc\nfg");
        let mut t = with(&[]);
        t.feed(b"abcd\r\nefgh");
        t.select_start_with(1, 0, true);
        t.select_to(2, 1);
        assert_eq!(t.selected_text().unwrap().trim(), "bc\nfg", "Alt flips the default");
    }

    #[test]
    fn cursor_shape_comes_from_the_session_and_the_host() {
        let mut t = with(&[("CurType", Value::Int(2)), ("BlinkCur", Value::Int(1))]);
        assert_eq!(t.cursor_style(), (CursorKind::Bar, true));
        t.feed(b"\x1b[4 q");
        assert_eq!(t.cursor_style(), (CursorKind::Underline, false));
    }

    #[test]
    fn the_real_title_is_reported_when_allowed() {
        let mut t = with(&[("RemoteQTitleAction", Value::Int(2))]);
        t.feed(b"\x1b]0;mine\x07");
        t.take_events();
        t.feed(b"\x1b[21t");
        assert!(t.take_events().contains(&TermEvent::Reply(b"\x1b]lmine\x1b\\".to_vec())));
    }

    #[test]
    fn rich_text_escapes_what_it_must() {
        let r = rtf("a{b}\\\né", "Courier New", 10);
        assert!(r.starts_with("{\\rtf1"));
        assert!(r.contains(r"\fs20 a\{b\}\\\par"), "{r}");
        assert!(r.ends_with("\\u233?}"), "{r}");
        assert!(rtf("x", "Evil}{\\", 10).contains("Evil;"));
    }

    #[test]
    fn wide_characters_are_marked() {
        let mut t = with(&[]);
        t.feed("日a".as_bytes());
        let s = t.screen();
        assert!(s.rows[0][0].wide && !s.rows[0][2].wide);
    }
}
