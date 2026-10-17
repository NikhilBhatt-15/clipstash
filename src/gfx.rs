//! Drawing helpers: a double-buffered canvas that uses GDI+ for antialiased
//! shapes and GDI for ClearType text, plus DPI-aware font sets.

use crate::theme::Rgb;
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT, SIZE};
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Graphics::GdiPlus::*;

pub fn startup() {
    let input = GdiplusStartupInput { GdiplusVersion: 1, DebugEventCallback: 0, SuppressBackgroundThread: 0, SuppressExternalCodecs: 0 };
    let mut token = 0usize;
    unsafe { GdiplusStartup(&mut token, &input, null_mut()) };
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT { left, top, right, bottom }
}

pub fn inset(r: RECT, dx: i32, dy: i32) -> RECT {
    rect(r.left + dx, r.top + dy, r.right - dx, r.bottom - dy)
}

pub fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Window chrome: rounded corners, dark-mode frame, and (Windows 11) border
/// and caption colours from the theme. Each call is a no-op where unsupported.
pub fn style_frame(hwnd: HWND, theme: &crate::theme::Theme, caption: bool) {
    unsafe {
        let set = |attr: i32, v: u32| DwmSetWindowAttribute(hwnd, attr as u32, &v as *const u32 as _, 4);
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, theme.dark as u32);
        set(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND as u32);
        set(DWMWA_BORDER_COLOR, theme.border.colorref());
        if caption {
            set(DWMWA_CAPTION_COLOR, theme.bg.colorref());
            set(DWMWA_TEXT_COLOR, theme.text.colorref());
        }
    }
}

pub struct Canvas {
    target: HDC,
    pub dc: HDC,
    bmp: HBITMAP,
    old_bmp: HGDIOBJ,
    g: *mut GpGraphics,
    w: i32,
    h: i32,
}

impl Canvas {
    pub fn new(target: HDC, w: i32, h: i32) -> Self {
        unsafe {
            let dc = CreateCompatibleDC(target);
            let bmp = CreateCompatibleBitmap(target, w.max(1), h.max(1));
            let old_bmp = SelectObject(dc, bmp);
            SetBkMode(dc, TRANSPARENT as i32);
            let mut g = null_mut();
            GdipCreateFromHDC(dc, &mut g);
            GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
            GdipSetPixelOffsetMode(g, PixelOffsetModeHalf);
            Canvas { target, dc, bmp, old_bmp, g, w, h }
        }
    }

    /// Copies the finished frame to the window.
    pub fn present(&self) {
        unsafe {
            GdipFlush(self.g, FlushIntentionFlush);
            BitBlt(self.target, 0, 0, self.w, self.h, self.dc, 0, 0, SRCCOPY);
        }
    }

    pub fn fill(&self, r: RECT, c: Rgb) {
        unsafe {
            let b = CreateSolidBrush(c.colorref());
            FillRect(self.dc, &r, b);
            DeleteObject(b);
        }
    }

    fn path(r: RECT, radius: i32) -> *mut GpPath {
        let mut p = null_mut();
        unsafe {
            GdipCreatePath(FillModeAlternate, &mut p);
            let d = (radius * 2).min(r.right - r.left).min(r.bottom - r.top).max(1);
            let (x, y, x2, y2) = (r.left, r.top, r.right - d, r.bottom - d);
            GdipAddPathArcI(p, x, y, d, d, 180.0, 90.0);
            GdipAddPathArcI(p, x2, y, d, d, 270.0, 90.0);
            GdipAddPathArcI(p, x2, y2, d, d, 0.0, 90.0);
            GdipAddPathArcI(p, x, y2, d, d, 90.0, 90.0);
            GdipClosePathFigure(p);
        }
        p
    }

    pub fn round(&self, r: RECT, radius: i32, c: Rgb) {
        self.round_alpha(r, radius, c, 255);
    }

    pub fn round_alpha(&self, r: RECT, radius: i32, c: Rgb, alpha: u8) {
        fill_round(self.g, r, radius, c.argb(alpha));
        unsafe { GdipFlush(self.g, FlushIntentionFlush) };
    }

