//! The settings window: custom-drawn cards with toggles, ‹ value › pickers,
//! a shortcut recorder and a couple of buttons. Every change is applied and
//! saved immediately.
//!
//! Keyboard: Up/Down or Tab move between rows, Left/Right change a picker,
//! Space/Enter toggle or activate, Esc closes (or cancels shortcut recording).

use crate::gfx::{self, contains, icon, rect, Canvas, Fonts};
use crate::hotkey::{self, Hotkey};
use crate::settings::{Settings, FONT_SIZES, HISTORY_SIZES, IMAGE_HISTORY_SIZES, POSITIONS, THEMES};
use crate::theme::Theme;
use crate::app;
use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use windows_sys::w;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Item {
    Theme,
    FontSize,
    ShowPreview,
    Position,
    Hotkey,
    PasteOnEnter,
    NormalizeEol,
    HistorySize,
    SaveImages,
    ImageHistory,
    RespectPrivacy,
    ClearHistory,
    StartWithWindows,
    OpenFolder,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctl {
    Toggle,
    Choice,
    Shortcut,
    Button,
}

const SECTIONS: &[(&str, &[Item])] = &[
    ("Appearance", &[Item::Theme, Item::FontSize, Item::ShowPreview, Item::Position]),
    ("Shortcut & pasting", &[Item::Hotkey, Item::PasteOnEnter, Item::NormalizeEol]),
    ("History", &[Item::HistorySize, Item::SaveImages, Item::ImageHistory, Item::RespectPrivacy, Item::ClearHistory]),
    ("System", &[Item::StartWithWindows, Item::OpenFolder]),
];

impl Item {
    fn label(self) -> &'static str {
        match self {
            Item::Theme => "Theme",
            Item::FontSize => "Text size",
            Item::ShowPreview => "Preview pane",
            Item::Position => "Open popup at",
            Item::Hotkey => "Shortcut",
            Item::PasteOnEnter => "Paste into the active app",
            Item::NormalizeEol => "Windows line endings",
            Item::HistorySize => "Keep text",
            Item::SaveImages => "Save images",
            Item::ImageHistory => "Keep images",
            Item::RespectPrivacy => "Ignore password managers",
            Item::ClearHistory => "Clear history",
            Item::StartWithWindows => "Start with Windows",
            Item::OpenFolder => "Data folder",
        }
    }

    fn desc(self) -> &'static str {
        match self {
            Item::Theme => "System follows Windows light/dark mode and accent colour",
            Item::FontSize => "Scales the whole popup",
            Item::ShowPreview => "Full text of the selected clip, with line numbers for code",
            Item::Position => "Falls back to the mouse when the app has no text cursor",
            Item::Hotkey => "Click, then press the keys you want",
            Item::PasteOnEnter => "When off, Enter only copies to the clipboard",
            Item::NormalizeEol => "Convert LF line endings to CRLF when pasting",
            Item::HistorySize => "Pinned items are always kept",
            Item::SaveImages => "Screenshots and copied pictures",
            Item::ImageHistory => "Kept separately from text; images take more space",
            Item::RespectPrivacy => "Skip copies marked private, e.g. passwords",
            Item::ClearHistory => "Deletes everything except pinned items",
            Item::StartWithWindows => "Launch clipstash when you sign in",
            Item::OpenFolder => "History database and settings.json",
        }
    }

    fn ctl(self) -> Ctl {
        match self {
            Item::Theme | Item::FontSize | Item::Position | Item::HistorySize | Item::ImageHistory => Ctl::Choice,
            Item::Hotkey => Ctl::Shortcut,
            Item::ClearHistory | Item::OpenFolder => Ctl::Button,
            _ => Ctl::Toggle,
        }
    }

    fn toggle_value(self, s: &Settings) -> bool {
        match self {
            Item::ShowPreview => s.show_preview,
            Item::PasteOnEnter => s.paste_on_enter,
            Item::NormalizeEol => s.normalize_line_endings,
            Item::RespectPrivacy => s.respect_privacy,
            Item::SaveImages => s.save_images,
            Item::StartWithWindows => s.start_with_windows,
            _ => false,
        }
    }

    fn flip(self, s: &mut Settings) {
        match self {
            Item::ShowPreview => s.show_preview ^= true,
            Item::PasteOnEnter => s.paste_on_enter ^= true,
            Item::NormalizeEol => s.normalize_line_endings ^= true,
            Item::RespectPrivacy => s.respect_privacy ^= true,
            Item::SaveImages => s.save_images ^= true,
            Item::StartWithWindows => s.start_with_windows ^= true,
            _ => {}
        }
    }

    fn value_text(self, s: &Settings) -> String {
        match self {
            Item::Theme => theme_label(&s.theme).into(),
            Item::FontSize => format!("{} pt", s.font_size),
            Item::Position => match s.position.as_str() {
                "mouse" => "Mouse pointer",
                "center" => "Screen centre",
                _ => "Text cursor",
            }
            .into(),
            Item::HistorySize => format!("{} items", s.max_history),
            Item::ImageHistory => format!("{} images", s.max_images),
            Item::Hotkey => s.hotkey.clone(),
            Item::ClearHistory => "Clear…".into(),
            Item::OpenFolder => "Open".into(),
            _ => String::new(),
        }
    }

    /// Moves a picker one step; wraps around for named options.
    fn step(self, s: &mut Settings, dir: i32) {
        fn cycle(list: &[&str], cur: &str, dir: i32) -> String {
            let i = list.iter().position(|x| *x == cur).unwrap_or(0) as i32;
            list[(i + dir).rem_euclid(list.len() as i32) as usize].to_string()
        }
        match self {
            Item::Theme => s.theme = cycle(THEMES, &s.theme, dir),
            Item::Position => s.position = cycle(POSITIONS, &s.position, dir),
            Item::FontSize => s.font_size = (s.font_size as i32 + dir).clamp(*FONT_SIZES.start() as i32, *FONT_SIZES.end() as i32) as u32,
            Item::HistorySize => s.max_history = step_in(HISTORY_SIZES, s.max_history, dir),
            Item::ImageHistory => s.max_images = step_in(IMAGE_HISTORY_SIZES, s.max_images, dir),
            _ => {}
        }
    }
}

