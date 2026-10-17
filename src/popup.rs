//! The floating search window.
//!
//! Everything except the text box is custom-drawn so it can follow the theme:
//! a search field, the list of clips, a preview pane and a key-hint footer.
//! Keyboard focus always stays in the text box; navigation keys are
//! intercepted in `pre_translate` before they reach it.

use crate::classify::{self, Kind};
use crate::gfx::{self, contains, icon, rect, Canvas, Fonts};
use crate::store::{now_ms, Clip};
use crate::theme::{Rgb, Theme};
use crate::{app, clipboard, focus, settings_ui};
use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use windows_sys::w;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{EM_GETSEL, EM_REPLACESEL, EM_SETCUEBANNER, EM_SETSEL, WM_MOUSELEAVE};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const MAX_RESULTS: usize = 300;
const TITLE_CHARS: usize = 300;
const PREVIEW_MAX_CHARS: usize = 64 * 1024;
const PREVIEW_MAX_LINES: usize = 5000;

/// Logical (96-DPI, 10pt) window size.
const WIDTH_WITH_PREVIEW: i32 = 860;
const WIDTH_LIST_ONLY: i32 = 560;
const HEIGHT: i32 = 520;

struct Entry {
    clip: Clip,
    kind: Kind,
    lines: usize,
    title: Vec<char>,
}

/// A line's characters and the char ranges of search hits in it.
type PreviewLine = (Vec<char>, Vec<(usize, usize)>);

struct Preview {
    id: i64,
    /// Monospace views: lines with tabs expanded, and the search hits in each.
    lines: Vec<PreviewLine>,
    /// Index of the first line containing a search hit.
    first_match: Option<usize>,
    /// Wrapped views: the (possibly truncated) text.
    text: String,
    /// (width, height) of `text` when wrapped, cached for scroll limits.
    wrapped: Option<(i32, i32)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hover {
    None,
    Row(usize),
    Gear,
}

#[derive(Clone, Copy)]
enum Action {
    Paste,
    PasteTrimmed,
    Copy,
}

enum Keep {
    Top,
    Id(i64),
    Index(usize),
}

struct State {
    entries: Vec<Entry>,
    total: usize,
    selected: usize,
    scroll: i32,
    preview_scroll: i32,
    hover: Hover,
    tracking_mouse: bool,
    terms: Vec<Vec<char>>,
    fonts: Option<Fonts>,
    edit_brush: HBRUSH,
    target: Option<focus::Target>,
    preview: Option<Preview>,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State {
        entries: Vec::new(),
        total: 0,
        selected: 0,
        scroll: 0,
        preview_scroll: 0,
        hover: Hover::None,
        tracking_mouse: false,
        terms: Vec::new(),
        fonts: None,
        edit_brush: null_mut(),
        target: None,
        preview: None,
    }) };
}

/// Runs `f` with the popup state. Never call Win32 functions that can send
/// messages back to the popup from inside `f`: the state would be re-borrowed.
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

struct Layout {
    client: RECT,
    search: RECT,
    edit: RECT,
    gear: RECT,
    list: RECT,
    preview: Option<RECT>,
    footer: RECT,
    row_h: i32,
}

impl Layout {
    fn new(f: &Fonts, client: RECT, show_preview: bool) -> Self {
        let s = |v| f.s(v);
        let pad = s(12);
        let search_h = f.ui_h + s(20);
        let gear = rect(client.right - pad - search_h, pad, client.right - pad, pad + search_h);
        let search = rect(pad, pad, gear.left - s(6), pad + search_h);
        let edit_h = f.ui_h + s(2);
        let edit_top = search.top + (search_h - edit_h) / 2;
        let edit = rect(search.left + s(38), edit_top, search.right - s(12), edit_top + edit_h);
        let footer = rect(0, client.bottom - f.small_h - s(18), client.right, client.bottom);
        let top = search.bottom + pad;
        let (list, preview) = if show_preview {
            let lw = (client.right * 9 / 20).max(s(280));
            (rect(0, top, lw, footer.top), Some(rect(lw, top, client.right, footer.top)))
        } else {
            (rect(0, top, client.right, footer.top), None)
        };
        let row_h = f.ui_h.max(f.mono_h) + f.small_h + s(20);
        Layout { client, search, edit, gear, list, preview, footer, row_h }
    }

    fn list_h(&self) -> i32 {
        self.list.bottom - self.list.top
    }

