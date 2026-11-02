//! RUSTuTTY's look: dark and light palettes taken from NetTime Next, applied to
//! ordinary Win32 windows. Controls keep their own behaviour and are only
//! painted differently, so keyboard handling and tab order stay Windows' own.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use windows::core::{w, BOOL, PCSTR, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::GdiPlus::{
    FillModeAlternate, GdipAddPathArc, GdipClosePathFigure, GdipCreateFromHDC, GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill,
    GdipDeleteBrush, GdipDeleteGraphics, GdipDeletePath, GdipDeletePen, GdipDrawLine, GdipDrawPath, GdipFillPath, GdipSetPenEndCap,
    GdipSetPenStartCap, GdipSetSmoothingMode, GdiplusStartup, GdiplusStartupInput, GpGraphics, LineCapRound, SmoothingModeAntiAlias,
    UnitPixel,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SetDialogDpiChangeBehavior, DDC_DISABLE_ALL};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;
use winreg::enums::*;
use winreg::RegKey;

const KEY: &str = r"Software\PuTTYDeck";
const VALUE: &str = "Appearance";
const MSG_ID_TEXT: i32 = 1000;
// Undocumented in the headers, and stable since Windows 11 began.
const DWMWA_CORNER: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(33);
const DWMWA_BORDER: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);
const DWMWA_CAPTION: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(35);
const DWMWA_TEXT: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(36);

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: COLORREF,
    pub field: COLORREF,
    pub control: COLORREF,
    pub hover: COLORREF,
    pub selected: COLORREF,
    pub selected_text: COLORREF,
    pub border: COLORREF,
    pub text: COLORREF,
    pub muted: COLORREF,
    pub accent: COLORREF,
    pub on_accent: COLORREF,
    pub good: COLORREF,
    pub warn: COLORREF,
    pub bad: COLORREF,
}

const DARK_PALETTE: Palette = Palette {
    bg: rgb(15, 21, 31),
    field: rgb(23, 31, 44),
    control: rgb(32, 44, 61),
    hover: rgb(43, 63, 87),
    selected: rgb(35, 76, 118),
    selected_text: rgb(255, 255, 255),
    border: rgb(57, 73, 94),
    text: rgb(221, 229, 242),
    muted: rgb(169, 185, 204),
    accent: rgb(91, 169, 255),
    on_accent: rgb(12, 18, 27),
    good: rgb(60, 204, 164),
    warn: rgb(231, 176, 86),
    bad: rgb(255, 171, 171),
};

const LIGHT_PALETTE: Palette = Palette {
    bg: rgb(244, 246, 250),
    field: rgb(255, 255, 255),
    control: rgb(232, 236, 243),
    hover: rgb(216, 224, 236),
    selected: rgb(201, 222, 246),
    selected_text: rgb(23, 36, 59),
    border: rgb(169, 181, 200),
    text: rgb(23, 36, 59),
    muted: rgb(82, 98, 123),
    accent: rgb(40, 100, 197),
    on_accent: rgb(255, 255, 255),
    good: rgb(20, 120, 90),
    warn: rgb(146, 84, 36),
    bad: rgb(163, 50, 50),
};

static DARK: AtomicBool = AtomicBool::new(true);
static SAVE: AtomicBool = AtomicBool::new(false);
static BRUSHES: Mutex<Option<HashMap<u32, isize>>> = Mutex::new(None);

pub fn dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

pub fn palette() -> Palette {
    if dark() {
        DARK_PALETTE
    } else {
        LIGHT_PALETTE
    }
}

/// Reads the person's choice. A test build passes false, and uses no registry.
pub fn init(remember: bool) {
    SAVE.store(remember, Ordering::Relaxed);
    if remember {
        let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY);
        let value: Option<u32> = key.ok().and_then(|k| k.get_value(VALUE).ok());
        DARK.store(value != Some(0), Ordering::Relaxed);
    }
    app_mode();
}

/// A number kept for next time, such as a window's size.
pub fn remembered(name: &str) -> Option<u32> {
    if !SAVE.load(Ordering::Relaxed) {
        return None;
    }
    RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY).ok()?.get_value(name).ok()
}

