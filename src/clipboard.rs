//! Reading and writing clipboard text and images, honouring the opt-out
//! formats that password managers and other clipboard tools put on the
//! clipboard.

use crate::image::{self, Rgba};
use std::ptr::copy_nonoverlapping;
use windows_sys::w;
use windows_sys::Win32::Foundation::{GlobalFree, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Threading::Sleep;

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_DIBV5: u32 = 17;

/// Registered formats clipstash looks at.
pub struct Formats {
    /// Set by clipboard tools, Ditto among them, on data they put back themselves.
    viewer_ignore: u32,
    /// Set by password managers (KeePass, 1Password, ...).
    exclude_monitor: u32,
    /// A DWORD; 0 means the owner asked for this not to enter clipboard history.
    can_include_history: u32,
    /// PNG bytes, offered by Snipping Tool, browsers and Office alongside CF_DIB.
    png: u32,
}

impl Formats {
    pub fn register() -> Self {
        unsafe {
            Self {
                viewer_ignore: RegisterClipboardFormatW(w!("Clipboard Viewer Ignore")),
                exclude_monitor: RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")),
                can_include_history: RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
                png: RegisterClipboardFormatW(w!("PNG")),
            }
        }
    }
}

pub enum Content {
    Text(String),
    /// Decoded pixels, plus the source PNG when the clipboard offered one
    /// (stored as-is, so transparency and compression are preserved).
    Image { img: Rgba, png: Option<Vec<u8>> },
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

fn available(fmt: u32) -> bool {
    fmt != 0 && unsafe { IsClipboardFormatAvailable(fmt) } != 0
}

/// Copies a clipboard format's bytes. The clipboard must be open.
unsafe fn read_bytes(fmt: u32) -> Option<Vec<u8>> {
    let h = GetClipboardData(fmt);
    let p = GlobalLock(h) as *const u8;
    if p.is_null() {
        return None;
    }
    let bytes = std::slice::from_raw_parts(p, GlobalSize(h)).to_vec();
    GlobalUnlock(h);
    Some(bytes)
}

/// Current clipboard contents worth recording, or None if there is nothing
/// usable or the owner opted out. Text wins when both text and an image are
/// offered (e.g. a spreadsheet range). With `respect_privacy` off, only the
/// clipboard-tool marker is honoured.
pub fn read(owner: HWND, f: &Formats, respect_privacy: bool, want_images: bool) -> Option<Content> {
    if available(f.viewer_ignore) || respect_privacy && available(f.exclude_monitor) {
        return None;
    }
    let has_text = available(CF_UNICODETEXT);
    let has_image = want_images && (available(f.png) || available(CF_DIBV5) || available(CF_DIB));
    if !has_text && !has_image {
        return None;
    }
    unsafe {
        let _open = Opened::new(owner)?;
        if respect_privacy && available(f.can_include_history) {
            if let Some(b) = read_bytes(f.can_include_history) {
                if b.len() >= 4 && u32::from_le_bytes(b[..4].try_into().unwrap()) == 0 {
                    return None;
                }
            }
        }
        if has_text {
            return read_text_open().map(Content::Text);
        }
        if available(f.png) {
            if let Some(png) = read_bytes(f.png) {
                if let Some(img) = image::decode_png(&png) {
                    return Some(Content::Image { img, png: Some(png) });
                }
            }
        }
        // CF_DIBV5 keeps alpha masks; Windows synthesises CF_DIB from any bitmap.
        for fmt in [CF_DIBV5, CF_DIB] {
            if let Some(img) = available(fmt).then(|| read_bytes(fmt)).flatten().and_then(|d| image::dib_to_rgba(&d)) {
                return Some(Content::Image { img, png: None });
            }
        }
        None
    }
}

unsafe fn read_text_open() -> Option<String> {
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

/// Puts `data` on the (open, emptied) clipboard as `fmt`.
unsafe fn set_bytes(fmt: u32, data: &[u8]) -> bool {
    let h = GlobalAlloc(GMEM_MOVEABLE, data.len());
    if h.is_null() {
        return false;
    }
    let p = GlobalLock(h) as *mut u8;
    if p.is_null() {
        GlobalFree(h);
        return false;
    }
    copy_nonoverlapping(data.as_ptr(), p, data.len());
    GlobalUnlock(h);
    // On success the system owns the memory; on failure we still do.
    if SetClipboardData(fmt, h).is_null() {
        GlobalFree(h);
        return false;
    }
    true
}

/// Replaces the clipboard contents with `text`.
pub fn write_text(owner: HWND, text: &str) -> bool {
    let bytes: Vec<u8> = text.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
    unsafe {
        let Some(_open) = Opened::new(owner) else { return false };
        EmptyClipboard();
        set_bytes(CF_UNICODETEXT, &bytes)
    }
}

/// Replaces the clipboard contents with an image, as both CF_DIB (understood
/// everywhere; Windows derives CF_BITMAP/CF_DIBV5 from it) and PNG (kept by
/// browsers, Office and chat apps, with transparency).
pub fn write_image(owner: HWND, f: &Formats, png: &[u8]) -> bool {
    let Some(img) = image::decode_png(png) else { return false };
    let dib = image::rgba_to_dib(&img);
    unsafe {
        let Some(_open) = Opened::new(owner) else { return false };
        EmptyClipboard();
        let ok = set_bytes(CF_DIB, &dib);
        set_bytes(f.png, png);
        ok
    }
}