    fn row_rect(&self, f: &Fonts, i: usize, scroll: i32) -> RECT {
        let y = self.list.top + i as i32 * self.row_h - scroll;
        rect(self.list.left + f.s(8), y + f.s(2), self.list.right - f.s(12), y + self.row_h - f.s(2))
    }
}

fn client_rect(hwnd: HWND) -> RECT {
    let mut rc: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(hwnd, &mut rc) };
    rc
}

fn current_layout(st: &State) -> Option<Layout> {
    let f = st.fonts.as_ref()?;
    let show_preview = app().settings.borrow().show_preview;
    Some(Layout::new(f, client_rect(app().popup.get()), show_preview))
}

pub fn create() -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(null());
        let class = w!("ClipStashPopup");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            lpszClassName: class,
            ..zeroed()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("clipstash"),
            WS_POPUP | WS_CLIPCHILDREN,
            0,
            0,
            100,
            100,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        let edit = CreateWindowExW(
            0,
            w!("EDIT"),
            null(),
            WS_CHILD | WS_VISIBLE | ES_AUTOHSCROLL as u32,
            0,
            0,
            0,
            0,
            hwnd,
            null_mut(),
            hinst,
            null(),
        );
        SendMessageW(edit, EM_SETCUEBANNER, 1, w!("Search clipboard history") as LPARAM);
        app().edit.set(edit);
        hwnd
    }
}

/// Hotkey handler: show the popup, or hide it if it is already up.
pub fn toggle() {
    if unsafe { IsWindowVisible(app().popup.get()) } != 0 {
        cancel();
    } else {
        show();
    }
}

fn ensure_fonts(dpi: i32) {
    let pt = app().settings.borrow().font_size as i32;
    let stale = with(|st| st.fonts.as_ref().is_none_or(|f| f.dpi != dpi || f.pt != pt));
    if stale {
        let fonts = Fonts::new(dpi, pt);
        let ui = fonts.ui;
        with(|st| st.fonts = Some(fonts));
        unsafe { SendMessageW(app().edit.get(), WM_SETFONT, ui as WPARAM, 0) };
    }
}

fn window_size() -> (i32, i32) {
    let show_preview = app().settings.borrow().show_preview;
    with(|st| {
        let f = st.fonts.as_ref().expect("fonts");
        (f.s(if show_preview { WIDTH_WITH_PREVIEW } else { WIDTH_LIST_ONLY }), f.s(HEIGHT))
    })
}

fn show() {
    let a = app();
    let (popup, edit) = (a.popup.get(), a.edit.get());
    let target = focus::Target::capture();
    let position = a.settings.borrow().position.clone();

    let mut cursor = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut cursor) };
    let anchor = match position.as_str() {
        "mouse" => Some(cursor),
        "center" => None,
        _ => target.caret.or(Some(cursor)),
    };
    // Monitor of the anchor, or of the window being typed in when centring.
    let monitor = unsafe {
        match anchor {
            Some(p) => MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST),
            None => MonitorFromWindow(target.window, MONITOR_DEFAULTTONEAREST),
        }
    };
    let work = monitor_work_area(monitor);

    // Move onto the right monitor first so GetDpiForWindow reports its DPI.
    unsafe { SetWindowPos(popup, null_mut(), work.left, work.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) };
    ensure_fonts(unsafe { GetDpiForWindow(popup) } as i32);
    let (w, h) = window_size();
    let (x, y) = match anchor {
        Some(p) => place_near(p, w, h, work),
        None => (work.left + (work.right - work.left - w) / 2, work.top + (work.bottom - work.top - h) / 3),
    };
    with(|st| {
        st.target = Some(target);
        st.scroll = 0;
        st.selected = 0;
        st.preview_scroll = 0;
        st.hover = Hover::None;
    });
    unsafe {
        SetWindowPos(popup, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
        apply_theme();
        layout_children();
        SetWindowTextW(edit, w!(""));
    }
    refresh(Keep::Top);
    unsafe {
        ShowWindow(popup, SW_SHOW);
        // Allowed: a process that just received a hotkey may take the foreground.
        SetForegroundWindow(popup);
        SetFocus(edit);
    }
}

fn hide() {
    unsafe { ShowWindow(app().popup.get(), SW_HIDE) };
    with(|st| {
        st.preview = None;
        st.entries.clear();
    });
}

