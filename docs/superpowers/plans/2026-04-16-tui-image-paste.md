# shore-tui Clipboard Image Paste Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `ctrl+v` to shore-tui that reads a binary image from the system clipboard, encodes it as PNG to a temp file, and queues it as a pending image attachment using the existing send pipeline.

**Architecture:** New `clipboard.rs` module wraps `arboard` with PNG encoding and temp-file write. A new `Action::PasteImage` variant is bound to a global `ctrl+v` handler that runs the clipboard read on `tokio::task::spawn_blocking` with a 1.5 s timeout. Paste-origin temp files are tracked on `App` and removed on TUI shutdown. No protocol or daemon changes — paste reuses the existing `pending_images: Vec<String>` flow.

**Tech Stack:** Rust 2021, ratatui 0.29, crossterm 0.28, tokio (rt-multi-thread), `arboard` 3.x (new dep, defaults: `image-data` + `wayland-data-control`), `image` 0.25 (already present, `png` feature).

**Spec:** `docs/superpowers/specs/2026-04-16-tui-image-paste-design.md`

---

## File Structure

| File                              | Status   | Responsibility                                                    |
|-----------------------------------|----------|-------------------------------------------------------------------|
| `shore-tui/Cargo.toml`            | modify   | Add `arboard = "3"` dependency.                                   |
| `shore-tui/src/clipboard.rs`      | **new**  | `ClipboardError`, RGBA→PNG encode, temp path, orchestrator.       |
| `shore-tui/src/main.rs`           | modify   | `mod clipboard;`, `Action::PasteImage` handler, shutdown cleanup. |
| `shore-tui/src/input.rs`          | modify   | `Action::PasteImage` variant; global `ctrl+v` binding + tests.    |
| `shore-tui/src/app.rs`            | modify   | `pub paste_temp_paths: Vec<PathBuf>` field + Default init.        |

---

## Task 1: Add arboard dependency

**Files:**
- Modify: `shore-tui/Cargo.toml`

- [ ] **Step 1: Add the dependency**

Open `shore-tui/Cargo.toml`. Find the `[dependencies]` section. Append after the existing `image = ...` line:

```toml
arboard = "3"
```

(arboard 3.x enables `image-data` and `wayland-data-control` by default, which is what we want.)

- [ ] **Step 2: Verify the workspace builds**

Run from repo root:

```bash
cargo check -p shore-tui
```

Expected: compiles cleanly. arboard fetches and resolves; no errors.

- [ ] **Step 3: Commit**

```bash
git add shore-tui/Cargo.toml Cargo.lock
git commit -m "build(tui): add arboard dependency for clipboard image paste"
```

---

## Task 2: Create clipboard module skeleton + PNG encode helper (TDD)

**Files:**
- Create: `shore-tui/src/clipboard.rs`

This task implements the pure-function PNG encoder and its unit test. The arboard call is added in Task 3 so this task stays unit-testable without a real clipboard.

- [ ] **Step 1: Write the failing test**

Create `shore-tui/src/clipboard.rs` with:

