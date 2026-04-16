# Spec: shore-tui clipboard image paste

**Date:** 2026-04-16
**Status:** Approved, ready for implementation plan
**Source TODO:** `TODO/features/2026-04-12-image-paste-support.md`

## Goal

Let the user press `ctrl+v` in shore-tui to attach a binary image from the system clipboard (screenshots, browser "copy image", etc.) to the next outgoing message, mirroring the existing `:image` / file-picker flow.

## Non-goals

- Pasting file paths from bracketed-paste text. (Possible future feature; out of scope here.)
- Pasting multiple images in one keystroke. `arboard` reads one image at a time; repeating `ctrl+v` is acceptable.
- Windows support. Linux + macOS only.
- Robust cleanup across hard kills. The OS cleans `temp_dir` eventually; that's enough.

## Background

shore-tui already has a working image-attachment pipeline:

- `:image` (no arg) → external file picker (yazi/fzf), populates `app.pending_images: Vec<String>`.
- `:image <path>` → direct attach.
- On Enter (insert mode), each path in `pending_images` is read from disk, base64-encoded, and shipped over the wire as `ImageUpload` (raw bytes) + `ImageRef` (path + base64 data).

Bracketed paste is enabled in `main.rs` and currently inserts pasted *text* into the input. It cannot deliver binary image data — terminal emulators don't carry image bytes through bracketed-paste sequences.

## Design

### Architecture

Single user-driven flow, no new long-lived state. Files touched:

| File                                  | Change                                                                                                  |
|---------------------------------------|---------------------------------------------------------------------------------------------------------|
| `shore-tui/src/clipboard.rs` *(new)*  | Wraps `arboard` clipboard read + PNG encode + temp-file write. Returns `Result<PathBuf, ClipboardError>`. |
| `shore-tui/src/input.rs`              | New `Action::PasteImage` variant; `ctrl+v` binding handled globally before mode dispatch.                |
| `shore-tui/src/app.rs`                | New `paste_temp_paths: Vec<PathBuf>` for cleanup tracking.                                              |
| `shore-tui/src/main.rs`               | Handles `Action::PasteImage` via `spawn_blocking` + `timeout`; cleans up paste temp files on shutdown.   |
| `shore-tui/Cargo.toml`                | Adds `arboard = { version = "3", default-features = false, features = ["wayland-data-control", "image-data"] }`. |

No protocol changes. No daemon changes. Paste produces the same `pending_images` entries as `:image`, so the existing send code is reused unchanged.

### Components

#### `clipboard::read_image_to_temp() -> Result<PathBuf, ClipboardError>`

Synchronous, designed to run inside `tokio::task::spawn_blocking`.

1. `arboard::Clipboard::new()?.get_image()?` → `ImageData { width, height, bytes }` (RGBA8).
2. Encode to PNG via the existing `image` crate: `image::ImageBuffer::from_raw(w, h, bytes)` → `write_to(cursor, ImageFormat::Png)`.
3. Write to `std::env::temp_dir()/shore_paste_<unix_ts_ms>.png`. On collision (vanishingly rare at ms resolution), append `_<n>` and retry once.
4. Return the `PathBuf`.

`ClipboardError` enum:
- `NoImage` — clipboard has no image (e.g., text-only or empty).
- `ClipboardUnavailable(String)` — `arboard` initialization failed (no display server, etc.).
- `EncodeFailed(String)` — PNG encode failed (should be unreachable with valid RGBA, but mapped for completeness).
- `WriteFailed(io::Error)` — disk full, permissions, etc.

Each variant has a `Display` impl producing the user-facing status string (see Error handling).

#### `Action::PasteImage`

New variant on the existing `Action` enum in `input.rs`. Bound to `(KeyModifiers::CONTROL, KeyCode::Char('v'))` in `handle_key` **before** the mode-dispatch match (mirrors `ctrl+c` and `ctrl+q` global handling), so the binding works in normal, insert, and command modes.

Verified there is no existing `ctrl+v` binding in any mode — no collision.

#### Paste handler in `main.rs::run_tui`