/// Esc / second hotkey press: close and hand focus back without pasting.
fn cancel() {
    if let Some(t) = with(|st| st.target.take()) {
        t.restore();
    }
    hide();
}

fn choose(index: usize, action: Action) {
    let a = app();
    let Some((id, text)) = with(|st| st.entries.get(index).map(|e| (e.clip.id, e.clip.text.clone()))) else { return };
    let settings = a.settings.borrow().clone();
    let mut out = match action {
        Action::PasteTrimmed => classify::dedent(&text),
        _ => text,
    };
    if settings.normalize_line_endings {
        out = classify::to_crlf(&out);
    }
    if !clipboard::write_text(a.main.get(), &out) {
        return;
    }
    // Our own write triggers WM_CLIPBOARDUPDATE; recognise it by sequence number
    // and bump the existing row instead of inserting.
    a.ignore_seq.set(unsafe { GetClipboardSequenceNumber() });
    let _ = a.store.touch(id);

    let restored = with(|st| st.target.take()).map(|t| t.restore()).unwrap_or(false);
    hide();
    if restored && settings.paste_on_enter && !matches!(action, Action::Copy) {
        focus::send_paste();
    }
}

fn refresh(keep: Keep) {
    let a = app();
    let query = window_text(a.edit.get());
    let terms: Vec<Vec<char>> = query.split_whitespace().map(|t| t.chars().collect()).collect();
    let clips = a.store.search(&query, MAX_RESULTS).unwrap_or_default();
    let total = a.store.count().unwrap_or(clips.len());
    let entries: Vec<Entry> = clips
        .into_iter()
        .map(|clip| Entry {
            kind: classify::classify(&clip.text),
            lines: classify::line_count(&clip.text),
            // While searching, show the line that matched rather than the first line.
            title: classify::title_matching(&clip.text, &terms, TITLE_CHARS).chars().collect(),
            clip,
        })
        .collect();
    with(|st| {
        let selected = match keep {
            Keep::Top => 0,
            Keep::Id(id) => entries.iter().position(|e| e.clip.id == id).unwrap_or(0),
            Keep::Index(i) => i.min(entries.len().saturating_sub(1)),
        };
        st.terms = terms;
        st.entries = entries;
        st.preview = None; // the highlights depend on the query
        st.total = total;
        if matches!(keep, Keep::Top) {
            st.scroll = 0;
        }
        select(st, selected);
    });
    invalidate();
}

/// Changes the selection, keeps it on screen and rebuilds the preview.
fn select(st: &mut State, i: usize) {
    st.selected = i.min(st.entries.len().saturating_sub(1));
    if let Some(l) = current_layout(st) {
        let top = st.selected as i32 * l.row_h;
        if top < st.scroll {
            st.scroll = top;
        } else if top + l.row_h > st.scroll + l.list_h() {
            st.scroll = top + l.row_h - l.list_h();
        }
        clamp_scroll(st, &l);
    }
    let id = st.entries.get(st.selected).map(|e| e.clip.id);
    if st.preview.as_ref().map(|p| p.id) != id {
        let preview = st.entries.get(st.selected).map(|e| build_preview(e, &st.terms));
        // Start the preview a couple of lines above the first search hit.
        st.preview_scroll = match (preview.as_ref().and_then(|p| p.first_match), st.fonts.as_ref()) {
            (Some(line), Some(f)) => (line as i32 - 2).max(0) * (f.mono_h + f.s(1)),
            _ => 0,
        };
        st.preview = preview;
    }
}

fn clamp_scroll(st: &mut State, l: &Layout) {
    let max = (st.entries.len() as i32 * l.row_h - l.list_h()).max(0);
    st.scroll = st.scroll.clamp(0, max);
}

fn build_preview(e: &Entry, terms: &[Vec<char>]) -> Preview {
    let mut text: String = e.clip.text.chars().take(PREVIEW_MAX_CHARS).collect();
    if text.len() < e.clip.text.len() {
        text.push_str("\n\n… (preview truncated)");
    }
    let mut lines = Vec::new();
    let mut first_match = None;
    if e.kind.is_mono() {
        for (i, l) in e.clip.text.lines().take(PREVIEW_MAX_LINES).enumerate() {
            let chars: Vec<char> = l.replace('\t', "    ").chars().collect();
            let hits = classify::match_ranges(&chars, terms);
            if first_match.is_none() && !hits.is_empty() {
                first_match = Some(i);
            }
            lines.push((chars, hits));
        }
    }
    Preview { id: e.clip.id, lines, first_match, text, wrapped: None }
}

