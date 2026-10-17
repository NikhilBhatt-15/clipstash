//! Notification-area icon with a small menu: settings, clear history, exit.

use crate::app;
use std::mem::{size_of, zeroed};
use std::ptr::null;
use std::sync::OnceLock;
use windows_sys::w;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const WM_TRAY: u32 = WM_APP + 1;
pub const CMD_SETTINGS: usize = 100;
pub const CMD_CLEAR: usize = 101;
pub const CMD_EXIT: usize = 102;

/// Explorer broadcasts this after it restarts; the icon must be re-added.
pub fn taskbar_created() -> u32 {
    static MSG: OnceLock<u32> = OnceLock::new();
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = unsafe { zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid
}

fn with_tip(mut nid: NOTIFYICONDATAW) -> NOTIFYICONDATAW {
    let tip: Vec<u16> = format!("clipstash — {}", app().settings.borrow().hotkey).encode_utf16().take(127).collect();
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    nid.uFlags |= NIF_TIP;
    nid
}

pub fn add(hwnd: HWND) {
    let mut nid = data(hwnd);
    nid.uFlags = NIF_ICON | NIF_MESSAGE;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = crate::app_icon_small();
    let nid = with_tip(nid);
    unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
}

/// Refreshes the tooltip after the shortcut changes.
pub fn update_tip(hwnd: HWND) {
    let nid = with_tip(data(hwnd));
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

pub fn remove(hwnd: HWND) {
    let nid = data(hwnd);
    unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

pub fn on_message(hwnd: HWND, lp: LPARAM) {
    let event = (lp & 0xffff) as u32;
    match event {
        WM_LBUTTONDBLCLK => crate::settings_ui::open(),
        WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
        _ => {}
    }
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu();
        let hint: Vec<u16> = format!("{} to open", app().settings.borrow().hotkey).encode_utf16().chain(Some(0)).collect();
        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, hint.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, CMD_SETTINGS, w!("Settings…"));
        AppendMenuW(menu, MF_STRING, CMD_CLEAR, w!("Clear history…"));
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("Exit"));
        SetMenuDefaultItem(menu, CMD_SETTINGS as u32, 0);
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, null());
        DestroyMenu(menu);
    }
}