    pub fn round_stroke(&self, r: RECT, radius: i32, c: Rgb, width: f32) {
        unsafe {
            // Stroke centred on the edge: inset by half the pen so it stays inside.
            let half = (width / 2.0).ceil() as i32;
            let p = Self::path(inset(r, half, half), radius);
            let mut pen = null_mut();
            GdipCreatePen1(c.argb(255), width, UnitPixel, &mut pen);
            GdipDrawPath(self.g, pen, p);
            GdipDeletePen(pen);
            GdipDeletePath(p);
            GdipFlush(self.g, FlushIntentionFlush);
        }
    }

    pub fn circle(&self, cx: i32, cy: i32, d: i32, c: Rgb) {
        unsafe {
            let mut b = null_mut();
            GdipCreateSolidFill(c.argb(255), &mut b);
            GdipFillEllipseI(self.g, b as *mut GpBrush, cx - d / 2, cy - d / 2, d, d);
            GdipDeleteBrush(b as *mut GpBrush);
            GdipFlush(self.g, FlushIntentionFlush);
        }
    }

    /// DrawTextW wrapper. `flags` are DT_* values; DT_NOPREFIX is always added.
    pub fn text(&self, s: &str, mut r: RECT, font: HFONT, c: Rgb, flags: u32) {
        let w = wide(s);
        unsafe {
            SelectObject(self.dc, font);
            SetTextColor(self.dc, c.colorref());
            DrawTextW(self.dc, w.as_ptr(), w.len() as i32, &mut r, flags | DT_NOPREFIX);
        }
    }

    /// Height DrawTextW would need for wrapped text of width `width`.
    pub fn measure_wrapped(&self, s: &str, width: i32, font: HFONT, flags: u32) -> i32 {
        let w = wide(s);
        let mut r = rect(0, 0, width, 0);
        unsafe {
            SelectObject(self.dc, font);
            DrawTextW(self.dc, w.as_ptr(), w.len() as i32, &mut r, flags | DT_NOPREFIX | DT_CALCRECT);
        }
        r.bottom
    }

    pub fn extent(&self, s: &[u16], font: HFONT) -> i32 {
        let mut sz = SIZE { cx: 0, cy: 0 };
        unsafe {
            SelectObject(self.dc, font);
            GetTextExtentPoint32W(self.dc, s.as_ptr(), s.len() as i32, &mut sz);
        }
        sz.cx
    }

    /// Draws a single line, clipped to `clip`, without wrapping.
    pub fn text_at(&self, s: &[u16], x: i32, y: i32, clip: &RECT, font: HFONT, c: Rgb) {
        unsafe {
            SelectObject(self.dc, font);
            SetTextColor(self.dc, c.colorref());
            ExtTextOutW(self.dc, x, y, ETO_CLIPPED, clip, s.as_ptr(), s.len() as u32, null());
        }
    }

    /// One line with highlighted char ranges (search hits), ending in an
    /// ellipsis if it doesn't fit. `hl` is the pill colour behind matches.
    #[allow(clippy::too_many_arguments)]
    pub fn highlighted_line(&self, chars: &[char], ranges: &[(usize, usize)], r: RECT, font: HFONT, c: Rgb, hl: Rgb, line_h: i32) {
        let ellipsis = wide("…");
        let ell_w = self.extent(&ellipsis, font);
        let mut x = r.left;
        let y = r.top;
        let mut pos = 0;
        let mut segments: Vec<(usize, usize, bool)> = Vec::new();
        for &(s, e) in ranges {
            if s > pos {
                segments.push((pos, s, false));
            }
            segments.push((s, e, true));
            pos = e;
        }
        if pos < chars.len() {
            segments.push((pos, chars.len(), false));
        }
        let total: i32 = {
            let all: String = chars.iter().collect();
            self.extent(&wide(&all), font)
        };
        let limit = if total > r.right - r.left { r.right - ell_w } else { r.right };
        for (s, e, is_hit) in segments {
            let seg: String = chars[s..e].iter().collect();
            let mut w16 = wide(&seg);
            let mut w = self.extent(&w16, font);
            let mut truncated = false;
            if x + w > limit {
                let mut fit = 0i32;
                let mut sz = SIZE { cx: 0, cy: 0 };
                unsafe {
                    SelectObject(self.dc, font);
                    GetTextExtentExPointW(self.dc, w16.as_ptr(), w16.len() as i32, (limit - x).max(0), &mut fit, null_mut(), &mut sz);
                }
                w16.truncate(fit.max(0) as usize);
                w = self.extent(&w16, font);
                truncated = true;
            }
            if is_hit && w > 0 {
                self.round(rect(x - 1, y, x + w + 1, y + line_h), 3, hl);
            }
            self.text_at(&w16, x, y, &r, font, c);
            x += w;
            if truncated {
                self.text_at(&ellipsis, x, y, &r, font, c);
                break;
            }
        }
    }
}

