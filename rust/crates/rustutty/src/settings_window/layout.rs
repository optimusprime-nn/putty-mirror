//! A port of the layout engine in PuTTY's windows/controls.c. Everything here is
//! in dialog units, and the result is a list of windows to make, in tab order.

use super::table::{Ctl, Kind, Set, NONE};
use windows::Win32::UI::WindowsAndMessaging::*;

const GAP_BETWEEN: i32 = 3;
const GAP_WITHIN: i32 = 1;
const GAP_X_BOX: i32 = 7;
const GAP_Y_BOX: i32 = 4;
const STATIC_HEIGHT: i32 = 8;
const TITLE_HEIGHT: i32 = 12;
const CHECK_HEIGHT: i32 = 8;
const RADIO_HEIGHT: i32 = 8;
const EDIT_HEIGHT: i32 = 12;
const LIST_HEIGHT: i32 = 11;
const LIST_INCREMENT: i32 = 8;
const COMBO_HEIGHT: i32 = 12;
const BUTTON_HEIGHT: i32 = 14;

const CHILD: u32 = WS_CHILD.0 | WS_VISIBLE.0;
const TAB: u32 = CHILD | WS_TABSTOP.0;
const BS_PUSH: u32 = 0x4000; // BS_NOTIFY
const AUTO_H: u32 = 0x80; // ES_AUTOHSCROLL
const PASSWORD: u32 = 0x20; // ES_PASSWORD
const GROUPBOX: u32 = 7;
const CHECKBOX: u32 = 3; // BS_AUTOCHECKBOX
const RADIO: u32 = 9; // BS_AUTORADIOBUTTON
const DEFAULT_BUTTON: u32 = 1;
const COMBO_EDIT: u32 = 0x202; // CBS_DROPDOWN | CBS_HASSTRINGS
const COMBO_LIST: u32 = 0x203; // CBS_DROPDOWNLIST | CBS_HASSTRINGS
const LIST: u32 = 0x1 | 0x40 | 0x80; // LBS_NOTIFY | LBS_HASSTRINGS | LBS_USETABSTOPS
const DRAG_LIST: u32 = 0x40 | 0x80;
const MULTI: u32 = 0x8; // LBS_MULTIPLESEL
const OWNER_DRAW: u32 = 0xd; // SS_OWNERDRAW
const NO_WRAP: u32 = 0xc; // SS_LEFTNOWORDWRAP
const CLIENT_EDGE: u32 = 0x200;

/// Which control of the set a window belongs to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Owner {
    Box,
    Title,
    Ctl(usize),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub owner: Owner,
    /// Offset from the control's first dialog id.
    pub sub: i32,
    pub ids: i32,
    pub class: &'static str,
    pub style: u32,
    pub ex: u32,
    pub text: String,
    /// Left, top, width, height.
    pub r: [i32; 4],
    pub tabs: Vec<i32>,
    pub drag: bool,
    pub default: bool,
    pub cancel: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Pos {
    pub ypos: i32,
    pub width: i32,
    pub xoff: i32,
    box_start: i32,
    live: bool,
}

/// `ctlposinit`, for a client area `client` dialog units wide.
pub fn start(client: i32, left: i32, right: i32, top: i32) -> Pos {
    Pos { ypos: top, width: client - 2 * GAP_BETWEEN - left - right, xoff: left, box_start: 0, live: true }
}

struct Out<'a> {
    items: Vec<Item>,
    /// Breaks text to fit a width, and says how many lines it took.
    wrap: &'a dyn Fn(&str, i32) -> (String, i32),
    owner: Owner,
    ids: i32,
}

