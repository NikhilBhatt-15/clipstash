//! Colour palettes. "system" reads Windows' app light/dark preference and the
//! user's accent colour from the registry and is rebuilt when they change.

use std::mem::size_of;
use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD};

/// RGB colour; converted to COLORREF / ARGB at draw time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn hex(v: u32) -> Self {
        Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
    pub fn colorref(self) -> u32 {
        self.0 as u32 | (self.1 as u32) << 8 | (self.2 as u32) << 16
    }
    pub fn argb(self, alpha: u8) -> u32 {
        (alpha as u32) << 24 | (self.0 as u32) << 16 | (self.1 as u32) << 8 | self.2 as u32
    }
    /// `self` over `under` at `t` (0..=1) opacity.
    pub fn mix(self, under: Rgb, t: f32) -> Rgb {
        let m = |a: u8, b: u8| (a as f32 * t + b as f32 * (1.0 - t)).round() as u8;
        Rgb(m(self.0, under.0), m(self.1, under.1), m(self.2, under.2))
    }
    fn luminance(self) -> f32 {
        (0.299 * self.0 as f32 + 0.587 * self.1 as f32 + 0.114 * self.2 as f32) / 255.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    /// Window background.
    pub bg: Rgb,
    /// Search field, hover rows, controls.
    pub surface: Rgb,
    /// Preview pane background.
    pub inset: Rgb,
    pub border: Rgb,
    pub text: Rgb,
    pub subtext: Rgb,
    pub accent: Rgb,
    /// Text drawn on a solid accent fill.
    pub on_accent: Rgb,
    /// Selected row fill (accent tinted into bg).
    pub selection: Rgb,
    pub danger: Rgb,
}

impl Theme {
    #[allow(clippy::too_many_arguments)]
    fn build(dark: bool, bg: Rgb, surface: Rgb, inset: Rgb, border: Rgb, text: Rgb, subtext: Rgb, accent: Rgb) -> Self {
        let on_accent = if accent.luminance() > 0.6 { Rgb::hex(0x101010) } else { Rgb::hex(0xffffff) };
        Theme {
            dark,
            bg,
            surface,
            inset,
            border,
            text,
            subtext,
            accent,
            on_accent,
            selection: accent.mix(bg, if dark { 0.28 } else { 0.16 }),
            danger: if dark { Rgb::hex(0xff6b6b) } else { Rgb::hex(0xc42b1c) },
        }
    }

    pub fn named(name: &str) -> Self {
        match name {
            "light" => Self::light(system_accent(false)),
            "dark" => Self::dark(system_accent(true)),
            "nord" => Self::build(true, Rgb::hex(0x2e3440), Rgb::hex(0x3b4252), Rgb::hex(0x292e39), Rgb::hex(0x434c5e), Rgb::hex(0xeceff4), Rgb::hex(0x9aa5b8), Rgb::hex(0x88c0d0)),
            "dracula" => Self::build(true, Rgb::hex(0x282a36), Rgb::hex(0x343746), Rgb::hex(0x21222c), Rgb::hex(0x44475a), Rgb::hex(0xf8f8f2), Rgb::hex(0x9ea8c7), Rgb::hex(0xbd93f9)),
            "catppuccin" => Self::build(true, Rgb::hex(0x1e1e2e), Rgb::hex(0x313244), Rgb::hex(0x181825), Rgb::hex(0x45475a), Rgb::hex(0xcdd6f4), Rgb::hex(0xa6adc8), Rgb::hex(0xcba6f7)),
            "solarized-light" => Self::build(false, Rgb::hex(0xfdf6e3), Rgb::hex(0xeee8d5), Rgb::hex(0xf5efdc), Rgb::hex(0xe4dcc4), Rgb::hex(0x073642), Rgb::hex(0x657b83), Rgb::hex(0x268bd2)),
            "solarized-dark" => Self::build(true, Rgb::hex(0x002b36), Rgb::hex(0x073642), Rgb::hex(0x00252e), Rgb::hex(0x0e4a57), Rgb::hex(0xd5dcdc), Rgb::hex(0x8a9a9a), Rgb::hex(0x2aa198)),
            _ => {
                let dark = system_prefers_dark();
                if dark {
                    Self::dark(system_accent(true))
                } else {
                    Self::light(system_accent(false))
                }
            }
        }
    }

    /// Windows 11 dark: Mica-like neutrals.
    fn dark(accent: Rgb) -> Self {
        Self::build(true, Rgb::hex(0x202020), Rgb::hex(0x2d2d2d), Rgb::hex(0x1a1a1a), Rgb::hex(0x3a3a3a), Rgb::hex(0xffffff), Rgb::hex(0xa8a8a8), accent)
    }

    fn light(accent: Rgb) -> Self {
        Self::build(false, Rgb::hex(0xf9f9f9), Rgb::hex(0xffffff), Rgb::hex(0xf1f1f1), Rgb::hex(0xe1e1e1), Rgb::hex(0x1b1b1b), Rgb::hex(0x5f5f5f), accent)
    }
}

fn read_dword(key: *const u16, value: *const u16) -> Option<u32> {
    let mut data = 0u32;
    let mut len = size_of::<u32>() as u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key, value, RRF_RT_REG_DWORD, null_mut(), &mut data as *mut u32 as _, &mut len) };
    (r == 0).then_some(data)
}

pub fn system_prefers_dark() -> bool {
    read_dword(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), w!("AppsUseLightTheme")) == Some(0)
}

/// The accent the Settings app shows. Dark mode uses a lighter shade from the
/// accent palette for contrast, as Windows itself does.
pub fn system_accent(dark: bool) -> Rgb {
    let mut palette = [0u8; 32];
    let mut len = palette.len() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            null_mut(),
            palette.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if r == 0 && len == 32 {
        // Eight RGBA entries: 0-2 lighter shades, 3 the base accent, 4-6 darker.
        // Like Windows: a light shade on dark backgrounds, a dark one on light.
        let i = if dark { 1 } else { 4 };
        let p = &palette[i * 4..i * 4 + 3];
        return Rgb(p[0], p[1], p[2]);
    }
    // ABGR
    match read_dword(w!("Software\\Microsoft\\Windows\\DWM"), w!("AccentColor")) {
        Some(v) => Rgb(v as u8, (v >> 8) as u8, (v >> 16) as u8),
        None => Rgb::hex(0x0078d4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_conversions() {
        let c = Rgb::hex(0x112233);
        assert_eq!(c.colorref(), 0x332211);
        assert_eq!(c.argb(0xff), 0xff112233);
        assert_eq!(Rgb::hex(0xffffff).mix(Rgb::hex(0x000000), 0.5), Rgb(128, 128, 128));
    }

    #[test]
    fn every_named_theme_builds_with_readable_contrast() {
        for name in crate::settings::THEMES {
            let t = Theme::named(name);
            assert!((t.text.luminance() - t.bg.luminance()).abs() > 0.5, "{name}");
            assert_eq!(t.dark, t.bg.luminance() < 0.5, "{name}");
        }
    }
}
