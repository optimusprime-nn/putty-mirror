//! The session settings window, built from `deck_core::options`.

use crate::wide;
use deck_core::options::{Kind, Option_, CATEGORIES, OPTIONS};
use deck_core::settings::{Settings, Value};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

const ID_CATEGORY: usize = 200;
const ID_SAVE: usize = 201;
const ID_OPEN: usize = 202;
const ID_CANCEL: usize = 203;
const ID_FIRST: usize = 300;
// Sizes are at 96 DPI, and scaled with the fonts
const WIDTH: i32 = 1210;
const HEIGHT: i32 = 720;
const NAMES: i32 = 150;
const ROW: i32 = 28;
const LABEL: i32 = 310;
const CONTROL: i32 = 190;
const COLUMN: i32 = LABEL + CONTROL + 14;
/// Rows before a category wraps into its second column.
const ROWS: i32 = (HEIGHT - 140) / ROW;

/// What the person chose to do with the settings.
#[derive(PartialEq)]
pub enum Outcome {
    Cancelled,
    Save,
    Open,
}

struct Row {
    option: &'static Option_,
    label: HWND,
    control: HWND,
}

struct Window {
    rows: Vec<Row>,
    category: HWND,
    name: HWND,
    name_label: HWND,
    settings: Settings,
    outcome: Option<Outcome>,
    dpi: u32,
}

static mut WINDOW: *mut Window = std::ptr::null_mut();

fn scaled(v: i32, dpi: u32) -> i32 {
    v * dpi as i32 / 96
}

