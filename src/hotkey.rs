//! Hotkeys as text ("Ctrl+Shift+V") for the settings file, and back.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_BACK, VK_DELETE, VK_END, VK_F1, VK_F24, VK_HOME,
    VK_INSERT, VK_NEXT, VK_OEM_1, VK_OEM_2, VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA,
    VK_OEM_MINUS, VK_OEM_PERIOD, VK_OEM_PLUS, VK_PRIOR, VK_SPACE, VK_TAB,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

/// Win+Alt+V: next to Windows' own clipboard history (Win+V), and apps don't
/// bind Win-key shortcuts, so it doesn't shadow anything inside them (unlike
/// Ctrl+Shift+V, which is "paste as plain text" in browsers and editors).
impl Default for Hotkey {
    fn default() -> Self {
        Self { mods: MOD_WIN | MOD_ALT, vk: b'V' as u32 }
    }
}

const NAMED: &[(u16, &str)] = &[
    (VK_SPACE, "Space"),
    (VK_TAB, "Tab"),
    (VK_BACK, "Backspace"),
    (VK_INSERT, "Insert"),
    (VK_DELETE, "Delete"),
    (VK_HOME, "Home"),
    (VK_END, "End"),
    (VK_PRIOR, "PageUp"),
    (VK_NEXT, "PageDown"),
    (VK_OEM_3, "`"),
    (VK_OEM_MINUS, "-"),
    (VK_OEM_PLUS, "="),
    (VK_OEM_4, "["),
    (VK_OEM_6, "]"),
    (VK_OEM_5, "\\"),
    (VK_OEM_1, ";"),
    (VK_OEM_7, "'"),
    (VK_OEM_COMMA, ","),
    (VK_OEM_PERIOD, "."),
    (VK_OEM_2, "/"),
];

pub fn key_name(vk: u32) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        v if (VK_F1 as u32..=VK_F24 as u32).contains(&v) => format!("F{}", v - VK_F1 as u32 + 1),
        v => NAMED
            .iter()
            .find(|(k, _)| *k as u32 == v)
            .map(|(_, n)| n.to_string())
            .unwrap_or_else(|| format!("0x{v:02X}")),
    }
}

/// Keys that may be the non-modifier part of a hotkey.
pub fn is_bindable(vk: u32) -> bool {
    !key_name(vk).starts_with("0x")
}

fn key_from_name(name: &str) -> Option<u32> {
    let up = name.to_ascii_uppercase();
    if up.len() == 1 {
        let c = up.as_bytes()[0];
        if c.is_ascii_alphanumeric() {
            return Some(c as u32);
        }
    }
    if let Some(n) = up.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(VK_F1 as u32 + n - 1);
        }
    }
    if let Some(hex) = up.strip_prefix("0X") {
        return u32::from_str_radix(hex, 16).ok();
    }
    NAMED.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(k, _)| *k as u32)
}

impl Hotkey {
    pub fn parse(s: &str) -> Option<Self> {
        let mut mods = 0;
        let mut vk = None;
        // Split on '+' but allow the key itself to be '+'/'=' by trimming parts.
        for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods |= MOD_CONTROL,
                "shift" => mods |= MOD_SHIFT,
                "alt" => mods |= MOD_ALT,
                "win" | "windows" | "super" => mods |= MOD_WIN,
                _ => {
                    if vk.is_some() {
                        return None;
                    }
                    vk = Some(key_from_name(part)?);
                }
            }
        }
        // A bare key would steal ordinary typing.
        if mods == 0 {
            return None;
        }
        Some(Self { mods, vk: vk? })
    }

    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        // Windows' own order: Win+Ctrl+Alt+Shift+key.
        for (m, n) in [(MOD_WIN, "Win"), (MOD_CONTROL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift")] {
            if self.mods & m != 0 {
                parts.push(n.to_string());
            }
        }
        parts.push(key_name(self.vk));
        parts.join("+")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for s in ["Win+Alt+V", "Ctrl+Shift+V", "Alt+Space", "Ctrl+Alt+F12", "Win+`", "Ctrl+Shift+0", "Ctrl+PageDown"] {
            let hk = Hotkey::parse(s).unwrap();
            assert_eq!(hk.display(), s);
        }
    }

    #[test]
    fn parsing_is_lenient_on_case_and_order() {
        assert_eq!(Hotkey::parse("shift + ctrl + v"), Hotkey::parse("Ctrl+Shift+V"));
        assert_eq!(Hotkey::parse("control+alt+f1").unwrap().display(), "Ctrl+Alt+F1");
        assert_eq!(Hotkey::parse("alt+win+v").unwrap().display(), "Win+Alt+V");
        assert_eq!(Hotkey::default().display(), "Win+Alt+V");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Hotkey::parse("V").is_none()); // no modifier
        assert!(Hotkey::parse("Ctrl+Shift").is_none()); // no key
        assert!(Hotkey::parse("Ctrl+A+B").is_none()); // two keys
        assert!(Hotkey::parse("Ctrl+Banana").is_none());
        assert!(Hotkey::parse("Ctrl+F25").is_none());
    }
}
