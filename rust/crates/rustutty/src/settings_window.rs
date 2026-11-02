//! The session settings dialog, PuTTY's configuration box in Rust. `table` is
//! config.c's list of controls and `layout` is the engine in controls.c. As in
//! PuTTY, a control writes its setting only when the person changes it, so one
//! that just shows a default leaves the key unset, and values no control knows
//! about stay in the session.

mod layout;
mod table;

use crate::wide;
use deck_core::config::Deleted;
use deck_core::settings::{Settings, Value};
use layout::{escape, Owner, Pos};
use std::collections::BTreeMap;
use table::{Bind, Ctl, Group, Kind, Prefs, APP, CODEPAGES, COLOURS, PROTOCOLS, TTY_MODES, WORDNESS};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Printing::*;
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetCapture, GetFocus, ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

// A test window opens off-screen, takes no focus, and uses no registry.
const TEST: bool = cfg!(any(test, feature = "test-hooks"));
const STYLE: u32 = 0x80 | 0x40 | WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0; // DS_MODALFRAME | DS_SETFONT
/// The settings window itself can be resized, and maximised.
const RESIZABLE: u32 = STYLE | WS_THICKFRAME.0 | WS_MAXIMIZEBOX.0;
/// The size PuTTY's panels are laid out for, which is also the smallest allowed.
const WIDTH: i32 = 300;
const HEIGHT: i32 = 252;
const ID_TREE_LABEL: i32 = 100;
const ID_TREE: i32 = 101;
const ID_FIRST: i32 = 200;
const NO_PRINTER: &str = "None (printing disabled)";
const LICENCE: &str = include_str!("../../../LICENCE");

/// What the person chose to do with the settings.
#[derive(PartialEq)]
pub enum Outcome {
    Cancelled,
    Open,
}

/// Saved sessions: the registry, or a private list in a test build.
#[cfg(not(any(test, feature = "test-hooks")))]
mod store {
    use super::*;
    use deck_core::config::{default_settings, delete_session, load_session, load_sessions, save_session};

    pub fn names() -> Vec<String> {
        load_sessions().into_iter().map(|(n, _)| n).collect()
    }

    pub fn load(name: &str) -> Settings {
        load_session(name).unwrap_or_default()
    }

    pub fn defaults() -> Settings {
        default_settings()
    }

    pub fn save(name: &str, s: &Settings) -> Result<(), String> {
        save_session(name, s).map_err(|e| e.to_string())
    }

    pub fn delete(name: &str) -> Result<Deleted, String> {
        delete_session(name).map_err(|e| e.to_string())
    }
}

#[cfg(any(test, feature = "test-hooks"))]
mod store {
    use super::*;
    use std::sync::Mutex;

    static SAVED: Mutex<Vec<(String, Settings)>> = Mutex::new(Vec::new());

    pub fn names() -> Vec<String> {
        SAVED.lock().unwrap().iter().map(|(n, _)| n.clone()).filter(|n| n != "Default Settings").collect()
    }

    pub fn load(name: &str) -> Settings {
        SAVED.lock().unwrap().iter().find(|(n, _)| n == name).map(|(_, s)| s.clone()).unwrap_or_default()
    }

    pub fn defaults() -> Settings {
        load("Default Settings")
    }

    pub fn save(name: &str, s: &Settings) -> Result<(), String> {
        let mut all = SAVED.lock().unwrap();
        all.retain(|(n, _)| n != name);
        all.push((name.to_string(), s.clone()));
        Ok(())
    }

    pub fn delete(name: &str) -> Result<Deleted, String> {
        let mut all = SAVED.lock().unwrap();
        let before = all.len();
        all.retain(|(n, _)| n != name);
        Ok(if all.len() == before { Deleted::Missing } else { Deleted::Done })
    }
}

/// One control on screen.
#[derive(Clone, Copy)]
struct Inst {
    set: usize,
    ctl: usize,
    base: i32,
    n: i32,
}

struct Dlg {
    hwnd: HWND,
    owner: HWND,
    font: HFONT,
    dpi: u32,
    /// The window's client area, in dialog units.
    size: (i32, i32),
    /// Pixels in 4 dialog units across and 8 down.
    bx: i32,
    by: i32,
    mid: bool,
    cbox: table::Cbox,
    path: String,
    settings: Settings,
    /// "Default Settings" first, as `get_sesslist` has it.
    names: Vec<String>,
    /// What's in the Saved Sessions box.
    saved: String,
    insts: Vec<Inst>,
    panel: Vec<HWND>,
    fixed: Vec<HWND>,
    titles: Vec<(i32, String)>,
    tree: HWND,
    refreshing: bool,
    initialised: bool,
    ended: Option<bool>,
    focused: Option<Bind>,
    last_focused: Option<Bind>,
    fixed_pitch: bool,
    drag_message: u32,
    drag: (i32, i32, bool),
    /// Beeps and messages, which a test reads instead of hearing or dismissing.
    log: Vec<String>,
}

static mut DLG: *mut Dlg = std::ptr::null_mut();

#[cfg(test)]
static mut SCRIPT: Option<fn(&mut Dlg)> = None;

fn text_of(h: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(h) } as usize;
    let mut buf = vec![0u16; len + 1];
    let n = unsafe { GetWindowTextW(h, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn set_text(h: HWND, text: &str) {
    let t = wide(text);
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
    }
}

fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    unsafe { SendMessageW(h, msg, Some(WPARAM(wp)), Some(LPARAM(lp))).0 }
}

fn add_string(h: HWND, msg: u32, text: &str) -> isize {
    let t = wide(text);
    send(h, msg, 0, t.as_ptr() as isize)
}

/// C's `atoi`: leading digits, and 0 if there are none.
fn atoi(text: &str) -> i32 {
    let t = text.trim_start();
    let (sign, digits) = match t.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, t.strip_prefix('+').unwrap_or(t)),
    };
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    sign * digits[..end].parse::<i32>().unwrap_or(0)
}

fn atof(text: &str) -> f64 {
    let t = text.trim_start();
    let end = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(t.len());
    t[..end].parse().unwrap_or(0.0)
}

fn launchable(s: &Settings) -> bool {
    if s.s("Protocol", "ssh") == "serial" {
        !s.s("SerialLine", "").is_empty()
    } else {
        !s.s("HostName", "").is_empty()
    }
}

fn default_port(protocol: &str) -> u32 {
    PROTOCOLS.iter().find(|p| p.id == protocol).map_or(0, |p| p.port)
}

/// PuTTY's `wmap`.
fn map_text(pairs: &[(String, String)], values: bool) -> String {
    let esc = |s: &str| s.chars().flat_map(|c| if matches!(c, '=' | ',' | '\\') { vec!['\\', c] } else { vec![c] }).collect::<String>();
    let items: Vec<String> = pairs.iter().map(|(k, v)| if values { format!("{}={}", esc(k), esc(v)) } else { esc(k) }).collect();
    items.join(",")
}

/// `host_strchr`: a colon that isn't inside IPv6 brackets.
fn host_has_colon(text: &str) -> bool {
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ':' if depth <= 0 => return true,
            _ => {}
        }
    }
    false
}

/// `validate_manual_hostkey`: the first word that's a fingerprint or a key blob.
fn valid_host_key(text: &str) -> Option<String> {
    for word in text.split([' ', '\t']).filter(|w| !w.is_empty()) {
        let b64 = |c: char| c.is_ascii_alphanumeric() || c == '+' || c == '/';
        if let Some(r) = word.strip_prefix("SHA256:") {
            if r.len() >= 43 && r[..43].chars().all(b64) {
                return Some(word[..50].to_string());
            }
        }
        let r = word.strip_prefix("MD5:").unwrap_or(word);
        if r.len() == 47 && r.bytes().enumerate().all(|(i, c)| if i % 3 == 2 { c == b':' } else { c.is_ascii_hexdigit() }) {
            return Some(r.to_lowercase());
        }
        let blob: String = word.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        if blob.len() % 4 == 0 && blob.len() > 8 && blob.trim_end_matches('=').chars().all(b64) {
            return Some(blob);
        }
    }
    None
}

fn template(title: &str, style: u32, width: i16, height: i16, ex: u32) -> Vec<u32> {
    let mut t: Vec<u16> = Vec::new();
    let pair = |v: u32| [v as u16, (v >> 16) as u16];
    t.extend(pair(style));
    t.extend(pair(ex));
    t.extend([0, 0, 0, width as u16, height as u16, 0, 0]);
    t.extend(title.encode_utf16().chain([0]));
    t.push(9);
    t.extend("Segoe UI".encode_utf16().chain([0]));
    t.chunks(2).map(|c| c[0] as u32 | (c.get(1).copied().unwrap_or(0) as u32) << 16).collect()
}

impl Dlg {
    fn px(&self, r: [i32; 4]) -> [i32; 4] {
        let x = |v: i32| (v * self.bx + 2).div_euclid(4);
        let y = |v: i32| (v * self.by + 4).div_euclid(8);
        [x(r[0]), y(r[1]), x(r[2]), y(r[3])]
    }

    fn beep(&mut self) {
        self.log.push("beep".into());
        if !TEST {
            unsafe {
                let _ = MessageBeep(MESSAGEBOX_STYLE(0));
            }
        }
    }

    fn message(&mut self, text: &str, title: &str, kind: MESSAGEBOX_STYLE) {
        self.log.push(text.to_string());
        if !TEST {
            let tone = if kind == MB_ICONERROR { deck_ui::Tone::Error } else if kind == MB_ICONWARNING { deck_ui::Tone::Warning } else { deck_ui::Tone::Info };
            deck_ui::tell(Some(self.hwnd), title, text, tone);
        }
    }

    fn error(&mut self, text: &str) {
        self.message(text, &format!("{APP} Error"), MB_ICONERROR);
    }

