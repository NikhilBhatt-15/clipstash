//! User settings, stored as JSON next to the history database. The file is
//! meant to be hand-editable; unknown or invalid values fall back to defaults.

use crate::hotkey::Hotkey;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const THEMES: &[&str] = &["system", "light", "dark", "nord", "dracula", "catppuccin", "solarized-light", "solarized-dark"];
pub const POSITIONS: &[&str] = &["caret", "mouse", "center"];
pub const FONT_SIZES: std::ops::RangeInclusive<u32> = 8..=16;
pub const HISTORY_SIZES: &[u32] = &[50, 100, 200, 500, 1000, 2000, 5000];
pub const IMAGE_HISTORY_SIZES: &[u32] = &[10, 25, 50, 100, 250];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// One of THEMES. "system" follows Windows light/dark mode and accent colour.
    pub theme: String,
    /// Global shortcut, e.g. "Win+Alt+V".
    pub hotkey: String,
    /// Unpinned text entries kept; pinned entries are never pruned.
    pub max_history: u32,
    /// Record copied images (screenshots etc.).
    pub save_images: bool,
    /// Unpinned images kept, separately from text since they are much larger.
    pub max_images: u32,
    /// UI font size in points.
    pub font_size: u32,
    /// Show the full-text preview pane next to the list.
    pub show_preview: bool,
    /// Where the popup opens: "caret" (text cursor, falling back to mouse), "mouse", "center".
    pub position: String,
    /// Enter pastes into the previous app; off means Enter only copies.
    pub paste_on_enter: bool,
    /// Convert lone LF/CR line endings to CRLF when pasting.
    pub normalize_line_endings: bool,
    /// Skip clips that password managers mark as private.
    pub respect_privacy: bool,
    /// Mirrored into HKCU\...\Run; the registry is the source of truth.
    pub start_with_windows: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            hotkey: Hotkey::default().display(),
            max_history: 500,
            save_images: true,
            max_images: 50,
            font_size: 10,
            show_preview: true,
            position: "caret".into(),
            paste_on_enter: true,
            normalize_line_endings: true,
            respect_privacy: true,
            start_with_windows: false,
        }
    }
}

impl Settings {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("settings.json")
    }

    pub fn load(dir: &Path) -> Self {
        let mut s: Settings = std::fs::read_to_string(Self::path(dir))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        s.sanitize();
        s.start_with_windows = autostart::enabled();
        s
    }

    pub fn save(&self, dir: &Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(dir), json);
        }
    }

    fn sanitize(&mut self) {
        let d = Settings::default();
        if !THEMES.contains(&self.theme.as_str()) {
            self.theme = d.theme;
        }
        if !POSITIONS.contains(&self.position.as_str()) {
            self.position = d.position;
        }
        if Hotkey::parse(&self.hotkey).is_none() {
            self.hotkey = d.hotkey;
        }
        self.font_size = self.font_size.clamp(*FONT_SIZES.start(), *FONT_SIZES.end());
        self.max_history = self.max_history.clamp(10, 100_000);
        self.max_images = self.max_images.clamp(1, 10_000);
    }

    pub fn hotkey(&self) -> Hotkey {
        Hotkey::parse(&self.hotkey).unwrap_or_default()
    }
}

/// Start-with-Windows via the per-user Run key.
pub mod autostart {
    use std::ptr::null_mut;
    use windows_sys::w;
    use windows_sys::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };

    const RUN: *const u16 = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    const NAME: *const u16 = w!("clipstash");

    pub fn enabled() -> bool {
        unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN, NAME, RRF_RT_REG_SZ, null_mut(), null_mut(), null_mut()) == 0 }
    }

    pub fn set(on: bool) -> bool {
        unsafe {
            if !on {
                let r = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, NAME);
                return r == 0 || r == 2; // ERROR_FILE_NOT_FOUND: already off
            }
            let Ok(exe) = std::env::current_exe() else { return false };
            let cmd: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(Some(0)).collect();
            RegSetKeyValueW(HKEY_CURRENT_USER, RUN, NAME, REG_SZ, cmd.as_ptr().cast(), (cmd.len() * 2) as u32) == 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_and_invalid_json_falls_back_per_field() {
        let mut s: Settings = serde_json::from_str(r#"{"theme":"nope","font_size":99,"show_preview":false,"hotkey":"V"}"#).unwrap();
        s.sanitize();
        assert_eq!(s.theme, "system");
        assert_eq!(s.font_size, 16);
        assert!(!s.show_preview);
        assert_eq!(s.hotkey, "Win+Alt+V");
        assert_eq!(s.max_history, 500);
        assert!(s.save_images);
        assert_eq!(s.max_images, 50);
    }

    #[test]
    fn round_trip() {
        let s = Settings { theme: "nord".into(), max_history: 2000, ..Default::default() };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