impl Out<'_> {
    fn add(&mut self, p: &Pos, sub: i32, class: &'static str, style: u32, ex: u32, text: &str, r: [i32; 4]) {
        if p.live {
            let r = [r[0] + p.xoff, r[1], r[2], r[3]];
            self.items.push(Item { owner: self.owner, sub, ids: self.ids, class, style, ex, text: text.into(), r, tabs: Vec::new(), drag: false, default: false, cancel: false });
        }
    }

    fn label(&mut self, p: &mut Pos, text: Option<&str>, gap: i32) {
        if let Some(text) = text {
            self.add(p, 0, "STATIC", CHILD, 0, text, [GAP_BETWEEN, p.ypos, p.width, STATIC_HEIGHT]);
            p.ypos += STATIC_HEIGHT + gap;
        }
    }

    fn static_left(&mut self, p: &Pos, text: Option<&str>, height: i32, lwid: i32) {
        if let Some(text) = text {
            self.add(p, 0, "STATIC", CHILD, 0, text, [GAP_BETWEEN, p.ypos + (height - STATIC_HEIGHT) / 2, lwid, STATIC_HEIGHT]);
        }
    }

    fn radios(&mut self, p: &mut Pos, label: Option<&str>, across: i32, buttons: &[String]) {
        let mut bottom = 0;
        if let Some(text) = label {
            self.add(p, 0, "STATIC", CHILD, 0, text, [GAP_BETWEEN, p.ypos, p.width, STATIC_HEIGHT]);
            p.ypos += STATIC_HEIGHT + GAP_WITHIN;
            bottom = STATIC_HEIGHT;
        }
        let mut group = WS_GROUP.0;
        let mut i = 0;
        let n = buttons.len();
        for (j, text) in buttons.iter().enumerate() {
            if i == across {
                p.ypos += bottom + if across > 1 { GAP_BETWEEN } else { GAP_WITHIN };
                i = 0;
            }
            let left = GAP_BETWEEN + i * (p.width + GAP_BETWEEN) / across;
            let right = if j < n - 1 { (i + 1) * (p.width + GAP_BETWEEN) / across - left } else { p.width - left };
            bottom = RADIO_HEIGHT;
            self.add(p, 1 + j as i32, "BUTTON", BS_PUSH | RADIO | TAB | group, 0, text, [left, p.ypos, right, RADIO_HEIGHT]);
            group = 0;
            i += 1;
        }
        p.ypos += bottom + GAP_BETWEEN;
    }

    fn control(&mut self, c: &Ctl, p: &mut Pos, outer_width: i32) {
        let width_at = |p: &Pos, percent: i32| {
            let rpos = GAP_BETWEEN + (100 - percent) * (p.width + GAP_BETWEEN) / 100;
            (rpos, rpos - 2 * GAP_BETWEEN, p.width + GAP_BETWEEN - rpos)
        };
        match &c.kind {
            Kind::Columns(_) | Kind::TabDelay(_) => {}
            Kind::Text(text) => {
                let (wrapped, lines) = (self.wrap)(text, p.width);
                let wrapped = escape(&wrapped, NONE);
                let h = STATIC_HEIGHT * lines;
                self.add(p, 0, "STATIC", CHILD | NO_WRAP, 0, &wrapped, [GAP_BETWEEN, p.ypos, p.width, h]);
                p.ypos += h + GAP_BETWEEN;
            }
            Kind::Edit { label, sc, pct, combo, password } => {
                self.ids = 2;
                let text = label.as_deref().map(|t| escape(t, *sc));
                let edit = TAB | AUTO_H | if *password { PASSWORD } else { 0 };
                let combo_style = TAB | WS_VSCROLL.0 | COMBO_EDIT;
                if *pct == 100 {
                    self.label(p, text.as_deref(), GAP_WITHIN);
                    if *combo {
                        self.add(p, 1, "COMBOBOX", combo_style, CLIENT_EDGE, "", [GAP_BETWEEN, p.ypos, p.width, COMBO_HEIGHT * 10]);
                        p.ypos += COMBO_HEIGHT + GAP_BETWEEN;
                    } else {
                        self.add(p, 1, "EDIT", edit, CLIENT_EDGE, "", [GAP_BETWEEN, p.ypos, p.width, EDIT_HEIGHT]);
                        p.ypos += EDIT_HEIGHT + GAP_BETWEEN;
                    }
                } else {
                    let (rpos, lwid, rwid) = width_at(p, *pct);
                    self.static_left(p, text.as_deref(), EDIT_HEIGHT, lwid);
                    self.add(p, 1, "EDIT", edit, CLIENT_EDGE, "", [rpos, p.ypos, rwid, EDIT_HEIGHT]);
                    p.ypos += EDIT_HEIGHT + GAP_BETWEEN;
                }
            }
            Kind::Radio { label, sc, across, buttons } => {
                self.ids = buttons.len() as i32 + 1;
                let text = label.as_deref().map(|t| escape(t, *sc));
                let names: Vec<String> = buttons.iter().map(|(t, s)| escape(t, *s)).collect();
                self.radios(p, text.as_deref(), *across, &names);
            }
            Kind::Check { label, sc } => {
                self.add(p, 0, "BUTTON", BS_PUSH | CHECKBOX | TAB, 0, &escape(label, *sc), [GAP_BETWEEN, p.ypos, p.width, CHECK_HEIGHT]);
                p.ypos += CHECK_HEIGHT + GAP_BETWEEN;
            }
            Kind::Button { label, sc, default, cancel } => {
                let style = BS_PUSH | TAB | if *default { DEFAULT_BUTTON } else { 0 };
                self.add(p, 0, "BUTTON", style, 0, &escape(label, *sc), [GAP_BETWEEN, p.ypos, p.width, BUTTON_HEIGHT]);
                if let Some(i) = self.items.last_mut().filter(|_| p.live) {
                    (i.default, i.cancel) = (*default, *cancel);
                }
                p.ypos += BUTTON_HEIGHT + GAP_BETWEEN;
            }
            Kind::List { label, sc, height, drag, multi, pct, tabs } => {
                self.ids = 2;
                let text = label.as_deref().map(|t| escape(t, *sc));
                if *drag {
                    self.ids = 4;
                    self.label(p, text.as_deref(), GAP_WITHIN);
                    let list = LIST_HEIGHT + (*height - 1) * LIST_INCREMENT;
                    let buttons = 2 * BUTTON_HEIGHT + GAP_BETWEEN;
                    let (total, at) = if list > buttons { (list, (list - buttons) / 2) } else { (buttons, 0) };
                    let w = p.width + GAP_BETWEEN;
                    let (left1, right1) = (w * 5 / 100 + GAP_BETWEEN, w * 80 / 100);
                    let (left2, right2) = (w * 80 / 100 + GAP_BETWEEN, w);
                    self.add(p, 1, "LISTBOX", TAB | WS_VSCROLL.0 | DRAG_LIST, CLIENT_EDGE, "", [left1, p.ypos, right1 - left1, list]);
                    if let Some(i) = self.items.last_mut().filter(|_| p.live) {
                        i.drag = true;
                    }
                    self.add(p, 2, "BUTTON", BS_PUSH | TAB, 0, "&Up", [left2, p.ypos + at, right2 - left2, BUTTON_HEIGHT]);
                    self.add(p, 3, "BUTTON", BS_PUSH | TAB, 0, "&Down", [left2, p.ypos + at + BUTTON_HEIGHT + GAP_BETWEEN, right2 - left2, BUTTON_HEIGHT]);
                    p.ypos += total + GAP_BETWEEN;
                } else if *height == 0 {
                    let style = TAB | WS_VSCROLL.0 | COMBO_LIST;
                    if *pct == 100 {
                        self.label(p, text.as_deref(), 0);
                        self.add(p, 1, "COMBOBOX", style, CLIENT_EDGE, "", [GAP_BETWEEN, p.ypos, p.width, COMBO_HEIGHT * 4]);
                    } else {
                        let (rpos, lwid, rwid) = width_at(p, *pct);
                        self.static_left(p, text.as_deref(), COMBO_HEIGHT, lwid);
                        self.add(p, 1, "COMBOBOX", style, CLIENT_EDGE, "", [rpos, p.ypos, rwid, COMBO_HEIGHT * 4]);
                    }
                    p.ypos += COMBO_HEIGHT + GAP_BETWEEN;
                } else {
                    self.label(p, text.as_deref(), GAP_WITHIN);
                    let h = LIST_HEIGHT + (*height - 1) * LIST_INCREMENT;
                    let style = TAB | WS_VSCROLL.0 | LIST | if *multi { MULTI } else { 0 };
                    self.add(p, 1, "LISTBOX", style, CLIENT_EDGE, "", [GAP_BETWEEN, p.ypos, p.width, h]);
                    p.ypos += h + GAP_BETWEEN;
                }
                if !tabs.is_empty() && p.live {
                    let width = outer_width * *pct;
                    let mut percent = 0;
                    let stops: Vec<i32> = tabs[..tabs.len() - 1]
                        .iter()
                        .map(|t| {
                            percent += t;
                            width * percent / 10000
                        })
                        .collect();
                    if let Some(i) = self.items.iter_mut().rev().find(|i| i.class == "LISTBOX") {
                        i.tabs = stops;
                    }
                }
            }
            Kind::File { label, sc, .. } => {
                self.ids = 3;
                self.label(p, Some(&escape(label, *sc)), GAP_WITHIN);
                let rpos = GAP_BETWEEN + 3 * (p.width + GAP_BETWEEN) / 4;
                let (lwid, rwid) = (rpos - 2 * GAP_BETWEEN, p.width + GAP_BETWEEN - rpos);
                let height = BUTTON_HEIGHT;
                self.add(p, 1, "EDIT", TAB | AUTO_H, CLIENT_EDGE, "", [GAP_BETWEEN, p.ypos + (height - EDIT_HEIGHT) / 2, lwid, EDIT_HEIGHT]);
                self.add(p, 2, "BUTTON", BS_PUSH | TAB, 0, "Browse...", [rpos, p.ypos, rwid, BUTTON_HEIGHT]);
                p.ypos += height + GAP_BETWEEN;
            }
            Kind::Font { label, sc } => {
                self.ids = 3;
                self.add(p, 0, "STATIC", CHILD | NO_WRAP, 0, &escape(label, *sc), [GAP_BETWEEN, p.ypos, p.width, STATIC_HEIGHT]);
                p.ypos += STATIC_HEIGHT + GAP_BETWEEN;
                let rpos = GAP_BETWEEN + 3 * (p.width + GAP_BETWEEN) / 4;
                let (lwid, rwid) = (rpos - 2 * GAP_BETWEEN, p.width + GAP_BETWEEN - rpos);
                self.add(p, 1, "STATIC", CHILD, 0, "", [GAP_BETWEEN, p.ypos + (BUTTON_HEIGHT - STATIC_HEIGHT) / 2, lwid, STATIC_HEIGHT]);
                self.add(p, 2, "BUTTON", BS_PUSH | TAB, 0, "Change...", [rpos, p.ypos, rwid, BUTTON_HEIGHT]);
                p.ypos += BUTTON_HEIGHT + GAP_BETWEEN;
            }
        }
    }
}