pub fn remember(name: &str, value: u32) {
    if SAVE.load(Ordering::Relaxed) {
        if let Ok((key, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY) {
            let _ = key.set_value(name, &value);
        }
    }
}

/// Changes the appearance. The caller repaints its windows with `restyle`.
pub fn set_dark(on: bool) {
    DARK.store(on, Ordering::Relaxed);
    if SAVE.load(Ordering::Relaxed) {
        if let Ok((key, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY) {
            let _ = key.set_value(VALUE, &(on as u32));
        }
    }
    app_mode();
}

/// Popup menus only go dark through uxtheme's unnamed exports. Without them
/// menus just stay light, so a missing export doesn't matter.
fn app_mode() {
    unsafe {
        let Ok(theme) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(theme, PCSTR(135 as *const u8)) {
            let set: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            // ForceDark or ForceLight, so that Windows' own setting doesn't matter
            set(if dark() { 2 } else { 3 });
        }
        if let Some(f) = GetProcAddress(theme, PCSTR(136 as *const u8)) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(f);
            flush();
        }
    }
}

fn allow_dark(h: HWND) {
    unsafe {
        let Ok(theme) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(theme, PCSTR(133 as *const u8)) {
            let allow: unsafe extern "system" fn(HWND, i32) -> i32 = std::mem::transmute(f);
            allow(h, dark() as i32);
        }
    }
}

fn brush(c: COLORREF) -> HBRUSH {
    let mut held = BRUSHES.lock().unwrap();
    let map = held.get_or_insert_with(HashMap::new);
    HBRUSH(*map.entry(c.0).or_insert_with(|| unsafe { CreateSolidBrush(c).0 as isize }) as *mut _)
}

fn class_of(h: HWND) -> String {
    let mut b = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut b) };
    String::from_utf16_lossy(&b[..n.max(0) as usize])
}

fn text_of(h: HWND) -> Vec<u16> {
    unsafe {
        let mut b = vec![0u16; GetWindowTextLengthW(h).max(0) as usize + 1];
        let n = GetWindowTextW(h, &mut b);
        b.truncate(n.max(0) as usize);
        b
    }
}

fn scale(h: HWND, v: i32) -> i32 {
    v * unsafe { GetDpiForWindow(h) }.max(96) as i32 / 96
}

/// The title bar, border and corners of a top-level window.
pub fn style_window(h: HWND) {
    let p = palette();
    allow_dark(h);
    unsafe {
        let set = |a: DWMWINDOWATTRIBUTE, v: u32| {
            let _ = DwmSetWindowAttribute(h, a, (&v as *const u32).cast(), 4);
        };
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, dark() as u32);
        set(DWMWA_CORNER, 2); // DWMWCP_ROUND
        set(DWMWA_BORDER, p.border.0);
        set(DWMWA_CAPTION, p.bg.0);
        set(DWMWA_TEXT, p.text.0);
    }
}

/// Fits one child control to the appearance. Safe to call again after a change.
pub fn style_control(h: HWND) {
    let p = palette();
    let explorer = if dark() { w!("DarkMode_Explorer") } else { w!("Explorer") };
    let fields = if dark() { w!("DarkMode_CFD") } else { w!("Explorer") };
    allow_dark(h);
    unsafe {
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        let frame = |h: HWND| {
            if ex & WS_EX_CLIENTEDGE.0 != 0 {
                let _ = SetWindowSubclass(h, Some(frame_proc), 2, 0);
            }
        };
        match class_of(h).as_str() {
            "Button" if GetWindowLongW(h, GWL_STYLE) & 0xf == 7 => {
                let _ = SetWindowSubclass(h, Some(group_proc), 1, 0);
            }
            "ComboBox" => {
                let _ = SetWindowTheme(h, fields, PCWSTR::null());
                // A list that can't be typed in is one box, so it's drawn here
                if GetWindowLongW(h, GWL_STYLE) & 3 == CBS_DROPDOWNLIST as i32 {
                    let _ = SetWindowSubclass(h, Some(combo_proc), 3, 0);
                }
            }
            "Edit" => {
                let _ = SetWindowTheme(h, fields, PCWSTR::null());
                frame(h);
            }
            "ListBox" => {
                let _ = SetWindowTheme(h, explorer, PCWSTR::null());
                frame(h);
            }
            "SysTreeView32" => {
                let _ = SetWindowTheme(h, explorer, PCWSTR::null());
                SendMessageW(h, TVM_SETBKCOLOR, None, Some(LPARAM(p.field.0 as isize)));
                SendMessageW(h, TVM_SETTEXTCOLOR, None, Some(LPARAM(p.text.0 as isize)));
                SendMessageW(h, TVM_SETLINECOLOR, None, Some(LPARAM(p.border.0 as isize)));
                frame(h);
            }
            _ => {
                let _ = SetWindowTheme(h, explorer, PCWSTR::null());
            }
        }
    }
}