fn invalidate() {
    unsafe { InvalidateRect(app().popup.get(), null(), 0) };
}

fn window_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// Re-reads colours from the current theme (also on Windows theme changes).
pub fn apply_theme() {
    let a = app();
    let t = a.theme.get();
    let brush = unsafe { CreateSolidBrush(t.surface.colorref()) };
    let old = with(|st| std::mem::replace(&mut st.edit_brush, brush));
    unsafe {
        if !old.is_null() {
            DeleteObject(old);
        }
        gfx::style_frame(a.popup.get(), &t, false);
        InvalidateRect(a.edit.get(), null(), 1);
    }
    invalidate();
}

/// Font size or preview setting changed.
pub fn on_settings_changed() {
    let popup = app().popup.get();
    if unsafe { IsWindowVisible(popup) } == 0 {
        return; // picked up on next show
    }
    ensure_fonts(unsafe { GetDpiForWindow(popup) } as i32);
    let (w, h) = window_size();
    unsafe { SetWindowPos(popup, null_mut(), 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE) };
    layout_children();
    with(|st| select(st, st.selected));
    invalidate();
}

fn layout_children() {
    let edit_rect = with(|st| current_layout(st).map(|l| l.edit));
    if let Some(r) = edit_rect {
        unsafe { MoveWindow(app().edit.get(), r.left, r.top, r.right - r.left, r.bottom - r.top, 1) };
    }
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk as i32) < 0 }
}

/// Keyboard handling for the search box, called from the message loop before
/// TranslateMessage. Returns true when the message was consumed.
pub fn pre_translate(msg: &MSG) -> bool {
    let a = app();
    if msg.hwnd != a.edit.get() || msg.message != WM_KEYDOWN {
        return false;
    }
    let vk = msg.wParam as u16;
    let ctrl = key_down(VK_CONTROL);
    let shift = key_down(VK_SHIFT);
    let selected = with(|st| st.selected);
    match vk {
        VK_RETURN => {
            let action = if ctrl {
                Action::Copy
            } else if shift {
                Action::PasteTrimmed
            } else {
                Action::Paste
            };
            choose(selected, action);
        }
        VK_ESCAPE => cancel(),
        VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT => {
            with(|st| {
                let page = current_layout(st).map_or(5, |l| (l.list_h() / l.row_h).max(1)) as isize;
                let delta = match vk {
                    VK_UP => -1,
                    VK_DOWN => 1,
                    VK_PRIOR => -page,
                    _ => page,
                };
                let next = (st.selected as isize + delta).max(0) as usize;
                select(st, next);
            });
            invalidate();
        }
        VK_HOME | VK_END if ctrl => {
            with(|st| select(st, if vk == VK_HOME { 0 } else { usize::MAX }));
            invalidate();
        }
        0x50 /* P */ if ctrl => {
            if let Some((id, pinned)) = with(|st| st.entries.get(st.selected).map(|e| (e.clip.id, e.clip.pinned))) {
                let _ = a.store.set_pinned(id, !pinned);
                refresh(Keep::Id(id));
            }
        }
        VK_DELETE if shift => {
            if let Some(id) = with(|st| st.entries.get(st.selected).map(|e| e.clip.id)) {
                let _ = a.store.delete(id);
                refresh(Keep::Index(selected));
            }
        }
        0x31..=0x39 if ctrl => choose((vk - 0x31) as usize, Action::Paste),
        VK_OEM_COMMA if ctrl => settings_ui::open(),
        // The stock edit control inserts a box character for Ctrl+Backspace.
        VK_BACK if ctrl => delete_word_before_caret(a.edit.get()),
        _ => return false,
    }
    true
}