    /// Makes the dialog font for a DPI, and finds its dialog base units.
    fn set_dpi(&mut self, dpi: u32) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font.into());
            }
            self.dpi = dpi;
            self.font = deck_ui::ui_font(dpi, 9, 400);
            let dc = GetDC(Some(self.hwnd));
            let old = SelectObject(dc, self.font.into());
            let mut size = SIZE::default();
            let alphabet: Vec<u16> = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz".encode_utf16().collect();
            let _ = GetTextExtentPoint32W(dc, &alphabet, &mut size);
            let mut metrics = TEXTMETRICW::default();
            let _ = GetTextMetricsW(dc, &mut metrics);
            SelectObject(dc, old);
            ReleaseDC(Some(self.hwnd), dc);
            (self.bx, self.by) = ((size.cx / 26 + 1) / 2, metrics.tmHeight);
        }
    }

    /// `staticwrap`: fits text to a width in dialog units.
    fn wrap(&self, text: &str, width: i32) -> (String, i32) {
        let width = self.px([width, 0, 0, 0])[0];
        let chars: Vec<u16> = text.encode_utf16().collect();
        let mut out: Vec<u16> = Vec::new();
        let (mut at, mut lines) = (0, 1);
        unsafe {
            let dc = GetDC(Some(self.hwnd));
            let old = SelectObject(dc, self.font.into());
            while at < chars.len() {
                let rest = &chars[at..];
                let mut fit = 0;
                let mut size = SIZE::default();
                let ok = GetTextExtentExPointW(dc, PCWSTR(rest.as_ptr()), rest.len() as i32, width, Some(&mut fit), None, &mut size).as_bool();
                if !ok || fit as usize >= rest.len() {
                    out.extend_from_slice(rest);
                    break;
                }
                let mut cut = fit as usize;
                if let Some(j) = (1..=cut).rev().find(|&j| rest[j] == b' ' as u16) {
                    cut = j;
                }
                out.extend_from_slice(&rest[..cut]);
                out.push(b'\n' as u16);
                at += cut;
                while at < chars.len() && chars[at] == b' ' as u16 {
                    at += 1;
                }
                lines += 1;
            }
            SelectObject(dc, old);
            ReleaseDC(Some(self.hwnd), dc);
        }
        (String::from_utf16_lossy(&out), lines)
    }

    fn make(&self, class: &str, style: u32, ex: u32, text: &str, r: [i32; 4], id: i32) -> HWND {
        let (c, t) = (wide(class), wide(text));
        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(None).unwrap().into();
            let h = CreateWindowExW(WINDOW_EX_STYLE(ex), PCWSTR(c.as_ptr()), PCWSTR(t.as_ptr()), WINDOW_STYLE(style), r[0], r[1], r[2], r[3], Some(self.hwnd), Some(HMENU(id as usize as *mut _)), Some(instance), None).unwrap_or_default();
            send(h, WM_SETFONT, self.font.0 as usize, 1);
            deck_ui::style_control(h);
            h
        }
    }

    fn create_set(&mut self, set: usize, cp: &mut Pos, next: &mut i32, keep: bool) {
        let items = layout::layout(&self.cbox.sets[set], cp, &|t, w| self.wrap(t, w));
        let mut bases: Vec<(Owner, i32)> = Vec::new();
        for item in items {
            let base = match bases.iter().find(|(o, _)| *o == item.owner) {
                Some((_, b)) => *b,
                None => {
                    let b = if item.cancel { 2 } else { *next };
                    if !item.cancel {
                        *next += item.ids;
                    }
                    bases.push((item.owner, b));
                    if let Owner::Ctl(ctl) = item.owner {
                        self.insts.push(Inst { set, ctl, base: b, n: item.ids });
                    }
                    b
                }
            };
            let id = base + item.sub;
            let r = self.px(item.r);
            let text = if item.owner == Owner::Title { "" } else { item.text.as_str() };
            let h = self.make(item.class, item.style, item.ex, text, r, id);
            if item.owner == Owner::Title {
                self.titles.push((id, item.text.clone()));
            }
            if item.class == "LISTBOX" {
                // A list box counts its lines with its creation font, so resize it
                unsafe {
                    let _ = SetWindowPos(h, None, 0, 0, r[2], r[3], SWP_NOACTIVATE | SWP_NOCOPYBITS | SWP_NOMOVE | SWP_NOZORDER);
                }
                if !item.tabs.is_empty() {
                    send(h, LB_SETTABSTOPS, item.tabs.len(), item.tabs.as_ptr() as isize);
                }
            }
            if item.drag {
                unsafe {
                    let _ = MakeDragList(h);
                }
            }
            if item.default {
                send(self.hwnd, DM_SETDEFID, id as usize, 0);
            }
            if keep { &mut self.fixed } else { &mut self.panel }.push(h);
        }
        self.align(set);
    }

    /// `align_next_to`: centres two controls on the same line.
    fn align(&self, set: usize) {
        for (i, c) in self.cbox.sets[set].ctrls.iter().enumerate() {
            let Some(other) = c.align else { continue };
            let find = |ctl| self.insts.iter().copied().find(|x| x.set == set && x.ctl == ctl);
            let (Some(a), Some(b)) = (find(i), find(other)) else { continue };
            let rect = |x: Inst| {
                let mut r = RECT::default();
                unsafe {
                    let _ = GetWindowRect(self.item(x, x.n - 1), &mut r);
                }
                r
            };
            let mid2 = [a, b].map(|x| rect(x)).iter().map(|r| r.top + r.bottom).max().unwrap();
            for x in [a, b] {
                let r = rect(x);
                let dy = (mid2 - (r.top + r.bottom)) / 2;
                for sub in 0..x.n {
                    let h = self.item(x, sub);
                    let mut w = RECT::default();
                    unsafe {
                        if h.is_invalid() || GetWindowRect(h, &mut w).is_err() {
                            continue;
                        }
                        let mut p = POINT { x: w.left, y: w.top + dy };
                        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut p);
                        let _ = SetWindowPos(h, None, p.x, p.y, 0, 0, SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER);
                    }
                }
            }
        }
    }

    fn client_dlu(&self) -> i32 {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        r.right * 4 / self.bx
    }

    fn create_panel(&mut self) {
        let mut cp = layout::start(self.client_dlu(), 100, 3, 13);
        let mut next = ID_FIRST + 40;
        let sets: Vec<usize> = (0..self.cbox.sets.len()).filter(|&i| self.cbox.sets[i].path == self.path).collect();
        for s in sets {
            self.create_set(s, &mut cp, &mut next, false);
        }
    }

    fn destroy_panel(&mut self) {
        for h in self.panel.drain(..) {
            unsafe {
                let _ = DestroyWindow(h);
            }
        }
        let cbox = &self.cbox;
        self.insts.retain(|i| cbox.sets[i.set].path.is_empty());
        self.titles.clear();
    }

    /// The window's outer size for a client size in dialog units.
    fn window_size(&self, width: i32, height: i32) -> (i32, i32) {
        let mut r = RECT { left: 0, top: 0, right: self.px([width, 0, 0, 0])[0], bottom: self.px([0, height, 0, 0])[1] };
        unsafe {
            let ex = WINDOW_EX_STYLE(GetWindowLongW(self.hwnd, GWL_EXSTYLE) as u32);
            let _ = AdjustWindowRectExForDpi(&mut r, WINDOW_STYLE(RESIZABLE), false, ex, self.dpi);
        }
        (r.right - r.left, r.bottom - r.top)
    }

    /// The window at its size, or at what fits on the monitor if that's less.
    fn fit_window(&mut self) {
        let (mut cx, mut cy) = self.window_size(self.size.0, self.size.1);
        unsafe {
            let monitor = MonitorFromWindow(if self.owner.is_invalid() { self.hwnd } else { self.owner }, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let (least_x, least_y) = self.window_size(WIDTH, HEIGHT);
            let shrunk = GetMonitorInfoW(monitor, &mut info).as_bool()
                && (cx > (info.rcWork.right - info.rcWork.left).max(least_x) || cy > (info.rcWork.bottom - info.rcWork.top).max(least_y));
            if shrunk {
                cx = cx.min((info.rcWork.right - info.rcWork.left).max(least_x));
                cy = cy.min((info.rcWork.bottom - info.rcWork.top).max(least_y));
            }
            let _ = SetWindowPos(self.hwnd, None, 0, 0, cx, cy, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            if shrunk {
                self.size = self.client_size();
            }
        }
    }

    fn client_size(&self) -> (i32, i32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        ((r.right * 4 / self.bx).max(WIDTH), (r.bottom * 8 / self.by).max(HEIGHT))
    }

    /// The buttons along the bottom.
    fn make_fixed(&mut self) {
        let base = (0..self.cbox.sets.len()).find(|&i| self.cbox.sets[i].path.is_empty()).unwrap();
        let mut next = ID_FIRST;
        let mut cp = layout::start(self.client_dlu(), 3, 3, self.size.1 - (HEIGHT - 235));
        self.create_set(base, &mut cp, &mut next, true);
    }

    fn tree_rect(&self) -> [i32; 4] {
        self.px([3, 13, 95, self.size.1 - (HEIGHT - 219)])
    }

    /// The person resized the window. The tree stays, keeping what's expanded;
    /// the rest is laid out again for the new size.
    fn resized(&mut self) {
        let size = self.client_size();
        if size == self.size {
            return;
        }
        self.size = size;
        self.initialised = false;
        send(self.hwnd, WM_SETREDRAW, 0, 0);
        unsafe {
            let focus = GetFocus();
            let focus_id = GetDlgCtrlID(focus);
            let label = GetDlgItem(Some(self.hwnd), ID_TREE_LABEL).unwrap_or_default();
            self.destroy_panel();
            let tree = self.tree;
            self.fixed.retain(|&h| {
                let keep = h == tree || h == label;
                if !keep {
                    let _ = DestroyWindow(h);
                }
                keep
            });
            self.insts.clear();
            let kept = std::mem::take(&mut self.fixed);
            self.make_fixed();
            let last = self.fixed.last().copied();
            self.fixed.extend(kept);
            // Back in their place in the tab order, after the buttons
            let r = self.tree_rect();
            let _ = SetWindowPos(label, last, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            let _ = SetWindowPos(tree, Some(label), r[0], r[1], r[2], r[3], SWP_NOACTIVATE);
            self.create_panel();
            self.refresh();
            self.initialised = true;
            send(self.hwnd, WM_SETREDRAW, 1, 0);
            let _ = RedrawWindow(Some(self.hwnd), None, None, RDW_ERASE | RDW_FRAME | RDW_INVALIDATE | RDW_ALLCHILDREN);
            if !TEST && focus_id != 0 {
                if let Ok(h) = GetDlgItem(Some(self.hwnd), focus_id) {
                    let _ = SetFocus(Some(h));
                }
            }
        }
    }

    /// Everything in the dialog, for this DPI.
    fn build(&mut self) {
        self.fit_window();
        self.make_fixed();

        let label = self.make("STATIC", WS_CHILD.0 | WS_VISIBLE.0, 0, "Cate&gory:", self.px([3, 3, 95, 10]), ID_TREE_LABEL);
        self.fixed.push(label);
        let style = WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | TVS_DISABLEDRAGDROP | TVS_HASBUTTONS | TVS_LINESATROOT | TVS_SHOWSELALWAYS | TVS_FULLROWSELECT;
        self.tree = self.make("SysTreeView32", style, 0x200, "", self.tree_rect(), ID_TREE);
        send(self.tree, TVM_SETITEMHEIGHT, (24 * self.dpi / 96) as usize, 0);
        send(self.tree, TVM_SETEXTENDEDSTYLE, 4, 4); // TVS_EX_DOUBLEBUFFER
        self.fixed.push(self.tree);
        self.fill_tree();
        self.create_panel();
    }

    /// The tree view, from the control sets' paths.
    fn fill_tree(&mut self) {
        let mut last = [HTREEITEM::default(); 4];
        let mut path = String::new();
        let mut wanted = HTREEITEM::default();
        for i in 0..self.cbox.sets.len() {
            let name = self.cbox.sets[i].path.clone();
            if name.is_empty() || name == path {
                continue;
            }
            let level = name.matches('/').count();
            let mut text = wide(name.rsplit('/').next().unwrap());
            let mut ins = TVINSERTSTRUCTW { hParent: if level > 0 { last[level - 1] } else { TVI_ROOT }, hInsertAfter: if last[level].0 == 0 { TVI_LAST } else { last[level] }, ..Default::default() };
            ins.Anonymous.item = TVITEMW { mask: TVIF_TEXT | TVIF_PARAM, pszText: PWSTR(text.as_mut_ptr()), lParam: LPARAM(i as isize), ..Default::default() };
            let item = HTREEITEM(send(self.tree, TVM_INSERTITEMW, 0, &ins as *const _ as isize));
            if level > 0 {
                send(self.tree, TVM_EXPAND, if level > 1 { TVE_COLLAPSE.0 as usize } else { TVE_EXPAND.0 as usize }, last[level - 1].0);
            }
            last[level] = item;
            for l in &mut last[level + 1..] {
                *l = HTREEITEM::default();
            }
            if name == self.path {
                wanted = item;
            }
            path = name;
        }
        send(self.tree, TVM_SELECTITEM, TVGN_CARET as usize, wanted.0);
    }

    /// Selecting a panel: PuTTY's WM_NOTIFY case.
    fn show_panel(&mut self, path: String) {
        self.path = path;
        send(self.hwnd, WM_SETREDRAW, 0, 0);
        self.destroy_panel();
        self.create_panel();
        self.refresh();
        send(self.hwnd, WM_SETREDRAW, 1, 0);
        unsafe {
            let _ = RedrawWindow(Some(self.hwnd), None, None, RDW_ERASE | RDW_FRAME | RDW_INVALIDATE | RDW_ALLCHILDREN);
            if !TEST {
                let _ = SetFocus(Some(self.tree));
            }
        }
    }

    /// Moved to a monitor with another DPI: everything is made again.
    fn dpi_changed(&mut self, dpi: u32, suggested: Option<RECT>) {
        self.initialised = false;
        self.destroy_panel();
        for h in self.fixed.drain(..) {
            unsafe {
                let _ = DestroyWindow(h);
            }
        }
        self.insts.clear();
        self.set_dpi(dpi);
        if let Some(r) = suggested {
            unsafe {
                let _ = SetWindowPos(self.hwnd, None, r.left, r.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        self.build();
        self.refresh();
        self.initialised = true;
        if !TEST {
            unsafe {
                let _ = SetFocus(Some(self.tree));
            }
        }
    }

    fn ctl(&self, i: Inst) -> &Ctl {
        &self.cbox.sets[i.set].ctrls[i.ctl]
    }

    fn item(&self, i: Inst, sub: i32) -> HWND {
        unsafe { GetDlgItem(Some(self.hwnd), i.base + sub).unwrap_or_default() }
    }

    fn part(&self, g: Group, n: u8) -> Option<Inst> {
        self.insts.iter().copied().find(|i| self.ctl(*i).bind == Bind::Part(g, n))
    }

    fn part_item(&self, g: Group, n: u8, sub: i32) -> HWND {
        self.part(g, n).map_or(HWND::default(), |i| self.item(i, sub))
    }

    fn set(&mut self, key: &str, v: u32) {
        self.settings.set(key, Value::Int(v));
    }

    fn set_str(&mut self, key: &str, v: &str) {
        self.settings.set(key, Value::Str(v.to_string()));
    }

    fn protocol(&self) -> String {
        self.settings.s("Protocol", "ssh")
    }

    fn set_radio(&self, i: Inst, which: Option<usize>) {
        for b in 1..i.n {
            send(self.item(i, b), BM_SETCHECK, (Some(b as usize - 1) == which) as usize, 0);
        }
    }

    fn radio(&self, i: Inst) -> usize {
        (1..i.n).find(|&b| send(self.item(i, b), BM_GETCHECK, 0, 0) == 1).map_or(0, |b| b as usize - 1)
    }

    /// `dlg_refresh`: every control on screen, from the settings.
    fn refresh(&mut self) {
        let was = std::mem::replace(&mut self.refreshing, true);
        for i in self.insts.clone() {
            self.refresh_one(i);
        }
        self.refreshing = was;
    }

    fn refresh_inst(&mut self, bind: Bind) {
        let was = std::mem::replace(&mut self.refreshing, true);
        if let Some(i) = self.insts.iter().copied().find(|i| self.ctl(*i).bind == bind) {
            self.refresh_one(i);
        }
        self.refreshing = was;
    }

    fn refresh_one(&mut self, i: Inst) {
        let bind = self.ctl(i).bind.clone();
        let h = self.item(i, 1);
        match bind {
            Bind::Str(k, d) => set_text(h, &self.settings.s(k, d)),
            Bind::Int(k, d) => set_text(h, &(self.settings.i(k, d) as i32).to_string()),
            Bind::Fixed(k, d, denom) => set_text(h, &(self.settings.i(k, d) as f64 / denom as f64).to_string()),
            Bind::Flag(k, d) => {
                send(self.item(i, 0), BM_SETCHECK, self.settings.b(k, d) as usize, 0);
            }
            Bind::NotFlag(k, d) => {
                send(self.item(i, 0), BM_SETCHECK, !self.settings.b(k, d) as usize, 0);
            }
            Bind::Radio(k, d, values) => {
                let v = self.settings.i(k, d);
                self.set_radio(i, values.iter().position(|x| *x == v));
            }
            Bind::Drop(k, d, items) => {
                send(h, CB_RESETCONTENT, 0, 0);
                for (text, _) in items {
                    add_string(h, CB_ADDSTRING, text);
                }
                let v = self.settings.i(k, d);
                send(h, CB_SETCURSEL, items.iter().position(|(_, x)| *x == v).unwrap_or(0), 0);
            }
            Bind::Clip(k, d) => {
                send(h, CB_RESETCONTENT, 0, 0);
                for text in ["No action", "Last selected text", "System clipboard"] {
                    add_string(h, CB_ADDSTRING, text);
                }
                let at = ["none", "implicit", "explicit"].iter().position(|x| *x == self.settings.s(k, d));
                send(h, CB_SETCURSEL, at.unwrap_or(0), 0);
            }
            Bind::Prefs(which) => {
                let (key, default, table) = which.info();
                send(h, LB_RESETCONTENT, 0, 0);
                for n in table::prefs_order(&self.settings.s(key, default), table) {
                    let at = add_string(h, LB_ADDSTRING, table[n].1);
                    send(h, LB_SETITEMDATA, at as usize, n as isize);
                }
            }
            Bind::Font(k) => {
                let (name, height) = (self.settings.s(k, "Consolas"), self.settings.i(&format!("{k}Height"), 0) as i32);
                let bold = if self.settings.b(&format!("{k}IsBold"), false) { "bold, " } else { "" };
                let text = match height {
                    0 => format!("Font: {name}, {bold}default height"),
                    h if h < 0 => format!("Font: {name}, {bold}{}-pixel", -h),
                    h => format!("Font: {name}, {bold}{h}-point"),
                };
                set_text(h, &text);
            }
            Bind::Host => {
                let serial = self.protocol() == "serial";
                set_text(self.item(i, 0), &escape(if serial { "Serial line" } else { "Host Name (or IP address)" }, 'n'));
                set_text(h, &self.settings.s(if serial { "SerialLine" } else { "HostName" }, ""));
            }
            Bind::Port => {
                let serial = self.protocol() == "serial";
                set_text(self.item(i, 0), &escape(if serial { "Speed" } else { "Port" }, 'p'));
                let v = if serial { self.settings.i("SerialSpeed", 9600) } else { self.settings.i("PortNumber", default_port(&self.protocol())) };
                set_text(h, &if v == 0 && !serial { String::new() } else { v.to_string() });
            }
            Bind::Ping => set_text(h, &self.settings.ping_seconds().to_string()),
            Bind::Keypad => {
                let which = if self.settings.b("NetHackKeypad", false) {
                    2
                } else {
                    self.settings.b("ApplicationKeypad", false) as usize
                };
                self.set_radio(i, Some(which));
            }
            Bind::Printer => {
                send(h, CB_RESETCONTENT, 0, 0);
                add_string(h, CB_ADDSTRING, NO_PRINTER);
                for p in printers() {
                    add_string(h, CB_ADDSTRING, &p);
                }
                set_text(h, &self.settings.s("Printer", ""));
            }
            Bind::Codepage => {
                send(h, CB_RESETCONTENT, 0, 0);
                for (name, _) in CODEPAGES {
                    add_string(h, CB_ADDSTRING, name);
                }
                let wanted = self.settings.s("LineCodePage", "");
                let name = table::codepage_name(&wanted);
                set_text(h, if name.is_empty() { &wanted } else { &name });
            }
            Bind::VarPitch => {
                send(self.item(i, 0), BM_SETCHECK, !self.fixed_pitch as usize, 0);
            }
            Bind::Part(g, n) => self.refresh_part(i, g, n),
            _ => {}
        }
    }

    fn refresh_part(&mut self, i: Inst, g: Group, n: u8) {
        let h = self.item(i, 1);
        match (g, n) {
            (Group::Proto, 0) => {
                let p = self.protocol();
                self.set_radio(i, Some(PROTOCOLS.iter().take(2).position(|x| x.id == p).unwrap_or(2)));
            }
            (Group::Proto, 1) => {
                send(h, CB_RESETCONTENT, 0, 0);
                for p in &PROTOCOLS[2..] {
                    add_string(h, CB_ADDSTRING, p.name);
                }
                let p = self.protocol();
                send(h, CB_SETCURSEL, PROTOCOLS[2..].iter().position(|x| x.id == p).unwrap_or(0), 0);
            }
            (Group::Saved, 0) => set_text(h, &self.saved),
            (Group::Saved, 1) => {
                send(h, LB_RESETCONTENT, 0, 0);
                for name in &self.names {
                    add_string(h, LB_ADDSTRING, name);
                }
            }
            (Group::Class, 0) => {
                send(h, LB_RESETCONTENT, 0, 0);
                for c in 0..128 {
                    let shown = if c >= 0x21 && c != 0x7f { c as u8 as char } else { ' ' };
                    add_string(h, LB_ADDSTRING, &format!("{c}\t(0x{c:02X})\t{shown}\t{}", self.wordness(c)));
                }
            }
            (Group::Colour, 0) => {
                send(h, LB_RESETCONTENT, 0, 0);
                for (name, _) in COLOURS {
                    add_string(h, LB_ADDSTRING, name);
                }
                self.show_rgb(None);
            }
            (Group::Tty, 0) => {
                send(h, LB_RESETCONTENT, 0, 0);
                for (k, v) in self.tty_modes() {
                    let shown = match v.chars().next() {
                        Some('A') => "(auto)",
                        Some('N') => "(don't send)",
                        _ => v.get(1..).unwrap_or(""),
                    };
                    add_string(h, LB_ADDSTRING, &format!("{k}\t{shown}"));
                }
            }
            (Group::Tty, 2) | (Group::Fwd, 5) | (Group::Fwd, 6) => self.set_radio(i, Some(0)),
            (Group::Env, 4) => {
                send(h, LB_RESETCONTENT, 0, 0);
                for (k, v) in self.sorted("Environment") {
                    add_string(h, LB_ADDSTRING, &format!("{k}\t{v}"));
                }
            }
            (Group::Fwd, 1) => {
                send(h, LB_RESETCONTENT, 0, 0);
                for (k, v) in self.forwards() {
                    let line = if v == "D" { format!("{}\t", k.replacen('L', "D", 1)) } else { format!("{k}\t{v}") };
                    add_string(h, LB_ADDSTRING, &line);
                }
            }
            (Group::Hk, 1) => {
                send(h, LB_RESETCONTENT, 0, 0);
                for (k, _) in self.sorted("SSHManualHostKeys") {
                    add_string(h, LB_ADDSTRING, &k);
                }
            }
            _ => {}
        }
    }

    /// A saved map as PuTTY holds it, sorted by key.
    fn sorted(&self, key: &str) -> Vec<(String, String)> {
        self.settings.map(key).into_iter().collect::<BTreeMap<_, _>>().into_iter().collect()
    }

    /// `gppmap` for port forwards: `D1080=` is held as `L1080=D`.
    fn forwards(&self) -> Vec<(String, String)> {
        let held = self.settings.map("PortForwardings").into_iter().map(|(k, v)| if k.contains('D') { (k.replacen('D', "L", 1), "D".to_string()) } else { (k, v) });
        held.collect::<BTreeMap<_, _>>().into_iter().collect()
    }

    fn save_forwards(&mut self, held: &[(String, String)]) {
        let saved: Vec<(String, String)> = held.iter().map(|(k, v)| if v == "D" { (k.replacen('L', "D", 1), String::new()) } else { (k.clone(), v.clone()) }).collect();
        self.set_str("PortForwardings", &map_text(&saved, true));
    }

    /// `gppmap` and the additions after it in `load_open_settings`.
    fn tty_modes(&self) -> Vec<(String, String)> {
        let mut m: BTreeMap<String, String> = self.settings.map("TerminalModes").into_iter().collect();
        let saved = self.settings.has("TerminalModes");
        for mode in TTY_MODES {
            let default = if !saved || *mode == "IUTF8" { "A" } else { "N" };
            m.entry(mode.to_string()).or_insert(default.to_string());
        }
        m.into_iter().collect()
    }

    fn wordness(&self, c: usize) -> i32 {
        let text = self.settings.s(&format!("Wordness{}", c / 32 * 32), WORDNESS[c / 32]);
        text.split(',').nth(c % 32).map_or(0, atoi)
    }

    fn colour(&self, i: usize) -> [i32; 3] {
        let mut text = self.settings.s(&format!("Colour{i}"), "");
        if text.split(',').count() != 3 {
            text = COLOURS[i].1.to_string();
        }
        let v: Vec<i32> = text.split(',').map(atoi).collect();
        [v[0], v[1], v[2]]
    }

    fn set_colour(&mut self, i: usize, c: [i32; 3]) {
        self.set_str(&format!("Colour{i}"), &format!("{},{},{}", c[0], c[1], c[2]));
    }

    fn show_rgb(&mut self, c: Option<[i32; 3]>) {
        let was = std::mem::replace(&mut self.refreshing, true);
        for n in 0..3 {
            set_text(self.part_item(Group::Colour, n as u8 + 1, 1), &c.map_or(String::new(), |c| c[n].to_string()));
        }
        self.refreshing = was;
    }

    fn list_index(&self, h: HWND) -> Option<usize> {
        usize::try_from(send(h, LB_GETCURSEL, 0, 0)).ok()
    }

    /// `EVENT_VALCHANGE`: the person changed a control.
    fn changed(&mut self, i: Inst) {
        let h = self.item(i, 1);
        match self.ctl(i).bind.clone() {
            Bind::Str(k, _) => {
                let t = text_of(h);
                self.set_str(k, &t);
            }
            Bind::Int(k, _) => self.set(k, atoi(&text_of(h)) as u32),
            Bind::Fixed(k, _, denom) => self.set(k, (atof(&text_of(h)) * denom as f64) as i32 as u32),
            Bind::Flag(k, _) => self.set(k, (send(self.item(i, 0), BM_GETCHECK, 0, 0) == 1) as u32),
            Bind::NotFlag(k, _) => self.set(k, (send(self.item(i, 0), BM_GETCHECK, 0, 0) != 1) as u32),
            Bind::Radio(k, _, values) => self.set(k, values[self.radio(i)]),
            Bind::Host => {
                let key = if self.protocol() == "serial" { "SerialLine" } else { "HostName" };
                let t = text_of(h);
                self.set_str(key, &t);
            }
            Bind::Port => {
                let key = if self.protocol() == "serial" { "SerialSpeed" } else { "PortNumber" };
                self.set(key, atoi(&text_of(h)) as u32);
            }
            Bind::Ping => {
                let v = atoi(&text_of(h)).max(0) as u32;
                self.set("PingInterval", v / 60);
                self.set("PingIntervalSecs", v % 60);
            }
            Bind::Keypad => {
                let which = self.radio(i);
                self.set("ApplicationKeypad", (which == 1) as u32);
                self.set("NetHackKeypad", (which == 2) as u32);
            }
            Bind::Printer => {
                let t = text_of(h);
                self.set_str("Printer", if t == NO_PRINTER { "" } else { &t });
            }
            Bind::Codepage => {
                let t = table::codepage_name(&text_of(h));
                self.set_str("LineCodePage", &t);
            }
            Bind::VarPitch => self.fixed_pitch = send(self.item(i, 0), BM_GETCHECK, 0, 0) != 1,
            Bind::Prefs(which) => self.save_prefs(h, which),
            Bind::Part(Group::Proto, 0) => {
                let button = self.radio(i);
                let id = if button < 2 {
                    PROTOCOLS[button].id
                } else {
                    let list = self.part_item(Group::Proto, 1, 1);
                    usize::try_from(send(list, CB_GETCURSEL, 0, 0)).ok().map_or(self.protocol_id(), |n| PROTOCOLS[n + 2].id)
                };
                self.new_protocol(id);
            }
            Bind::Part(Group::Saved, 0) => self.saved_typed(),
            Bind::Part(Group::Colour, n @ 1..=3) => {
                let cval = atoi(&text_of(h)).clamp(0, 255);
                if let Some(c) = self.list_index(self.part_item(Group::Colour, 0, 1)) {
                    let mut rgb = self.colour(c);
                    rgb[n as usize - 1] = cval;
                    self.set_colour(c, rgb);
                }
            }
            Bind::Part(Group::Tty, 3) => {
                if let Some(radio) = self.part(Group::Tty, 2) {
                    self.set_radio(radio, Some(2));
                }
            }
            _ => {}
        }
    }

    fn protocol_id(&self) -> &'static str {
        let p = self.protocol();
        PROTOCOLS.iter().find(|x| x.id == p).map_or("ssh", |x| x.id)
    }

    fn new_protocol(&mut self, id: &'static str) {
        let old = self.protocol();
        if old == id {
            return;
        }
        self.set_str("Protocol", id);
        // The port follows only if it hadn't been changed from the old default
        if self.settings.i("PortNumber", default_port(&old)) == default_port(&old) {
            self.set("PortNumber", default_port(id));
        }
        self.refresh_inst(Bind::Host);
        self.refresh_inst(Bind::Port);
    }

    fn save_prefs(&mut self, h: HWND, which: Prefs) {
        let (key, _, table) = which.info();
        let count = send(h, LB_GETCOUNT, 0, 0) as usize;
        let names: Vec<&str> = (0..count).filter_map(|n| table.get(send(h, LB_GETITEMDATA, n, 0) as usize)).map(|p| p.0).collect();
        self.set_str(key, &names.join(","));
    }

    /// Typing in Saved Sessions moves the list to the nearest name.
    fn saved_typed(&mut self) {
        self.saved = text_of(self.part_item(Group::Saved, 0, 1));
        let top = self.names.partition_point(|n| n.as_str() < self.saved.as_str()).min(self.names.len() - 1);
        send(self.part_item(Group::Saved, 1, 1), LB_SETCURSEL, top, 0);
    }

    fn reload_names(&mut self) {
        self.names = std::iter::once("Default Settings".to_string()).chain(sorted_names(store::names())).collect();
    }

    fn selected_session(&self) -> Option<usize> {
        self.list_index(self.part_item(Group::Saved, 1, 1))
    }

    fn read_session(&self, i: usize) -> (Settings, bool) {
        let def = i == 0;
        (if def { store::defaults() } else { store::load(&self.names[i]) }, !def)
    }

    /// `load_selected_session`. Says if the session may be opened straight away.
    fn load_selected(&mut self) -> Option<bool> {
        let Some(i) = self.selected_session() else {
            self.beep();
            return None;
        };
        let (settings, launch) = self.read_session(i);
        self.settings = settings;
        self.saved = if launch { self.names[i].clone() } else { String::new() };
        self.auto_fixed_pitch();
        self.refresh();
        send(self.part_item(Group::Saved, 1, 1), LB_SETCURSEL, i, 0);
        Some(launch)
    }

    fn save(&mut self) {
        if self.saved.is_empty() {
            let Some(i) = self.selected_session() else {
                self.beep();
                return;
            };
            self.saved = if i == 0 { String::new() } else { self.names[i].clone() };
        }
        let name = if self.saved.is_empty() { "Default Settings".to_string() } else { self.saved.clone() };
        if let Err(e) = store::save(&name, &self.settings) {
            self.error(&format!("Unable to save the session: {e}"));
        }
        self.reload_names();
        self.refresh_inst(Bind::Part(Group::Saved, 0));
        self.refresh_inst(Bind::Part(Group::Saved, 1));
    }

    fn delete(&mut self) {
        let Some(i) = self.selected_session().filter(|i| *i > 0) else {
            self.beep();
            return;
        };
        let name = self.names[i].clone();
        match store::delete(&name) {
            Ok(Deleted::OnlyInPutty) => self.message(&format!("\"{name}\" is one of PuTTY's own saved sessions. {APP} never changes those, so it has not been deleted. Use PuTTY to delete it."), APP, MB_ICONINFORMATION),
            Ok(Deleted::PuttyRemains) => self.message(&format!("{APP}'s copy of \"{name}\" has been deleted. PuTTY's own session of that name is still listed, because {APP} never changes those."), APP, MB_ICONINFORMATION),
            Ok(_) => {}
            Err(e) => self.error(&format!("Unable to delete the session: {e}")),
        }
        self.reload_names();
        self.refresh_inst(Bind::Part(Group::Saved, 1));
    }

    /// Open, with PuTTY's special case: no host, and the list last had the
    /// focus, means load the session selected there and go.
    fn ok(&mut self) {
        if self.mid {
            self.ended = Some(true);
            return;
        }
        let list = Bind::Part(Group::Saved, 1);
        let ok = Bind::Part(Group::Saved, 5);
        let last = if self.focused.as_ref() == Some(&ok) { self.last_focused.clone() } else { self.focused.clone() };
        if last.as_ref() == Some(&list) && !launchable(&self.settings) && self.part(Group::Saved, 1).is_some() {
            match self.selected_session().map(|i| self.read_session(i)) {
                Some((settings, true)) if launchable(&settings) => {
                    self.settings = settings;
                    self.saved = self.selected_session().map_or(String::new(), |i| self.names[i].clone());
                    self.ended = Some(true);
                }
                _ => self.beep(),
            }
            return;
        }
        if launchable(&self.settings) || TEST {
            self.ended = Some(true);
        } else {
            self.beep();
        }
    }

    /// `EVENT_ACTION`: a button pressed, or a list item double-clicked.
    fn action(&mut self, i: Inst) {
        match self.ctl(i).bind.clone() {
            Bind::About => about(self.hwnd),
            Bind::Help => self.message(&format!("{APP} has no help file yet."), &format!("{APP} Help"), MB_ICONINFORMATION),
            Bind::HostCas => self.message(&format!("{APP} doesn't support host certification authorities yet."), APP, MB_ICONINFORMATION),
            Bind::Part(g, n) => self.part_action(g, n),
            _ => {}
        }
    }

    fn part_action(&mut self, g: Group, n: u8) {
        match (g, n) {
            (Group::Saved, 1) if !self.mid => {
                if self.load_selected() == Some(true) && launchable(&self.settings) {
                    self.ended = Some(true);
                }
            }
            (Group::Saved, 2) => {
                self.load_selected();
            }
            (Group::Saved, 3) => self.save(),
            (Group::Saved, 4) => self.delete(),
            (Group::Saved, 5) => self.ok(),
            (Group::Saved, 6) => self.ended = Some(false),
            (Group::Class, 2) => {
                let class = atoi(&text_of(self.part_item(Group::Class, 1, 1)));
                let list = self.part_item(Group::Class, 0, 1);
                for chunk in 0..4 {
                    let mut v: Vec<i32> = (chunk * 32..(chunk + 1) * 32).map(|c| self.wordness(c)).collect();
                    let mut touched = false;
                    for (c, slot) in v.iter_mut().enumerate() {
                        if send(list, LB_GETSEL, chunk * 32 + c, 0) > 0 {
                            *slot = class;
                            touched = true;
                        }
                    }
                    if touched {
                        let text = v.iter().map(i32::to_string).collect::<Vec<_>>().join(",");
                        self.set_str(&format!("Wordness{}", chunk * 32), &text);
                    }
                }
                self.refresh_inst(Bind::Part(Group::Class, 0));
            }
            (Group::Colour, 4) => {
                let Some(c) = self.list_index(self.part_item(Group::Colour, 0, 1)) else {
                    self.beep();
                    return;
                };
                if let Some(rgb) = choose_colour(self.hwnd, self.colour(c)) {
                    self.set_colour(c, rgb);
                    self.show_rgb(Some(rgb));
                }
            }
            (Group::Tty, 1) => {
                let Some(at) = self.list_index(self.part_item(Group::Tty, 0, 1)) else {
                    self.beep();
                    return;
                };
                let kind = self.part(Group::Tty, 2).map_or(0, |r| self.radio(r));
                let value = match kind {
                    0 => "A".to_string(),
                    1 => "N".to_string(),
                    _ => format!("V{}", text_of(self.part_item(Group::Tty, 3, 1))),
                };
                let mut modes = self.tty_modes();
                modes[at].1 = value;
                self.set_str("TerminalModes", &map_text(&modes, true));
                self.refresh_inst(Bind::Part(Group::Tty, 0));
                send(self.part_item(Group::Tty, 0, 1), LB_SETCURSEL, at, 0);
            }
            (Group::Env, 2) => {
                let (key, val) = (text_of(self.part_item(Group::Env, 0, 1)), text_of(self.part_item(Group::Env, 1, 1)));
                if key.is_empty() || val.is_empty() {
                    self.beep();
                    return;
                }
                let mut all: BTreeMap<_, _> = self.sorted("Environment").into_iter().collect();
                all.insert(key, val);
                self.set_str("Environment", &map_text(&all.into_iter().collect::<Vec<_>>(), true));
                set_text(self.part_item(Group::Env, 0, 1), "");
                set_text(self.part_item(Group::Env, 1, 1), "");
                self.refresh_inst(Bind::Part(Group::Env, 4));
            }
            (Group::Env, 3) => {
                let Some(at) = self.list_index(self.part_item(Group::Env, 4, 1)) else {
                    self.beep();
                    return;
                };
                let mut all = self.sorted("Environment");
                let (key, val) = all.remove(at);
                set_text(self.part_item(Group::Env, 0, 1), &key);
                set_text(self.part_item(Group::Env, 1, 1), &val);
                self.set_str("Environment", &map_text(&all, true));
                self.refresh_inst(Bind::Part(Group::Env, 4));
            }
            (Group::Fwd, 2) => self.add_forward(),
            (Group::Fwd, 0) => self.remove_forward(),
            (Group::Hk, 3) => {
                let text = text_of(self.part_item(Group::Hk, 2, 1));
                if text.is_empty() {
                    self.error("You need to specify a host key or fingerprint");
                    return;
                }
                let mut all = self.sorted("SSHManualHostKeys");
                match valid_host_key(&text) {
                    None => self.error("Host key is not in a valid format"),
                    Some(k) if all.iter().any(|(x, _)| *x == k) => self.error("Specified host key is already listed"),
                    Some(k) => {
                        all.push((k, String::new()));
                        all.sort();
                        self.set_str("SSHManualHostKeys", &map_text(&all, false));
                    }
                }
                self.refresh_inst(Bind::Part(Group::Hk, 1));
            }
            (Group::Hk, 0) => {
                let Some(at) = self.list_index(self.part_item(Group::Hk, 1, 1)) else {
                    self.beep();
                    return;
                };
                let mut all = self.sorted("SSHManualHostKeys");
                let (key, _) = all.remove(at);
                set_text(self.part_item(Group::Hk, 2, 1), &key);
                self.set_str("SSHManualHostKeys", &map_text(&all, false));
                self.refresh_inst(Bind::Part(Group::Hk, 1));
            }
            _ => {}
        }
    }

    fn add_forward(&mut self) {
        let radio = |d: &Dlg, n| d.part(Group::Fwd, n).map_or(0, |r| d.radio(r));
        let family = ["", "4", "6"][radio(self, 6)];
        let direction = radio(self, 5);
        let src = text_of(self.part_item(Group::Fwd, 3, 1));
        if src.is_empty() {
            self.error("You need to specify a source port number");
            return;
        }
        let (kind, val) = if direction == 2 {
            ("L", "D".to_string())
        } else {
            let val = text_of(self.part_item(Group::Fwd, 4, 1));
            if val.is_empty() || !host_has_colon(&val) {
                self.error("You need to specify a destination address\nin the form \"host.name:port\"");
                return;
            }
            (["L", "R"][direction], val)
        };
        let key = format!("{family}{kind}{src}");
        let mut all = self.forwards();
        if all.iter().any(|(k, _)| *k == key) {
            self.error("Specified forwarding already exists");
        } else {
            all.push((key, val));
            all.sort();
            self.save_forwards(&all);
        }
        self.refresh_inst(Bind::Part(Group::Fwd, 1));
    }

    fn remove_forward(&mut self) {
        let Some(at) = self.list_index(self.part_item(Group::Fwd, 1, 1)) else {
            self.beep();
            return;
        };
        let mut all = self.forwards();
        let (key, val) = all.remove(at);
        let mut rest = key.as_str();
        let family = "A46".find(rest.chars().next().unwrap_or(' '));
        if family.is_some() {
            rest = &rest[1..];
        }
        let dir = if val == "D" { 'D' } else { rest.chars().next().unwrap_or('L') };
        if let Some(r) = self.part(Group::Fwd, 6) {
            self.set_radio(r, Some(family.unwrap_or(0)));
        }
        if let Some(r) = self.part(Group::Fwd, 5) {
            self.set_radio(r, "LRD".find(dir));
        }
        set_text(self.part_item(Group::Fwd, 3, 1), rest.get(1..).unwrap_or(""));
        set_text(self.part_item(Group::Fwd, 4, 1), if val == "D" { "" } else { &val });
        self.save_forwards(&all);
        self.refresh_inst(Bind::Part(Group::Fwd, 1));
    }

    fn browse(&mut self, i: Inst) {
        let Kind::File { title, filter, write, .. } = self.ctl(i).kind else { return };
        let edit = self.item(i, 1);
        let mut name: Vec<u16> = text_of(edit).encode_utf16().collect();
        name.resize(32768, 0);
        let (filter, title) = (wide(filter), wide(title));
        let mut of = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: self.hwnd,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            nFilterIndex: 1,
            lpstrFile: PWSTR(name.as_mut_ptr()),
            nMaxFile: name.len() as u32,
            lpstrTitle: PCWSTR(title.as_ptr()),
            Flags: OFN_NOCHANGEDIR | if write { OFN_OVERWRITEPROMPT } else { OFN_FILEMUSTEXIST },
            ..Default::default()
        };
        let chosen = unsafe { if write { GetSaveFileNameW(&mut of).as_bool() } else { GetOpenFileNameW(&mut of).as_bool() } };
        if chosen {
            let end = name.iter().position(|c| *c == 0).unwrap_or(0);
            set_text(edit, &String::from_utf16_lossy(&name[..end]));
        }
    }

    fn choose_font(&mut self, i: Inst) {
        let Bind::Font(key) = self.ctl(i).bind else { return };
        let (name, height) = (self.settings.s(key, "Consolas"), self.settings.i(&format!("{key}Height"), 10) as i32);
        let bold = self.settings.b(&format!("{key}IsBold"), false);
        let mut lf = LOGFONTW {
            lfHeight: -(height * self.dpi as i32 + 36) / 72,
            lfWeight: if bold { 700 } else { 0 },
            lfCharSet: FONT_CHARSET(self.settings.i(&format!("{key}CharSet"), 0) as u8),
            lfOutPrecision: OUT_DEFAULT_PRECIS,
            lfClipPrecision: CLIP_DEFAULT_PRECIS,
            lfQuality: DEFAULT_QUALITY,
            lfPitchAndFamily: FIXED_PITCH.0 as u8,
            ..Default::default()
        };
        for (slot, c) in lf.lfFaceName.iter_mut().take(31).zip(name.encode_utf16()) {
            *slot = c;
        }
        let mut cf = CHOOSEFONTW {
            lStructSize: std::mem::size_of::<CHOOSEFONTW>() as u32,
            hwndOwner: self.hwnd,
            lpLogFont: &mut lf,
            Flags: (if self.fixed_pitch { CF_FIXEDPITCHONLY } else { CHOOSEFONT_FLAGS(0) }) | CF_FORCEFONTEXIST | CF_INITTOLOGFONTSTRUCT | CF_SCREENFONTS,
            ..Default::default()
        };
        if unsafe { ChooseFontW(&mut cf).as_bool() } {
            let end = lf.lfFaceName.iter().position(|c| *c == 0).unwrap_or(32);
            self.set_str(key, &String::from_utf16_lossy(&lf.lfFaceName[..end]));
            self.set(&format!("{key}IsBold"), (lf.lfWeight == 700) as u32);
            self.set(&format!("{key}CharSet"), lf.lfCharSet.0 as u32);
            self.set(&format!("{key}Height"), (cf.iPointSize / 10) as u32);
            self.refresh_inst(Bind::Font(key));
            self.auto_fixed_pitch();
            self.refresh_inst(Bind::VarPitch);
        }
    }

    /// `dlg_auto_set_fixed_pitch_flag`: a proportional font keeps those on offer.
    fn auto_fixed_pitch(&mut self) {
        let name = wide(&self.settings.s("Font", "Consolas"));
        unsafe {
            let font = CreateFontW(0, 0, 0, 0, 0, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, DEFAULT_QUALITY, FIXED_PITCH.0 as u32, PCWSTR(name.as_ptr()));
            let dc = GetDC(None);
            let old = SelectObject(dc, font.into());
            let mut tm = TEXTMETRICW::default();
            // TMPF_FIXED_PITCH is set for a variable-pitch font
            if GetTextMetricsW(dc, &mut tm).as_bool() && tm.tmPitchAndFamily.0 & 1 != 0 {
                self.fixed_pitch = false;
            }
            SelectObject(dc, old);
            ReleaseDC(None, dc);
            let _ = DeleteObject(font.into());
        }
    }

    fn focus(&mut self, bind: Bind, has: bool) {
        if has {
            if self.focused.is_some() {
                self.last_focused = self.focused.take();
            }
            self.focused = Some(bind);
        } else if self.focused.as_ref() == Some(&bind) {
            self.last_focused = self.focused.take();
        }
    }

    /// `winctrl_handle_command`.
    fn command(&mut self, id: i32, code: u32, hwnd_from: HWND) {
        let Some(i) = self.insts.iter().copied().find(|i| id >= i.base && id < i.base + i.n) else { return };
        let sub = id - i.base;
        let bind = self.ctl(i).bind.clone();
        let quiet = self.refreshing;
        match self.ctl(i).kind.clone() {
            Kind::Edit { combo: false, .. } => {
                if matches!(code, EN_SETFOCUS | EN_KILLFOCUS) {
                    self.focus(bind, code == EN_SETFOCUS);
                } else if code == EN_CHANGE && !quiet {
                    self.changed(i);
                }
            }
            Kind::Edit { combo: true, .. } => match code {
                CBN_SETFOCUS | CBN_KILLFOCUS => {
                    self.focus(bind.clone(), code == CBN_SETFOCUS);
                    if code == CBN_KILLFOCUS {
                        self.refresh_inst(bind);
                    }
                }
                CBN_SELCHANGE if !quiet => {
                    let at = send(hwnd_from, CB_GETCURSEL, 0, 0);
                    let mut buf = vec![0u16; send(hwnd_from, CB_GETLBTEXTLEN, at as usize, 0).max(0) as usize + 1];
                    send(hwnd_from, CB_GETLBTEXT, at as usize, buf.as_mut_ptr() as isize);
                    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
                    let was = std::mem::replace(&mut self.refreshing, true);
                    set_text(hwnd_from, &String::from_utf16_lossy(&buf[..end]));
                    self.refreshing = was;
                    self.changed(i);
                }
                CBN_EDITCHANGE if !quiet => self.changed(i),
                _ => {}
            },
            Kind::Radio { .. } => {
                if matches!(code, BN_SETFOCUS | BN_KILLFOCUS) {
                    self.focus(bind, code == BN_SETFOCUS);
                } else if matches!(code, BN_CLICKED | BN_DOUBLECLICKED) && send(hwnd_from, BM_GETCHECK, 0, 0) == 1 && !quiet {
                    self.changed(i);
                }
            }
            Kind::Check { .. } => {
                if matches!(code, BN_SETFOCUS | BN_KILLFOCUS) {
                    self.focus(bind, code == BN_SETFOCUS);
                } else if matches!(code, BN_CLICKED | BN_DOUBLECLICKED) && !quiet {
                    self.changed(i);
                }
            }
            Kind::Button { .. } => {
                if matches!(code, BN_SETFOCUS | BN_KILLFOCUS) {
                    self.focus(bind, code == BN_SETFOCUS);
                } else if matches!(code, BN_CLICKED | BN_DOUBLECLICKED) {
                    self.action(i);
                }
            }
            Kind::List { height: 0, .. } => match code {
                CBN_SETFOCUS | CBN_KILLFOCUS => self.focus(bind, code == CBN_SETFOCUS),
                CBN_SELCHANGE if !quiet => self.drop_changed(i),
                _ => {}
            },
            Kind::List { drag: true, .. } => {
                if sub >= 2 && matches!(code, BN_CLICKED | BN_DOUBLECLICKED) {
                    let list = self.item(i, 1);
                    let Some(at) = self.list_index(list) else {
                        self.beep();
                        return;
                    };
                    let count = send(list, LB_GETCOUNT, 0, 0) as usize;
                    if sub == 2 && at > 0 {
                        move_item(list, at, at - 1);
                    } else if sub == 3 && at + 1 < count {
                        move_item(list, at, at + 1);
                    }
                    self.changed(i);
                }
            }
            Kind::List { .. } => match code {
                LBN_SETFOCUS | LBN_KILLFOCUS => self.focus(bind, code == LBN_SETFOCUS),
                LBN_DBLCLK => {
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                    self.action(i);
                }
                LBN_SELCHANGE => self.list_selected(i),
                _ => {}
            },
            Kind::File { .. } => {
                if sub == 1 && matches!(code, EN_SETFOCUS | EN_KILLFOCUS) {
                    self.focus(bind, code == EN_SETFOCUS);
                } else if sub == 1 && code == EN_CHANGE && !quiet {
                    self.changed(i);
                } else if sub == 2 && matches!(code, BN_CLICKED | BN_DOUBLECLICKED) {
                    self.browse(i);
                }
            }
            Kind::Font { .. } => {
                if sub == 2 && matches!(code, BN_CLICKED | BN_DOUBLECLICKED) {
                    self.choose_font(i);
                }
            }
            _ => {}
        }
    }

    fn drop_changed(&mut self, i: Inst) {
        let at = send(self.item(i, 1), CB_GETCURSEL, 0, 0);
        let Ok(at) = usize::try_from(at) else { return };
        match self.ctl(i).bind.clone() {
            Bind::Drop(k, _, items) => self.set(k, items[at].1),
            Bind::Clip(k, _) => self.set_str(k, ["none", "implicit", "explicit"][at]),
            Bind::Part(Group::Proto, 1) => {
                if let Some(radio) = self.part(Group::Proto, 0) {
                    self.set_radio(radio, Some(2));
                }
                self.new_protocol(PROTOCOLS[at + 2].id);
            }
            _ => {}
        }
    }

    fn list_selected(&mut self, i: Inst) {
        match self.ctl(i).bind {
            Bind::Part(Group::Colour, 0) => {
                let rgb = self.list_index(self.item(i, 1)).map(|c| self.colour(c));
                self.show_rgb(rgb);
            }
            Bind::Part(Group::Tty, 0) => {
                let Some(at) = self.list_index(self.item(i, 1)) else { return };
                let value = self.tty_modes()[at].1.clone();
                let was = std::mem::replace(&mut self.refreshing, true);
                set_text(self.part_item(Group::Tty, 3, 1), value.get(1..).unwrap_or(""));
                self.refreshing = was;
                if let Some(radio) = self.part(Group::Tty, 2) {
                    self.set_radio(radio, Some(match value.chars().next() {
                        Some('A') => 0,
                        Some('N') => 1,
                        _ => 2,
                    }));
                }
            }
            _ => {}
        }
    }

    /// `handle_prefslist`'s dragging. The result goes back in the message result.
    fn drag_message(&mut self, id: i32, info: &DRAGLISTINFO) -> isize {
        let Some(i) = self.insts.iter().copied().find(|i| id == i.base + 1) else { return 0 };
        let list = info.hWnd;
        let point = |p: POINT, scroll| unsafe { LBItemFromPt(list, p, scroll) };
        let gap = |p: POINT| {
            // Which gap between items the pointer is nearest
            let item = point(p, true);
            if item == -1 {
                return -1;
            }
            let item = point(p, false);
            let (mut up, mut down) = (0, 0);
            for d in 1..4096 {
                if up != 0 && down != 0 {
                    break;
                }
                if up == 0 && point(POINT { x: p.x, y: p.y - d }, false) != item {
                    up = d;
                }
                if down == 0 && point(POINT { x: p.x, y: p.y + d }, false) != item {
                    down = d;
                }
            }
            item + (down < up) as i32
        };
        match info.uNotification {
            DL_BEGINDRAG => {
                self.drag = (point(info.ptCursor, true), add_string(list, LB_ADDSTRING, "") as i32, false);
                1
            }
            DL_CANCELDRAG => {
                unsafe { DrawInsert(self.hwnd, list, -1) };
                send(list, LB_DELETESTRING, self.drag.1 as usize, 0);
                self.drag.2 = false;
                0
            }
            DL_DRAGGING => {
                self.drag.2 = true;
                let dest = gap(info.ptCursor).min(self.drag.1);
                unsafe { DrawInsert(self.hwnd, list, dest) };
                (if dest >= 0 { DL_MOVECURSOR } else { DL_STOPCURSOR }) as isize
            }
            DL_DROPPED => {
                let (src, dummy, dragging) = self.drag;
                let dest = if dragging { gap(info.ptCursor).min(dummy) } else { -1 };
                if dragging {
                    unsafe { DrawInsert(self.hwnd, list, -1) };
                }
                send(list, LB_DELETESTRING, dummy as usize, 0);
                self.drag.2 = false;
                if dragging && dest >= 0 && src >= 0 {
                    let dest = if dest > src { dest - 1 } else { dest };
                    move_item(list, src as usize, dest as usize);
                    self.changed(i);
                }
                0
            }
            _ => 0,
        }
    }

    fn draw_title(&self, di: &DRAWITEMSTRUCT) {
        let Some((_, text)) = self.titles.iter().find(|(id, _)| *id == di.CtlID as i32) else { return };
        let text: Vec<u16> = text.encode_utf16().collect();
        let mut text = text;
        let mut r = di.rcItem;
        let p = deck_ui::palette();
        unsafe {
            let hdc = di.hDC;
            deck_ui::fill(hdc, &r, p.bg);
            deck_ui::fill(hdc, &RECT { top: r.bottom - 1, ..r }, p.border);
            let font = deck_ui::ui_font(self.dpi, 10, 600);
            let old = SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, p.text);
            DrawTextW(hdc, &mut text, &mut r, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_NOPREFIX);
            SelectObject(hdc, old);
            let _ = DeleteObject(font.into());
        }
    }

    fn centre(&self) {
        unsafe {
            let monitor = MonitorFromWindow(if self.owner.is_invalid() { self.hwnd } else { self.owner }, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let mut w = RECT::default();
            if !GetMonitorInfoW(monitor, &mut info).as_bool() || GetWindowRect(self.hwnd, &mut w).is_err() {
                return;
            }
            let a = info.rcWork;
            let x = (a.left + a.right - (w.right - w.left)) / 2;
            let y = (a.top + a.bottom - (w.bottom - w.top)) / 2;
            let _ = SetWindowPos(self.hwnd, None, if TEST { -4000 } else { x }, y.max(a.top), 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

/// As `get_sesslist` sorts them: by bytes, so that typing a name can find it.
fn sorted_names(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names
}

fn move_item(list: HWND, src: usize, dst: usize) {
    let mut buf = vec![0u16; send(list, LB_GETTEXTLEN, src, 0).max(0) as usize + 1];
    send(list, LB_GETTEXT, src, buf.as_mut_ptr() as isize);
    let data = send(list, LB_GETITEMDATA, src, 0);
    send(list, LB_DELETESTRING, src, 0);
    send(list, LB_INSERTSTRING, dst, buf.as_ptr() as isize);
    send(list, LB_SETITEMDATA, dst, data);
    send(list, LB_SETCURSEL, dst, 0);
}

fn printers() -> Vec<String> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let (mut needed, mut count) = (0, 0);
    unsafe {
        let _ = EnumPrintersW(flags, PCWSTR::null(), 4, None, &mut needed, &mut count);
        if needed == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u64; needed as usize / 8 + 1];
        let bytes = std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<u8>(), needed as usize);
        if EnumPrintersW(flags, PCWSTR::null(), 4, Some(bytes), &mut needed, &mut count).is_err() {
            return Vec::new();
        }
        let info = std::slice::from_raw_parts(buf.as_ptr().cast::<PRINTER_INFO_4W>(), count as usize);
        info.iter().filter_map(|p| p.pPrinterName.to_string().ok()).collect()
    }
}

fn choose_colour(owner: HWND, c: [i32; 3]) -> Option<[i32; 3]> {
    static mut CUSTOM: [COLORREF; 16] = [COLORREF(0); 16];
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: COLORREF(c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16),
        lpCustColors: &raw mut CUSTOM as *mut COLORREF,
        Flags: CC_FULLOPEN | CC_RGBINIT,
        ..Default::default()
    };
    unsafe { ChooseColorW(&mut cc).as_bool() }.then(|| {
        let v = cc.rgbResult.0;
        [(v & 255) as i32, (v >> 8 & 255) as i32, (v >> 16 & 255) as i32]
    })
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    let Some(d) = DLG.as_mut() else { return 0 };
    if let Some(answer) = deck_ui::dialog_message(hwnd, msg, wp, lp) {
        return answer;
    }
    match msg {
        WM_INITDIALOG => {
            d.hwnd = hwnd;
            deck_ui::style_window(hwnd);
            // The dialog is rebuilt on a DPI change, so Windows mustn't rescale it too
            let _ = SetDialogDpiChangeBehavior(hwnd, DDC_DISABLE_ALL, DDC_DISABLE_ALL);
            d.set_dpi(GetDpiForWindow(hwnd).max(96));
            d.auto_fixed_pitch();
            d.build();
            d.refresh();
            d.centre();
            let dpi = GetDpiForWindow(hwnd).max(96);
            if dpi != d.dpi {
                d.dpi_changed(dpi, None);
                d.centre();
            }
            d.initialised = true;
            0
        }
        WM_COMMAND => {
            if d.initialised {
                d.command((wp.0 & 0xffff) as i32, (wp.0 >> 16) as u32 & 0xffff, HWND(lp.0 as *mut _));
            }
            0
        }
        WM_DRAWITEM => {
            d.draw_title(&*(lp.0 as *const DRAWITEMSTRUCT));
            1
        }
        WM_NOTIFY => {
            let n = &*(lp.0 as *const NMHDR);
            if n.idFrom == ID_TREE as usize && n.code == TVN_SELCHANGEDW && d.initialised {
                let set = (*(lp.0 as *const NMTREEVIEWW)).itemNew.lParam.0 as usize;
                let path = d.cbox.sets[set].path.clone();
                d.show_panel(path);
            }
            0
        }
        WM_GETMINMAXINFO if d.initialised => {
            let (cx, cy) = d.window_size(WIDTH, HEIGHT);
            (*(lp.0 as *mut MINMAXINFO)).ptMinTrackSize = POINT { x: cx, y: cy };
            1
        }
        WM_SIZE if d.initialised && wp.0 != SIZE_MINIMIZED as usize => {
            d.resized();
            0
        }
        WM_DPICHANGED => {
            let r = *(lp.0 as *const RECT);
            d.dpi_changed((wp.0 & 0xffff) as u32, Some(r));
            1
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            0
        }
        WM_CLOSE => {
            d.ended = Some(false);
            0
        }
        m if m == d.drag_message && m != 0 => {
            let result = d.drag_message(wp.0 as i32, &*(lp.0 as *const DRAGLISTINFO));
            SetWindowLongPtrW(hwnd, WINDOW_LONG_PTR_INDEX(0), result);
            1
        }
        _ => 0,
    }
}

const ID_ABOUT_LICENCE: i32 = 300;
const ID_ABOUT_TEXT: i32 = 301;

unsafe extern "system" fn about_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    if let Some(answer) = deck_ui::dialog_message(hwnd, msg, wp, lp) {
        return answer;
    }
    if msg == WM_INITDIALOG {
        deck_ui::style_window(hwnd);
    }
    let map = |x, y, cx, cy| {
        let mut r = RECT { left: x, top: y, right: x + cx, bottom: y + cy };
        let _ = MapDialogRect(hwnd, &mut r);
        r
    };
    let child = |class: PCWSTR, text: &str, style: u32, ex: u32, id: i32, r: RECT| {
        let t = wide(text);
        let h = CreateWindowExW(WINDOW_EX_STYLE(ex), class, PCWSTR(t.as_ptr()), WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style), r.left, r.top, r.right - r.left, r.bottom - r.top, Some(hwnd), Some(HMENU(id as usize as *mut _)), None, None).unwrap_or_default();
        send(h, WM_SETFONT, send(hwnd, WM_GETFONT, 0, 0) as usize, 1);
        deck_ui::style_control(h);
    };
    let edit = (ES_READONLY | ES_MULTILINE | ES_CENTER) as u32;
    match msg {
        WM_INITDIALOG if lp.0 == 0 => {
            let text = format!("{APP}\r\n\r\nVersion {}\r\n\r\nA terminal for SSH, Telnet, Rlogin, SUPDUP, Raw and serial sessions, with PuTTY's settings.", env!("CARGO_PKG_VERSION"));
            child(w!("EDIT"), &text, edit, 0x20000, ID_ABOUT_TEXT, map(10, 6, 250, 110));
            child(w!("BUTTON"), "View &Licence", WS_TABSTOP.0, 0, ID_ABOUT_LICENCE, map(6, 118, 70, 14));
            child(w!("BUTTON"), "&Close", WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32, 0, 1, map(216, 118, 48, 14));
            1
        }
        WM_INITDIALOG => {
            let text = LICENCE.replace("\r\n", "\n").replace('\n', "\r\n");
            child(w!("EDIT"), &text, (ES_READONLY | ES_MULTILINE) as u32 | WS_VSCROLL.0, 0x20000, ID_ABOUT_TEXT, map(10, 6, 250, 180));
            child(w!("BUTTON"), "OK", WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32, 0, 1, map(216, 190, 48, 14));
            1
        }
        WM_COMMAND => {
            match (wp.0 & 0xffff) as i32 {
                1 | 2 => {
                    let _ = EndDialog(hwnd, 1);
                }
                ID_ABOUT_LICENCE => show_box(hwnd, &format!("{APP} Licence"), 270, 210, 1),
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            let _ = EndDialog(hwnd, 1);
            0
        }
        _ => 0,
    }
}

fn show_box(owner: HWND, title: &str, width: i16, height: i16, param: isize) {
    let t = template(title, STYLE, width, height, 0);
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        DialogBoxIndirectParamW(Some(instance), t.as_ptr().cast(), Some(owner), Some(about_proc), LPARAM(param));
    }
}

fn about(owner: HWND) {
    show_box(owner, &format!("About {APP}"), 270, 136, 0);
}

fn run(owner: HWND, mid: bool, name: &str, settings: Settings) -> (bool, String, Settings) {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let protocol = settings.s("Protocol", "ssh");
    let kept = |name, least: i32| deck_ui::remembered(name).map_or(least, |v| (v as i32).clamp(least, 4000));
    let mut d = Dlg {
        hwnd: HWND::default(),
        owner,
        font: HFONT::default(),
        dpi: 96,
        size: (kept("SettingsWidth", WIDTH), kept("SettingsHeight", HEIGHT)),
        bx: 6,
        by: 13,
        mid,
        cbox: table::build(mid, &protocol, &user),
        path: "Session".into(),
        settings,
        names: Vec::new(),
        saved: name.to_string(),
        insts: Vec::new(),
        panel: Vec::new(),
        fixed: Vec::new(),
        titles: Vec::new(),
        tree: HWND::default(),
        refreshing: false,
        initialised: false,
        ended: None,
        focused: None,
        last_focused: None,
        fixed_pitch: true,
        drag_message: unsafe { RegisterWindowMessageW(w!("commctrl_DragListMsg")) },
        drag: (0, 0, false),
        log: Vec::new(),
    };
    d.reload_names();
    let title = format!("{APP} {}", if mid { "Reconfiguration" } else { "Configuration" });
    let ex = if TEST { (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW).0 } else { 0 };
    let tmpl = template(&title, RESIZABLE, WIDTH as i16, HEIGHT as i16, ex);
    unsafe {
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_TREEVIEW_CLASSES | ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES });
        let instance: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        DLG = &mut d;
        let parent = (!owner.is_invalid()).then_some(owner);
        let Ok(hwnd) = CreateDialogIndirectParamW(Some(instance), tmpl.as_ptr().cast(), parent, Some(proc), LPARAM(0)) else {
            DLG = std::ptr::null_mut();
            return (false, d.saved, d.settings);
        };
        if let Ok(icon) = LoadIconW(Some(instance), PCWSTR(1 as *const u16)) {
            send(hwnd, WM_SETICON, ICON_BIG as usize, icon.0 as isize);
        }
        let _ = ShowWindow(hwnd, if TEST { SW_SHOWNOACTIVATE } else { SW_SHOWNORMAL });
        let _ = EnableWindow(owner, false);
        if !TEST {
            let first = d.insts.iter().copied().find(|i| !d.cbox.sets[i.set].path.is_empty() && matches!(d.ctl(*i).kind, Kind::Edit { .. }));
            let _ = SetFocus(Some(first.map_or(d.tree, |i| d.item(i, 1))));
        } else {
            #[cfg(test)]
            if let Some(script) = SCRIPT {
                script(&mut d);
            }
            let ok = d.insts.iter().copied().find(|i| d.ctl(*i).bind == Bind::Part(Group::Saved, 5));
            if let Some(i) = ok.filter(|_| d.ended.is_none()) {
                let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(i.base as usize), LPARAM(0));
            }
        }

        let mut msg = MSG::default();
        // A double click ends the dialog only once the button is up, so that the
        // release doesn't land on the window underneath
        while d.ended.is_none() || GetCapture() == hwnd {
            let got = GetMessageW(&mut msg, None, 0, 0).0;
            if got <= 0 {
                if got == 0 {
                    PostQuitMessage(msg.wParam.0 as i32);
                }
                let _ = ReleaseCapture();
                d.ended = d.ended.or(Some(false));
            } else if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let _ = EnableWindow(owner, true);
        if !IsZoomed(hwnd).as_bool() {
            deck_ui::remember("SettingsWidth", d.size.0 as u32);
            deck_ui::remember("SettingsHeight", d.size.1 as u32);
        }
        let _ = DestroyWindow(hwnd);
        let _ = DeleteObject(d.font.into());
        DLG = std::ptr::null_mut();
    }
    (d.ended == Some(true), d.saved, d.settings)
}

