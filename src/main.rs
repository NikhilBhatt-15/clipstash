//! clipstash: a lightweight Windows clipboard manager.
//!
//! A hidden window receives WM_CLIPBOARDUPDATE (AddClipboardFormatListener)
//! and WM_HOTKEY. Copied text goes into a SQLite history. The hotkey opens a
//! floating search popup that pastes the chosen entry back into the app you
//! came from.

#![windows_subsystem = "windows"]

mod classify;
mod clipboard;
mod focus;
mod gfx;
mod hotkey;
mod image;
mod popup;
mod settings;
mod settings_ui;
mod store;
mod theme;
mod tray;

use settings::Settings;
use std::cell::{Cell, OnceCell, RefCell};
use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use store::Store;
use theme::Theme;
use windows_sys::w;
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, GetClipboardSequenceNumber, RemoveClipboardFormatListener,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const HOTKEY_ID: i32 = 1;
const TIMER_CAPTURE: usize = 1;
/// Ditto also defers the read with a timer (id 7) after the change
/// notification. The source app often still has the clipboard open, or is
/// about to add more formats.
const CAPTURE_DELAY_MS: u32 = 75;
/// Longer side of the stored list thumbnail; covers ~3x DPI scaling.
const THUMB_PX: u32 = 144;
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;

/// UI-thread state. Everything runs on one thread, so plain Cells suffice.
pub struct App {
    pub main: Cell<HWND>,
    pub popup: Cell<HWND>,
    pub edit: Cell<HWND>,
    pub settings_wnd: Cell<HWND>,
    pub store: Store,
    pub settings: RefCell<Settings>,
    pub theme: Cell<Theme>,
    /// %LOCALAPPDATA%\clipstash: database and settings.json.
    pub dir: PathBuf,
    /// Clipboard sequence number of our own last write, so it isn't re-recorded.
    pub ignore_seq: Cell<u32>,
    pub formats: clipboard::Formats,
}

thread_local! {
    static APP: OnceCell<&'static App> = const { OnceCell::new() };
}

pub fn app() -> &'static App {
    APP.with(|a| *a.get().expect("app initialised"))
}

pub fn app_icon() -> HICON {
    static ICON: OnceLock<usize> = OnceLock::new();
    *ICON.get_or_init(|| gfx::make_icon(32) as usize) as HICON
}

pub fn app_icon_small() -> HICON {
    static ICON: OnceLock<usize> = OnceLock::new();
    *ICON.get_or_init(|| gfx::make_icon(unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16)) as usize) as HICON
}

fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("clipstash");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn error_box(text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe { MessageBoxW(null_mut(), wide.as_ptr(), w!("clipstash"), MB_OK | MB_ICONWARNING) };
}

pub fn register_hotkey(hk: hotkey::Hotkey) -> bool {
    unsafe {
        let main = app().main.get();
        UnregisterHotKey(main, HOTKEY_ID);
        RegisterHotKey(main, HOTKEY_ID, hk.mods | MOD_NOREPEAT, hk.vk) != 0
    }
}

pub fn unregister_hotkey() {
    unsafe { UnregisterHotKey(app().main.get(), HOTKEY_ID) };
}

/// Stores and saves new settings and applies whatever changed. Hotkey changes
/// are registered by the settings window before it calls this.
pub fn apply_settings(new: Settings) {
    let a = app();
    let old = a.settings.replace(new.clone());
    new.save(&a.dir);
    if old.theme != new.theme {
        reload_theme();
    }
    if old.max_history != new.max_history {
        let _ = a.store.set_keep(new.max_history as usize);
    }
    if old.max_images != new.max_images {
        let _ = a.store.set_keep_images(new.max_images as usize);
    }
    if old.start_with_windows != new.start_with_windows && !settings::autostart::set(new.start_with_windows) {
        a.settings.borrow_mut().start_with_windows = settings::autostart::enabled();
    }
    if old.font_size != new.font_size || old.show_preview != new.show_preview {
        popup::on_settings_changed();
    }
    tray::update_tip(a.main.get());
}

fn reload_theme() {
    let a = app();
    a.theme.set(Theme::named(&a.settings.borrow().theme));
    popup::apply_theme();
    settings_ui::apply_theme();
}