fn delete_word_before_caret(edit: HWND) {
    let text: Vec<char> = window_text(edit).chars().collect();
    let mut start = 0u32;
    let mut end = 0u32;
    unsafe { SendMessageW(edit, EM_GETSEL, &mut start as *mut u32 as WPARAM, &mut end as *mut u32 as LPARAM) };
    if start != end {
        unsafe { SendMessageW(edit, EM_REPLACESEL, 1, w!("") as LPARAM) };
        return;
    }
    // EM_GETSEL positions are UTF-16 units; map back to chars.
    let mut units = 0usize;
    let mut caret = 0usize;
    while caret < text.len() && units < end as usize {
        units += text[caret].len_utf16();
        caret += 1;
    }
    let mut i = caret;
    while i > 0 && text[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !text[i - 1].is_whitespace() {
        i -= 1;
    }
    let from: usize = text[..i].iter().map(|c| c.len_utf16()).sum();
    unsafe {
        SendMessageW(edit, EM_SETSEL, from, end as LPARAM);
        SendMessageW(edit, EM_REPLACESEL, 1, w!("") as LPARAM);
    }
}

fn hit_test(x: i32, y: i32) -> Hover {
    with(|st| {
        let Some(l) = current_layout(st) else { return Hover::None };
        if contains(&l.gear, x, y) {
            return Hover::Gear;
        }
        if contains(&l.list, x, y) {
            let i = ((y - l.list.top + st.scroll) / l.row_h) as usize;
            if i < st.entries.len() {
                return Hover::Row(i);
            }
        }
        Hover::None
    })
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_SIZE => {
            layout_children();
            0
        }
        WM_CTLCOLOREDIT => {
            let t = app().theme.get();
            let dc = wp as HDC;
            SetTextColor(dc, t.text.colorref());
            SetBkColor(dc, t.surface.colorref());
            with(|st| st.edit_brush) as LRESULT
        }
        WM_COMMAND => {
            if lp as HWND == app().edit.get() && ((wp >> 16) & 0xffff) as u32 == EN_CHANGE {
                refresh(Keep::Top);
            }
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
            let hover = hit_test(x, y);
            let (changed, track) = with(|st| {
                let changed = st.hover != hover;
                st.hover = hover;
                let track = !st.tracking_mouse;
                st.tracking_mouse = true;
                (changed, track)
            });
            if track {
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                TrackMouseEvent(&mut tme);
            }
            if changed {
                invalidate();
            }
            0
        }
        WM_MOUSELEAVE => {
            with(|st| {
                st.hover = Hover::None;
                st.tracking_mouse = false;
            });
            invalidate();
            0
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (x, y) = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
            match hit_test(x, y) {
                Hover::Row(i) if msg == WM_LBUTTONDBLCLK => choose(i, Action::Paste),
                Hover::Row(i) => {
                    with(|st| select(st, i));
                    invalidate();
                    SetFocus(app().edit.get());
                }
                Hover::Gear => settings_ui::open(),
                Hover::None => {
                    SetFocus(app().edit.get());
                }
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) & 0xffff) as i16 as i32;
            let mut pt = POINT { x: (lp & 0xffff) as i16 as i32, y: ((lp >> 16) & 0xffff) as i16 as i32 };
            ScreenToClient(hwnd, &mut pt);
            with(|st| {
                let Some(l) = current_layout(st) else { return };
                let f = st.fonts.as_ref().unwrap();
                if l.preview.is_some_and(|p| contains(&p, pt.x, pt.y)) {
                    st.preview_scroll -= delta * 3 * f.mono_h / 120;
                    st.preview_scroll = st.preview_scroll.max(0); // upper bound applied in paint
                } else {
                    st.scroll -= delta * l.row_h / 120;
                    clamp_scroll(st, &l);
                }
            });
            invalidate();
            0
        }
        // Clicking anywhere else dismisses the popup, leaving focus where the user clicked.
        WM_ACTIVATE if (wp & 0xffff) as u32 == WA_INACTIVE => {
            with(|st| st.target = None);
            hide();
            0
        }
        WM_DPICHANGED => {
            ensure_fonts(((wp >> 16) & 0xffff) as i32);
            let r = &*(lp as *const RECT);
            SetWindowPos(hwnd, null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            layout_children();
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ---------------------------------------------------------------- painting

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps: PAINTSTRUCT = zeroed();
        let dc = BeginPaint(hwnd, &mut ps);
        let t = app().theme.get();
        let paste_on_enter = app().settings.borrow().paste_on_enter;
        with(|st| {
            let Some(l) = current_layout(st) else { return };
            let c = Canvas::new(dc, l.client.right, l.client.bottom);
            c.fill(l.client, t.bg);
            paint_list(&c, &t, &l, st);
            if let Some(p) = l.preview {
                paint_preview(&c, &t, p, st);
            }
            paint_header(&c, &t, &l, st);
            paint_footer(&c, &t, &l, st, paste_on_enter);
            c.present();
        });
        EndPaint(hwnd, &ps);
    }
}

fn kind_icon(k: Kind) -> &'static str {
    match k {
        Kind::Code => icon::CODE,
        Kind::Link => icon::LINK,
        Kind::Path => icon::FOLDER,
        _ => icon::DOCUMENT,
    }
}