/// Doubles ampersands, and puts one before the shortcut letter.
pub fn escape(text: &str, shortcut: char) -> String {
    let mut out = String::new();
    let mut pending = shortcut != NONE;
    for c in text.chars() {
        if c == '&' {
            out.push('&');
        } else if pending && c.to_ascii_lowercase() == shortcut.to_ascii_lowercase() {
            out.push('&');
            pending = false;
        }
        out.push(c);
    }
    out
}

/// `winctrl_layout`: lays out one control set, moving `cp` down past it.
pub fn layout(set: &Set, cp: &mut Pos, wrap: &dyn Fn(&str, i32) -> (String, i32)) -> Vec<Item> {
    let mut out = Out { items: Vec::new(), wrap, owner: Owner::Box, ids: 1 };
    let boxed = set.name.as_deref().is_some_and(|n| !n.is_empty());
    if boxed {
        cp.box_start = cp.ypos;
        match &set.title {
            Some(_) => cp.ypos += STATIC_HEIGHT,
            None => cp.box_start -= STATIC_HEIGHT / 2,
        }
        cp.ypos += GAP_Y_BOX;
        cp.width -= 2 * GAP_X_BOX;
        cp.xoff += GAP_X_BOX;
    }
    if set.name.is_none() {
        out.owner = Owner::Title;
        let title = set.title.clone().unwrap_or_default();
        out.add(cp, 0, "STATIC", CHILD | OWNER_DRAW, 0, &title, [GAP_BETWEEN, cp.ypos, cp.width, TITLE_HEIGHT]);
        cp.ypos += TITLE_HEIGHT + GAP_BETWEEN;
    }

    let mut columns = vec![*cp];
    let mut delayed: Vec<(usize, Pos)> = Vec::new();
    for (i, c) in set.ctrls.iter().enumerate() {
        out.owner = Owner::Ctl(i);
        out.ids = 1;
        match &c.kind {
            Kind::Columns(widths) => {
                if columns.len() == 1 {
                    let first = columns[0];
                    columns = vec![first; widths.len()];
                    let mut left = 0;
                    for (col, w) in columns.iter_mut().zip(widths) {
                        let right = left + w;
                        let lx = col.xoff + left * (col.width + GAP_BETWEEN) / 100;
                        let rx = col.xoff + right * (col.width + GAP_BETWEEN) / 100;
                        col.xoff = lx;
                        col.width = rx - lx - GAP_BETWEEN;
                        left = right;
                    }
                } else {
                    let maxy = columns.iter().map(|c| c.ypos).max().unwrap();
                    columns = vec![Pos { ypos: maxy, ..*cp }];
                }
            }
            Kind::TabDelay(which) => {
                if let Some((_, pos)) = delayed.iter().find(|(d, _)| d == which) {
                    let mut pos = *pos;
                    out.owner = Owner::Ctl(*which);
                    out.control(&set.ctrls[*which], &mut pos, cp.width);
                }
            }
            _ => {
                let (start, end) = (c.col.0, c.col.0 + c.col.1);
                let mut pos = columns[start];
                pos.width = columns[end - 1].width + (columns[end - 1].xoff - columns[start].xoff);
                for col in &columns[start..end] {
                    pos.ypos = pos.ypos.max(col.ypos);
                }
                if c.delay {
                    delayed.push((i, pos));
                    pos.live = false;
                }
                out.control(c, &mut pos, cp.width);
                for col in &mut columns[start..end] {
                    col.ypos = pos.ypos;
                }
            }
        }
    }
    for c in &columns {
        cp.ypos = cp.ypos.max(c.ypos);
    }
    if boxed {
        out.owner = Owner::Box;
        out.ids = 1;
        cp.xoff -= GAP_X_BOX;
        cp.width += 2 * GAP_X_BOX;
        cp.ypos += GAP_Y_BOX - GAP_BETWEEN;
        let title = set.title.clone().unwrap_or_default();
        let r = [GAP_BETWEEN, cp.box_start, cp.width, cp.ypos - cp.box_start];
        out.add(cp, 0, "BUTTON", GROUPBOX | CHILD, 0, &title, r);
        cp.ypos += GAP_Y_BOX;
    }
    out.items
}