fn fill_round(g: *mut GpGraphics, r: RECT, radius: i32, argb: u32) {
    unsafe {
        let p = Canvas::path(r, radius);
        let mut b = null_mut();
        GdipCreateSolidFill(argb, &mut b);
        GdipFillPath(g, b as *mut GpBrush, p);
        GdipDeleteBrush(b as *mut GpBrush);
        GdipDeletePath(p);
    }
}

const PIXEL_FORMAT_32BPP_ARGB: i32 = 0x0026_200A;

/// The app icon, drawn at `size` px: a white clipboard on a blue rounded square.
pub fn make_icon(size: i32) -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    const BRAND: Rgb = Rgb::hex(0x2563eb);
    const BRAND_DARK: Rgb = Rgb::hex(0x1e3a8a);
    let p = |v: f32| (v * size as f32).round() as i32;
    unsafe {
        let mut bmp = null_mut();
        GdipCreateBitmapFromScan0(size, size, 0, PIXEL_FORMAT_32BPP_ARGB, null(), &mut bmp);
        let mut g = null_mut();
        GdipGetImageGraphicsContext(bmp as *mut GpImage, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        GdipSetPixelOffsetMode(g, PixelOffsetModeHalf);
        fill_round(g, rect(0, 0, size, size), p(0.22), BRAND.argb(255));
        fill_round(g, rect(p(0.24), p(0.20), p(0.76), p(0.86)), p(0.08), Rgb::hex(0xffffff).argb(255));
        fill_round(g, rect(p(0.36), p(0.12), p(0.64), p(0.28)), p(0.05), BRAND_DARK.argb(255));
        let bar = p(0.06).max(1);
        for (y, end) in [(0.42, 0.67), (0.55, 0.67), (0.68, 0.55)] {
            fill_round(g, rect(p(0.33), p(y), p(end), p(y) + bar), bar / 2, BRAND.argb(255));
        }
        GdipDeleteGraphics(g);
        let mut icon = null_mut();
        GdipCreateHICONFromBitmap(bmp, &mut icon);
        GdipDisposeImage(bmp as *mut GpImage);
        icon
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteGraphics(self.g);
            SelectObject(self.dc, self.old_bmp);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}

unsafe extern "system" fn found_face(_: *const LOGFONTW, _: *const TEXTMETRICW, _: u32, lp: LPARAM) -> i32 {
    *(lp as *mut bool) = true;
    0
}

fn face_exists(name: &str) -> bool {
    unsafe {
        let mut lf: LOGFONTW = zeroed();
        lf.lfCharSet = DEFAULT_CHARSET;
        for (i, c) in name.encode_utf16().take(31).enumerate() {
            lf.lfFaceName[i] = c;
        }
        let dc = GetDC(null_mut());
        let mut found = false;
        EnumFontFamiliesExW(dc, &lf, Some(found_face), &mut found as *mut bool as LPARAM, 0);
        ReleaseDC(null_mut(), dc);
        found
    }
}

fn first_face(candidates: &'static [&'static str], cache: &'static OnceLock<&'static str>) -> &'static str {
    cache.get_or_init(|| candidates.iter().copied().find(|f| face_exists(f)).unwrap_or(candidates[candidates.len() - 1]))
}