/// Next value in `list` from `cur` (which may be a hand-edited value not in it).
fn step_in(list: &[u32], cur: u32, dir: i32) -> u32 {
    let i = list.iter().position(|&n| n >= cur).unwrap_or(list.len() - 1) as i32;
    list[(i + dir).clamp(0, list.len() as i32 - 1) as usize]
}

fn theme_label(name: &str) -> &'static str {
    match name {
        "light" => "Light",
        "dark" => "Dark",
        "nord" => "Nord",
        "dracula" => "Dracula",
        "catppuccin" => "Catppuccin Mocha",
        "solarized-light" => "Solarized Light",
        "solarized-dark" => "Solarized Dark",
        _ => "System",
    }
}

fn items() -> Vec<Item> {
    SECTIONS.iter().flat_map(|(_, items)| items.iter().copied()).collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Card,
    Left,
    Right,
    Button,
}

struct State {
    fonts: Option<Fonts>,
    focus: usize,
    /// Focus ring only after keyboard use, as in Windows itself.
    keyboard: bool,
    hover: Option<usize>,
    tracking_mouse: bool,
    scroll: i32,
    capturing: bool,
    status: Option<(String, bool)>,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State {
        fonts: None,
        focus: 0,
        keyboard: false,
        hover: None,
        tracking_mouse: false,
        scroll: 0,
        capturing: false,
        status: None,
    }) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

const WIDTH: i32 = 640;
const HEIGHT: i32 = 680;
const HEADER: i32 = 92;
const SECTION_H: i32 = 44;
const ROW_H: i32 = 70;

enum Line {
    Section(&'static str),
    Row(usize, Item),
}

/// Content lines with their y offset (unscrolled, relative to content top) and height.
fn lines(f: &Fonts) -> (Vec<(Line, i32, i32)>, i32) {
    let mut out = Vec::new();
    let mut y = 0;
    let mut idx = 0;
    for (title, items) in SECTIONS {
        out.push((Line::Section(title), y, f.s(SECTION_H)));
        y += f.s(SECTION_H);
        for &item in *items {
            out.push((Line::Row(idx, item), y, f.s(ROW_H)));
            y += f.s(ROW_H);
            idx += 1;
        }
    }
    (out, y + f.s(16))
}

fn content_top(f: &Fonts) -> i32 {
    f.s(HEADER)
}

fn card_rect(f: &Fonts, client: &RECT, y: i32, h: i32) -> RECT {
    rect(f.s(24), y, client.right - f.s(24), y + h - f.s(4))
}

fn control_rect(f: &Fonts, card: &RECT, ctl: Ctl) -> RECT {
    let (w, h) = match ctl {
        Ctl::Toggle => (f.s(44), f.s(22)),
        Ctl::Choice | Ctl::Shortcut => (f.s(210), f.s(32)),
        Ctl::Button => (f.s(110), f.s(32)),
    };
    let cy = (card.top + card.bottom) / 2;
    rect(card.right - f.s(18) - w, cy - h / 2, card.right - f.s(18), cy - h / 2 + h)
}

fn client_rect(hwnd: HWND) -> RECT {
    let mut rc: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(hwnd, &mut rc) };
    rc
}