```rust
//! System clipboard image paste support.
//!
//! Reads RGBA image data from the system clipboard via arboard, encodes
//! it as PNG, and writes it to a temp file. The resulting path is fed
//! into shore-tui's existing pending-image flow.

use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum ClipboardError {
    NoImage,
    ClipboardUnavailable(String),
    EncodeFailed(String),
    WriteFailed(io::Error),
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClipboardError::NoImage => write!(f, "clipboard has no image"),
            ClipboardError::ClipboardUnavailable(e) => write!(f, "clipboard unavailable: {e}"),
            ClipboardError::EncodeFailed(e) => write!(f, "failed to encode pasted image: {e}"),
            ClipboardError::WriteFailed(e) => write!(f, "failed to write paste temp: {e}"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// Encode an RGBA8 buffer as PNG bytes.
fn encode_rgba_to_png(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>, ClipboardError> {
    let buffer: image::RgbaImage = image::ImageBuffer::from_raw(width, height, rgba)
        .ok_or_else(|| ClipboardError::EncodeFailed("buffer dimensions invalid".into()))?;
    let mut out = Vec::new();
    buffer
        .write_to(&mut io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| ClipboardError::EncodeFailed(e.to_string()))?;
    Ok(out)
}

/// Generate a unique temp-file path under the OS temp dir.
fn fresh_temp_path() -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut path = std::env::temp_dir();
    path.push(format!("shore_paste_{ts}.png"));
    if path.exists() {
        // Vanishingly rare at ms resolution; one retry with a counter.
        for n in 1..1000 {
            let mut alt = std::env::temp_dir();
            alt.push(format!("shore_paste_{ts}_{n}.png"));
            if !alt.exists() {
                return alt;
            }
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_rgba_to_png_roundtrip() {
        // 2x1 image: red pixel, then green pixel.
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255];
        let png = encode_rgba_to_png(2, 1, rgba.clone()).expect("encode");
        let decoded = image::load_from_memory(&png).expect("decode");
        assert_eq!(decoded.width(), 2);
        assert_eq!(decoded.height(), 1);
        let round = decoded.into_rgba8().into_raw();
        assert_eq!(round, rgba);
    }

    #[test]
    fn temp_path_format() {
        let p = fresh_temp_path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("shore_paste_"), "name was {name}");
        assert!(name.ends_with(".png"), "name was {name}");
        assert_eq!(p.parent().unwrap(), std::env::temp_dir().as_path());
    }
}
```

- [ ] **Step 2: Register the module so tests can compile**

Open `shore-tui/src/main.rs`. The existing `mod` declarations are at the top:

```rust
mod app;
mod connection;
mod images;
mod input;
mod markdown;
mod ui;
```

Add `clipboard` immediately after `app` so the list stays alphabetical:

```rust
mod app;
mod clipboard;
mod connection;
mod images;
mod input;
mod markdown;
mod ui;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p shore-tui clipboard::tests
```

Expected: both tests pass (`encode_rgba_to_png_roundtrip`, `temp_path_format`).

- [ ] **Step 4: Commit**

```bash
git add shore-tui/src/clipboard.rs shore-tui/src/main.rs
git commit -m "feat(tui): add clipboard module with PNG encode helper"
```

---

## Task 3: Add the clipboard read orchestrator

**Files:**
- Modify: `shore-tui/src/clipboard.rs`

This is the function that runs inside `spawn_blocking`. It cannot be unit-tested without a real clipboard; live verification covers it in Task 7.

- [ ] **Step 1: Add the orchestrator function**

In `shore-tui/src/clipboard.rs`, add this function above the `#[cfg(test)]` block:

```rust
/// Read an image from the system clipboard, encode as PNG, and write to
/// a temp file. Returns the path on success.
///
/// Synchronous and blocking — designed to be invoked via
/// `tokio::task::spawn_blocking`. arboard's Wayland backend can briefly
/// stall during the clipboard handoff; the caller is expected to wrap
/// this in a timeout.
pub fn read_image_to_temp() -> Result<PathBuf, ClipboardError> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| ClipboardError::ClipboardUnavailable(e.to_string()))?;

    let img = match clipboard.get_image() {
        Ok(img) => img,
        Err(arboard::Error::ContentNotAvailable) => return Err(ClipboardError::NoImage),
        Err(e) => return Err(ClipboardError::ClipboardUnavailable(e.to_string())),
    };

    let png = encode_rgba_to_png(img.width as u32, img.height as u32, img.bytes.into_owned())?;

    let path = fresh_temp_path();
    std::fs::write(&path, &png).map_err(ClipboardError::WriteFailed)?;
    Ok(path)
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check -p shore-tui
```

Expected: compiles cleanly. The existing tests still pass.

- [ ] **Step 3: Re-run unit tests to ensure nothing regressed**

```bash
cargo test -p shore-tui clipboard
```

Expected: both `encode_rgba_to_png_roundtrip` and `temp_path_format` still pass.

- [ ] **Step 4: Commit**

```bash
git add shore-tui/src/clipboard.rs
git commit -m "feat(tui): add clipboard read_image_to_temp orchestrator"
```

---

## Task 4: Add `Action::PasteImage` and the `ctrl+v` binding (TDD)

**Files:**
- Modify: `shore-tui/src/input.rs`