/// Styles a window and all of its children and paints them again.
pub fn restyle(h: HWND) {
    unsafe extern "system" fn each(h: HWND, _: LPARAM) -> BOOL {
        style_control(h);
        TRUE
    }
    unsafe {
        style_window(h);
        let _ = EnumChildWindows(Some(h), Some(each), LPARAM(0));
        let _ = RedrawWindow(Some(h), None, None, RDW_ERASE | RDW_FRAME | RDW_INVALIDATE | RDW_ALLCHILDREN);
    }
}

/// What every window using this look does with the colour messages.
pub unsafe fn ctl_colour(msg: u32, wp: WPARAM, lp: LPARAM) -> Option<isize> {
    let p = palette();
    let dc = HDC(wp.0 as *mut _);
    let child = HWND(lp.0 as *mut _);
    let back = match msg {
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => p.field,
        WM_CTLCOLORSTATIC if class_of(child) == "Edit" => p.field,
        WM_CTLCOLORSTATIC | WM_CTLCOLORDLG | WM_CTLCOLORBTN => p.bg,
        _ => return None,
    };
    let enabled = msg == WM_CTLCOLORDLG || windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(child).as_bool();
    SetTextColor(dc, if enabled { p.text } else { p.muted });
    SetBkColor(dc, back);
    Some(brush(back).0 as isize)
}

fn gdiplus() {
    static START: Once = Once::new();
    START.call_once(|| unsafe {
        let mut token = 0usize;
        let input = GdiplusStartupInput { GdiplusVersion: 1, ..Default::default() };
        GdiplusStartup(&mut token, &input, std::ptr::null_mut());
    });
}

fn argb(c: COLORREF) -> u32 {
    0xff00_0000 | (c.0 & 0xff) << 16 | (c.0 & 0xff00) | (c.0 >> 16 & 0xff)
}

/// A smooth shape on a device context.
struct Pen(*mut GpGraphics);

impl Pen {
    unsafe fn on(dc: HDC) -> Self {
        gdiplus();
        let mut g = std::ptr::null_mut();
        GdipCreateFromHDC(dc, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        Pen(g)
    }

    unsafe fn round(&self, r: RECT, radius: i32, fill: Option<COLORREF>, line: Option<(COLORREF, f32)>) {
        let (x, y, w, h) = (r.left as f32, r.top as f32, (r.right - r.left - 1) as f32, (r.bottom - r.top - 1) as f32);
        let d = (radius * 2) as f32;
        let mut path = std::ptr::null_mut();
        GdipCreatePath(FillModeAlternate, &mut path);
        GdipAddPathArc(path, x, y, d, d, 180., 90.);
        GdipAddPathArc(path, x + w - d, y, d, d, 270., 90.);
        GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0., 90.);
        GdipAddPathArc(path, x, y + h - d, d, d, 90., 90.);
        GdipClosePathFigure(path);
        if let Some(c) = fill {
            let mut b = std::ptr::null_mut();
            GdipCreateSolidFill(argb(c), &mut b);
            GdipFillPath(self.0, b.cast(), path);
            GdipDeleteBrush(b.cast());
        }
        if let Some((c, width)) = line {
            let mut pen = std::ptr::null_mut();
            GdipCreatePen1(argb(c), width, UnitPixel, &mut pen);
            GdipDrawPath(self.0, pen, path);
            GdipDeletePen(pen);
        }
        GdipDeletePath(path);
    }

    unsafe fn lines(&self, c: COLORREF, width: f32, points: &[(f32, f32)]) {
        let mut pen = std::ptr::null_mut();
        GdipCreatePen1(argb(c), width, UnitPixel, &mut pen);
        GdipSetPenStartCap(pen, LineCapRound);
        GdipSetPenEndCap(pen, LineCapRound);
        for w in points.windows(2) {
            GdipDrawLine(self.0, pen, w[0].0, w[0].1, w[1].0, w[1].1);
        }
        GdipDeletePen(pen);
    }
}