fn meta_line(e: &Entry, now: i64) -> String {
    let size = if e.lines > 1 {
        format!("{} lines", e.lines)
    } else {
        let n = e.clip.text.chars().count();
        format!("{n} char{}", if n == 1 { "" } else { "s" })
    };
    format!("{} · {} · {}", e.kind.label(), size, classify::relative_time(e.clip.used_ms, now))
}

const SINGLE: u32 = DT_SINGLELINE | DT_VCENTER;

fn paint_header(c: &Canvas, t: &Theme, l: &Layout, st: &State) {
    let f = st.fonts.as_ref().unwrap();
    let s = |v| f.s(v);
    c.fill(rect(0, 0, l.client.right, l.list.top), t.bg);
    let radius = s(8);
    c.round(l.search, radius, t.surface);
    c.round_stroke(l.search, radius, t.border, 1.0);
    // Focus underline, as in Windows 11 text boxes.
    c.round(rect(l.search.left + radius / 2, l.search.bottom - s(2), l.search.right - radius / 2, l.search.bottom), s(1), t.accent);
    c.text(icon::SEARCH, rect(l.search.left + s(4), l.search.top, l.search.left + s(36), l.search.bottom), f.icon, t.subtext, SINGLE | DT_CENTER);

    let gear_hover = st.hover == Hover::Gear;
    if gear_hover {
        c.round(l.gear, radius, t.surface);
    }
    c.text(icon::SETTINGS, l.gear, f.icon, if gear_hover { t.text } else { t.subtext }, SINGLE | DT_CENTER);
    c.fill(rect(0, l.list.top - 1, l.client.right, l.list.top), t.border);
}

fn paint_list(c: &Canvas, t: &Theme, l: &Layout, st: &State) {
    let f = st.fonts.as_ref().unwrap();
    let s = |v| f.s(v);
    if st.entries.is_empty() {
        let (glyph_rect, msg_rect) = {
            let cy = l.list.top + l.list_h() / 2;
            (rect(l.list.left, cy - s(44), l.list.right, cy - s(4)), rect(l.list.left + s(16), cy + s(4), l.list.right - s(16), cy + s(60)))
        };
        let msg = if st.terms.is_empty() {
            "No clipboard history yet.\nCopy some text and it will show up here."
        } else {
            "No matches."
        };
        c.text(icon::PASTE, glyph_rect, f.title, t.subtext, SINGLE | DT_CENTER);
        c.text(msg, msg_rect, f.ui, t.subtext, DT_CENTER | DT_WORDBREAK);
        return;
    }

    let now = now_ms();
    let first = (st.scroll / l.row_h) as usize;
    let count = (l.list_h() / l.row_h + 2) as usize;
    for i in first..(first + count).min(st.entries.len()) {
        let e = &st.entries[i];
        let row = l.row_rect(f, i, st.scroll);
        let selected = i == st.selected;
        let hovered = st.hover == Hover::Row(i);
        let row_bg = if selected {
            t.selection
        } else if hovered {
            t.surface
        } else {
            t.bg
        };
        if selected || hovered {
            c.round(row, s(6), row_bg);
        }
        if selected {
            c.round(rect(row.left, row.top + s(12), row.left + s(3), row.bottom - s(12)), s(2), t.accent);
        }

        let icon_rect = rect(row.left + s(12), row.top, row.left + s(34), row.bottom);
        let (cx, cy) = ((icon_rect.left + icon_rect.right) / 2, (row.top + row.bottom) / 2);
        match e.kind {
            Kind::Color(r, g, b) => {
                c.circle(cx, cy, s(18), t.border);
                c.circle(cx, cy, s(16), Rgb(r, g, b));
            }
            k => c.text(kind_icon(k), icon_rect, f.icon, if selected { t.accent } else { t.subtext }, SINGLE | DT_CENTER),
        }

        let mut right = row.right - s(10);
        if i < 9 {
            c.text(&(i + 1).to_string(), rect(right - s(16), row.top, right, row.bottom), f.small, t.subtext, SINGLE | DT_RIGHT);
            right -= s(22);
        }
        if e.clip.pinned {
            c.text(icon::PIN, rect(right - s(16), row.top, right, row.bottom), f.icon_small, t.accent, SINGLE | DT_CENTER);
            right -= s(22);
        }

        let tx = icon_rect.right + s(10);
        let (font, line_h) = if e.kind.is_mono() { (f.mono, f.mono_h) } else { (f.ui, f.ui_h) };
        let block = line_h + s(2) + f.small_h;
        let ty = row.top + (row.bottom - row.top - block) / 2;
        let ranges = classify::match_ranges(&e.title, &st.terms);
        c.highlighted_line(&e.title, &ranges, rect(tx, ty, right, ty + line_h), font, t.text, t.accent.mix(row_bg, 0.35), line_h);
        c.text(&meta_line(e, now), rect(tx, ty + line_h + s(2), right, ty + block), f.small, t.subtext, DT_SINGLELINE | DT_END_ELLIPSIS);
    }

    // Overlay scrollbar.
    let content = st.entries.len() as i32 * l.row_h;
    if content > l.list_h() {
        let track = l.list_h() - s(8);
        let thumb = (track * l.list_h() / content).max(s(28));
        let y = l.list.top + s(4) + (track - thumb) * st.scroll / (content - l.list_h());
        let x = l.list.right - s(6);
        c.round(rect(x, y, x + s(3), y + thumb), s(2), t.subtext.mix(t.bg, 0.5));
    }
}