fn main() {
    unsafe {
        CreateMutexW(null(), 1, w!("Local\\clipstash-single-instance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
    }
    gfx::startup();

    let dir = data_dir();
    let settings = Settings::load(&dir);
    let db = dir.join("history.db");
    let store = match Store::open(&db, settings.max_history as usize) {
        Ok(s) => s,
        Err(e) => {
            error_box(&format!("Could not open {}:\n{e}", db.display()));
            return;
        }
    };
    let _ = store.set_keep_images(settings.max_images as usize);

    let state: &'static App = Box::leak(Box::new(App {
        main: Cell::new(null_mut()),
        popup: Cell::new(null_mut()),
        edit: Cell::new(null_mut()),
        settings_wnd: Cell::new(null_mut()),
        store,
        theme: Cell::new(Theme::named(&settings.theme)),
        settings: RefCell::new(settings),
        dir,
        ignore_seq: Cell::new(0),
        formats: clipboard::Formats::register(),
    }));
    APP.with(|a| a.set(state).ok());

    unsafe {
        let hinst = GetModuleHandleW(null());
        let class = w!("ClipStashMain");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_wndproc),
            hInstance: hinst,
            lpszClassName: class,
            ..zeroed()
        };
        RegisterClassExW(&wc);
        // A real (never shown) top-level window rather than HWND_MESSAGE, so it
        // also receives TaskbarCreated and WM_SETTINGCHANGE broadcasts.
        let main = CreateWindowExW(WS_EX_TOOLWINDOW, class, w!("clipstash"), WS_POPUP, 0, 0, 0, 0, null_mut(), null_mut(), hinst, null());
        state.main.set(main);
        state.popup.set(popup::create());

        if AddClipboardFormatListener(main) == 0 {
            error_box("AddClipboardFormatListener failed; clipboard history will not be recorded.");
        }
        let hk = state.settings.borrow().hotkey();
        if !register_hotkey(hk) {
            error_box(&format!(
                "{} is already used by another program.\nHistory is still recorded; pick another shortcut in Settings.",
                hk.display()
            ));
            settings_ui::open();
        }
        tray::add(main);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if popup::pre_translate(&msg) {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn capture() {
    let a = app();
    if unsafe { GetClipboardSequenceNumber() } == a.ignore_seq.get() {
        return;
    }
    let (respect_privacy, save_images) = {
        let s = a.settings.borrow();
        (s.respect_privacy, s.save_images)
    };
    match clipboard::read(a.main.get(), &a.formats, respect_privacy, save_images) {
        Some(clipboard::Content::Text(text)) => {
            let _ = a.store.add(&text);
        }
        Some(clipboard::Content::Image { img, png }) => {
            // Keep the source PNG when there is one; otherwise encode the bitmap.
            let Some(png) = png.or_else(|| image::encode_png(&img)) else { return };
            let Some(thumb) = image::encode_png(&image::thumbnail(&img, THUMB_PX)) else { return };
            let _ = a.store.add_image(&png, &thumb, img.w, img.h, image::content_hash(&img));
        }
        None => {}
    }
}

/// True for the WM_SETTINGCHANGE that Windows sends when light/dark mode or
/// the accent colour changes.
unsafe fn is_color_change(lp: LPARAM) -> bool {
    if lp == 0 {
        return false;
    }
    let p = lp as *const u16;
    let mut len = 0;
    while len < 64 && *p.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, len)) == "ImmersiveColorSet"
}

unsafe extern "system" fn main_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => {
            // Restarting the timer coalesces bursts of updates into one read.
            SetTimer(hwnd, TIMER_CAPTURE, CAPTURE_DELAY_MS, None);
            0
        }
        WM_TIMER if wp == TIMER_CAPTURE => {
            KillTimer(hwnd, TIMER_CAPTURE);
            capture();
            0
        }
        WM_HOTKEY if wp as i32 == HOTKEY_ID => {
            popup::toggle();
            0
        }
        WM_SETTINGCHANGE if is_color_change(lp) => {
            reload_theme();
            0
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => {
            reload_theme();
            0
        }
        tray::WM_TRAY => {
            tray::on_message(hwnd, lp);
            0
        }
        WM_COMMAND => {
            match wp & 0xffff {
                tray::CMD_SETTINGS => settings_ui::open(),
                tray::CMD_CLEAR => {
                    let r = MessageBoxW(null_mut(), w!("Delete all clipboard history except pinned items?"), w!("clipstash"), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2);
                    if r == IDYES {
                        let _ = app().store.clear_unpinned();
                    }
                }
                tray::CMD_EXIT => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            RemoveClipboardFormatListener(hwnd);
            UnregisterHotKey(hwnd, HOTKEY_ID);
            tray::remove(hwnd);
            PostQuitMessage(0);
            0
        }
        m if m == tray::taskbar_created() => {
            tray::add(hwnd);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