/// Runs the dialog and returns the result with the session's name and settings.
/// Load, Save and Delete happen inside it. `font` isn't used: this is PuTTY's
/// dialog font. A new session starts from "Default Settings", as in PuTTY.
pub fn edit(owner: HWND, _font: HFONT, name: &str, settings: Settings) -> (Outcome, String, Settings) {
    let settings = if name.is_empty() && settings.iter().next().is_none() { store::defaults() } else { settings };
    let (open, name, settings) = run(owner, false, name, settings);
    (if open { Outcome::Open } else { Outcome::Cancelled }, name, settings)
}

/// PuTTY's Change Settings: the same dialog in mid-session mode.
pub fn reconfigure(owner: HWND, _font: HFONT, settings: Settings) -> Option<Settings> {
    let (apply, _, settings) = run(owner, true, "", settings);
    apply.then_some(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    fn with_script(script: fn(&mut Dlg), f: impl FnOnce()) {
        let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { SCRIPT = Some(script) };
        f();
        unsafe { SCRIPT = None };
    }

    fn sets(pairs: &[(&str, Value)]) -> Settings {
        let mut s = Settings::default();
        for (k, v) in pairs {
            s.set(k, v.clone());
        }
        s
    }

    fn host(name: &str) -> Settings {
        sets(&[("HostName", Value::Str(name.into()))])
    }

    fn click(d: &mut Dlg, g: Group, n: u8) {
        let i = d.part(g, n).unwrap();
        send(d.hwnd, WM_COMMAND, i.base as usize, d.item(i, 0).0 as isize);
    }

    fn panel(d: &mut Dlg, path: &str) {
        d.show_panel(path.to_string());
    }

    fn type_in(d: &Dlg, g: Group, n: u8, text: &str) {
        set_text(d.part_item(g, n, 1), text);
    }

    #[test]
    fn looking_at_every_panel_writes_nothing() {
        with_script(
            |d| {
                let paths: Vec<String> = d.cbox.sets.iter().map(|s| s.path.clone()).filter(|p| !p.is_empty()).collect();
                for p in paths {
                    panel(d, &p);
                }
            },
            || {
                let before = host("h");
                let (out, _, after) = edit(HWND::default(), HFONT::default(), "s", before.clone());
                assert!(out == Outcome::Open);
                assert_eq!(after, before);
            },
        );
    }

    #[test]
    fn saved_sessions_behave_like_putty() {
        with_script(
            |d| {
                // Blank name, and nothing selected: a beep and nothing saved
                d.saved.clear();
                d.log.clear();
                click(d, Group::Saved, 3);
                assert_eq!(d.log, ["beep"]);
                assert_eq!(d.names, ["Default Settings"]);

                // Delete can't take Default Settings, or nothing
                d.log.clear();
                click(d, Group::Saved, 4);
                assert_eq!(d.log, ["beep"]);

                type_in(d, Group::Saved, 0, "beta");
                click(d, Group::Saved, 3);
                type_in(d, Group::Saved, 0, "alpha");
                click(d, Group::Saved, 3);
                assert_eq!(d.names, ["Default Settings", "alpha", "beta"]);

                // Typing moves the list, and Load takes the session
                type_in(d, Group::Saved, 0, "be");
                assert_eq!(d.selected_session(), Some(2));
                d.settings = Settings::default();
                click(d, Group::Saved, 2);
                assert_eq!(d.settings.s("HostName", ""), "h");
                assert_eq!(d.saved, "beta");

                // A blank name with a session selected saves over that session
                type_in(d, Group::Saved, 0, "");
                send(d.part_item(Group::Saved, 1, 1), LB_SETCURSEL, 1, 0);
                d.settings.set("HostName", Value::Str("changed".into()));
                click(d, Group::Saved, 3);
                assert_eq!(store::load("alpha").s("HostName", ""), "changed");

                // Default Settings can be saved and loaded
                send(d.part_item(Group::Saved, 1, 1), LB_SETCURSEL, 0, 0);
                type_in(d, Group::Saved, 0, "");
                send(d.part_item(Group::Saved, 1, 1), LB_SETCURSEL, 0, 0);
                click(d, Group::Saved, 3);
                assert_eq!(store::defaults().s("HostName", ""), "changed");

                send(d.part_item(Group::Saved, 1, 1), LB_SETCURSEL, 1, 0);
                click(d, Group::Saved, 4);
                assert_eq!(d.names, ["Default Settings", "beta"]);
                store::delete("beta").unwrap();
                store::delete("Default Settings").unwrap();
            },
            || {
                edit(HWND::default(), HFONT::default(), "s", host("h"));
            },
        );
    }

    #[test]
    fn the_connection_type_changes_port_and_labels() {
        with_script(
            |d| {
                let radio = d.part(Group::Proto, 0).unwrap();
                let press = |d: &mut Dlg, b: usize| {
                    d.set_radio(radio, Some(b));
                    send(d.hwnd, WM_COMMAND, (radio.base + 1 + b as i32) as usize, d.item(radio, 1 + b as i32).0 as isize);
                };
                let list = d.part(Group::Proto, 1).unwrap();
                send(d.item(list, 1), CB_SETCURSEL, 0, 0);
                d.drop_changed(list);
                assert_eq!((d.protocol(), d.settings.i("PortNumber", 0)), ("telnet".to_string(), 23));
                assert_eq!(d.radio(radio), 2);
                press(d, 1);
                assert_eq!(d.protocol(), "serial");
                let hostbox = d.insts.iter().copied().find(|i| d.ctl(*i).bind == Bind::Host).unwrap();
                assert_eq!(text_of(d.item(hostbox, 0)), "Serial li&ne");
                press(d, 0);
                assert_eq!((d.protocol(), d.settings.i("PortNumber", 0)), ("ssh".to_string(), 22));
                assert_eq!(text_of(d.item(hostbox, 0)), "Host &Name (or IP address)");
                // A port the person changed stays
                d.set("PortNumber", 2222);
                let list = d.part(Group::Proto, 1).unwrap();
                send(d.item(list, 1), CB_SETCURSEL, 1, 0);
                d.drop_changed(list);
                assert_eq!(d.settings.i("PortNumber", 0), 2222);
            },
            || {
                edit(HWND::default(), HFONT::default(), "s", host("h"));
            },
        );
    }

    #[test]
    fn lists_write_putty_s_formats() {
        with_script(
            |d| {
                panel(d, "Connection/SSH/Tunnels");
                type_in(d, Group::Fwd, 3, "8080");
                type_in(d, Group::Fwd, 4, "web:80");
                click(d, Group::Fwd, 2);
                let dir = d.part(Group::Fwd, 5).unwrap();
                d.set_radio(dir, Some(2));
                type_in(d, Group::Fwd, 3, "1080");
                click(d, Group::Fwd, 2);
                assert_eq!(d.settings.s("PortForwardings", ""), r"D1080=,L8080=web\:80".replace(r"\:", ":"));
                d.log.clear();
                d.set_radio(dir, Some(0));
                type_in(d, Group::Fwd, 4, "nocolon");
                click(d, Group::Fwd, 2);
                assert!(d.log[0].starts_with("You need to specify a destination"));
                let list = d.part_item(Group::Fwd, 1, 1);
                assert_eq!(send(list, LB_GETCOUNT, 0, 0), 2);
                send(list, LB_SETCURSEL, 0, 0);
                click(d, Group::Fwd, 0);
                assert_eq!(d.settings.s("PortForwardings", ""), "L8080=web:80");
                assert_eq!(text_of(d.part_item(Group::Fwd, 3, 1)), "1080");

                panel(d, "Connection/Data");
                type_in(d, Group::Env, 0, "A");
                type_in(d, Group::Env, 1, "x,y");
                click(d, Group::Env, 2);
                assert_eq!(d.settings.s("Environment", ""), r"A=x\,y");
                send(d.part_item(Group::Env, 4, 1), LB_SETCURSEL, 0, 0);
                click(d, Group::Env, 3);
                assert_eq!(d.settings.s("Environment", ""), "");

                panel(d, "Connection/SSH/TTY");
                let list = d.part_item(Group::Tty, 0, 1);
                assert_eq!(send(list, LB_GETCOUNT, 0, 0), TTY_MODES.len() as isize);
                send(list, LB_SETCURSEL, 0, 0);
                let radio = d.part(Group::Tty, 2).unwrap();
                d.set_radio(radio, Some(1));
                click(d, Group::Tty, 1);
                let saved = d.settings.map("TerminalModes");
                assert_eq!(saved.len(), TTY_MODES.len());
                assert_eq!(saved[0], ("CS7".to_string(), "N".to_string()));

                panel(d, "Connection/SSH/Cipher");
                let i = d.insts.iter().copied().find(|i| matches!(d.ctl(*i).bind, Bind::Prefs(_))).unwrap();
                let list = d.item(i, 1);
                send(list, LB_SETCURSEL, 1, 0);
                send(d.hwnd, WM_COMMAND, (i.base + 2) as usize, d.item(i, 2).0 as isize);
                assert!(d.settings.s("Cipher", "").starts_with("chacha20,aes,aesgcm"), "{}", d.settings.s("Cipher", ""));

                panel(d, "Window/Colours");
                let list = d.part(Group::Colour, 0).unwrap();
                send(d.item(list, 1), LB_SETCURSEL, 8, 0);
                d.list_selected(list);
                assert_eq!(text_of(d.part_item(Group::Colour, 1, 1)), "187");
                assert!(!d.settings.has("Colour8"), "selecting a colour isn't changing it");
                type_in(d, Group::Colour, 2, "300");
                assert_eq!(d.settings.s("Colour8", ""), "187,255,0");

                panel(d, "Window/Selection/Copy");
                let list = d.part_item(Group::Class, 0, 1);
                send(list, LB_SETSEL, 1, 65);
                type_in(d, Group::Class, 1, "7");
                click(d, Group::Class, 2);
                assert_eq!(d.wordness(65), 7);
                assert_eq!(d.wordness(66), 2);
                assert_eq!(d.settings.s("Wordness64", "").split(',').count(), 32);
            },
            || {
                edit(HWND::default(), HFONT::default(), "s", host("h"));
            },
        );
    }

    #[test]
    fn the_password_box_is_masked_and_values_survive() {
        with_script(
            |d| {
                panel(d, "Connection/Proxy");
                let i = d.insts.iter().copied().find(|i| d.ctl(*i).bind == Bind::Str("ProxyPassword", "")).unwrap();
                assert_ne!(unsafe { GetWindowLongW(d.item(i, 1), GWL_STYLE) } as u32 & 0x20, 0);
                assert_eq!(text_of(d.item(i, 1)), "kept");
            },
            || {
                let mut s = host("h");
                s.set("ProxyPassword", Value::Str("kept".into()));
                s.set("SomethingNew", Value::Int(7));
                let (_, _, after) = edit(HWND::default(), HFONT::default(), "s", s);
                assert_eq!((after.s("ProxyPassword", ""), after.i("SomethingNew", 0)), ("kept".to_string(), 7));
            },
        );
    }

    #[test]
    fn numbers_keep_putty_s_units() {
        with_script(
            |d| {
                panel(d, "Terminal/Bell");
                let f = |d: &Dlg, key| d.insts.iter().copied().find(|i| matches!(d.ctl(*i).bind, Bind::Fixed(k, ..) if k == key)).unwrap();
                let i = f(d, "BellOverloadT");
                assert_eq!(text_of(d.item(i, 1)), "2");
                set_text(d.item(i, 1), "2.5");
                assert_eq!(d.settings.i("BellOverloadT", 0), 2500);
                panel(d, "Connection");
                let i = d.insts.iter().copied().find(|i| d.ctl(*i).bind == Bind::Ping).unwrap();
                set_text(d.item(i, 1), "125");
                assert_eq!((d.settings.i("PingInterval", 0), d.settings.i("PingIntervalSecs", 0)), (2, 5));
            },
            || {
                edit(HWND::default(), HFONT::default(), "s", host("h"));
            },
        );
    }

    #[test]
    fn it_rebuilds_for_another_dpi() {
        with_script(
            |d| {
                panel(d, "Window/Colours");
                let small = (d.bx, d.by);
                let mut r = RECT::default();
                unsafe { GetClientRect(d.hwnd, &mut r).unwrap() };
                d.dpi_changed(192, None);
                let mut r2 = RECT::default();
                unsafe { GetClientRect(d.hwnd, &mut r2).unwrap() };
                assert!(d.bx > small.0 && d.by > small.1);
                assert!(r2.right > r.right && r2.bottom > r.bottom, "{r:?} {r2:?}");
                assert_eq!(d.path, "Window/Colours");
                assert!(d.part(Group::Colour, 0).is_some());
                assert!(!d.part_item(Group::Saved, 5, 0).is_invalid());
            },
            || {
                edit(HWND::default(), HFONT::default(), "s", host("h"));
            },
        );
    }

    #[test]
    fn reconfigure_applies_and_shows_what_config_c_keeps() {
        with_script(
            |d| {
                assert_eq!(text_of(d.hwnd), "RUSTuTTY Reconfiguration");
                assert!(d.part(Group::Saved, 2).is_none() && d.part(Group::Saved, 4).is_none() && d.part(Group::Proto, 0).is_none());
                assert!(d.part(Group::Saved, 3).is_some());
                assert_eq!(text_of(d.part_item(Group::Saved, 5, 0)), "&Apply");
            },
            || {
                assert!(reconfigure(HWND::default(), HFONT::default(), host("h")).is_some());
            },
        );
        with_script(|d| d.ended = Some(false), || assert!(reconfigure(HWND::default(), HFONT::default(), host("h")).is_none()));
    }

    #[test]
    fn it_can_be_resized_without_losing_anything() {
        with_script(
            |d| {
                let bottom = |h: HWND| {
                    let mut r = RECT::default();
                    unsafe { GetWindowRect(h, &mut r).unwrap() };
                    r
                };
                panel(d, "Terminal");
                d.set_str("TerminalType", "vt220");
                let (before, tree) = (bottom(d.hwnd), bottom(d.tree));
                let hwnd = d.hwnd;
                let grow = |cx, cy| unsafe {
                    SetWindowPos(hwnd, None, 0, 0, cx, cy, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE).unwrap();
                };
                let (w, h) = (before.right - before.left, before.bottom - before.top);
                grow(w + 300, h + 200);
                assert!(d.size.0 > WIDTH && d.size.1 > HEIGHT);
                assert_eq!(d.path, "Terminal", "the same panel stays open");
                assert_eq!(bottom(d.tree).bottom - tree.bottom, 200, "the tree takes the new height");
                assert_eq!(bottom(d.tree).right - bottom(d.tree).left, tree.right - tree.left, "and keeps its width");
                assert_eq!(d.settings.s("TerminalType", ""), "vt220", "what was changed is still changed");
                let widest = d.panel.iter().map(|&h| bottom(h).right).max().unwrap();
                assert!(widest > before.right, "the panel's controls take the new width");
                grow(10, 10);
                let small = bottom(d.hwnd);
                assert_eq!((small.right - small.left, small.bottom - small.top), (w, h), "it can't be made too small for a panel");
                assert_eq!((d.size.0, d.size.1), (WIDTH, HEIGHT));
            },
            || {
                edit(HWND::default(), HFONT::default(), "s", host("example"));
            },
        );
    }

    /// Run by hand with RUSTUTTY_SHOTS set to a folder. Writes every panel as a BMP.
    #[test]
    #[ignore]
    fn render_every_panel() {
        with_script(
            |d| {
                let dir = std::env::var("RUSTUTTY_SHOTS").unwrap();
                let paths: Vec<String> = {
                    let mut p: Vec<String> = Vec::new();
                    for s in d.cbox.sets.iter().filter(|s| !s.path.is_empty()) {
                        if p.last() != Some(&s.path) {
                            p.push(s.path.clone());
                        }
                    }
                    p
                };
                for (n, p) in paths.iter().enumerate() {
                    panel(d, p);
                    shot(d.hwnd, &format!("{dir}/{n:02}-{}.bmp", p.replace(['/', ' '], "-")));
                }
            },
            || {
                edit(HWND::default(), HFONT::default(), "s", host("example"));
            },
        );
    }

    fn shot(hwnd: HWND, file: &str) {
        use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
        unsafe {
            let mut r = RECT::default();
            GetWindowRect(hwnd, &mut r).unwrap();
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let dc = GetDC(Some(hwnd));
            let mem = CreateCompatibleDC(Some(dc));
            let bmp = CreateCompatibleBitmap(dc, w, h);
            let old = SelectObject(mem, bmp.into());
            let _ = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(2));
            let mut info = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: 40, biWidth: w, biHeight: h, biPlanes: 1, biBitCount: 32, ..Default::default() }, ..Default::default() };
            let mut pixels = vec![0u8; (w * h * 4) as usize];
            GetDIBits(mem, bmp, 0, h as u32, Some(pixels.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
            SelectObject(mem, old);
            let mut out = Vec::new();
            out.extend(b"BM");
            out.extend((54 + pixels.len() as u32).to_le_bytes());
            out.extend([0u8; 4]);
            out.extend(54u32.to_le_bytes());
            out.extend(40u32.to_le_bytes());
            out.extend(w.to_le_bytes());
            out.extend(h.to_le_bytes());
            out.extend(1u16.to_le_bytes());
            out.extend(32u16.to_le_bytes());
            out.extend([0u8; 24]);
            out.extend(pixels);
            std::fs::write(file, out).unwrap();
        }
    }
}