fn paint_preview(c: &Canvas, t: &Theme, r: RECT, st: &mut State) {
    let f = st.fonts.as_ref().unwrap();
    let s = |v| f.s(v);
    c.fill(r, t.inset);
    let Some(e) = st.entries.get(st.selected) else { return };
    let Some(p) = st.preview.as_mut() else { return };

    let pad = s(16);
    let header = rect(r.left + pad, r.top + s(12), r.right - pad, r.top + s(12) + f.small_h);
    let body = rect(r.left + pad, header.bottom + s(12), r.right - pad, r.bottom - s(8));
    let body_h = body.bottom - body.top;

    let mut scroll = st.preview_scroll;
    match e.kind {
        Kind::Color(rr, gg, bb) => {
            let sw = rect(body.left, body.top, body.left + s(96), body.top + s(96));
            c.round(sw, s(12), Rgb(rr, gg, bb));
            c.round_stroke(sw, s(12), t.border, 1.0);
            let info = format!("{}\n\nrgb({rr}, {gg}, {bb})", e.clip.text.trim());
            c.text(&info, rect(sw.right + s(16), body.top + s(8), body.right, body.bottom), f.mono, t.text, DT_WORDBREAK);
            scroll = 0;
        }
        k if k.is_mono() => {
            let line_h = f.mono_h + s(1);
            let max = (p.lines.len() as i32 * line_h - body_h).max(0);
            scroll = scroll.min(max);
            let digits = p.lines.len().max(1).to_string().len();
            let gutter = c.extent(&gfx::wide(&"0".repeat(digits)), f.mono) + s(16);
            let text_clip = rect(body.left + gutter, body.top, body.right, body.bottom);
            let first = (scroll / line_h) as usize;
            for (i, (chars, hits)) in p.lines.iter().enumerate().skip(first) {
                let y = body.top + i as i32 * line_h - scroll;
                if y > body.bottom {
                    break;
                }
                let hit_line = !hits.is_empty();
                let num_color = if hit_line { t.accent } else { t.subtext.mix(t.inset, 0.6) };
                let num_rect = rect(body.left, y, body.left + gutter - s(12), (y + line_h).min(body.bottom));
                c.text(&(i + 1).to_string(), num_rect, f.mono, num_color, DT_SINGLELINE | DT_RIGHT);
                let line: String = chars.iter().collect();
                let w16 = gfx::wide(&line);
                for &(hs, he) in hits {
                    let x0 = text_clip.left + c.extent(&gfx::wide(&chars[..hs].iter().collect::<String>()), f.mono);
                    let x1 = text_clip.left + c.extent(&gfx::wide(&chars[..he].iter().collect::<String>()), f.mono);
                    if x0 < text_clip.right && y + line_h > body.top {
                        c.round(rect(x0 - 1, y, x1.min(text_clip.right) + 1, y + f.mono_h), s(3), t.accent.mix(t.inset, 0.35));
                    }
                }
                c.text_at(&w16, text_clip.left, y, &text_clip, f.mono, t.text);
            }
        }
        k => {
            let flags = DT_WORDBREAK | DT_EDITCONTROL | DT_EXPANDTABS;
            let width = body.right - body.left;
            let h = match p.wrapped {
                Some((w, h)) if w == width => h,
                _ => {
                    let h = c.measure_wrapped(&p.text, width, f.ui, flags);
                    p.wrapped = Some((width, h));
                    h
                }
            };
            scroll = scroll.min((h - body_h).max(0));
            let color = if k == Kind::Link { t.accent } else { t.text };
            c.text(&p.text, rect(body.left, body.top - scroll, body.right, body.top - scroll + h), f.ui, color, flags);
        }
    }
    st.preview_scroll = scroll;

    // Cover anything scrolled past the body, then draw the header on top.
    let f = st.fonts.as_ref().unwrap();
    c.fill(rect(r.left, r.top, r.right, body.top), t.inset);
    c.fill(rect(r.left, body.bottom, r.right, r.bottom), t.inset);
    c.fill(rect(r.left, r.top, r.left + 1, r.bottom), t.border);
    let e = &st.entries[st.selected];
    let chars = e.clip.text.chars().count();
    let mut info = format!(
        "{} · {} line{} · {} chars · {}",
        e.kind.label(),
        e.lines,
        if e.lines == 1 { "" } else { "s" },
        chars,
        classify::relative_time(e.clip.used_ms, now_ms())
    );
    if e.clip.pinned {
        info.push_str(" · Pinned");
    }
    c.text(&info, header, f.small, t.subtext, DT_SINGLELINE | DT_END_ELLIPSIS);
}