#[cfg(test)]
mod tests {
    use super::super::table::build;
    use super::*;
    use std::collections::HashSet;

    fn one_line(text: &str, width: i32) -> (String, i32) {
        (text.into(), (text.len() as i32 * 4 + width - 1) / width.max(1))
    }

    #[test]
    fn every_panel_lays_out_without_losing_a_control() {
        for (mid, proto) in [(false, "ssh"), (true, "ssh"), (true, "telnet"), (true, "serial")] {
            let b = build(mid, proto, "u");
            let panels: HashSet<&str> = b.sets.iter().map(|s| s.path.as_str()).filter(|p| !p.is_empty()).collect();
            for path in panels {
                let mut cp = start(300, 100, 3, 13);
                for set in b.sets.iter().filter(|s| s.path == path) {
                    let items = layout(set, &mut cp, &one_line);
                    for (i, c) in set.ctrls.iter().enumerate() {
                        if !matches!(c.kind, Kind::Columns(_) | Kind::TabDelay(_)) {
                            assert!(items.iter().any(|t| t.owner == Owner::Ctl(i)), "{path}: {c:?}");
                        }
                    }
                }
                assert!(cp.ypos <= 245, "{path} runs to {}", cp.ypos);
            }
        }
    }

    #[test]
    fn shortcuts_and_ampersands() {
        assert_eq!(escape("Host Name", 'n'), "Host &Name");
        assert_eq!(escape("&Y and", NONE), "&&Y and");
    }

    #[test]
    fn the_buttons_stay_in_their_columns() {
        let b = build(false, "ssh", "u");
        let mut cp = start(300, 3, 3, 235);
        let base = b.sets.iter().find(|s| s.path.is_empty()).unwrap();
        let items = layout(base, &mut cp, &one_line);
        let left: Vec<i32> = items.iter().map(|i| i.r[0]).collect();
        assert!(left[0] > left[3] && left[1] > left[0], "{left:?}");
        assert!(items.iter().all(|i| i.r[1] == 235));
    }
}