- [ ] **Step 1: Write the failing tests**

Open `shore-tui/src/input.rs`. Find the `#[cfg(test)] mod tests` block (around line 727). Add these three tests at the end of the module, just before the closing `}`:

```rust
    #[test]
    fn ctrl_v_returns_paste_image_in_insert_mode() {
        let mut app = App::default();
        app.input.mode = InputMode::Insert;
        let action = handle_key(
            &mut app,
            make_key(KeyModifiers::CONTROL, KeyCode::Char('v')),
        );
        assert!(matches!(action, Action::PasteImage));
    }

    #[test]
    fn ctrl_v_returns_paste_image_in_normal_mode() {
        let mut app = App::default();
        app.input.mode = InputMode::Normal;
        let action = handle_key(
            &mut app,
            make_key(KeyModifiers::CONTROL, KeyCode::Char('v')),
        );
        assert!(matches!(action, Action::PasteImage));
    }

    #[test]
    fn ctrl_v_returns_paste_image_in_command_mode() {
        let mut app = App::default();
        app.input.enter_command_mode();
        let action = handle_key(
            &mut app,
            make_key(KeyModifiers::CONTROL, KeyCode::Char('v')),
        );
        assert!(matches!(action, Action::PasteImage));
    }
```

- [ ] **Step 2: Run them and confirm they fail**

```bash
cargo test -p shore-tui input::tests::ctrl_v_returns_paste_image
```

Expected: compile error — `Action::PasteImage` does not exist yet.

- [ ] **Step 3: Add the `Action::PasteImage` variant**

In `shore-tui/src/input.rs`, find the `Action` enum (currently around line 11):

```rust
pub enum Action {
    None,
    Send(ConnCommand),
    /// Send multiple commands at once.
    SendMulti(Vec<ConnCommand>),
    Quit,
    Redraw,
    OpenInEditor,
    /// Open external file picker to select an image.
    PickImage(Option<String>),
}
```

Append the new variant at the end:

```rust
pub enum Action {
    None,
    Send(ConnCommand),
    /// Send multiple commands at once.
    SendMulti(Vec<ConnCommand>),
    Quit,
    Redraw,
    OpenInEditor,
    /// Open external file picker to select an image.
    PickImage(Option<String>),
    /// Read an image from the system clipboard and attach it.
    PasteImage,
}
```

- [ ] **Step 4: Add the global `ctrl+v` binding**

In the same file, find `handle_key` (around line 42). The "Global shortcuts" match block currently looks like:

```rust
    // Global shortcuts (work in any mode)
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            if app.stream.active {
                app.stream.reset();
                return Action::Send(ConnCommand::Send(ClientMessage::Cancel(Cancel {})));
            }
            return Action::Quit;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('q')) => return Action::Quit,
        _ => {}
    }
```

Add the `ctrl+v` arm immediately after the `ctrl+q` arm:

```rust
    // Global shortcuts (work in any mode)
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            if app.stream.active {
                app.stream.reset();
                return Action::Send(ConnCommand::Send(ClientMessage::Cancel(Cancel {})));
            }
            return Action::Quit;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('q')) => return Action::Quit,
        (KeyModifiers::CONTROL, KeyCode::Char('v')) => return Action::PasteImage,
        _ => {}
    }
```

- [ ] **Step 5: Run the tests and confirm they pass**

```bash
cargo test -p shore-tui input::tests
```

Expected: all `input::tests` pass, including the three new `ctrl_v_*` tests. Existing tests (`ctrl_c_quits`, `insert_mode_*`, `normal_mode_*`, `esc_returns_to_normal`, `scroll_shortcuts`, `shift_enter_inserts_newline`, `character_command_*`, `delete_command_*`) still pass.

- [ ] **Step 6: Commit**

```bash
git add shore-tui/src/input.rs
git commit -m "feat(tui): bind ctrl+v to Action::PasteImage in all modes"
```

---

## Task 5: Add `paste_temp_paths` field to `App`

**Files:**
- Modify: `shore-tui/src/app.rs`

- [ ] **Step 1: Add the field**

Open `shore-tui/src/app.rs`. Find the `App` struct (around line 381). Find this line:

```rust
    /// Images queued for attachment to the next outgoing message.
    pub pending_images: Vec<String>,
```

Add immediately after it:

```rust
    /// Temp-file paths for paste-origin images, removed on TUI shutdown.
    pub paste_temp_paths: Vec<std::path::PathBuf>,
```

- [ ] **Step 2: Initialize it in `Default`**

In the same file, find the `impl Default for App` block (around line 416). Find this line:

```rust
            pending_images: Vec::new(),
```

Add immediately after it:

```rust
            paste_temp_paths: Vec::new(),
```

- [ ] **Step 3: Verify it compiles and existing tests still pass**

```bash
cargo check -p shore-tui
cargo test -p shore-tui
```

Expected: clean compile and all tests still pass.

- [ ] **Step 4: Commit**

```bash
git add shore-tui/src/app.rs
git commit -m "feat(tui): add paste_temp_paths field for clipboard-paste cleanup"
```

---

## Task 6: Wire `Action::PasteImage` and shutdown cleanup in `main.rs`

**Files:**
- Modify: `shore-tui/src/main.rs`

- [ ] **Step 1: Add the `Action::PasteImage` handler**

Open `shore-tui/src/main.rs`. Find the action-dispatch match in `run_tui` (around line 320). It currently looks like:

```rust
                        Action::PickImage(start_dir) => {
                            match pick_image(&mut terminal, start_dir.as_deref()) {
                                Ok(paths) if paths.is_empty() => {
                                    // User cancelled — no status needed
                                }
                                Ok(paths) => {
                                    let count = paths.len();
                                    app.pending_images.extend(paths);
                                    app.set_status(format!(
                                        "attached {count} image(s) ({} pending)",
                                        app.pending_images.len()
                                    ));
                                }
                                Err(e) => {
                                    app.set_status(format!("image picker: {e}"));
                                }
                            }
                        }
                        Action::Redraw | Action::None => {}
```

Insert the new arm immediately after `Action::PickImage(...)` and before `Action::Redraw | Action::None`:

```rust
                        Action::PasteImage => {
                            let result = tokio::time::timeout(
                                Duration::from_millis(1500),
                                tokio::task::spawn_blocking(clipboard::read_image_to_temp),
                            )
                            .await;
                            match result {
                                Ok(Ok(Ok(path))) => {
                                    let path_str = path.to_string_lossy().into_owned();
                                    app.pending_images.push(path_str);
                                    app.paste_temp_paths.push(path);
                                    app.set_status(format!(
                                        "pasted image ({} pending)",
                                        app.pending_images.len()
                                    ));
                                }
                                Ok(Ok(Err(e))) => app.set_status(e.to_string()),
                                Ok(Err(_join)) => app.set_status("paste task panicked"),
                                Err(_elapsed) => app.set_status("clipboard read timed out"),
                            }
                        }
```

- [ ] **Step 2: Add the shutdown cleanup**

In the same file, find the shutdown block (around line 364):

```rust
    info!("TUI exiting");
    // Save preferences and shutdown
    save_prefs(&app);
    let _ = cmd_tx.send(ConnCommand::Shutdown).await;

    // Restore terminal
    io::stdout().execute(DisableBracketedPaste)?;
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
```

Insert the cleanup loop between the shutdown send and terminal restore:

```rust
    info!("TUI exiting");
    // Save preferences and shutdown
    save_prefs(&app);
    let _ = cmd_tx.send(ConnCommand::Shutdown).await;

    // Best-effort cleanup of paste-origin temp files.
    for path in &app.paste_temp_paths {
        let _ = std::fs::remove_file(path);
    }

    // Restore terminal
    io::stdout().execute(DisableBracketedPaste)?;
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
```

- [ ] **Step 3: Type-check and run the full test suite**

```bash
cargo check -p shore-tui
cargo test -p shore-tui
```

Expected: clean compile, all tests pass.

- [ ] **Step 4: Build the workspace to surface any cross-crate breakage**

```bash
cargo build --workspace
```

Expected: clean build of the whole workspace.

- [ ] **Step 5: Commit**