fn paint_footer(c: &Canvas, t: &Theme, l: &Layout, st: &State, paste_on_enter: bool) {
    let f = st.fonts.as_ref().unwrap();
    let s = |v| f.s(v);
    let r = l.footer;
    c.fill(r, t.bg);
    c.fill(rect(r.left, r.top, r.right, r.top + 1), t.border);

    let count = if st.terms.is_empty() {
        format!("{} items", st.total)
    } else {
        format!("{} of {}", st.entries.len(), st.total)
    };
    let count_w = c.extent(&gfx::wide(&count), f.small);
    c.text(&count, rect(r.right - s(14) - count_w, r.top, r.right - s(14), r.bottom), f.small, t.subtext, SINGLE | DT_RIGHT);

    let (enter, shift_enter) = if paste_on_enter { ("Paste", "Paste trimmed") } else { ("Copy", "Copy trimmed") };
    let hints = [("Enter", enter), ("Shift+Enter", shift_enter), ("Ctrl+Enter", "Copy"), ("Ctrl+P", "Pin"), ("Shift+Del", "Delete")];
    let limit = r.right - s(28) - count_w;
    let mut x = r.left + s(12);
    let pill_h = f.small_h + s(4);
    let py = (r.top + r.bottom - pill_h) / 2 + 1;
    for (key, label) in hints {
        let kw = c.extent(&gfx::wide(key), f.small) + s(10);
        let lw = c.extent(&gfx::wide(label), f.small);
        if x + kw + s(6) + lw > limit {
            break;
        }
        let pill = rect(x, py, x + kw, py + pill_h);
        c.round(pill, s(4), t.surface);
        c.round_stroke(pill, s(4), t.border, 1.0);
        c.text(key, pill, f.small, t.text, SINGLE | DT_CENTER);
        x = pill.right + s(6);
        c.text(label, rect(x, r.top, x + lw, r.bottom), f.small, t.subtext, SINGLE);
        x += lw + s(14);
    }
}

fn monitor_work_area(mon: HMONITOR) -> RECT {
    unsafe {
        let mut mi: MONITORINFO = zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(mon, &mut mi);
        mi.rcWork
    }
}

/// Below the anchor if it fits, otherwise above it; always inside the work area.
fn place_near(anchor: POINT, w: i32, h: i32, wa: RECT) -> (i32, i32) {
    let mut y = anchor.y + 6;
    if y + h > wa.bottom {
        y = anchor.y - h - 28;
    }
    let x = anchor.x.clamp(wa.left, (wa.right - w).max(wa.left));
    (x, y.clamp(wa.top, (wa.bottom - h).max(wa.top)))
}
