//! PuTTYDeck: terminal sessions in one window, listed down the left.
#![cfg_attr(all(not(debug_assertions), not(feature = "test-hooks")), windows_subsystem = "windows")]

mod clipboard;
mod printer;
mod settings_window;

use deck_core::compose::{compose, cyrillic};
use deck_core::config::{load_sessions, save_session, target_from, Protocol, Target};
use deck_core::hostkeys::{HostKeyStore, MemoryStore, RegistryStore};
use deck_core::filter::BellLimit;
use deck_core::keys::{shortcut, Key, KeyConfig, Mods, Shortcut};
use deck_core::session::{self, Command, Event};
use deck_core::settings::{Settings, Value};
use deck_core::term::{rtf, CursorKind, TermEvent, Terminal};
use settings_window::Outcome;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
use windows::Win32::System::Diagnostics::Debug::{Beep, MessageBeep};
use windows::Win32::UI::Controls::{SetScrollInfo, ShowScrollBar};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const EM_SETCUEBANNER: u32 = 0x1501;
const WM_DECK_EVENT: u32 = WM_APP + 1;
#[cfg(feature = "test-hooks")]
const WM_TEST_KEY: u32 = WM_APP + 0x10;
#[cfg(feature = "test-hooks")]
const WM_TEST_DUMP: u32 = WM_APP + 0x11;
#[cfg(feature = "test-hooks")]
const WM_TEST_CLIP: u32 = WM_APP + 0x12;
#[cfg(feature = "test-hooks")]
const WM_TEST_SETTINGS: u32 = WM_APP + 0x13;

/// The most one print job takes: a host can otherwise print forever.
const MAX_PRINT_JOB: usize = 64 << 20;
const ID_OPEN: usize = 100;
const ID_LIST: usize = 101;
const ID_SAVED: usize = 102;
const ID_COPY: usize = 103;
const ID_PASTE: usize = 104;
const ID_COPY_ALL: usize = 105;
const ID_PASTE_ALL: usize = 106;
const ID_SETTINGS: usize = 107;
const ID_MENU_DUPLICATE: usize = 110;
const ID_MENU_CLOSE: usize = 111;
const ID_MENU_NEW: usize = 112;
const ID_MENU_EDIT: usize = 113;

const SIDEBAR: i32 = 240;
const PAD: i32 = 6;
const ROW: i32 = 26;
const SCROLL_STEP: i32 = 3;
const TIMER_BLINK: usize = 1;
const TIMER_FLASH: usize = 2;
const BLINK_MS: u32 = 530;
const FLASH_MS: u32 = 100;

type Inbox = Arc<Mutex<Vec<(u64, Event)>>>;

struct Session {
    id: u64,
    target: Target,
    title: Option<String>,
    term: Terminal,
    tx: UnboundedSender<Command>,
    closed: bool,
    bell: bool,
    keys: KeyConfig,
    bells: BellLimit,
    job: Option<printer::Job>,
    /// The job failed or ran too long, so the rest of it is dropped.
    job_dead: bool,
    printed: usize,
}

impl Session {
    fn send(&self, bytes: Vec<u8>) {
        if !self.closed && !bytes.is_empty() {
            let _ = self.tx.send(Command::Input(bytes));
        }
    }
    fn label(&self) -> String {
        let name = self.title.clone().unwrap_or_else(|| self.target.label());
        match (self.closed, self.bell) {
            (true, _) => format!("(ended) {name}"),
            (_, true) => format!("* {name}"),
            _ => name,
        }
    }
}

struct Deck {
    main: HWND,
    content: HWND,
    edit: HWND,
    open: HWND,
    list: HWND,
    saved_list: HWND,
    labels: [HWND; 2],
    buttons: [HWND; 5],
    gui_font: HFONT,
    font: HFONT,
    bold: HFONT,
    wide_font: HFONT,
    wide_bold: HFONT,
    font_in_use: (String, i32),
    cell: (i32, i32),
    dpi: u32,
    sessions: Vec<Session>,
    active: Option<usize>,
    next_id: u64,
    saved: Vec<(String, Target)>,
    inbox: Inbox,
    rt: tokio::runtime::Runtime,
    store: Arc<dyn HostKeyStore>,
    skip_char: bool,
    high_surrogate: Option<u16>,
    selecting: bool,
    focused: bool,
    started: std::time::Instant,
    cursor_off: bool,
    flash: bool,
    pointer_hidden: bool,
    compose_state: u8,
    compose_first: u8,
    full_screen: Option<(WINDOWPLACEMENT, i32)>,
}

static DECK: AtomicPtr<Deck> = AtomicPtr::new(std::ptr::null_mut());

/// The deck. Everything here runs on the UI thread, and window procedures
/// re-enter each other, so this is a raw pointer rather than a RefCell.
fn deck() -> Option<&'static mut Deck> {
    unsafe { DECK.load(Ordering::Acquire).as_mut() }
}

/// SetFocus can bring a window to the front, which a test build must never do.
fn take_focus(hwnd: HWND) {
    if !cfg!(feature = "test-hooks") {
        unsafe {
            let _ = SetFocus(Some(hwnd));
        }
    }
}

/// What's about to be pasted, in full if it's short and saying so if it isn't,
/// so that something past the first lines can't go unseen.
fn paste_preview(text: &str) -> String {
    const SHOWN: usize = 400;
    let (chars, lines) = (text.chars().count(), text.trim_end().lines().count());
    let start: String = text.chars().take(SHOWN).filter(|&c| !c.is_control() || c == '\n').collect();
    if chars > SHOWN {
        format!("{start}\n\n... and {} more characters. {lines} lines in all.", chars - SHOWN)
    } else {
        format!("{start}\n\n{lines} lines in all.")
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Copies text, as rich text too if the session's `PasteRTF` asks for it.
fn copy_out(s: &Settings, text: &str) {
    if s.b("PasteRTF", false) {
        let font = s.s("Font", "");
        let font = if font.is_empty() { "Consolas" } else { &font };
        clipboard::set_with_rtf(text, &rtf(text, font, s.i("FontHeight", 10)));
    } else {
        clipboard::set(text);
    }
}

fn alt_down() -> bool {
    unsafe { GetKeyState(VK_MENU.0 as i32) < 0 }
}
fn ctrl_down() -> bool {
    unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 }
}
fn shift_down() -> bool {
    unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 }
}

fn map_key(vk: u16) -> Option<Key> {
    Some(match VIRTUAL_KEY(vk) {
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_INSERT => Key::Insert,
        VK_DELETE => Key::Delete,
        VK_BACK => Key::Backspace,
        VK_TAB => Key::Tab,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        v if (VK_F1.0..=VK_F20.0).contains(&v.0) => Key::F((v.0 - VK_F1.0 + 1) as u8),
        _ => return None,
    })
}

/// A numeric keypad key as PuTTY's `format_numeric_keypad_key` names it.
fn keypad_key(vk: u16, extended: bool) -> Option<u8> {
    Some(match VIRTUAL_KEY(vk) {
        v if (VK_NUMPAD0.0..=VK_NUMPAD9.0).contains(&v.0) => b'0' + (v.0 - VK_NUMPAD0.0) as u8,
        VK_DECIMAL => b'.',
        VK_ADD => b'+',
        VK_SUBTRACT => b'-',
        VK_MULTIPLY => b'*',
        VK_DIVIDE => b'/',
        VK_NUMLOCK => b'G',
        VK_RETURN if extended => b'\r',
        _ => return None,
    })
}

impl Deck {
    fn px(&self, v: i32) -> i32 {
        v * self.dpi as i32 / 96
    }

    /// The font the active session asks for, or the deck's own.
    fn wanted_font(&self) -> (String, i32) {
        let s = self.active.and_then(|i| self.sessions.get(i)).map(|s| &s.target.settings);
        let name = s.map_or(String::new(), |s| s.s("Font", ""));
        // FontHeight is in points, and 10pt is 13px at 96 dpi
        let points = s.map_or(0, |s| s.i("FontHeight", 0)) as i32;
        let px = if points > 0 { points * 96 / 72 } else { 14 };
        (if name.is_empty() { "Consolas".to_string() } else { name }, px)
    }