```bash
git add shore-tui/src/main.rs
git commit -m "feat(tui): handle Action::PasteImage and clean paste temps on exit"
```

---

## Task 7: Live verification (mandatory per CLAUDE.md)

This task does not produce code — it confirms the feature works against a real Wayland clipboard before declaring done. It must be run by a human (or by a session with display access); skip with a status note if no display is reachable.

**Prereqs:** Wayland session, `wl-clipboard` installed (`wl-copy`, `wl-paste`), a sample image file (e.g., `~/Pictures/test.png`), and a running shore daemon.

- [ ] **Step 1: Build a release binary**

```bash
cargo build --release -p shore-tui
```

- [ ] **Step 2: Verify "image on clipboard" → attaches**

In one terminal:

```bash
wl-copy --type image/png < ~/Pictures/test.png
```

In another terminal, run the TUI:

```bash
./target/release/shore-tui
```

Press `ctrl+v`. Expected:
- Status line shows `pasted image (1 pending)`.
- A file matching `/tmp/shore_paste_*.png` exists (`ls /tmp/shore_paste_*.png` in a third terminal).

Type a short message and press Enter. Expected:
- The image renders inline in the conversation (just like `:image`).
- The daemon receives it (visible in daemon logs as `ImageUpload`).

- [ ] **Step 3: Verify "no image on clipboard" → graceful status**

```bash
wl-copy "just text"
```

In the TUI, press `ctrl+v`. Expected:
- Status line shows `clipboard has no image`.
- No new files in `/tmp/shore_paste_*.png`.
- TUI remains responsive (no UI freeze).

- [ ] **Step 4: Verify "empty clipboard" → graceful status**

```bash
wl-copy --clear
```

In the TUI, press `ctrl+v`. Expected: status line shows `clipboard has no image` (or `clipboard unavailable: ...` depending on compositor behavior — both are acceptable; it must be a clear, non-fatal message).

- [ ] **Step 5: Verify cleanup on exit**

While the TUI is running, confirm the paste temp file from Step 2 still exists. Quit cleanly with `ctrl+q` or `:quit`. After the TUI exits, run:

```bash
ls /tmp/shore_paste_*.png 2>/dev/null
```

Expected: no matching files.

- [ ] **Step 6: Verify all three modes (sanity check)**

In a fresh TUI session with an image on the clipboard (`wl-copy --type image/png < ~/Pictures/test.png`):
- Press `Esc` to enter normal mode → press `ctrl+v` → status updates.
- Press `i` to enter insert mode → press `ctrl+v` → status updates again (count increments).
- Press `Esc`, then `:` to enter command mode → press `ctrl+v` → status updates.

Expected: each press attaches a fresh paste; pending count increments to 3.

- [ ] **Step 7: Update the TODO**

If all live checks pass, remove the entry from `TODO/TODO.md` and delete the source file:

```bash
git rm TODO/features/2026-04-12-image-paste-support.md
```

In `TODO/TODO.md`, delete the line:

```
- [ ] image paste support [./features/2026-04-12-image-paste-support.md]
```

Stage and commit:

```bash
git add TODO/TODO.md
git commit -m "chore(todo): mark image-paste TODO complete"
```

- [ ] **Step 8: Update DECISIONS.md and ARCHITECTURE.md per CLAUDE.md**

Append a brief entry to `docs/DECISIONS.md`:

```markdown
## 2026-04-16 — TUI clipboard image paste

Added `ctrl+v` binding in shore-tui that reads a binary image from the
system clipboard via `arboard`, encodes it as PNG to a temp file, and
queues it through the existing `pending_images` flow. Symmetric with the
`:image` file-picker path; no protocol or daemon changes. Wayland
support requires `wlr-data-control` (sway/hyprland/wayfire/river: yes;
GNOME mutter: limited). 1.5 s timeout protects the UI from a wedged
clipboard handoff.
```

Append to `docs/ARCHITECTURE.md` under the shore-tui section (or add a new line if no such section exists):

```markdown
- shore-tui depends on `arboard` for system clipboard image reads (paste).
```

Commit:

```bash
git add docs/DECISIONS.md docs/ARCHITECTURE.md
git commit -m "docs: record TUI clipboard image paste"
```
