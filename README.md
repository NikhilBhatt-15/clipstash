# clipstash

A lightweight clipboard manager for Windows. It records the text and images
you copy, including screenshots, into a local SQLite database. Press
**Win+Alt+V** to open a search popup at your text cursor, pick an entry, and
it is pasted straight into the app you were typing in.

Single native executable, no runtime, ~2 MB.

## Features

- **Matches your system theme.** Follows Windows light/dark mode and your
  accent colour, and switches live when you change them. Named themes are also
  available: Light, Dark, Nord, Dracula, Catppuccin Mocha, Solarized
  Light/Dark. Rounded corners and themed borders on Windows 11.
- **Code-aware.** Each clip is classified as code, link, path, colour or
  text.
  - Code is shown in a monospace font (Cascadia Mono or Consolas), with
    line numbers in the preview.
  - Colours get a swatch.
  - *Paste trimmed* (<kbd>Shift</kbd>+<kbd>Enter</kbd>) removes the
    indentation shared by every line, trailing spaces and blank edge lines.
    Useful when lifting a snippet out of a nested block.
  - Line endings are converted to CRLF on paste (optional).
- **Images and screenshots.** Anything copied as a picture is kept: Snipping
  Tool / Win+Shift+S, Print Screen, "Copy image" in browsers. Rows show a
  thumbnail and the preview shows the full image. Pasting puts it back as both
  a bitmap and PNG, so any app accepts it. Identical images are de-duplicated,
  and images have their own history limit (default 50) since they are larger
  than text.
- **Search** across everything; every word must match. Hits are highlighted,
  the row shows the matching line, and the preview scrolls to the first hit.
- **Pin** clips you reuse (<kbd>Ctrl</kbd>+<kbd>P</kbd>). Pinned clips stay
  at the top and are never pruned. **Delete** with
  <kbd>Shift</kbd>+<kbd>Del</kbd>.
- **Focus goes back where it was.** The popup remembers the foreground window
  and the exact control that had focus, restores both, then sends Ctrl+V.
- **Respects password managers.** Copies marked
  `ExcludeClipboardContentFromMonitorProcessing` or
  `CanIncludeInClipboardHistory = 0` are not stored (can be turned off).
- **Settings window**, from the tray icon, the gear button, or
  <kbd>Ctrl</kbd>+<kbd>,</kbd>. It covers theme, text size, preview pane,
  popup position, shortcut, paste behaviour, history size, start with
  Windows, and clearing history.

### Popup keys

| Key | Action |
|---|---|
| type | filter (images match "image", "screenshot" or a size like "1920x1080") |
| <kbd>↑</kbd> <kbd>↓</kbd> <kbd>PgUp</kbd> <kbd>PgDn</kbd>, <kbd>Ctrl</kbd>+<kbd>Home</kbd>/<kbd>End</kbd> | move |
| <kbd>Enter</kbd> / double-click | paste |
| <kbd>Shift</kbd>+<kbd>Enter</kbd> | paste trimmed (dedented) |
| <kbd>Ctrl</kbd>+<kbd>Enter</kbd> | copy only |
| <kbd>Ctrl</kbd>+<kbd>1</kbd>…<kbd>9</kbd> | paste that row |
| <kbd>Ctrl</kbd>+<kbd>P</kbd> | pin / unpin |
| <kbd>Shift</kbd>+<kbd>Del</kbd> | delete |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | settings |
| <kbd>Esc</kbd> / the shortcut again | close |

## Install

Download `clipstash.exe` from the latest release and run it. It lives in the
notification area. Turn on *Start with Windows* in Settings to launch it at
sign-in.

## Configuration

Settings are saved to `%LOCALAPPDATA%\clipstash\settings.json` as you change
them, next to `history.db`. The file is hand-editable; invalid values fall
back to defaults.

```json
{
  "theme": "system",
  "hotkey": "Win+Alt+V",
  "max_history": 500,
  "save_images": true,
  "max_images": 50,
  "font_size": 10,
  "show_preview": true,
  "position": "caret",
  "paste_on_enter": true,
  "normalize_line_endings": true,
  "respect_privacy": true,
  "start_with_windows": false
}
```

## Build

Requires Rust (MSVC toolchain) and a C compiler for the bundled SQLite. The
Visual Studio Build Tools provide both.

```
cargo build --release
cargo test
```

## How it works

| Piece | File | Mechanism |
|---|---|---|
| Change notifications | `src/main.rs` | A hidden window calls `AddClipboardFormatListener`. Each `WM_CLIPBOARDUPDATE` restarts a 75 ms timer and the read happens on `WM_TIMER`. |
| Reading/writing text | `src/clipboard.rs` | `OpenClipboard` with retries, `CF_UNICODETEXT` via `GlobalLock`, opt-out format checks. |
| History | `src/store.rs` | `clips` table with `UNIQUE(text)` upsert, `pinned` + `used_ms` ordering, prune after insert (text and images separately), `LIKE … ESCAPE` search. Images are stored as PNG plus a small thumbnail, keyed by a hash of their pixels. Versioned with `PRAGMA user_version`. |
| Images | `src/image.rs`, `src/clipboard.rs` | Reads the `PNG` clipboard format when offered (kept as-is), otherwise `CF_DIBV5`/`CF_DIB` (24/32-bit, bit-field masks, either row order), encoded to PNG. Writes `CF_DIB` + `PNG`. |
| Classification | `src/classify.rs` | Heuristics for code (line endings, indentation, symbol density, keywords, minus prose signals), links, paths, colours; dedent and CRLF transforms. |
| Rendering | `src/gfx.rs`, `src/popup.rs`, `src/settings_ui.rs` | Custom-drawn, double-buffered windows: GDI+ for antialiased shapes, GDI for ClearType text. The only stock control is the search box (an EDIT, coloured via `WM_CTLCOLOREDIT`). Per-monitor DPI aware. |
| Theme | `src/theme.rs` | `AppsUseLightTheme` and `AccentPalette` from the registry; reloaded on `WM_SETTINGCHANGE("ImmersiveColorSet")`. DWM attributes for dark frame, border and caption colour. |
| Hotkey | `src/main.rs`, `src/hotkey.rs` | `RegisterHotKey(… \| MOD_NOREPEAT)`, default Win+Alt+V: apps don't bind Win-key shortcuts, so it shadows nothing inside them. Shortcuts are stored as text (`"Ctrl+Shift+V"`). The settings recorder unregisters the old one while recording and reports conflicts. |
| Returning focus | `src/focus.rs` | `SetForegroundWindow(target)` while clipstash is still foreground, then `AttachThreadInput` + `SetFocus(focused child)`. Waits for Shift/Alt/Win to be released, then sends Ctrl+V with `SendInput`. |

The design draws on a static look at how Ditto handles the same problems; see
[docs/prior-art.md](docs/prior-art.md).

## Known limitations

- Copied files (from Explorer) and rich text formatting are not recorded;
  rich text is kept as plain text.
- Pasting into an elevated (administrator) window from a non-elevated
  clipstash is blocked by Windows (UIPI). The entry is still placed on the
  clipboard, so a manual Ctrl+V works.

## License

MIT
