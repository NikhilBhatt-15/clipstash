//! Reading and writing CF_UNICODETEXT, honouring the opt-out formats that
//! password managers and other clipboard tools put on the clipboard.

use std::ptr::copy_nonoverlapping;
use windows_sys::w;
use windows_sys::Win32::Foundation::{GlobalFree, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Threading::Sleep;

const CF_UNICODETEXT: u32 = 13;

/// Registered formats whose presence (or value) means "do not record this".
pub struct OptOutFormats {
    /// Set by clipboard tools, Ditto among them, on data they put back themselves.
    viewer_ignore: u32,
    /// Set by password managers (KeePass, 1Password, ...).
    exclude_monitor: u32,
    /// A DWORD; 0 means the owner asked for this not to enter clipboard history.
    can_include_history: u32,
}

impl OptOutFormats {
    pub fn register() -> Self {
        unsafe {
            Self {
                viewer_ignore: RegisterClipboardFormatW(w!("Clipboard Viewer Ignore")),
                exclude_monitor: RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")),
                can_include_history: RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
            }
        }
    }
}

/// Closes the clipboard when dropped so early returns can't leave it open.
struct Opened;

impl Opened {
    /// Another process may hold the clipboard briefly right after it changes,
    /// so retry for ~150 ms before giving up.
    fn new(owner: HWND) -> Option<Self> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(owner) } != 0 {
                return Some(Opened);
            }
            unsafe { Sleep(15) };
        }
        None
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

/// Current clipboard text, or None if there is none or the owner opted out.
/// With `respect_privacy` off, only the clipboard-tool marker is honoured.
pub fn read_text(owner: HWND, opt_out: &OptOutFormats, respect_privacy: bool) -> Option<String> {
    unsafe {
        if IsClipboardFormatAvailable(opt_out.viewer_ignore) != 0
            || respect_privacy && IsClipboardFormatAvailable(opt_out.exclude_monitor) != 0
            || IsClipboardFormatAvailable(CF_UNICODETEXT) == 0
        {
            return None;
        }
        let _open = Opened::new(owner)?;

        if respect_privacy && IsClipboardFormatAvailable(opt_out.can_include_history) != 0 {
            let h = GetClipboardData(opt_out.can_include_history);
            let p = GlobalLock(h) as *const u32;
            if !p.is_null() {
                let allowed = GlobalSize(h) < 4 || p.read_unaligned() != 0;
                GlobalUnlock(h);
                if !allowed {
                    return None;
                }
            }
        }

        let h = GetClipboardData(CF_UNICODETEXT);
        let p = GlobalLock(h) as *const u16;
        if p.is_null() {
            return None;
        }
        // Bounded by the allocation in case the text is not NUL-terminated.
        let max = GlobalSize(h) / 2;
        let mut len = 0;
        while len < max && *p.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        GlobalUnlock(h);
        Some(text)
    }
}

/// Replaces the clipboard contents with `text`.
pub fn write_text(owner: HWND, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let Some(_open) = Opened::new(owner) else { return false };
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        if h.is_null() {
            return false;
        }
        let p = GlobalLock(h) as *mut u16;
        if p.is_null() {
            GlobalFree(h);
            return false;
        }
        copy_nonoverlapping(wide.as_ptr(), p, wide.len());
        GlobalUnlock(h);
        // On success the system owns the memory; on failure we still do.
        if SetClipboardData(CF_UNICODETEXT, h).is_null() {
            GlobalFree(h);
            return false;
        }
        true
    }
}