fn text_of(h: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(h) } as usize;
    let mut buf = vec![0u16; len + 1];
    let n = unsafe { GetWindowTextW(h, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

impl Window {
    fn show_category(&self, index: usize) {
        let px = |v| scaled(v, self.dpi);
        let category = CATEGORIES.get(index).copied().unwrap_or("Session");
        let mut row = 0;
        for r in &self.rows {
            let on = r.option.category == category;
            let cmd = if on { SW_SHOWNA } else { SW_HIDE };
            unsafe {
                let _ = ShowWindow(r.label, cmd);
                let _ = ShowWindow(r.control, cmd);
                if on {
                    let tall = if matches!(r.option.kind, Kind::Choice(..)) { 200 } else { 24 };
                    let (x, y) = (NAMES + 16 + (row / ROWS) * COLUMN, 44 + (row % ROWS) * ROW);
                    let _ = MoveWindow(r.label, px(x), px(y + 3), px(LABEL - 6), px(22), true);
                    let _ = MoveWindow(r.control, px(x + LABEL), px(y), px(CONTROL), px(tall), true);
                    row += 1;
                }
            }
        }
    }

    /// Copies the controls back into the settings. A control still showing the
    /// default for a setting that was never set stays unset, so that a blank
    /// box can't replace a default such as the terminal type.
    fn collect(&mut self) {
        for r in &self.rows {
            let key = r.option.key;
            let value = match r.option.kind {
                Kind::Text => Value::Str(text_of(r.control)),
                Kind::Number(default) => Value::Int(text_of(r.control).trim().parse().unwrap_or(default)),
                Kind::Flag(_) => Value::Int((unsafe { SendMessageW(r.control, BM_GETCHECK, None, None) }.0 == 1) as u32),
                Kind::Choice(default, choices) => {
                    let i = unsafe { SendMessageW(r.control, CB_GETCURSEL, None, None) }.0;
                    let chosen = usize::try_from(i).ok().and_then(|i| choices.get(i));
                    Value::Int(chosen.map_or(default, |(_, v)| *v))
                }
            };
            let default = match r.option.kind {
                Kind::Text => Value::Str(String::new()),
                Kind::Number(d) | Kind::Choice(d, _) => Value::Int(d),
                Kind::Flag(d) => Value::Int(d as u32),
            };
            if self.settings.has(key) || value != default {
                self.settings.set(key, value);
            }
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(win) = WINDOW.as_mut() else { return DefWindowProcW(hwnd, msg, wp, lp) };
    match msg {
        WM_COMMAND => {
            let (id, code) = (wp.0 & 0xffff, (wp.0 >> 16) as u32 & 0xffff);
            match (id, code) {
                (ID_CATEGORY, LBN_SELCHANGE) => {
                    let i = SendMessageW(win.category, LB_GETCURSEL, None, None).0;
                    win.show_category(i.max(0) as usize);
                }
                (ID_SAVE, _) => {
                    win.collect();
                    win.outcome = Some(Outcome::Save);
                }
                (ID_OPEN, _) => {
                    win.collect();
                    win.outcome = Some(Outcome::Open);
                }
                (ID_CANCEL, _) => win.outcome = Some(Outcome::Cancelled),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            win.outcome = Some(Outcome::Cancelled);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Runs the window and returns the result with the session's new name and settings.
pub fn edit(owner: HWND, font: HFONT, name: &str, settings: Settings) -> (Outcome, String, Settings) {
    debug_assert!(CATEGORIES.iter().all(|c| OPTIONS.iter().filter(|o| o.category == *c).count() as i32 <= ROWS * 2));
    // A test window opens off-screen, and presses Open by itself.
    let test = cfg!(feature = "test-hooks");
    unsafe {
        let dpi = GetDpiForWindow(owner).max(96);
        let instance: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut _),
            lpszClassName: w!("RustuTTYSettings"),
            ..Default::default()
        });
        let Ok(hwnd) = CreateWindowExW(
            if test { WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW } else { WS_EX_DLGMODALFRAME },
            w!("RustuTTYSettings"),
            w!("Session settings"),
            WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN,
            if test { -4000 } else { CW_USEDEFAULT }, CW_USEDEFAULT, scaled(WIDTH, dpi), scaled(HEIGHT, dpi),
            Some(owner), None, Some(instance), None,
        ) else {
            return (Outcome::Cancelled, name.to_string(), settings);
        };
        let child = |class: PCWSTR, text: PCWSTR, style: u32, ex: WINDOW_EX_STYLE, id: usize, x, y, cx, cy| {
            let h = CreateWindowExW(ex, class, text, WS_CHILD | WINDOW_STYLE(style) | WS_TABSTOP, scaled(x, dpi), scaled(y, dpi), scaled(cx, dpi), scaled(cy, dpi),
                Some(hwnd), Some(HMENU(id as *mut _)), Some(instance), None).unwrap();
            SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            h
        };
        let visible = WS_VISIBLE.0;
        let name_label = child(w!("STATIC"), w!("Saved session name"), visible, WINDOW_EX_STYLE(0), 0, 12, 12, NAMES, 22);
        let name_edit = child(w!("EDIT"), PCWSTR(wide(name).as_ptr()), visible | ES_AUTOHSCROLL as u32, WS_EX_CLIENTEDGE, 0, NAMES + 16, 10, 300, 24);
        let category = child(w!("LISTBOX"), w!(""), visible | LBS_NOTIFY as u32 | WS_BORDER.0, WINDOW_EX_STYLE(0), ID_CATEGORY, 12, 44, NAMES - 8, HEIGHT - 150);
        for c in CATEGORIES {
            let text = wide(c);
            SendMessageW(category, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
        }
        SendMessageW(category, LB_SETCURSEL, Some(WPARAM(0)), None);

        let mut rows = Vec::new();
        for (i, option) in OPTIONS.iter().enumerate() {
            let id = ID_FIRST + i;
            let label = child(w!("STATIC"), PCWSTR(wide(option.label).as_ptr()), 0, WINDOW_EX_STYLE(0), 0, 0, 0, 10, 10);
            let control = match option.kind {
                Kind::Text => child(w!("EDIT"), PCWSTR(wide(&settings.s(option.key, "")).as_ptr()), ES_AUTOHSCROLL as u32, WS_EX_CLIENTEDGE, id, 0, 0, 10, 10),
                Kind::Number(d) => child(w!("EDIT"), PCWSTR(wide(&settings.i(option.key, d).to_string()).as_ptr()), ES_NUMBER as u32, WS_EX_CLIENTEDGE, id, 0, 0, 10, 10),
                Kind::Flag(d) => {
                    let h = child(w!("BUTTON"), w!(""), BS_AUTOCHECKBOX as u32, WINDOW_EX_STYLE(0), id, 0, 0, 10, 10);
                    SendMessageW(h, BM_SETCHECK, Some(WPARAM(settings.b(option.key, d) as usize)), None);
                    h
                }
                Kind::Choice(d, choices) => {
                    let h = child(w!("COMBOBOX"), w!(""), CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0, WINDOW_EX_STYLE(0), id, 0, 0, 10, 10);
                    let current = settings.i(option.key, d);
                    for (n, (text, value)) in choices.iter().enumerate() {
                        let text = wide(text);
                        SendMessageW(h, CB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
                        if *value == current {
                            SendMessageW(h, CB_SETCURSEL, Some(WPARAM(n)), None);
                        }
                    }
                    h
                }
            };
            rows.push(Row { option, label, control });
        }
        let y = HEIGHT - 84;
        child(w!("BUTTON"), w!("Open"), visible | BS_DEFPUSHBUTTON as u32, WINDOW_EX_STYLE(0), ID_OPEN, WIDTH - 310, y, 90, 28);
        child(w!("BUTTON"), w!("Save"), visible, WINDOW_EX_STYLE(0), ID_SAVE, WIDTH - 212, y, 90, 28);
        child(w!("BUTTON"), w!("Cancel"), visible, WINDOW_EX_STYLE(0), ID_CANCEL, WIDTH - 114, y, 90, 28);

        let mut window = Window { rows, category, name: name_edit, name_label, settings, outcome: None, dpi };
        WINDOW = &mut window;
        window.show_category(0);
        let _ = ShowWindow(hwnd, if test { SW_SHOWNOACTIVATE } else { SW_SHOW });
        let _ = EnableWindow(owner, false);
        if test {
            let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(ID_OPEN), LPARAM(0));
        } else {
            let _ = SetFocus(Some(name_edit));
        }

        let mut msg = MSG::default();
        while window.outcome.is_none() && GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let new_name = text_of(window.name);
        let _ = window.name_label;
        let _ = EnableWindow(owner, true);
        let _ = DestroyWindow(hwnd);
        WINDOW = std::ptr::null_mut();
        let _ = UnregisterClassW(w!("RustuTTYSettings"), Some(instance));
        (window.outcome.unwrap_or(Outcome::Cancelled), new_name, window.settings)
    }
}