impl Drop for Pen {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteGraphics(self.0);
        }
    }
}

pub fn fill(dc: HDC, r: &RECT, c: COLORREF) {
    unsafe {
        FillRect(dc, r, brush(c));
    }
}

pub fn round(dc: HDC, r: RECT, radius: i32, back: Option<COLORREF>, line: Option<COLORREF>) {
    unsafe { Pen::on(dc).round(r, radius, back, line.map(|c| (c, 1.))) }
}

/// The small cross on a row or tab.
pub fn cross(dc: HDC, x: i32, y: i32, arm: i32, c: COLORREF) {
    let (x, y, a) = (x as f32, y as f32, arm as f32);
    unsafe {
        let p = Pen::on(dc);
        p.lines(c, 1.4, &[(x - a, y - a), (x + a, y + a)]);
        p.lines(c, 1.4, &[(x + a, y - a), (x - a, y + a)]);
    }
}

/// Draws buttons, check boxes and radio buttons. Answers NM_CUSTOMDRAW for a
/// button, whose behaviour stays Windows' own.
pub unsafe fn custom_draw(lp: LPARAM, primary: bool) -> Option<isize> {
    let n = &*(lp.0 as *const NMCUSTOMDRAW);
    if n.hdr.code != NM_CUSTOMDRAW || class_of(n.hdr.hwndFrom) != "Button" {
        return None;
    }
    if n.dwDrawStage != CDDS_PREPAINT {
        return None;
    }
    let h = n.hdr.hwndFrom;
    let p = palette();
    let dc = n.hdc;
    let style = GetWindowLongW(h, GWL_STYLE) as u32;
    let kind = style & 0xf;
    let state = n.uItemState;
    let (held, hot, off, focus) = (state.0 & CDIS_SELECTED.0 != 0, state.0 & CDIS_HOT.0 != 0, state.0 & CDIS_DISABLED.0 != 0, state.0 & CDIS_FOCUS.0 != 0);
    let r = n.rc;
    fill(dc, &r, p.bg);
    let font = SendMessageW(h, WM_GETFONT, None, None);
    let old = SelectObject(dc, HFONT(font.0 as *mut _).into());
    SetBkMode(dc, TRANSPARENT);
    let mut text = text_of(h);
    let pen = Pen::on(dc);
    if matches!(kind, 2 | 3 | 4 | 5 | 6 | 9) {
        let round_mark = matches!(kind, 4 | 9);
        // PuTTY's layout leaves the width of a standard 13px box for this
        let size = scale(h, 13);
        let top = r.top + (r.bottom - r.top - size) / 2;
        let bx = RECT { left: r.left + 1, top, right: r.left + 1 + size, bottom: top + size };
        let check = SendMessageW(h, BM_GETCHECK, None, None).0 as u32;
        let on = check != 0;
        let (back, line) = if on && !off { (p.accent, p.accent) } else { (p.field, if hot || focus { p.accent } else { p.border }) };
        pen.round(bx, if round_mark { size / 2 } else { scale(h, 3) }, Some(back), Some((line, 1.)));
        if on {
            let mark = if off { p.muted } else { p.on_accent };
            let (x, y, s) = (bx.left as f32, bx.top as f32, size as f32);
            if round_mark {
                let inset = size * 3 / 10;
                pen.round(RECT { left: bx.left + inset, top: bx.top + inset, right: bx.right - inset, bottom: bx.bottom - inset }, size / 4, Some(mark), None);
            } else {
                pen.lines(mark, 1.8, &[(x + s * 0.24, y + s * 0.52), (x + s * 0.42, y + s * 0.70), (x + s * 0.76, y + s * 0.30)]);
            }
        }
        SetTextColor(dc, if off { p.muted } else { p.text });
        let mut area = RECT { left: bx.right + scale(h, 5), ..r };
        let wrap = style & BS_MULTILINE as u32 != 0;
        let format = DT_LEFT | DT_NOCLIP | if wrap { DT_WORDBREAK } else { DT_SINGLELINE | DT_VCENTER };
        if wrap {
            let mut measure = area;
            let tall = DrawTextW(dc, &mut text, &mut measure, format | DT_CALCRECT);
            area.top += (r.bottom - r.top - tall).max(0) / 2;
        }
        DrawTextW(dc, &mut text, &mut area, format);
    } else {
        let main = primary || kind == 1;
        let (back, line, ink) = match (main, off) {
            (_, true) => (p.control, p.control, p.muted),
            (true, _) if held => (p.selected, p.accent, p.selected_text),
            (true, _) if dark() => (p.selected, p.selected, p.selected_text),
            (true, _) => (p.accent, p.accent, p.on_accent),
            (false, _) if held => (p.selected, p.selected, p.selected_text),
            (false, _) if hot => (p.hover, p.hover, p.text),
            _ => (p.control, p.control, p.text),
        };
        let shape = RECT { left: r.left + 1, top: r.top + 1, right: r.right - 1, bottom: r.bottom - 1 };
        pen.round(shape, scale(h, 5), Some(back), Some(if focus { (p.accent, 1.5) } else { (line, 1.) }));
        SetTextColor(dc, ink);
        let mut area = r;
        DrawTextW(dc, &mut text, &mut area, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
    }
    drop(pen);
    SelectObject(dc, old);
    Some(CDRF_SKIPDEFAULT as isize)
}

/// Group boxes as cards: an outline, with the title in the top edge.
unsafe extern "system" fn group_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _: usize, _: usize) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let p = palette();
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(h, &mut ps);
            let mut r = RECT::default();
            let _ = GetClientRect(h, &mut r);
            let font = SendMessageW(h, WM_GETFONT, None, None);
            let old = SelectObject(dc, HFONT(font.0 as *mut _).into());
            let mut text = text_of(h);
            let mut size = SIZE::default();
            let _ = GetTextExtentPoint32W(dc, &text, &mut size);
            let edge = RECT { top: r.top + size.cy / 2, ..r };
            round(dc, edge, scale(h, 6), None, Some(p.border));
            if !text.is_empty() {
                let gap = scale(h, 4);
                let left = r.left + scale(h, 10);
                let back = RECT { left, top: r.top, right: left + size.cx + gap * 2, bottom: r.top + size.cy };
                fill(dc, &back, p.bg);
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, p.muted);
                let mut area = RECT { left: left + gap, ..back };
                DrawTextW(dc, &mut text, &mut area, DT_SINGLELINE | DT_LEFT | DT_TOP);
            }
            SelectObject(dc, old);
            let _ = EndPaint(h, &ps);
            LRESULT(0)
        }
        _ => DefSubclassProc(h, msg, wp, lp),
    }
}