fn hit_test(hwnd: HWND, x: i32, y: i32) -> Option<(usize, Item, Part)> {
    let client = client_rect(hwnd);
    with(|st| {
        let f = st.fonts.as_ref()?;
        let top = content_top(f) - st.scroll;
        if y < content_top(f) {
            return None;
        }
        let (lines, _) = lines(f);
        for (line, ly, h) in lines {
            let Line::Row(idx, item) = line else { continue };
            let card = card_rect(f, &client, top + ly, h);
            if !contains(&card, x, y) {
                continue;
            }
            let ctl = control_rect(f, &card, item.ctl());
            let part = if !contains(&ctl, x, y) {
                Part::Card
            } else if item.ctl() == Ctl::Button {
                Part::Button
            } else if item.ctl() == Ctl::Choice && x < ctl.left + (ctl.right - ctl.left) * 2 / 5 {
                Part::Left
            } else {
                Part::Right
            };
            return Some((idx, item, part));
        }
        None
    })
}

pub fn open() {
    let a = app();
    let existing = a.settings_wnd.get();
    if !existing.is_null() {
        unsafe {
            ShowWindow(existing, SW_SHOWNORMAL);
            SetForegroundWindow(existing);
        }
        return;
    }
    unsafe {
        let hinst = GetModuleHandleW(null());
        let class = w!("ClipStashSettings");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hIcon: crate::app_icon(),
            lpszClassName: class,
            ..zeroed()
        };
        RegisterClassExW(&wc); // fails harmlessly when already registered
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        // Start on the monitor under the mouse so the DPI below is the right one.
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        let hwnd = CreateWindowExW(0, class, w!("clipstash settings"), style, pt.x, pt.y, 10, 10, null_mut(), null_mut(), hinst, null());
        a.settings_wnd.set(hwnd);
        let dpi = GetDpiForWindow(hwnd);
        let pt_size = a.settings.borrow().font_size.min(12) as i32;
        let fonts = Fonts::new(dpi as i32, pt_size);
        let mut r = rect(0, 0, fonts.s(WIDTH), fonts.s(HEIGHT));
        with(|st| {
            st.fonts = Some(fonts);
            st.focus = 0;
            st.scroll = 0;
            st.keyboard = false;
            st.status = None;
        });
        AdjustWindowRectExForDpi(&mut r, style, 0, 0, dpi);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;
        let h = h.min(wa.bottom - wa.top);
        SetWindowPos(hwnd, null_mut(), wa.left + (wa.right - wa.left - w) / 2, wa.top + (wa.bottom - wa.top - h) / 2, w, h, SWP_NOZORDER);
        gfx::style_frame(hwnd, &a.theme.get(), true);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

pub fn apply_theme() {
    let hwnd = app().settings_wnd.get();
    if !hwnd.is_null() {
        gfx::style_frame(hwnd, &app().theme.get(), true);
        unsafe { InvalidateRect(hwnd, null(), 0) };
    }
}

fn invalidate(hwnd: HWND) {
    unsafe { InvalidateRect(hwnd, null(), 0) };
}

fn set_status(msg: impl Into<String>, error: bool) {
    with(|st| st.status = Some((msg.into(), error)));
}

fn change(f: impl FnOnce(&mut Settings)) {
    let mut s = app().settings.borrow().clone();
    f(&mut s);
    crate::apply_settings(s);
}

fn activate(hwnd: HWND, item: Item, part: Part) {
    match item.ctl() {
        Ctl::Toggle => {
            change(|s| item.flip(s));
            if item == Item::StartWithWindows && app().settings.borrow().start_with_windows != crate::settings::autostart::enabled() {
                set_status("Couldn't update the Windows startup entry", true);
            }
        }
        Ctl::Choice => change(|s| item.step(s, if part == Part::Left { -1 } else { 1 })),
        Ctl::Shortcut => start_capture(),
        Ctl::Button => {
            if part != Part::Button && item == Item::ClearHistory {
                return; // destructive: only the button itself
            }
            run_button(hwnd, item);
        }
    }
    invalidate(hwnd);
}

fn run_button(hwnd: HWND, item: Item) {
    let a = app();
    match item {
        Item::ClearHistory => {
            let r = unsafe {
                MessageBoxW(hwnd, w!("Delete all clipboard history except pinned items?"), w!("clipstash"), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2)
            };
            if r == IDYES {
                let _ = a.store.clear_unpinned();
                set_status("History cleared", false);
            }
        }
        Item::OpenFolder => {
            let dir: Vec<u16> = a.dir.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect();
            unsafe { ShellExecuteW(hwnd, w!("open"), dir.as_ptr(), null(), null(), SW_SHOWNORMAL) };
        }
        _ => {}
    }
}

fn start_capture() {
    crate::unregister_hotkey();
    with(|st| st.capturing = true);
    set_status("Press the new shortcut, or Esc to cancel", false);
}

fn end_capture() {
    if with(|st| std::mem::replace(&mut st.capturing, false)) {
        crate::register_hotkey(app().settings.borrow().hotkey());
    }
}

/// A key pressed while recording a shortcut.
fn capture_key(hwnd: HWND, vk: u32) {
    if matches!(vk as u16, VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN | VK_LSHIFT | VK_RSHIFT | VK_LCONTROL | VK_RCONTROL | VK_LMENU | VK_RMENU) {
        return; // wait for the actual key
    }
    if vk as u16 == VK_ESCAPE {
        end_capture();
        with(|st| st.status = None);
        invalidate(hwnd);
        return;
    }
    let down = |k: u16| unsafe { GetKeyState(k as i32) < 0 };
    let mut mods = 0;
    if down(VK_CONTROL) {
        mods |= MOD_CONTROL;
    }
    if down(VK_MENU) {
        mods |= MOD_ALT;
    }
    if down(VK_SHIFT) {
        mods |= MOD_SHIFT;
    }
    if down(VK_LWIN) || down(VK_RWIN) {
        mods |= MOD_WIN;
    }
    if mods == 0 || !hotkey::is_bindable(vk) {
        set_status("Use a key together with Ctrl, Alt, Shift or Win", true);
        invalidate(hwnd);
        return;
    }
    let hk = Hotkey { mods, vk };
    with(|st| st.capturing = false);
    if crate::register_hotkey(hk) {
        change(|s| s.hotkey = hk.display());
        set_status(format!("Shortcut set to {}", hk.display()), false);
    } else {
        crate::register_hotkey(app().settings.borrow().hotkey());
        set_status(format!("{} is already used by Windows or another app", hk.display()), true);
    }
    invalidate(hwnd);
}

fn move_focus(hwnd: HWND, delta: i32) {
    let n = items().len() as i32;
    let client = client_rect(hwnd);
    with(|st| {
        st.keyboard = true;
        st.focus = (st.focus as i32 + delta).rem_euclid(n) as usize;
        // Keep the focused card in view.
        let Some(f) = st.fonts.as_ref() else { return };
        let (lines, total) = lines(f);
        let view = client.bottom - content_top(f);
        for (line, y, h) in lines {
            if let Line::Row(i, _) = line {
                if i == st.focus {
                    // Include the section header above the first row of a section.
                    let top = y - if i == 0 { f.s(SECTION_H) } else { 0 };
                    if top < st.scroll {
                        st.scroll = top.max(0);
                    } else if y + h > st.scroll + view {
                        st.scroll = y + h - view;
                    }
                }
            }
        }
        st.scroll = st.scroll.clamp(0, (total - view).max(0));
    });
    invalidate(hwnd);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp as u32;
            if with(|st| st.capturing) {
                capture_key(hwnd, vk);
                return 0;
            }
            let item_at = |i: usize| items()[i];
            let focus = with(|st| st.focus);
            match vk as u16 {
                VK_ESCAPE => {
                    DestroyWindow(hwnd);
                }
                VK_UP => move_focus(hwnd, -1),
                VK_DOWN => move_focus(hwnd, 1),
                VK_TAB => move_focus(hwnd, if GetKeyState(VK_SHIFT as i32) < 0 { -1 } else { 1 }),
                VK_LEFT | VK_RIGHT => {
                    with(|st| st.keyboard = true);
                    let item = item_at(focus);
                    if item.ctl() == Ctl::Choice {
                        change(|s| item.step(s, if vk as u16 == VK_LEFT { -1 } else { 1 }));
                    }
                    invalidate(hwnd);
                }
                VK_SPACE | VK_RETURN => {
                    with(|st| st.keyboard = true);
                    activate(hwnd, item_at(focus), Part::Button);
                }
                _ => {
                    if msg == WM_SYSKEYDOWN {
                        return DefWindowProcW(hwnd, msg, wp, lp);
                    }
                }
            }
            0
        }
        // Swallow WM_SYSCHAR while recording so Alt+key doesn't beep.
        WM_SYSCHAR if with(|st| st.capturing) => 0,
        WM_MOUSEMOVE => {
            let (x, y) = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
            let hover = hit_test(hwnd, x, y).map(|(i, ..)| i);
            let (changed, track) = with(|st| {
                let changed = st.hover != hover;
                st.hover = hover;
                (changed, !std::mem::replace(&mut st.tracking_mouse, true))
            });
            if track {
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                TrackMouseEvent(&mut tme);
            }
            if changed {
                invalidate(hwnd);
            }
            0
        }
        WM_MOUSELEAVE => {
            with(|st| {
                st.hover = None;
                st.tracking_mouse = false;
            });
            invalidate(hwnd);
            0
        }
        WM_LBUTTONDOWN => {
            let (x, y) = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
            if with(|st| st.capturing) {
                end_capture();
                with(|st| st.status = None);
            }
            if let Some((idx, item, part)) = hit_test(hwnd, x, y) {
                with(|st| {
                    st.focus = idx;
                    st.keyboard = false;
                });
                activate(hwnd, item, part);
            }
            invalidate(hwnd);
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) & 0xffff) as i16 as i32;
            let client = client_rect(hwnd);
            with(|st| {
                let Some(f) = st.fonts.as_ref() else { return };
                let (_, total) = lines(f);
                let view = client.bottom - content_top(f);
                st.scroll = (st.scroll - delta * f.s(ROW_H) / 120).clamp(0, (total - view).max(0));
            });
            invalidate(hwnd);
            0
        }
        WM_DPICHANGED => {
            let pt = app().settings.borrow().font_size.min(12) as i32;
            with(|st| st.fonts = Some(Fonts::new(((wp >> 16) & 0xffff) as i32, pt)));
            let r = &*(lp as *const RECT);
            SetWindowPos(hwnd, null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            0
        }
        WM_DESTROY => {
            end_capture();
            with(|st| st.fonts = None);
            app().settings_wnd.set(null_mut());
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

const SINGLE: u32 = DT_SINGLELINE | DT_VCENTER;

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps: PAINTSTRUCT = zeroed();
        let dc = BeginPaint(hwnd, &mut ps);
        let client = client_rect(hwnd);
        let t = app().theme.get();
        let settings = app().settings.borrow().clone();
        with(|st| {
            let Some(f) = st.fonts.as_ref() else { return };
            let c = Canvas::new(dc, client.right, client.bottom);
            c.fill(client, t.bg);
            paint_rows(&c, f, &t, &client, st, &settings);
            paint_header(&c, f, &t, &client, st);
            c.present();
        });
        EndPaint(hwnd, &ps);
    }
}

