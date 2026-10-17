//! Remembering the window the user was typing in, giving focus back to it,
//! and pasting into it.
//!
//! Same approach as Ditto's ExternalWindowTracker: save the foreground window
//! and its focused child when the popup opens. Afterwards call
//! SetForegroundWindow on the window, attach to its input queue and SetFocus
//! the child, so the paste lands in the same text box and not just the same
//! top-level window.

use std::mem::{size_of, zeroed};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId, Sleep};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, SetFocus, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, IsWindow, SetForegroundWindow, GUITHREADINFO,
};

#[derive(Clone, Copy)]
pub struct Target {
    pub window: HWND,
    pub focus: HWND,
    /// Text caret in screen coordinates, when the app exposes one.
    pub caret: Option<POINT>,
}

impl Target {
    pub fn capture() -> Self {
        unsafe {
            let window = GetForegroundWindow();
            let mut info: GUITHREADINFO = zeroed();
            info.cbSize = size_of::<GUITHREADINFO>() as u32;
            let tid = GetWindowThreadProcessId(window, null_mut());
            if tid == 0 || GetGUIThreadInfo(tid, &mut info) == 0 {
                return Self { window, focus: null_mut(), caret: None };
            }
            let rc = info.rcCaret;
            let caret = if !info.hwndCaret.is_null() && (rc.right > rc.left || rc.bottom > rc.top) {
                let mut pt = POINT { x: rc.left, y: rc.bottom };
                (ClientToScreen(info.hwndCaret, &mut pt) != 0).then_some(pt)
            } else {
                None
            };
            Self { window, focus: info.hwndFocus, caret }
        }
    }

    /// Gives focus back. Must be called while this process is still in the
    /// foreground (the popup is active), or Windows refuses SetForegroundWindow.
    pub fn restore(&self) -> bool {
        unsafe {
            if self.window.is_null() || IsWindow(self.window) == 0 {
                return false;
            }
            SetForegroundWindow(self.window);
            if !self.focus.is_null() && IsWindow(self.focus) != 0 {
                let ours = GetCurrentThreadId();
                let theirs = GetWindowThreadProcessId(self.window, null_mut());
                if theirs != 0 && theirs != ours && AttachThreadInput(ours, theirs, 1) != 0 {
                    SetFocus(self.focus);
                    AttachThreadInput(ours, theirs, 0);
                }
            }
            // The foreground switch completes asynchronously in the target's thread.
            for _ in 0..20 {
                if GetForegroundWindow() == self.window {
                    return true;
                }
                Sleep(10);
            }
            GetForegroundWindow() == self.window
        }
    }
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }, time: 0, dwExtraInfo: 0 },
        },
    }
}

/// Sends Ctrl+V to whatever has keyboard focus.
pub fn send_paste() {
    // Wait (briefly) for the user to let go of Shift/Alt/Win, which are often
    // still held from the hotkey. Otherwise the target would see e.g.
    // Win+Alt+Ctrl+V or Ctrl+Shift+V instead of a plain paste, or our own
    // hotkey could fire again.
    for _ in 0..75 {
        if ![VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN].into_iter().any(key_down) {
            break;
        }
        unsafe { Sleep(20) };
    }
    const VK_V: VIRTUAL_KEY = b'V' as VIRTUAL_KEY;
    let ctrl_held = key_down(VK_CONTROL);
    let mut inputs = Vec::with_capacity(4);
    if !ctrl_held {
        inputs.push(key(VK_CONTROL, false));
    }
    inputs.push(key(VK_V, false));
    inputs.push(key(VK_V, true));
    if !ctrl_held {
        inputs.push(key(VK_CONTROL, true));
    }
    unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
}