unsafe extern "system" fn combo_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _: usize, _: usize) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let p = palette();
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(h, &mut ps);
            let mut r = RECT::default();
            let _ = GetClientRect(h, &mut r);
            let on = windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(h).as_bool();
            let focused = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() == h;
            fill(dc, &r, p.bg);
            round(dc, r, scale(h, 4), Some(if on { p.field } else { p.bg }), Some(if focused { p.accent } else { p.border }));
            let font = SendMessageW(h, WM_GETFONT, None, None);
            let old = SelectObject(dc, HFONT(font.0 as *mut _).into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, if on { p.text } else { p.muted });
            let mut text = text_of(h);
            let arrow = scale(h, 22);
            let mut area = RECT { left: r.left + scale(h, 7), right: r.right - arrow, ..r };
            DrawTextW(dc, &mut text, &mut area, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
            SelectObject(dc, old);
            let (x, y, a) = ((r.right - arrow / 2) as f32, ((r.top + r.bottom) / 2) as f32, scale(h, 4) as f32);
            Pen::on(dc).lines(if on { p.muted } else { p.border }, 1.4, &[(x - a, y - a / 2.), (x, y + a / 2.), (x + a, y - a / 2.)]);
            let _ = EndPaint(h, &ps);
            LRESULT(0)
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            let done = DefSubclassProc(h, msg, wp, lp);
            let _ = InvalidateRect(Some(h), None, false);
            done
        }
        _ => DefSubclassProc(h, msg, wp, lp),
    }
}

