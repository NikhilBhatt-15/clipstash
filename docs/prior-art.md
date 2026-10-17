# Prior art: how Ditto does it

Before writing clipstash I looked at the shipped Ditto binary with REA (Ghidra
12.1.4 backend). The goal was to learn which Win32 mechanisms a mature
clipboard manager relies on, not to reuse any code. This note records what was
observed and what clipstash took from it.

Target: `Ditto.exe` from `DittoPortable_3_25_113_0.zip` (GitHub release
`sabrogden/Ditto` 3.25.113.0), x86-64 MFC application.
SHA-256 `d187eb96e7c0fb15b8e2aaa38b70f10ff8aae9a4fdee3b0499b0ff0844dd7377`.

## Clipboard change notifications

- **`AddClipboardFormatListener` is not in the import table.** It appears only
  as a string. The "connect to clipboard" routine (`0x140107f30`,
  `ClipboardViewer.cpp`) checks the OS version, calls `LoadLibraryW("USER32.dll")`
  and `GetProcAddress("AddClipboardFormatListener")`, and registers its window.
  If that fails, it falls back to the pre-Vista `SetClipboardViewer` chain
  (statically imported, with `ChangeClipboardChain`) and stores the next viewer
  in the chain.
- The `CClipboardViewer` MFC message map (`0x140318010`) routes
  `WM_CLIPBOARDUPDATE` (0x31D), `WM_DRAWCLIPBOARD` (0x308), `WM_CHANGECBCHAIN`,
  `WM_TIMER`, `WM_DESTROY` and one private `WM_USER` message.
- The `WM_CLIPBOARDUPDATE` handler only logs and calls the `WM_DRAWCLIPBOARD`
  handler, so both mechanisms share one code path. That path:
  1. returns early if a registered "ignore" format is on the clipboard;
  2. otherwise restarts timer 7 (`KillTimer` + `SetTimer`) instead of reading
     immediately. The actual read happens in `WM_TIMER`;
  3. forwards `WM_DRAWCLIPBOARD` to the next viewer when it is in legacy chain mode.

**Taken for clipstash:** a debounce timer between notification and read, plus
honouring the opt-out formats (`Clipboard Viewer Ignore`,
`ExcludeClipboardContentFromMonitorProcessing`, `CanIncludeInClipboardHistory`).
**Changed:** clipstash targets Windows 10+, so it links
`AddClipboardFormatListener` directly and has no viewer-chain fallback.

## Hotkeys

- `RegisterHotKey` and `UnregisterHotKey` are imported. A small wrapper
  (`0x140148120`) registers each configured hotkey against one global window
  handle, converting stored modifier bits to `MOD_*` flags and recording
  whether registration succeeded.
- Two message maps have `WM_HOTKEY` (0x312) entries. The main one
  (`0x14014e310`) logs "On Show Ditto HotKey". If the window is already visible
  it hides it; otherwise it shows the window.

**Taken:** plain `RegisterHotKey` on the app's own window; pressing the hotkey
again while the popup is open closes it.

## Focus and pasting back

- `ExternalWindowTracker.cpp` (`0x14013dd90`) restores focus: it attaches to the
  target window's input queue with `AttachThreadInput`, calls `SetFocus` on the
  remembered child control if it isn't already focused, then detaches.
  `SetForegroundWindow`, `AllowSetForegroundWindow` and `GetForegroundWindow` are imported.
- The popup position comes from the text caret (`0x14013e080`). It tries an
  accessibility interface, then `GetGUIThreadInfo().rcCaret`, then
  `AttachThreadInput` + `GetCaretPos`.
- Paste keystrokes are synthesised in `0x1401b8100` with `SendInput` (older
  systems use `keybd_event`). A configurable delay sits between keys.

**Taken:** remember the foreground window *and* its focused child when the
popup opens. On selection, `SetForegroundWindow` → attach → `SetFocus` →
detach, then send Ctrl+V with `SendInput`. Position the popup at the caret from
`GetGUIThreadInfo`, falling back to the mouse cursor.

## Limits of this analysis

Static analysis only; Ditto was not executed. Function names are Ghidra
placeholders, because Ditto's PDB was not available. Behaviour is inferred
from decompiled control flow, log strings and message-map data. Code paths
not listed here (storage, the UI beyond the hotkey handler, network sync)
were not examined.