fn paint_header(c: &Canvas, f: &Fonts, t: &Theme, client: &RECT, st: &State) {
    let s = |v| f.s(v);
    c.fill(rect(0, 0, client.right, content_top(f)), t.bg);
    c.text("Settings", rect(s(24), s(14), client.right - s(24), s(14) + s(44)), f.title, t.text, SINGLE);
    let (msg, color) = match &st.status {
        Some((m, true)) => (m.as_str(), t.danger),
        Some((m, false)) => (m.as_str(), t.accent),
        None => ("Changes are saved automatically", t.subtext),
    };
    c.text(msg, rect(s(24), s(58), client.right - s(24), s(58) + f.small_h + s(4)), f.small, color, DT_SINGLELINE | DT_END_ELLIPSIS);
}

fn paint_rows(c: &Canvas, f: &Fonts, t: &Theme, client: &RECT, st: &State, settings: &Settings) {
    let s = |v| f.s(v);
    let top = content_top(f) - st.scroll;
    let (lines, _) = lines(f);
    for (line, y, h) in lines {
        let y = top + y;
        if y + h < content_top(f) || y > client.bottom {
            continue;
        }
        match line {
            Line::Section(title) => {
                c.text(title, rect(s(28), y + h - f.ui_h - s(8), client.right, y + h - s(8)), f.ui_bold, t.text, SINGLE);
            }
            Line::Row(idx, item) => {
                let card = card_rect(f, client, y, h);
                let hovered = st.hover == Some(idx);
                c.round(card, s(6), if hovered { t.surface.mix(t.bg, 0.7) } else { t.surface.mix(t.bg, 0.45) });
                c.round_stroke(card, s(6), t.border.mix(t.bg, 0.6), 1.0);

                let ctl_rect = control_rect(f, &card, item.ctl());
                let text_right = ctl_rect.left - s(16);
                let block = f.ui_h + s(2) + f.small_h;
                let ty = (card.top + card.bottom - block) / 2;
                c.text(item.label(), rect(card.left + s(18), ty, text_right, ty + f.ui_h), f.ui, t.text, DT_SINGLELINE | DT_END_ELLIPSIS);
                c.text(item.desc(), rect(card.left + s(18), ty + f.ui_h + s(2), text_right, ty + block), f.small, t.subtext, DT_SINGLELINE | DT_END_ELLIPSIS);

                let focused = st.keyboard && st.focus == idx;
                paint_control(c, f, t, item, ctl_rect, settings, st.capturing && item == Item::Hotkey, focused);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_control(c: &Canvas, f: &Fonts, t: &Theme, item: Item, r: RECT, settings: &Settings, capturing: bool, focused: bool) {
    let s = |v| f.s(v);
    match item.ctl() {
        Ctl::Toggle => {
            let on = item.toggle_value(settings);
            let h = r.bottom - r.top;
            if on {
                c.round(r, h / 2, t.accent);
            } else {
                c.round(r, h / 2, t.surface);
                c.round_stroke(r, h / 2, t.subtext, 1.0);
            }
            let knob = h - s(10);
            let cx = if on { r.right - h / 2 } else { r.left + h / 2 };
            c.circle(cx, (r.top + r.bottom) / 2, knob, if on { t.on_accent } else { t.subtext });
            let label_r = rect(r.left - s(48), r.top, r.left - s(10), r.bottom);
            c.text(if on { "On" } else { "Off" }, label_r, f.small, t.subtext, SINGLE | DT_RIGHT);
        }
        Ctl::Choice | Ctl::Shortcut => {
            c.round(r, s(5), t.surface);
            c.round_stroke(r, s(5), if capturing { t.accent } else { t.border }, if capturing { 2.0 } else { 1.0 });
            if item.ctl() == Ctl::Choice {
                c.text(icon::CHEVRON_LEFT, rect(r.left + s(4), r.top, r.left + s(28), r.bottom), f.icon_small, t.subtext, SINGLE | DT_CENTER);
                c.text(icon::CHEVRON_RIGHT, rect(r.right - s(28), r.top, r.right - s(4), r.bottom), f.icon_small, t.subtext, SINGLE | DT_CENTER);
                c.text(&item.value_text(settings), rect(r.left + s(28), r.top, r.right - s(28), r.bottom), f.ui, t.text, SINGLE | DT_CENTER | DT_END_ELLIPSIS);
            } else {
                let (text, color) = if capturing { ("Press keys…".to_string(), t.accent) } else { (item.value_text(settings), t.text) };
                c.text(&text, r, f.ui, color, SINGLE | DT_CENTER);
            }
        }
        Ctl::Button => {
            c.round(r, s(5), t.surface);
            c.round_stroke(r, s(5), t.border, 1.0);
            let color = if item == Item::ClearHistory { t.danger } else { t.text };
            c.text(&item.value_text(settings), r, f.ui, color, SINGLE | DT_CENTER);
        }
    }
    if focused {
        let pad = s(4);
        let ring = rect(r.left - pad, r.top - pad, r.right + pad, r.bottom + pad);
        c.round_stroke(ring, s(8), t.text, 2.0);
    }
}