```text
Action::PasteImage =>
    match timeout(Duration::from_millis(1500),
                  spawn_blocking(clipboard::read_image_to_temp)).await {
        Ok(Ok(Ok(path))) => {
            app.pending_images.push(path.to_string_lossy().into_owned());
            app.paste_temp_paths.push(path);
            app.set_status(format!("pasted image ({} pending)", app.pending_images.len()));
        }
        Ok(Ok(Err(e))) => app.set_status(e.to_string()),
        Ok(Err(_join_err)) => app.set_status("paste task panicked"),
        Err(_timeout) => app.set_status("clipboard read timed out"),
    }
```

No alt-screen exit, no `terminal.clear()` — paste is visually instant.

#### Cleanup

In the `run_tui` shutdown block (after the event loop exits, before `disable_raw_mode`/`LeaveAlternateScreen`), iterate `app.paste_temp_paths` and `let _ = std::fs::remove_file(p)` each. Best-effort; ignore failures (file already gone is normal).

### Data flow

```
ctrl+v keypress
    └── Event::Key → input::handle_key → Action::PasteImage
                                              │
        main.rs run_tui loop ─────────────────┘
            └── timeout(1500ms, spawn_blocking(read_image_to_temp))
                    └── arboard.get_image() → RGBA bytes
                            └── image crate → PNG bytes → write to temp_dir
                                    └── return PathBuf
            └── app.pending_images.push(path)
            └── app.paste_temp_paths.push(path)
            └── set_status("pasted image (N pending)")

(later) Enter pressed → existing send code reads file, base64s, ships ImageUpload
(on exit) main.rs cleanup loop → remove_file for each paste_temp_paths entry
```

### Error handling

| Failure                    | Status message                          | Notes                                        |
|----------------------------|-----------------------------------------|----------------------------------------------|
| Clipboard has no image     | `clipboard has no image`                | Most common case (text-only clipboard).      |
| `arboard` init failed      | `clipboard unavailable: <reason>`       | No display server, etc.                      |
| PNG encode failed          | `failed to encode pasted image`         | Should be unreachable.                       |
| Temp file write failed     | `failed to write paste temp: <io err>`  | Disk full, permissions.                      |
| 1.5s timeout               | `clipboard read timed out`              | Wedged Wayland compositor / clipboard manager. |
| `spawn_blocking` panic     | `paste task panicked`                   | Defensive; should never fire.                |

All errors are non-fatal status updates. No log noise on the empty-clipboard path (it's an expected user case).

### Testing

Per `CLAUDE.md` testing policy, this is upstream of `shore-llm-client`, so unit tests with trait doubles are appropriate. Live verification against a real clipboard is the canonical "done" gate.

**Unit tests in `clipboard.rs`** (no real clipboard access):
- `encode_rgba_to_png_roundtrip` — feed known RGBA bytes to the PNG-encoding helper, decode the PNG back, assert dimensions and pixel match.
- `temp_path_format` — assert the generated filename matches `shore_paste_*.png` and lives under `temp_dir()`.

**Unit tests in `input.rs`:**
- `ctrl_v_returns_paste_image_action_in_insert_mode`
- `ctrl_v_returns_paste_image_action_in_normal_mode`
- `ctrl_v_returns_paste_image_action_in_command_mode`

**Live verification (mandatory before declaring done):**
- Wayland: `wl-copy --type image/png < some.png`, run shore-tui, press `ctrl+v`, send → confirm image arrives at the daemon and renders. Repeat with `grim - | wl-copy` (fresh screenshot).
- Empty clipboard → press `ctrl+v` → status shows `clipboard has no image`.
- Text-only clipboard → press `ctrl+v` → status shows `clipboard has no image`.
- Inspect `/tmp/shore_paste_*.png` after a session — files exist during runtime, are gone after a clean exit.

## Open questions

None.

## Risks

- **Wayland reliability.** `arboard`'s Wayland backend (via `wayland-data-control`) requires the compositor to support the `wlr-data-control` protocol. Most modern compositors (sway, river, hyprland, wayfire) do; GNOME's mutter does not (until very recently). Mitigation: 1.5s timeout prevents UI hangs; status surfaces the failure clearly.
- **Cargo dep weight.** `arboard` pulls in `wl-clipboard-rs` and X11 bindings. Acceptable cost — image paste is a real UX win and the deps are well-established.