    fn make_fonts(&mut self) {
        unsafe {
            for f in [self.font, self.bold, self.gui_font, self.wide_font, self.wide_bold] {
                if !f.is_invalid() {
                    let _ = DeleteObject(f.into());
                }
            }
            // FontQuality: 0 default, 1 antialiased, 2 not, 3 ClearType
            let active = self.active.and_then(|i| self.sessions.get(i));
            let quality = match active.map_or(3, |s| s.target.settings.i("FontQuality", 3)) {
                0 => DEFAULT_QUALITY,
                1 => ANTIALIASED_QUALITY,
                2 => NONANTIALIASED_QUALITY,
                _ => CLEARTYPE_QUALITY,
            };
            let bold_name = active.map_or(String::new(), |s| s.target.settings.s("BoldFont", ""));
            let make = |h: i32, weight: i32, face: PCWSTR| {
                CreateFontW(h, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                    quality, (FF_MODERN.0 | FIXED_PITCH.0) as u32, face)
            };
            let (name, size) = self.wanted_font();
            self.font_in_use = (name.clone(), size);
            let face = wide(&name);
            self.font = make(-self.px(size), FW_NORMAL.0 as i32, PCWSTR(face.as_ptr()));
            // a named bold font is used as it is; otherwise it's the normal one, emboldened
            let bold_face = wide(&bold_name);
            self.bold = if bold_name.is_empty() {
                make(-self.px(size), FW_BOLD.0 as i32, PCWSTR(face.as_ptr()))
            } else {
                make(-self.px(size), FW_NORMAL.0 as i32, PCWSTR(bold_face.as_ptr()))
            };
            // WideFont and WideBoldFont: otherwise wide text uses the usual fonts
            let named = |key: &str| {
                let name = self.active.and_then(|i| self.sessions.get(i)).map_or(String::new(), |s| s.target.settings.s(key, ""));
                let face = wide(&name);
                if name.is_empty() { HFONT::default() } else { make(-self.px(size), FW_NORMAL.0 as i32, PCWSTR(face.as_ptr())) }
            };
            let (wide_font, wide_bold) = (named("WideFont"), named("WideBoldFont"));
            self.wide_font = wide_font;
            self.wide_bold = wide_bold;
            self.gui_font = make(-self.px(13), FW_NORMAL.0 as i32, w!("Segoe UI"));
            let dc = GetDC(Some(self.content));
            let old = SelectObject(dc, self.font.into());
            let mut tm = TEXTMETRICW::default();
            let _ = GetTextMetricsW(dc, &mut tm);
            SelectObject(dc, old);
            ReleaseDC(Some(self.content), dc);
            self.cell = (tm.tmAveCharWidth.max(1), tm.tmHeight.max(1));
            let all = self.labels.iter().chain(&self.buttons).chain([&self.edit, &self.open, &self.list, &self.saved_list]);
            for &h in all {
                SendMessageW(h, WM_SETFONT, Some(WPARAM(self.gui_font.0 as usize)), Some(LPARAM(1)));
            }
        }
    }

    /// `WindowBorder`: the gap in pixels between the edge and the text.
    fn border(&self) -> i32 {
        self.active.and_then(|i| self.sessions.get(i)).map_or(1, |s| s.target.settings.i("WindowBorder", 1) as i32)
    }