fn ui_face() -> &'static str {
    static F: OnceLock<&str> = OnceLock::new();
    first_face(&["Segoe UI Variable Text", "Segoe UI"], &F)
}
fn mono_face() -> &'static str {
    static F: OnceLock<&str> = OnceLock::new();
    first_face(&["Cascadia Mono", "Cascadia Code", "Consolas"], &F)
}
fn icon_face() -> &'static str {
    static F: OnceLock<&str> = OnceLock::new();
    first_face(&["Segoe Fluent Icons", "Segoe MDL2 Assets"], &F)
}

/// Segoe Fluent Icons / MDL2 Assets code points.
pub mod icon {
    pub const SEARCH: &str = "\u{E721}";
    pub const SETTINGS: &str = "\u{E713}";
    pub const PIN: &str = "\u{E718}";
    pub const DOCUMENT: &str = "\u{E8A5}";
    pub const CODE: &str = "\u{E943}";
    pub const LINK: &str = "\u{E71B}";
    pub const FOLDER: &str = "\u{E8B7}";
    pub const PASTE: &str = "\u{E77F}";
    pub const CHEVRON_LEFT: &str = "\u{E76B}";
    pub const CHEVRON_RIGHT: &str = "\u{E76C}";
}

fn make_font(face: &str, px: i32, weight: i32) -> HFONT {
    let f: Vec<u16> = face.encode_utf16().chain(Some(0)).collect();
    unsafe {
        CreateFontW(-px, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, f.as_ptr())
    }
}

fn line_height(font: HFONT) -> i32 {
    unsafe {
        let dc = GetDC(null_mut());
        let old = SelectObject(dc, font);
        let mut tm: TEXTMETRICW = zeroed();
        GetTextMetricsW(dc, &mut tm);
        SelectObject(dc, old);
        ReleaseDC(null_mut(), dc);
        tm.tmHeight
    }
}

/// All fonts the UI uses at one DPI and base size, with their line heights.
pub struct Fonts {
    pub dpi: i32,
    pub pt: i32,
    pub ui: HFONT,
    pub ui_bold: HFONT,
    pub small: HFONT,
    pub title: HFONT,
    pub mono: HFONT,
    pub icon: HFONT,
    pub icon_small: HFONT,
    pub ui_h: i32,
    pub small_h: i32,
    pub mono_h: i32,
}

impl Fonts {
    pub fn new(dpi: i32, pt: i32) -> Self {
        let px = |p: f32| (p * dpi as f32 / 72.0).round() as i32;
        let base = pt as f32;
        let ui = make_font(ui_face(), px(base), FW_NORMAL as i32);
        let small = make_font(ui_face(), px(base - 1.5), FW_NORMAL as i32);
        let mono = make_font(mono_face(), px(base - 0.5), FW_NORMAL as i32);
        Fonts {
            dpi,
            pt,
            ui,
            ui_bold: make_font(ui_face(), px(base), FW_SEMIBOLD as i32),
            small,
            title: make_font(ui_face(), px(base * 1.6), FW_SEMIBOLD as i32),
            mono,
            icon: make_font(icon_face(), px(base + 1.0), FW_NORMAL as i32),
            icon_small: make_font(icon_face(), px(base - 2.0), FW_NORMAL as i32),
            ui_h: line_height(ui),
            small_h: line_height(small),
            mono_h: line_height(mono),
        }
    }

    /// Logical (96-DPI, 10pt) pixels to device pixels.
    pub fn s(&self, v: i32) -> i32 {
        (v as f32 * self.dpi as f32 / 96.0 * self.pt as f32 / 10.0).round() as i32
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [self.ui, self.ui_bold, self.small, self.title, self.mono, self.icon, self.icon_small] {
            unsafe { DeleteObject(f) };
        }
    }
}