/// A flat border in place of the sunken edge, lit while the control has the
/// keyboard. Scroll bars are in the frame too, so Windows paints first.
unsafe extern "system" fn frame_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _: usize, _: usize) -> LRESULT {
    let done = DefSubclassProc(h, msg, wp, lp);
    if matches!(msg, WM_NCPAINT | WM_SETFOCUS | WM_KILLFOCUS) {
        let p = palette();
        let dc = GetWindowDC(Some(h));
        let mut r = RECT::default();
        let _ = GetWindowRect(h, &mut r);
        let outer = RECT { left: 0, top: 0, right: r.right - r.left, bottom: r.bottom - r.top };
        let focused = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() == h;
        let _ = FrameRect(dc, &outer, brush(if focused { p.accent } else { p.border }));
        let inner = RECT { left: 1, top: 1, right: outer.right - 1, bottom: outer.bottom - 1 };
        let _ = FrameRect(dc, &inner, brush(p.field));
        ReleaseDC(Some(h), dc);
    }
    done
}

/// A font in the look's face, for a window's DPI.
pub fn ui_font(dpi: u32, points: i32, weight: i32) -> HFONT {
    let height = -((points * dpi as i32 + 36) / 72);
    unsafe {
        CreateFontW(height, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"))
    }
}

/// Handles what a dialog using this look needs. Call it first in a dialog
/// procedure: the answer is what the procedure returns.
pub unsafe fn dialog_message(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<isize> {
    if msg == WM_NOTIFY {
        let result = custom_draw(lp, false)?;
        SetWindowLongPtrW(hwnd, WINDOW_LONG_PTR_INDEX(0), result);
        return Some(1);
    }
    ctl_colour(msg, wp, lp)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Info,
    Question,
    Warning,
    Error,
}

struct Message {
    text: Vec<u16>,
    tone: Tone,
    yes_no: bool,
    font: HFONT,
    bold: HFONT,
}

fn message_template(title: &str) -> Vec<u32> {
    let mut t: Vec<u16> = Vec::new();
    let style = 0x80 | 0x40 | WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0;
    t.extend([style as u16, (style >> 16) as u16, 0, 0, 0, 0, 0, 200, 80, 0, 0]);
    t.extend(title.encode_utf16().chain([0]));
    t.push(9);
    t.extend("Segoe UI".encode_utf16().chain([0]));
    t.chunks(2).map(|c| c[0] as u32 | (c.get(1).copied().unwrap_or(0) as u32) << 16).collect()
}

unsafe extern "system" fn message_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    if let Some(answer) = dialog_message(hwnd, msg, wp, lp) {
        return answer;
    }
    match msg {
        WM_INITDIALOG => {
            let m = &mut *(lp.0 as *mut Message);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp.0);
            style_window(hwnd);
            // Sized here for the monitor it ends up on, so Windows mustn't rescale it too
            let _ = SetDialogDpiChangeBehavior(hwnd, DDC_DISABLE_ALL, DDC_DISABLE_ALL);
            let mut around = RECT::default();
            let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
            if owner.is_invalid() || GetWindowRect(owner, &mut around).is_err() {
                let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(monitor, &mut info);
                around = info.rcWork;
            }
            let _ = SetWindowPos(hwnd, None, (around.left + around.right) / 2, (around.top + around.bottom) / 2, 0, 0, SWP_NOZORDER | SWP_NOSIZE);
            let dpi = GetDpiForWindow(hwnd).max(96);
            m.font = ui_font(dpi, 10, 400);
            m.bold = ui_font(dpi, 16, 600);
            let px = |v: i32| v * dpi as i32 / 96;
            let dc = GetDC(Some(hwnd));
            let old = SelectObject(dc, m.font.into());
            let mut text = m.text.clone();
            let mut fit = RECT { right: px(420), ..Default::default() };
            DrawTextW(dc, &mut text, &mut fit, DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX);
            SelectObject(dc, old);
            ReleaseDC(Some(hwnd), dc);
            let left = px(72);
            let scrolls = fit.bottom > px(440);
            fit.bottom = fit.bottom.min(px(440));
            fit.right += if scrolls { px(24) } else { 0 };
            let (width, height) = (left + fit.right + px(24), fit.bottom.max(px(40)) + px(92));
            let mut outer = RECT { right: width, bottom: height, ..Default::default() };
            let frame = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
            let _ = windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi(&mut outer, frame, false, WINDOW_EX_STYLE(0), dpi);
            let (cx, cy) = (outer.right - outer.left, outer.bottom - outer.top);
            let (x, y) = ((around.left + around.right - cx) / 2, (around.top + around.bottom - cy) / 2);
            let _ = SetWindowPos(hwnd, None, x, y, cx, cy, SWP_NOZORDER);
            let instance = GetModuleHandleW(None).ok().map(|i| HINSTANCE(i.0));
            let child = |class: PCWSTR, text: PCWSTR, style: u32, id: i32, r: [i32; 4]| {
                let h = CreateWindowExW(WINDOW_EX_STYLE(0), class, text, WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style), r[0], r[1], r[2], r[3], Some(hwnd), Some(HMENU(id as usize as *mut _)), instance, None).unwrap_or_default();
                SendMessageW(h, WM_SETFONT, Some(WPARAM(m.font.0 as usize)), Some(LPARAM(1)));
                style_control(h);
                h
            };
            let mut owned = m.text.clone();
            let r = [left, px(22), fit.right + px(4), fit.bottom + px(4)];
            if scrolls {
                owned = String::from_utf16_lossy(&owned).replace('\n', "\r\n").encode_utf16().collect();
                owned.push(0);
                child(w!("EDIT"), PCWSTR(owned.as_ptr()), (ES_MULTILINE | ES_READONLY) as u32 | WS_VSCROLL.0, MSG_ID_TEXT, r);
            } else {
                owned.push(0);
                child(w!("STATIC"), PCWSTR(owned.as_ptr()), 0x80, MSG_ID_TEXT, r);
            }
            let (bw, bh, by) = (px(92), px(32), height - px(48));
            let style = WS_TABSTOP.0;
            let first = if m.yes_no {
                child(w!("BUTTON"), w!("No"), style, IDNO.0, [width - px(16) - bw, by, bw, bh]);
                child(w!("BUTTON"), w!("&Yes"), style | BS_DEFPUSHBUTTON as u32, IDYES.0, [width - px(24) - bw * 2, by, bw, bh])
            } else {
                child(w!("BUTTON"), w!("OK"), style | BS_DEFPUSHBUTTON as u32, IDOK.0, [width - px(16) - bw, by, bw, bh])
            };
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(first));
            let _ = SetForegroundWindow(hwnd);
            0
        }
        WM_PAINT => {
            let m = &*(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Message);
            let p = palette();
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let (mark, colour) = match m.tone {
                Tone::Info => ("i", p.accent),
                Tone::Question => ("?", p.accent),
                Tone::Warning => ("!", p.warn),
                Tone::Error => ("!", p.bad),
            };
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let px = |v: i32| v * dpi / 96;
            let mut r = RECT { left: px(24), top: px(22), right: px(60), bottom: px(58) };
            round(dc, r, px(18), Some(colour), None);
            let old = SelectObject(dc, m.bold.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, p.on_accent);
            let mut glyph: Vec<u16> = mark.encode_utf16().collect();
            DrawTextW(dc, &mut glyph, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SelectObject(dc, old);
            let _ = EndPaint(hwnd, &ps);
            1
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xffff) as i32;
            if id == IDYES.0 || id == IDNO.0 || id == IDOK.0 || id == IDCANCEL.0 {
                let _ = EndDialog(hwnd, id as isize);
            }
            1
        }
        WM_CLOSE => {
            let _ = EndDialog(hwnd, IDCANCEL.0 as isize);
            1
        }
        _ => 0,
    }
}

fn show(owner: Option<HWND>, title: &str, text: &str, tone: Tone, yes_no: bool) -> i32 {
    let mut m = Message { text: text.encode_utf16().collect(), tone, yes_no, font: HFONT::default(), bold: HFONT::default() };
    let template = message_template(title);
    unsafe {
        let instance = GetModuleHandleW(None).ok().map(|i| HINSTANCE(i.0));
        let answer = DialogBoxIndirectParamW(instance, template.as_ptr().cast(), owner, Some(message_proc), LPARAM(&mut m as *mut _ as isize));
        for f in [m.font, m.bold] {
            if !f.is_invalid() {
                let _ = DeleteObject(f.into());
            }
        }
        answer as i32
    }
}

/// A message with an OK button.
pub fn tell(owner: Option<HWND>, title: &str, text: &str, tone: Tone) {
    show(owner, title, text, tone, false);
}

/// A question. True only for Yes: Esc and the close button mean No.
pub fn ask(owner: Option<HWND>, title: &str, text: &str, tone: Tone) -> bool {
    show(owner, title, text, tone, true) == IDYES.0
}