    fn grid(&self) -> (u32, u32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.content, &mut r);
        }
        let b = self.border() * 2;
        (((r.right - r.left - b) / self.cell.0).max(1) as u32, ((r.bottom - r.top - b) / self.cell.1).max(1) as u32)
    }

    fn layout(&mut self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.main, &mut r);
        }
        let (w, h) = (r.right, r.bottom);
        let (side, pad, row) = (self.px(SIDEBAR), self.px(PAD), self.px(ROW));
        let inner = side - pad * 2;
        let place = |hwnd: HWND, x, y, cx, cy| unsafe {
            // No copying of old pixels: the panel is repainted whole below
            let _ = SetWindowPos(hwnd, None, x, y, cx, cy, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS);
        };
        let open_w = self.px(52);
        place(self.edit, pad, pad, inner - open_w - pad, row);
        place(self.open, pad + inner - open_w, pad, open_w, row);
        let mut y = pad * 2 + row;
        let bottom = h - pad - row * 3 - pad;
        let lists = (bottom - y - row * 2 - pad).max(0);
        place(self.labels[0], pad, y, inner, row);
        y += row;
        place(self.list, pad, y, inner, lists / 2);
        y += lists / 2 + pad;
        place(self.labels[1], pad, y, inner, row);
        y += row;
        place(self.saved_list, pad, y, inner, lists - lists / 2);
        let half = (inner - pad) / 2;
        for (i, &b) in self.buttons.iter().enumerate() {
            let x = pad + (i as i32 % 2) * (half + pad);
            // Settings has the last row to itself
            let cx = if i == self.buttons.len() - 1 { inner } else { half };
            place(b, x, bottom + pad + (i as i32 / 2) * row, cx, row - 2);
        }
        let full = self.full_screen.is_some();
        let (left, width) = if full { (0, w) } else { (side, (w - side).max(1)) };
        place(self.content, left, 0, width, h);
        for h in self.labels.iter().chain(&self.buttons).chain([&self.edit, &self.open, &self.list, &self.saved_list]) {
            unsafe {
                let _ = ShowWindow(*h, if full { SW_HIDE } else { SW_SHOWNA });
            }
        }
        if !full {
            let panel = RECT { left: 0, top: 0, right: side, bottom: h };
            unsafe {
                let _ = RedrawWindow(Some(self.main), Some(&panel), None, RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW);
            }
        }
        let (cols, rows) = self.grid();
        for s in &mut self.sessions {
            // LockSize 1: the session keeps its size, whatever the window does
            if s.target.settings.i("LockSize", 0) == 1 {
                continue;
            }
            if s.term.size() != (cols as usize, rows as usize) {
                s.term.resize(cols as usize, rows as usize);
                let _ = s.tx.send(Command::Resize { cols, rows });
            }
        }
        self.redraw();
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.content), None, false);
        }
        self.update_scrollbar();
    }

    fn update_scrollbar(&self) {
        let Some(s) = self.active.and_then(|i| self.sessions.get(i)) else { return };
        let settings = &s.target.settings;
        let wanted = if self.full_screen.is_some() { settings.b("ScrollBarFullScreen", false) } else { settings.b("ScrollBar", true) };
        unsafe {
            let _ = ShowScrollBar(self.content, SB_VERT, wanted);
            if wanted {
                let (history, up) = s.term.scroll_position();
                let page = s.term.size().1;
                let info = SCROLLINFO {
                    cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_ALL | SIF_DISABLENOSCROLL,
                    nMin: 0,
                    nMax: (history + page).saturating_sub(1) as i32,
                    nPage: page as u32,
                    nPos: (history - up.min(history)) as i32,
                    nTrackPos: 0,
                };
                SetScrollInfo(self.content, SB_VERT, &info, true);
            }
        }
    }

    fn on_scrollbar(&mut self, wp: WPARAM) {
        let code = SCROLLBAR_COMMAND((wp.0 & 0xffff) as i32);
        let thumb = (wp.0 >> 16) as usize & 0xffff;
        if let Some(s) = self.current() {
            let page = s.term.size().1 as i32;
            let (history, _) = s.term.scroll_position();
            match code {
                SB_LINEUP => s.term.scroll(1),
                SB_LINEDOWN => s.term.scroll(-1),
                SB_PAGEUP => s.term.scroll(page),
                SB_PAGEDOWN => s.term.scroll(-page),
                SB_THUMBTRACK | SB_THUMBPOSITION => s.term.scroll_to(history.saturating_sub(thumb)),
                _ => {}
            }
        }
        self.redraw();
    }

    fn toggle_full_screen(&mut self) {
        unsafe {
            if let Some((placement, style)) = self.full_screen.take() {
                SetWindowLongW(self.main, GWL_STYLE, style);
                let _ = SetWindowPlacement(self.main, &placement);
                let _ = SetWindowPos(self.main, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
            } else {
                let mut placement = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                let _ = GetWindowPlacement(self.main, &mut placement);
                let style = GetWindowLongW(self.main, GWL_STYLE);
                self.full_screen = Some((placement, style));
                let monitor = MonitorFromWindow(self.main, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(monitor, &mut info);
                SetWindowLongW(self.main, GWL_STYLE, style & !(WS_OVERLAPPEDWINDOW.0 as i32));
                let r = info.rcMonitor;
                let _ = SetWindowPos(self.main, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_FRAMECHANGED);
            }
        }
        self.layout();
    }

    /// The window settings that follow whichever session is showing.
    fn apply_window_settings(&mut self) {
        let Some(s) = self.active.and_then(|i| self.sessions.get(i)) else { return };
        let settings = &s.target.settings;
        let (top, edge, left) = (settings.b("AlwaysOnTop", false), settings.b("SunkenEdge", false), settings.b("ScrollbarOnLeft", false));
        unsafe {
            if !cfg!(feature = "test-hooks") {
                let order = if top { HWND_TOPMOST } else { HWND_NOTOPMOST };
                let _ = SetWindowPos(self.main, Some(order), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            let mut ex = 0;
            if edge {
                ex |= WS_EX_CLIENTEDGE.0;
            }
            if left {
                ex |= WS_EX_LEFTSCROLLBAR.0;
            }
            SetWindowLongW(self.content, GWL_EXSTYLE, ex as i32);
            let _ = SetWindowPos(self.content, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        }
    }

    /// What a bell does, after `BellOverload` has had its say.
    fn ring(&mut self, id: u64) {
        let now = self.started.elapsed().as_millis() as u64;
        let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) else { return };
        if !s.bells.ring(now) {
            return;
        }
        let (mode, sound, indicate) = (s.target.settings.i("Beep", 1), s.target.settings.s("BellWaveFile", ""), s.target.settings.i("BeepInd", 0));
        if cfg!(feature = "test-hooks") {
            return;
        }
        unsafe {
            match mode {
                1 => {
                    let _ = MessageBeep(MB_OK);
                }
                2 => {
                    self.flash = true;
                    SetTimer(Some(self.content), TIMER_FLASH, FLASH_MS, None);
                }
                3 => {
                    let name = wide(&sound);
                    let _ = PlaySoundW(PCWSTR(name.as_ptr()), None, SND_ASYNC | SND_FILENAME | SND_NODEFAULT);
                }
                4 => {
                    let _ = Beep(800, 100);
                }
                _ => {}
            }
            if indicate != 0 && GetForegroundWindow() != self.main {
                // 1 flashes until the window is visited, 2 just lights it up
                let flags = if indicate == 1 { FLASHW_TRAY | FLASHW_TIMERNOFG } else { FLASHW_TRAY };
                let info = FLASHWINFO { cbSize: std::mem::size_of::<FLASHWINFO>() as u32, hwnd: self.main, dwFlags: flags, uCount: 1, dwTimeout: 0 };
                let _ = FlashWindowEx(&info);
            }
        }
    }

    fn hide_pointer(&mut self) {
        let hide = self.current().is_some_and(|s| s.target.settings.b("HideMousePtr", false));
        if hide && !self.pointer_hidden && !cfg!(feature = "test-hooks") {
            self.pointer_hidden = true;
            unsafe {
                ShowCursor(false);
            }
        }
    }

    fn show_pointer(&mut self) {
        if std::mem::take(&mut self.pointer_hidden) {
            unsafe {
                ShowCursor(true);
            }
        }
    }

    fn current(&mut self) -> Option<&mut Session> {
        self.active.and_then(|i| self.sessions.get_mut(i))
    }

    fn refresh_list(&self) {
        unsafe {
            SendMessageW(self.list, LB_RESETCONTENT, None, None);
            for s in &self.sessions {
                let text = wide(&s.label());
                SendMessageW(self.list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
            }
            let sel = self.active.map_or(usize::MAX, |i| i);
            SendMessageW(self.list, LB_SETCURSEL, Some(WPARAM(sel)), None);
            let title = match self.active.and_then(|i| self.sessions.get(i)) {
                Some(s) => format!("{} - PuTTYDeck", s.label()),
                None => "PuTTYDeck".to_string(),
            };
            let _ = SetWindowTextW(self.main, PCWSTR(wide(&title).as_ptr()));
        }
    }

    fn open_target(&mut self, target: Target) {
        let id = self.next_id;
        self.next_id += 1;
        let (cols, rows) = self.grid();
        let inbox = self.inbox.clone();
        let main = self.main.0 as isize;
        let sink = Arc::new(move |e: Event| {
            let mut q = inbox.lock().unwrap();
            let wake = q.is_empty();
            q.push((id, e));
            drop(q);
            if wake {
                unsafe {
                    let _ = PostMessageW(Some(HWND(main as *mut _)), WM_DECK_EVENT, WPARAM(0), LPARAM(0));
                }
            }
        });
        let tx = session::spawn(self.rt.handle(), target.clone(), self.store.clone(), (cols, rows), sink);
        let settings = &target.settings;
        // a session with a locked size keeps the one it was saved with
        let locked = settings.i("LockSize", 0) == 1;
        let (tc, tr) = (settings.i("TermWidth", cols), settings.i("TermHeight", rows));
        let (ic, ir) = if locked { (tc, tr) } else { (cols, rows) };
        let mut term = Terminal::with_settings(ic as usize, ir as usize, settings);
        if settings.b("UseSystemColours", false) {
            let colour = |i: SYS_COLOR_INDEX| {
                let c = unsafe { GetSysColor(i) };
                (c as u8, (c >> 8) as u8, (c >> 16) as u8)
            };
            term.palette_mut().use_system(colour(COLOR_WINDOWTEXT), colour(COLOR_WINDOW));
        }
        let title = Some(settings.s("WinTitle", "")).filter(|t| !t.is_empty());
        let (keys, bells) = (KeyConfig::new(settings), BellLimit::new(settings));
        let first = self.sessions.is_empty();
        let sized = settings.has("TermWidth") && settings.has("TermHeight");
        let fit = settings.b("FitWindow", false);
        self.sessions.push(Session { id, target, title, term, tx, closed: false, bell: false, keys, bells, job: None, job_dead: false, printed: 0 });
        self.select(self.sessions.len() - 1);
        // the first session sets the window's size only if it asks to
        if first && fit && sized && self.full_screen.is_none() && !cfg!(feature = "test-hooks") {
            let side = self.px(SIDEBAR);
            let mut r = RECT { left: 0, top: 0, right: side + tc as i32 * self.cell.0 + self.border() * 2, bottom: tr as i32 * self.cell.1 + self.border() * 2 };
            unsafe {
                let _ = AdjustWindowRectEx(&mut r, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE(0));
                let _ = SetWindowPos(self.main, None, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER);
            }
        }
    }

    fn select(&mut self, i: usize) {
        if i >= self.sessions.len() {
            return;
        }
        self.active = Some(i);
        self.sessions[i].bell = false;
        self.apply_window_settings();
        if self.wanted_font() != self.font_in_use {
            self.make_fonts();
            self.layout();
        }
        self.refresh_list();
        self.redraw();
        take_focus(self.content);
    }

    fn step(&mut self, delta: i32) {
        let n = self.sessions.len() as i32;
        if let (Some(i), true) = (self.active, n > 0) {
            self.select((i as i32 + delta).rem_euclid(n) as usize);
        }
    }

    fn close_session(&mut self, i: usize) {
        if i >= self.sessions.len() {
            return;
        }
        let s = self.sessions.remove(i);
        let _ = s.tx.send(Command::Close);
        if self.sessions.is_empty() {
            self.active = None;
            self.refresh_list();
            self.redraw();
        } else {
            let a = self.active.unwrap_or(0);
            let a = if a > i { a - 1 } else { a };
            self.select(a.min(self.sessions.len() - 1));
        }
    }

    fn pump_events(&mut self) {
        let events = std::mem::take(&mut *self.inbox.lock().unwrap());
        let mut list_changed = false;
        let mut finished = Vec::new();
        let mut bells = Vec::new();
        let active_id = self.current().map(|s| s.id);
        for (id, event) in events {
            let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) else { continue };
            match event {
                Event::Output(bytes) | Event::Local(bytes) => s.term.feed(&bytes),
                Event::Closed(why) => {
                    s.term.stop_printing();
                    s.job = None;
                    s.term.feed(format!("\r\n\x1b[1m[{}]\x1b[0m\r\n", deck_core::session::clean(&why)).as_bytes());
                    s.closed = true;
                    list_changed = true;
                    // CloseOnExit is 0 for never, 1 for a clean exit, 2 for always
                    let clean = why == "Server closed the session.";
                    let close = s.target.settings.i("CloseOnExit", 0);
                    if close == 2 || (close == 1 && clean) {
                        finished.push(id);
                    }
                }
            }
            for e in s.term.take_events() {
                match e {
                    TermEvent::Reply(b) => {
                        let _ = s.tx.send(Command::Reply(b));
                    }
                    TermEvent::Title(_) if s.target.settings.b("NoRemoteWinTitle", false) => {}
                    TermEvent::Title(t) => {
                        s.title = Some(t);
                        list_changed = true;
                    }
                    TermEvent::Print(data) => {
                        if s.job_dead {
                            continue;
                        }
                        if s.job.is_none() && !cfg!(feature = "test-hooks") {
                            s.job = printer::Job::start(&s.target.settings.s("Printer", ""));
                            s.job_dead = s.job.is_none() && !cfg!(feature = "test-hooks");
                            s.printed = 0;
                        }
                        s.printed += data.len();
                        if s.printed > MAX_PRINT_JOB {
                            s.job = None;
                            s.job_dead = true;
                        }
                        if let Some(job) = &s.job {
                            job.write(&data);
                        }
                    }
                    TermEvent::PrintEnd => {
                        s.job = None;
                        s.job_dead = false;
                    }
                    TermEvent::Bell => {
                        bells.push(id);
                        if Some(id) != active_id {
                            s.bell = true;
                            list_changed = true;
                        }
                    }
                }
            }
        }
        for id in bells {
            self.ring(id);
        }
        for id in finished {
            if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
                self.close_session(i);
            }
        }
        if list_changed {
            self.refresh_list();
        }
        self.redraw();
    }

    /// Opens the settings window for a saved session, or a new one.
    fn edit_saved(&mut self, index: Option<usize>) {
        let (name, settings) = match index.and_then(|i| self.saved.get(i)) {
            Some((n, t)) => (n.clone(), t.settings.clone()),
            None => (String::new(), Settings::default()),
        };
        let (outcome, name, mut settings) = settings_window::edit(self.main, self.gui_font, &name, settings);
        if outcome == Outcome::Cancelled {
            return;
        }
        if settings.s("Protocol", "").is_empty() {
            settings.set("Protocol", Value::Str("ssh".into()));
        }
        if outcome == Outcome::Save && !name.trim().is_empty() {
            if let Err(e) = save_session(name.trim(), &settings) {
                self.confirm(&format!("Could not save the session: {e}"));
            }
            self.reload_saved();
        }
        if outcome == Outcome::Open {
            self.open_target(target_from(settings));
        }
    }

    fn reload_saved(&mut self) {
        self.saved = load_sessions();
        unsafe {
            SendMessageW(self.saved_list, LB_RESETCONTENT, None, None);
            for (name, _) in &self.saved {
                let text = wide(name);
                SendMessageW(self.saved_list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
            }
        }
    }

    fn copy(&mut self) {
        if let Some(s) = self.current() {
            if let Some(text) = s.term.selected_text() {
                copy_out(&s.target.settings, &text);
                s.term.clear_selection();
            }
        }
        self.redraw();
    }

    fn copy_all(&mut self) {
        if let Some(s) = self.current() {
            {
                let text = s.term.all_text();
                copy_out(&s.target.settings, &text);
            }
        }
    }

    fn paste(&mut self) {
        let Some(text) = clipboard::get() else { return };
        if !self.paste_allowed(&text) {
            return;
        }
        if let Some(s) = self.current() {
            let bytes = s.term.paste_bytes(&text);
            s.term.scroll_to_bottom();
            s.send(bytes);
        }
        self.redraw();
    }

    /// `PasteWarnLines`: text that would run line by line is shown first, as it
    /// may not be the one line that was copied from a page.
    fn paste_allowed(&self, text: &str) -> bool {
        let Some(s) = self.active.and_then(|i| self.sessions.get(i)) else { return true };
        !s.target.settings.b("PasteWarnLines", true)
            || !s.term.paste_runs_lines(text)
            || self.confirm(&format!("Paste these lines? Each one may run as a command.\n\n{}", paste_preview(text)))
    }

    fn confirm(&self, text: &str) -> bool {
        if cfg!(feature = "test-hooks") {
            return true;
        }
        let r = unsafe { MessageBoxW(Some(self.main), PCWSTR(wide(text).as_ptr()), w!("PuTTYDeck"), MB_YESNO | MB_ICONQUESTION) };
        r == IDYES
    }

    fn paste_all(&mut self) {
        let Some(text) = clipboard::get() else { return };
        let live = self.sessions.iter().filter(|s| !s.closed).count();
        if live == 0 {
            return;
        }
        if !self.confirm(&format!("Paste this into all {live} open sessions?\n\n{}", paste_preview(&text))) {
            return;
        }
        for s in &mut self.sessions {
            let bytes = s.term.paste_bytes(&text);
            s.term.scroll_to_bottom();
            s.send(bytes);
        }
        self.redraw();
    }

    /// Handles a key press. True means it was used, so its WM_CHAR is dropped.
    fn key_down(&mut self, vk: u16, ctrl: bool, shift: bool) -> bool {
        self.key_down_with(vk, Mods { shift, ctrl, alt: false }, false)
    }

    fn compose_key(&mut self, vk: u16, extended: bool) -> bool {
        vk == VK_APPS.0 || (vk == VK_MENU.0 && extended && self.current().is_some_and(|s| s.target.settings.b("ComposeKey", false)))
    }

    /// PuTTY's compose sequence: the key goes down and up, then two characters.
    fn compose_key_up(&mut self, vk: u16, extended: bool) {
        if self.compose_state == 1 && self.compose_key(vk, extended) {
            self.compose_state = 2;
        }
    }

    fn key_down_with(&mut self, vk: u16, m: Mods, extended: bool) -> bool {
        let (ctrl, shift) = (m.ctrl, m.shift);
        self.hide_pointer();
        if self.compose_key(vk, extended) {
            self.compose_state = 1;
            return true;
        }
        if self.compose_state == 1 && vk != VK_CONTROL.0 {
            self.compose_state = 0;
        }
        // CtrlAltKeys: left Alt with Ctrl is that, not AltGr. AltGr itself is
        // the right Alt, which layouts report with Ctrl, and is left alone.
        if ctrl && (b'A' as u16..=b'Z' as u16).contains(&vk) && unsafe { GetKeyState(VK_LMENU.0 as i32) } < 0 {
            if let Some(s) = self.current() {
                if s.target.settings.b("CtrlAltKeys", true) {
                    s.send(vec![0x1b, vk as u8 & 0x1f]);
                    return true;
                }
            }
        }
        if ctrl && vk == VK_TAB.0 {
            self.step(if shift { -1 } else { 1 });
            return true;
        }
        if let Some(pad) = keypad_key(vk, extended) {
            if let Some(s) = self.current() {
                let bytes = s.keys.keypad(pad, m, s.term.app_keypad());
                if !bytes.is_empty() {
                    s.send(bytes);
                    self.redraw();
                    return true;
                }
            }
        }
        // Ctrl+Insert and Shift+Insert, which CtrlShiftIns can turn off
        if vk == VK_INSERT.0 && ctrl != shift {
            let on = self.current().map_or(true, |s| s.target.settings.s("CtrlShiftIns", "explicit") != "none");
            if on {
                if ctrl {
                    self.copy();
                } else {
                    self.paste();
                }
                return true;
            }
        }
        let (has_sel, alt) = self.current().map_or((false, false), |s| (s.term.has_selection(), s.term.alt_screen()));
        let action = if vk < 128 { shortcut(vk as u8, ctrl, shift, has_sel, alt) } else { Shortcut::PassThrough };
        match action {
            Shortcut::Copy => self.copy(),
            Shortcut::Paste => self.paste(),
            Shortcut::NewSession => take_focus(self.edit),
            Shortcut::Duplicate => {
                if let Some(t) = self.current().map(|s| s.target.clone()) {
                    self.open_target(t);
                }
            }
            Shortcut::CloseSession => {
                if let Some(i) = self.active {
                    self.close_session(i);
                }
            }
            Shortcut::GoTo(i) => self.select(i),
            Shortcut::PassThrough => {
                let Some(s) = self.current() else { return false };
                if shift && vk == VK_PRIOR.0 {
                    let page = s.term.size().1 as i32;
                    s.term.scroll(page);
                } else if shift && vk == VK_NEXT.0 {
                    let page = s.term.size().1 as i32;
                    s.term.scroll(-page);
                } else if ctrl && vk == VK_SPACE.0 {
                    s.send(vec![0]);
                } else if let Some(key) = map_key(vk) {
                    let mut bytes = s.keys.encode(key, m, s.term.app_cursor());
                    if m.alt && !s.keys.alt_included(key) {
                        bytes.insert(0, 0x1b);
                    }
                    if s.term.scroll_on_key() {
                        s.term.scroll_to_bottom();
                    }
                    s.send(bytes);
                } else {
                    return false;
                }
                self.redraw();
            }
        }
        true
    }

    fn on_char(&mut self, unit: u16) {
        let text = match (self.high_surrogate.take(), unit) {
            (None, 0xD800..=0xDBFF) => {
                self.high_surrogate = Some(unit);
                return;
            }
            (Some(hi), _) => String::from_utf16_lossy(&[hi, unit]),
            (None, _) => String::from_utf16_lossy(&[unit]),
        };
        let ascii = text.as_bytes().first().copied().filter(|b| text.len() == 1 && *b >= b' ' && *b < 0x7f);
        let text = match (self.compose_state, ascii) {
            (2, Some(c)) => {
                self.compose_first = c;
                self.compose_state = 3;
                return;
            }
            (3, Some(c)) => {
                self.compose_state = 0;
                match compose(self.compose_first, c) {
                    Some(done) => done.to_string(),
                    None => {
                        unsafe {
                            let _ = MessageBeep(MB_ICONHAND);
                        }
                        return;
                    }
                }
            }
            _ => {
                self.compose_state = 0;
                self.caps_cyrillic(text)
            }
        };
        if let Some(s) = self.current() {
            if s.term.scroll_on_key() {
                s.term.scroll_to_bottom();
            }
            let bytes = s.term.encode_text(&text);
            s.send(bytes);
        }
        self.redraw();
    }

    /// `CapsLockCyr`: Caps Lock switches to Cyrillic instead of to capitals, so
    /// the case Windows gave a letter is undone first.
    fn caps_cyrillic(&mut self, text: String) -> String {
        let caps = unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0;
        if !caps || !self.current().is_some_and(|s| s.target.settings.b("CapsLockCyr", false)) {
            return text;
        }
        text.chars()
            .map(|c| {
                let c = if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else if c.is_ascii_lowercase() {
                    c.to_ascii_uppercase()
                } else {
                    c
                };
                cyrillic(c)
            })
            .collect()
    }

    /// Alt with another key. True means it's been dealt with here, and false
    /// leaves it to Windows.
    fn sys_key(&mut self, vk: u16, shift: bool, extended: bool) -> bool {
        let Some(s) = self.current() else { return false };
        let settings = s.target.settings.clone();
        if vk == VK_F4.0 && settings.b("AltF4", true) {
            return false;
        }
        if vk == VK_SPACE.0 && settings.b("AltSpace", false) {
            unsafe {
                SendMessageW(self.main, WM_SYSCOMMAND, Some(WPARAM(SC_KEYMENU as usize)), Some(LPARAM(' ' as isize)));
            }
            return true;
        }
        if vk == VK_RETURN.0 && !extended && settings.b("FullScreenOnAltEnter", false) {
            self.toggle_full_screen();
            return true;
        }
        if vk == VK_MENU.0 {
            return false;
        }
        self.skip_char = self.key_down_with(vk, Mods { shift, ctrl: ctrl_down(), alt: true }, extended);
        self.skip_char
    }

    /// `MouseIsXterm`: what the middle and right buttons do. 0 is PuTTY's
    /// default of extend and paste, 1 is xterm's the other way about, and 2 is
    /// Windows style, where the right button opens a menu.
    fn mouse_style(&mut self) -> u32 {
        self.current().map_or(0, |s| s.target.settings.i("MouseIsXterm", 0))
    }

    fn may_paste_with_mouse(&mut self) -> bool {
        self.current().map_or(true, |s| s.target.settings.s("MousePaste", "explicit") != "none")
    }

    fn extend_or_paste(&mut self, extend: bool, lp: LPARAM) {
        if extend {
            let (x, y) = self.cell_at(lp);
            let auto = self.autocopy();
            if let Some(s) = self.current() {
                s.term.select_extend(x, y);
                if let (true, Some(text)) = (auto, s.term.selected_text()) {
                    copy_out(&s.target.settings, &text);
                }
            }
            self.redraw();
        } else if self.may_paste_with_mouse() {
            self.paste();
        }
    }

    fn autocopy(&mut self) -> bool {
        self.current().map_or(true, |s| s.target.settings.b("MouseAutocopy", true))
    }

    fn on_timer(&mut self, id: usize) {
        if id == TIMER_FLASH {
            unsafe {
                let _ = KillTimer(Some(self.content), TIMER_FLASH);
            }
            self.flash = false;
            self.redraw();
        } else if self.current().is_some_and(|s| s.term.cursor_style().1) {
            self.cursor_off = !self.cursor_off;
            unsafe {
                let _ = InvalidateRect(Some(self.content), None, false);
            }
        }
    }

    fn report(&mut self, button: u8, cell: (usize, usize), pressed: bool, motion: bool) -> bool {
        // MouseOverride: Shift keeps the mouse for selecting
        if shift_down() && self.current().map_or(true, |s| s.target.settings.b("MouseOverride", true)) {
            return false;
        }
        let Some(s) = self.current() else { return false };
        match s.term.mouse_report(button, cell.0, cell.1, pressed, motion) {
            Some(bytes) => {
                s.send(bytes);
                true
            }
            None => false,
        }
    }

    fn report_mouse(&mut self, msg: u32, lp: LPARAM) -> bool {
        let (button, pressed) = match msg {
            WM_LBUTTONDOWN => (0, true),
            WM_LBUTTONUP => (0, false),
            WM_MBUTTONDOWN => (1, true),
            WM_MBUTTONUP => (1, false),
            WM_RBUTTONDOWN => (2, true),
            _ => (2, false),
        };
        self.report(button, self.cell_at(lp), pressed, false)
    }

    fn report_drag(&mut self, wp: WPARAM, lp: LPARAM) -> bool {
        // MK_LBUTTON is 1, MK_RBUTTON 2 and MK_MBUTTON 16
        let button = if wp.0 & 1 != 0 { 0 } else if wp.0 & 16 != 0 { 1 } else { 2 };
        self.report(button, self.cell_at(lp), true, true)
    }

    fn report_wheel(&mut self, wp: WPARAM, lp: LPARAM) -> bool {
        let delta = ((wp.0 >> 16) & 0xffff) as i16;
        // the wheel's position is in screen coordinates
        let mut p = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
        unsafe {
            let _ = ScreenToClient(self.content, &mut p);
        }
        let cell = ((p.x / self.cell.0).max(0) as usize, (p.y / self.cell.1).max(0) as usize);
        self.report(if delta > 0 { 64 } else { 65 }, cell, true, false)
    }

    fn cell_at(&self, lp: LPARAM) -> (usize, usize) {
        let b = self.border();
        let x = (lp.0 & 0xffff) as i16 as i32 - b;
        let y = ((lp.0 >> 16) & 0xffff) as i16 as i32 - b;
        ((x / self.cell.0).max(0) as usize, (y / self.cell.1).max(0) as usize)
    }

    fn command(&mut self, id: usize, code: u32) {
        match (id, code) {
            (ID_OPEN, _) => self.open_from_box(),
            (ID_LIST, LBN_SELCHANGE) => {
                let i = unsafe { SendMessageW(self.list, LB_GETCURSEL, None, None) }.0;
                if i >= 0 {
                    self.select(i as usize);
                }
            }
            (ID_SAVED, LBN_DBLCLK) => {
                let i = unsafe { SendMessageW(self.saved_list, LB_GETCURSEL, None, None) }.0;
                if let Some((_, t)) = usize::try_from(i).ok().and_then(|i| self.saved.get(i)) {
                    self.open_target(t.clone());
                }
            }
            (ID_COPY, _) => self.copy(),
            (ID_PASTE, _) => self.paste(),
            (ID_COPY_ALL, BN_CLICKED) => self.copy_all(),
            (ID_PASTE_ALL, BN_CLICKED) => self.paste_all(),
            (ID_MENU_DUPLICATE, _) => {
                if let Some(t) = self.current().map(|s| s.target.clone()) {
                    self.open_target(t);
                }
            }
            (ID_MENU_CLOSE, _) => {
                if let Some(i) = self.active {
                    self.close_session(i);
                }
            }
            (ID_MENU_NEW, _) => self.edit_saved(None),
            (ID_MENU_EDIT, _) | (ID_SETTINGS, BN_CLICKED) => {
                let i = unsafe { SendMessageW(self.saved_list, LB_GETCURSEL, None, None) }.0;
                self.edit_saved(usize::try_from(i).ok());
            }
            _ => {}
        }
    }

    fn open_from_box(&mut self) {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(self.edit, &mut buf) } as usize;
        let text = String::from_utf16_lossy(&buf[..n]);
        if let Some(t) = Target::parse(&text) {
            unsafe {
                let _ = SetWindowTextW(self.edit, w!(""));
            }
            self.open_target(t);
        }
    }

    fn row_from_point(&self, list: HWND, lp: LPARAM) -> Option<usize> {
        let r = unsafe { SendMessageW(list, LB_ITEMFROMPOINT, None, Some(lp)) }.0 as u32;
        ((r >> 16) == 0).then_some((r & 0xffff) as usize)
    }

    fn session_menu(&mut self, i: usize) {
        self.select(i);
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, MF_STRING, ID_MENU_DUPLICATE, w!("Duplicate session"));
            let _ = AppendMenuW(menu, MF_STRING, ID_MENU_CLOSE, w!("Close session"));
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, p.x, p.y, Some(0), self.main, None);
            let _ = DestroyMenu(menu);
        }
    }

    fn context_menu(&mut self) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, MF_STRING, ID_COPY, w!("Copy"));
            let _ = AppendMenuW(menu, MF_STRING, ID_PASTE, w!("Paste"));
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, p.x, p.y, Some(0), self.main, None);
            let _ = DestroyMenu(menu);
        }
    }

    fn saved_menu(&mut self, row: Option<usize>) {
        unsafe {
            if let Some(i) = row {
                SendMessageW(self.saved_list, LB_SETCURSEL, Some(WPARAM(i)), None);
            }
            let Ok(menu) = CreatePopupMenu() else { return };
            if row.is_some() {
                let _ = AppendMenuW(menu, MF_STRING, ID_MENU_EDIT, w!("Settings..."));
            }
            let _ = AppendMenuW(menu, MF_STRING, ID_MENU_NEW, w!("New session..."));
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, p.x, p.y, Some(0), self.main, None);
            let _ = DestroyMenu(menu);
        }
    }

    fn paint(&mut self, dc: HDC, area: RECT) {
        unsafe {
            let bg = self.current().map_or((0, 0, 0), |s| s.term.palette().background());
            let rgb = |c: (u8, u8, u8)| COLORREF(c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16);
            let brush = CreateSolidBrush(rgb(bg));
            FillRect(dc, &area, brush);
            let _ = DeleteObject(brush.into());
            let Some(s) = self.active.and_then(|i| self.sessions.get(i)) else {
                let old = SelectObject(dc, self.gui_font.into());
                SetBkColor(dc, COLORREF(0));
                SetTextColor(dc, COLORREF(0xbbbbbb));
                let text: Vec<u16> = "Type a host at the top left, or double-click a saved session.".encode_utf16().collect();
                let _ = TextOutW(dc, self.px(16), self.px(16), &text);
                SelectObject(dc, old);
                return;
            };
            let screen = s.term.screen();
            let (cw, ch) = self.cell;
            let origin = s.target.settings.i("WindowBorder", 1) as i32;
            let (bold_font, shadow) = (s.term.palette().bold_font, s.target.settings.b("ShadowBold", false));
            let shadow_at = s.target.settings.i("ShadowBoldOffset", 1) as i32;
            let cursor_colour = s.term.palette().cursor;
            let (cursor_kind, _) = s.term.cursor_style();
            let old = SelectObject(dc, self.font.into());
            for (y, row) in screen.rows.iter().enumerate() {
                let y = y as i32 * ch + origin;
                let mut x = 0;
                while x < row.len() {
                    let first = row[x];
                    let mut end = x;
                    let (mut text, mut dx) = (Vec::with_capacity(row.len()), Vec::with_capacity(row.len()));
                    let style = |c: &deck_core::term::RenderCell| (c.fg, c.bg, c.bold, c.underline, c.wide);
                    // the blank half of a wide character stays with it
                    while end < row.len() && (style(&row[end]) == style(&first) || (end > x && row[end - 1].wide)) {
                        let c = if row[end].c == '\0' { ' ' } else { row[end].c };
                        let mut buf = [0u16; 2];
                        for (i, u) in c.encode_utf16(&mut buf).iter().enumerate() {
                            text.push(*u);
                            dx.push(if i == 0 { cw } else { 0 });
                        }
                        end += 1;
                    }
                    let bolded = first.bold && bold_font && !shadow;
                    let face = match (first.wide, bolded) {
                        (true, true) if !self.wide_bold.is_invalid() => self.wide_bold,
                        (true, false) if !self.wide_font.is_invalid() => self.wide_font,
                        (_, true) => self.bold,
                        _ => self.font,
                    };
                    SelectObject(dc, face.into());
                    SetBkColor(dc, rgb(first.bg));
                    SetTextColor(dc, rgb(first.fg));
                    let r = RECT { left: x as i32 * cw + origin, top: y, right: end as i32 * cw + origin, bottom: y + ch };
                    let _ = ExtTextOutW(dc, r.left, y, ETO_OPAQUE | ETO_CLIPPED, Some(&r), PCWSTR(text.as_ptr()), text.len() as u32, Some(dx.as_ptr()));
                    if first.bold && bold_font && shadow {
                        // ShadowBold: the normal font again, a little to the right
                        SetBkMode(dc, TRANSPARENT);
                        let _ = ExtTextOutW(dc, r.left + shadow_at, y, ETO_CLIPPED, Some(&r), PCWSTR(text.as_ptr()), text.len() as u32, Some(dx.as_ptr()));
                        SetBkMode(dc, OPAQUE);
                    }
                    if first.underline {
                        let line = RECT { left: r.left, top: y + ch - 1, right: r.right, bottom: y + ch };
                        let b = CreateSolidBrush(rgb(first.fg));
                        FillRect(dc, &line, b);
                        let _ = DeleteObject(b.into());
                    }
                    x = end;
                }
            }
            SelectObject(dc, old);
            if let Some((cx, cy)) = screen.cursor {
                let (left, top) = (cx as i32 * cw + origin, cy as i32 * ch + origin);
                let r = RECT { left, top, right: left + cw, bottom: top + ch };
                let b = CreateSolidBrush(rgb(cursor_colour));
                if !self.focused {
                    let _ = FrameRect(dc, &r, b);
                } else if !self.cursor_off {
                    match cursor_kind {
                        CursorKind::Block => {
                            let _ = InvertRect(dc, &r);
                        }
                        CursorKind::Underline => {
                            FillRect(dc, &RECT { top: r.bottom - self.px(2), ..r }, b);
                        }
                        CursorKind::Bar => {
                            FillRect(dc, &RECT { right: r.left + self.px(2), ..r }, b);
                        }
                    }
                }
                let _ = DeleteObject(b.into());
            }
            if self.flash {
                let _ = InvertRect(dc, &area);
            }
        }
    }

    #[cfg(feature = "test-hooks")]
    fn dump(&mut self) {
        let Ok(path) = std::env::var("PUTTYDECK_TEST_DUMP") else { return };
        let (cols, rows) = self.grid();
        let mut out = format!("sessions={}\nactive={:?}\ngrid={cols}x{rows}\n", self.sessions.len(), self.active);
        for (i, s) in self.sessions.iter().enumerate() {
            out += &format!("label{i}={}\n", s.label());
        }
        out += &format!("clip={:?}\n", clipboard::get().unwrap_or_default());
        if let Some(s) = self.current() {
            out += &format!("selection={}\nalt={}\n", s.term.has_selection(), s.term.alt_screen());
            for r in 0..s.term.size().1 {
                out += &format!("row={}\n", s.term.row_text(r));
            }
        }
        let tmp = format!("{path}.tmp");
        if std::fs::write(&tmp, out).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(d) = deck() else { return DefWindowProcW(hwnd, msg, wp, lp) };
    match msg {
        WM_SIZE => d.layout(),
        WM_SETFOCUS => {
            take_focus(d.content);
        }
        WM_COMMAND => d.command(wp.0 & 0xffff, (wp.0 >> 16) as u32 & 0xffff),
        WM_DECK_EVENT => d.pump_events(),
        WM_DPICHANGED => {
            d.dpi = (wp.0 & 0xffff) as u32;
            d.make_fonts();
            let r = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        WM_CLOSE => {
            let warn = |s: &&Session| !s.closed && s.target.settings.b("WarnOnClose", true);
            let live = d.sessions.iter().filter(warn).count();
            if live == 0 || d.confirm(&format!("Close PuTTYDeck and its {live} open sessions?")) {
                let _ = DestroyWindow(hwnd);
            }
        }
        WM_DESTROY => {
            for s in &d.sessions {
                let _ = s.tx.send(Command::Close);
            }
            PostQuitMessage(0);
        }
        #[cfg(feature = "test-hooks")]
        WM_TEST_KEY => {
            d.key_down(wp.0 as u16, lp.0 & 1 != 0, lp.0 & 2 != 0);
            d.skip_char = false;
        }
        #[cfg(feature = "test-hooks")]
        WM_TEST_DUMP => d.dump(),
        #[cfg(feature = "test-hooks")]
        WM_TEST_SETTINGS => {
            let mut s = Settings::default();
            s.set("HostName", Value::Str("127.0.0.1".into()));
            s.set("PortNumber", Value::Int(wp.0 as u32));
            s.set("Protocol", Value::Str("raw".into()));
            let (_, _, s) = settings_window::edit(hwnd, d.gui_font, "test", s);
            assert_eq!(s.s("TerminalType", "xterm"), "xterm", "a blank box mustn't replace a default");
            d.open_target(target_from(s));
        }
        #[cfg(feature = "test-hooks")]
        WM_TEST_CLIP => {
            if let Ok(p) = std::env::var("PUTTYDECK_TEST_CLIP") {
                clipboard::set(&std::fs::read_to_string(p).unwrap_or_default());
            }
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

unsafe extern "system" fn content_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(d) = deck() else { return DefWindowProcW(hwnd, msg, wp, lp) };
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut r = RECT::default();
            let _ = GetClientRect(hwnd, &mut r);
            let mem = CreateCompatibleDC(Some(dc));
            let bmp = CreateCompatibleBitmap(dc, r.right, r.bottom);
            let old = SelectObject(mem, bmp.into());
            d.paint(mem, r);
            let _ = BitBlt(dc, 0, 0, r.right, r.bottom, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            let _ = EndPaint(hwnd, &ps);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SETFOCUS => {
            d.focused = true;
            d.redraw();
        }
        WM_KILLFOCUS => {
            d.focused = false;
            d.redraw();
        }
        WM_GETDLGCODE => return LRESULT(DLGC_WANTALLKEYS as isize),
        WM_KEYDOWN => {
            d.cursor_off = false;
            let extended = lp.0 >> 24 & 1 != 0;
            d.skip_char = d.key_down_with(wp.0 as u16, Mods { shift: shift_down(), ctrl: ctrl_down(), alt: false }, extended);
        }
        WM_SYSKEYDOWN => {
            let extended = lp.0 >> 24 & 1 != 0;
            if !d.sys_key(wp.0 as u16, shift_down(), extended) {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
        }
        WM_KEYUP => d.compose_key_up(wp.0 as u16, lp.0 >> 24 & 1 != 0),
        WM_SYSKEYUP if d.compose_state == 1 && wp.0 as u16 == VK_MENU.0 => d.compose_key_up(VK_MENU.0, lp.0 >> 24 & 1 != 0),
        // Alt by itself: AltOnly lets it open the window menu
        WM_SYSKEYUP if wp.0 as u16 == VK_MENU.0 && !d.current().is_some_and(|s| s.target.settings.b("AltOnly", false)) => {}
        WM_TIMER => d.on_timer(wp.0),
        WM_VSCROLL => d.on_scrollbar(wp),
        WM_CHAR => {
            if !std::mem::take(&mut d.skip_char) {
                d.on_char(wp.0 as u16);
            }
        }
        WM_SYSCHAR => {
            if let Some(s) = d.current() {
                let c = char::from_u32(wp.0 as u32).unwrap_or(' ');
                s.send(format!("\x1b{c}").into_bytes());
            }
        }
        // A program with mouse reporting on gets the mouse, unless Shift is held.
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP
            if !d.selecting && d.report_mouse(msg, lp) => {}
        WM_MOUSEMOVE if !d.selecting && wp.0 & 0x13 != 0 && d.report_drag(wp, lp) => {}
        WM_MOUSEWHEEL if d.report_wheel(wp, lp) => {}
        WM_LBUTTONDOWN => {
            take_focus(hwnd);
            let (x, y) = d.cell_at(lp);
            SetCapture(hwnd);
            d.selecting = true;
            let flip = alt_down();
            if let Some(s) = d.current() {
                s.term.select_start_with(x, y, flip);
            }
            d.redraw();
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = d.cell_at(lp);
            let auto = d.autocopy();
            if let Some(s) = d.current() {
                s.term.select_word(x, y);
                if let (true, Some(text)) = (auto, s.term.selected_text()) {
                    copy_out(&s.target.settings, &text);
                }
            }
            d.redraw();
        }
        WM_MOUSEMOVE if d.pointer_hidden && !d.selecting => d.show_pointer(),
        WM_MOUSEMOVE if d.selecting => {
            let (x, y) = d.cell_at(lp);
            if let Some(s) = d.current() {
                s.term.select_to(x, y);
            }
            d.redraw();
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            d.selecting = false;
            let auto = d.autocopy();
            if let Some(s) = d.current() {
                if let (true, Some(text)) = (auto, s.term.selected_text()) {
                    copy_out(&s.target.settings, &text);
                }
            }
        }
        WM_MBUTTONUP => {
            let style = d.mouse_style();
            d.extend_or_paste(style != 1, lp);
        }
        WM_RBUTTONUP => match d.mouse_style() {
            0 => d.extend_or_paste(false, lp),
            1 => d.extend_or_paste(true, lp),
            _ => d.context_menu(),
        },
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as i16 as i32;
            if let Some(s) = d.current() {
                s.term.scroll(delta.signum() * SCROLL_STEP);
            }
            d.redraw();
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// Target from `-ssh`, `-telnet`, `-raw`, `-P port`, `-l user`, `-load name` and a host.
fn parse_args(saved: &[(String, Target)]) -> Option<Target> {
    let mut args = std::env::args().skip(1);
    let (mut protocol, mut port, mut user, mut host, mut load, mut key) = (None, None, None, None, None, None);
    let mut forwards = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-ssh" => protocol = Some(Protocol::Ssh),
            "-telnet" => protocol = Some(Protocol::Telnet),
            "-rlogin" => protocol = Some(Protocol::Rlogin),
            "-raw" => protocol = Some(Protocol::Raw),
            "-serial" => protocol = Some(Protocol::Serial),
            "-P" => port = args.next().and_then(|p| p.parse().ok()),
            "-l" => user = args.next(),
            "-i" => key = args.next(),
            "-load" => load = args.next(),
            "-L" | "-R" | "-D" => {
                if let Some(spec) = args.next() {
                    forwards.push(forward_key(&a[1..], &spec));
                }
            }
            _ => host = Some(a),
        }
    }
    if let Some(name) = load {
        return saved.iter().find(|(n, _)| *n == name).map(|(_, t)| t.clone());
    }
    let mut t = Target::parse(&host?)?;
    if let Some(p) = protocol {
        if p == Protocol::Telnet {
            t.port = 23;
        }
        t.protocol = p;
    }
    t.port = port.unwrap_or(t.port);
    t.user = user.or(t.user);
    t.key_file = key;
    if !forwards.is_empty() {
        t.settings.set("PortForwardings", Value::Str(forwards.join(",")));
    }
    Some(t)
}

/// `-L 8080:host:80` or `-D 1080` as PuTTY's `L8080=host:80` or `D1080=`.
fn forward_key(kind: &str, spec: &str) -> String {
    match spec.split_once(':') {
        Some((port, dest)) if kind != "D" => format!("{kind}{port}={dest}"),
        _ => format!("{kind}{spec}="),
    }
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let cursor = LoadCursorW(None, IDC_IBEAM).unwrap_or_default();
        let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(main_proc),
            hInstance: instance,
            hCursor: arrow,
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut _),
            lpszClassName: w!("PuTTYDeckMain"),
            ..Default::default()
        });
        RegisterClassW(&WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(content_proc),
            hInstance: instance,
            hCursor: cursor,
            lpszClassName: w!("PuTTYDeckContent"),
            ..Default::default()
        });
        // Test windows open off-screen, so they don't flash up over someone's work.
        let x = if cfg!(feature = "test-hooks") { -4000 } else { CW_USEDEFAULT };
        let ex = if cfg!(feature = "test-hooks") { WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW } else { WINDOW_EX_STYLE(0) };
        let main = CreateWindowExW(ex, w!("PuTTYDeckMain"), w!("PuTTYDeck"), WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            x, CW_USEDEFAULT, 1200, 760, None, None, Some(instance), None).unwrap();
        let child = |class: PCWSTR, text: PCWSTR, style: u32, ex: u32, id: usize| {
            CreateWindowExW(WINDOW_EX_STYLE(ex), class, text, WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WINDOW_STYLE(style), 0, 0, 10, 10,
                Some(main), Some(HMENU(id as *mut _)), Some(instance), None).unwrap()
        };
        let listbox = (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32 | WS_VSCROLL.0 | WS_TABSTOP.0;
        let content = child(w!("PuTTYDeckContent"), w!(""), WS_VSCROLL.0, 0, 0);
        let edit = child(w!("EDIT"), w!(""), ES_AUTOHSCROLL as u32 | WS_TABSTOP.0, WS_EX_CLIENTEDGE.0, 99);
        let _ = SendMessageW(edit, EM_SETCUEBANNER, Some(WPARAM(1)), Some(LPARAM(w!("user@host").as_ptr() as isize)));
        let open = child(w!("BUTTON"), w!("Open"), BS_PUSHBUTTON as u32 | WS_TABSTOP.0, 0, ID_OPEN);
        let l1 = child(w!("STATIC"), w!("Open sessions"), 0, 0, 0);
        let list = child(w!("LISTBOX"), w!(""), listbox, WS_EX_CLIENTEDGE.0, ID_LIST);
        let l2 = child(w!("STATIC"), w!("Saved sessions"), 0, 0, 0);
        let saved_list = child(w!("LISTBOX"), w!(""), listbox, WS_EX_CLIENTEDGE.0, ID_SAVED);
        let buttons = [(w!("Copy"), ID_COPY), (w!("Paste"), ID_PASTE), (w!("Copy all"), ID_COPY_ALL), (w!("Paste to all"), ID_PASTE_ALL), (w!("Settings"), ID_SETTINGS)]
            .map(|(t, id)| child(w!("BUTTON"), t, BS_PUSHBUTTON as u32, 0, id));

        let test = cfg!(feature = "test-hooks");
        let saved = if test { Vec::new() } else { load_sessions() };
        let store: Arc<dyn HostKeyStore> = if test { Arc::new(MemoryStore::default()) } else { Arc::new(RegistryStore) };
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let d = Box::into_raw(Box::new(Deck {
            main, content, edit, open, list, saved_list, labels: [l1, l2], buttons,
            gui_font: HFONT::default(), font: HFONT::default(), bold: HFONT::default(),
            wide_font: HFONT::default(), wide_bold: HFONT::default(),
            cell: (8, 16), dpi: GetDpiForWindow(main).max(96), font_in_use: (String::new(), 0),
            sessions: Vec::new(), active: None, next_id: 1, saved,
            inbox: Default::default(), rt, store,
            skip_char: false, high_surrogate: None, selecting: false, focused: false,
            started: std::time::Instant::now(), cursor_off: false, flash: false, pointer_hidden: false, compose_state: 0, compose_first: 0, full_screen: None,
        }));
        DECK.store(d, Ordering::Release);
        let d = &mut *d;
        d.make_fonts();
        for (name, _) in &d.saved {
            let text = wide(name);
            SendMessageW(d.saved_list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
        }
        d.layout();
        SetTimer(Some(content), TIMER_BLINK, BLINK_MS, None);
        // A test window must never take the keyboard from whoever is using this PC.
        let _ = ShowWindow(main, if test { SW_SHOWNOACTIVATE } else { SW_SHOW });
        if let Some(t) = parse_args(&d.saved) {
            d.open_target(t);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_KEYDOWN {
                let vk = msg.wParam.0 as u16;
                if msg.hwnd == d.edit && vk == VK_RETURN.0 {
                    d.open_from_box();
                    continue;
                }
                if vk == VK_TAB.0 && ctrl_down() {
                    d.key_down(vk, true, shift_down());
                    continue;
                }
            }
            if msg.hwnd == d.list && (msg.message == WM_MBUTTONUP || msg.message == WM_RBUTTONUP) {
                if let Some(i) = d.row_from_point(d.list, msg.lParam) {
                    if msg.message == WM_MBUTTONUP {
                        d.close_session(i);
                    } else {
                        d.session_menu(i);
                    }
                }
                continue;
            }
            if msg.hwnd == d.saved_list && msg.message == WM_RBUTTONUP {
                let row = d.row_from_point(d.saved_list, msg.lParam);
                d.saved_menu(row);
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
