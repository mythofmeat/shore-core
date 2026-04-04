# Fullscreen Image Viewer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fullscreen image viewer to the TUI, triggered by `o` and scroll-position-aware, with `j`/`k` navigation between images.

**Architecture:** Store pixel dimensions in `TransmittedImage` so we can recompute cell dimensions at any size. Build an image index during the conversation rendering pass that maps images to their line positions. When `o` is pressed, find the nearest image to the viewport center, enter fullscreen mode. Fullscreen renders as a ratatui overlay with placeholder cells at full terminal size, plus a dimmed status bar. Fullscreen input handling is a separate branch in `handle_key` that intercepts before mode dispatch.

**Tech Stack:** Rust, ratatui, crossterm, kitty graphics protocol

---

### Task 1: Store pixel dimensions in TransmittedImage

**Files:**
- Modify: `shore-tui/src/images.rs:14-18` (struct), `shore-tui/src/images.rs:178-180` (ensure_transmitted insert), `shore-tui/src/images.rs:216-218` (ensure_transmitted_from_b64 insert)

- [ ] **Step 1: Add `pw` and `ph` fields to `TransmittedImage`**

In `shore-tui/src/images.rs`, change the struct (line 14):

```rust
/// An image that has been transmitted to the terminal and is ready for display.
pub struct TransmittedImage {
    pub id: KittyImageId,
    pub cols: u16,
    pub rows: u16,
    /// Original pixel width of the source image.
    pub pw: u32,
    /// Original pixel height of the source image.
    pub ph: u32,
}
```

- [ ] **Step 2: Update `ensure_transmitted` to store pixel dimensions**

In `shore-tui/src/images.rs`, the cache insert in `ensure_transmitted` (~line 178-179):

```rust
self.cache
    .insert(path.to_string(), TransmittedImage { id, cols, rows, pw, ph });
```

- [ ] **Step 3: Update `ensure_transmitted_from_b64` to store pixel dimensions**

In `shore-tui/src/images.rs`, the cache insert in `ensure_transmitted_from_b64` (~line 216-217):

```rust
self.cache
    .insert(key.to_string(), TransmittedImage { id, cols, rows, pw, ph });
```

- [ ] **Step 4: Update tests that construct `TransmittedImage` directly**

In `shore-tui/src/images.rs`, search for `TransmittedImage {` in tests. The `placeholder_lines_correct_dimensions` test (~line 531) constructs one — add `pw: 160, ph: 80` (arbitrary, not used by that test).

Also update the three `calculate_cells_*` tests that construct `ImageCache` directly — those don't construct `TransmittedImage` so they should be fine.

- [ ] **Step 5: Add `calculate_cells` as a public method**

Currently `calculate_cells` is private. We'll need it from `ui.rs` to compute fullscreen dimensions. In `shore-tui/src/images.rs`, change line 237:

```rust
pub fn calculate_cells(&self, pw: u32, ph: u32, max_cols: u16, max_rows: u16) -> (u16, u16) {
```

- [ ] **Step 6: Build to verify**

Run: `cargo build -p shore-tui`
Expected: compiles cleanly

- [ ] **Step 7: Run tests**

Run: `cargo test -p shore-tui`
Expected: all pass

- [ ] **Step 8: Commit**

```bash
git add shore-tui/src/images.rs
git commit -m "feat(tui): store pixel dimensions in TransmittedImage and expose calculate_cells"
```

---

### Task 2: Add ImageEntry struct and fullscreen state to App

**Files:**
- Modify: `shore-tui/src/app.rs:362-420`

- [ ] **Step 1: Add `ImageEntry` struct before `App`**

In `shore-tui/src/app.rs`, add before the `App` struct (before line 362):

```rust
/// An image in the conversation, with its position in the rendered line list.
#[derive(Clone, Debug)]
pub struct ImageEntry {
    /// Cache key (image path).
    pub path: String,
    /// Display name for the status bar.
    pub display_name: String,
    /// Line index in the conversation lines vec where this image starts.
    pub line: usize,
}
```

- [ ] **Step 2: Add `image_index` and `fullscreen` fields to `App`**

In `shore-tui/src/app.rs`, add after `editing_ref` in the `App` struct:

```rust
/// Index of all rendered images with their line positions, rebuilt each frame.
pub image_index: Vec<ImageEntry>,
/// When set, the fullscreen image viewer is active showing this image index.
pub fullscreen: Option<usize>,
```

And in `Default`:

```rust
image_index: Vec::new(),
fullscreen: None,
```

- [ ] **Step 3: Build to verify**

Run: `cargo build -p shore-tui`
Expected: compiles cleanly

- [ ] **Step 4: Commit**

```bash
git add shore-tui/src/app.rs
git commit -m "feat(tui): add ImageEntry and fullscreen state to App"
```

---

### Task 3: Populate image_index during rendering

**Files:**
- Modify: `shore-tui/src/ui.rs:584-623` (render_images), `shore-tui/src/ui.rs:413` and `shore-tui/src/ui.rs:451` (call sites)

- [ ] **Step 1: Update `render_images` to accept and populate an image index**

In `shore-tui/src/ui.rs`, replace the `render_images` function (lines 584-623) with:

```rust
/// Render image entries — kitty placeholders when available, text fallback otherwise.
/// Populates `index` with the line position of each transmitted image.
fn render_images(
    lines: &mut Vec<Line<'static>>,
    img_refs: &[shore_protocol::types::ImageRef],
    cache: &images::ImageCache,
    show_inline: bool,
    index: &mut Vec<crate::app::ImageEntry>,
) {
    if img_refs.is_empty() {
        return;
    }

    // Blank line before images for visual separation
    lines.push(Line::from(""));

    for img in img_refs {
        // Extract display name: caption, filename, or full path
        let display = img.caption.as_deref().unwrap_or_else(|| {
            std::path::Path::new(&img.path)
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or(&img.path)
        });

        if show_inline {
            if let Some(transmitted) = cache.get(&img.path) {
                lines.push(Line::from(Span::styled(
                    format!("  [{display}]"),
                    Style::default().fg(Color::Magenta),
                )));
                let img_start_line = lines.len();
                lines.extend(images::placeholder_lines(transmitted));
                index.push(crate::app::ImageEntry {
                    path: img.path.clone(),
                    display_name: display.to_string(),
                    line: img_start_line,
                });
                continue;
            }
        }

        // Text fallback (no kitty, or inline images toggled off)
        lines.push(Line::from(Span::styled(
            format!("  [image: {display}]"),
            Style::default().fg(Color::Magenta),
        )));
    }
}
```

- [ ] **Step 2: Update both call sites and wire up `image_index`**

In `draw_conversation`, at the top (after `let mut lines: Vec<Line<'static>> = Vec::new();` on line 351), add:

```rust
let mut image_index: Vec<crate::app::ImageEntry> = Vec::new();
```

Update the User entry call site (~line 413):

```rust
render_images(&mut lines, images, &app.image_cache, app.show_images, &mut image_index);
```

Update the Assistant entry call site (~line 451):

```rust
render_images(&mut lines, images, &app.image_cache, app.show_images, &mut image_index);
```

After `squeeze_blank_lines(&mut lines);` (~line 538), add:

```rust
app.image_index = image_index;
```

- [ ] **Step 3: Build to verify**

Run: `cargo build -p shore-tui`
Expected: compiles cleanly

- [ ] **Step 4: Commit**

```bash
git add shore-tui/src/ui.rs
git commit -m "feat(tui): populate image_index during conversation rendering"
```

---

### Task 4: Add `draw_fullscreen_image` function

**Files:**
- Modify: `shore-tui/src/ui.rs` (add new function + call from `draw`)
- Modify: `shore-tui/src/images.rs` (add `placeholder_lines_at` function)

- [ ] **Step 1: Add `placeholder_lines_at` to `images.rs`**

In `shore-tui/src/images.rs`, after the existing `placeholder_lines` function (~line 281), add:

```rust
/// Generate placeholder lines for an image at arbitrary cell dimensions.
/// Used for fullscreen display where dimensions differ from the cached inline size.
pub fn placeholder_lines_at(id: KittyImageId, cols: u16, rows: u16) -> Vec<Line<'static>> {
    let style = id_to_style(id);
    let mut lines = Vec::with_capacity(rows as usize);
    for row in 0..rows {
        let mut text = String::with_capacity(cols as usize * 12);
        for col in 0..cols {
            text.push('\u{2800}');
            text.push(diacritic(row as u8));
            text.push(diacritic(col as u8));
        }
        lines.push(Line::from(Span::styled(text, style)));
    }
    lines
}
```

- [ ] **Step 2: Add `draw_fullscreen_image` to `ui.rs`**

In `shore-tui/src/ui.rs`, add a new function after `draw_conversation` (after line 581):

```rust
/// Render the fullscreen image viewer overlay.
fn draw_fullscreen_image(frame: &mut Frame, app: &App, area: Rect) {
    let idx = match app.fullscreen {
        Some(i) if i < app.image_index.len() => i,
        _ => return,
    };
    let entry = &app.image_index[idx];
    let transmitted = match app.image_cache.get(&entry.path) {
        Some(t) => t,
        None => return,
    };

    // Layout: image area + 1-row status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // image
            Constraint::Length(1), // status bar
        ])
        .split(area);

    let img_area = chunks[0];
    let status_area = chunks[1];

    // Compute fullscreen cell dimensions preserving aspect ratio
    let (fs_cols, fs_rows) =
        app.image_cache
            .calculate_cells(transmitted.pw, transmitted.ph, img_area.width, img_area.height);

    // Center the image vertically in the image area
    let v_pad = img_area.height.saturating_sub(fs_rows) / 2;
    let mut img_lines: Vec<Line<'static>> = Vec::new();
    for _ in 0..v_pad {
        img_lines.push(Line::from(""));
    }
    img_lines.extend(images::placeholder_lines_at(transmitted.id, fs_cols, fs_rows));

    let paragraph = Paragraph::new(Text::from(img_lines));
    frame.render_widget(paragraph, img_area);

    // Status bar: "  3/7 — filename.png"
    let total = app.image_index.len();
    let status_text = format!("  {}/{} — {}", idx + 1, total, entry.display_name);
    let status = Paragraph::new(Line::from(Span::styled(
        status_text,
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(status, status_area);

    // Fix up placeholder cells in the image area
    images::fixup_placeholder_cells(frame.buffer_mut(), img_area);
}
```

- [ ] **Step 3: Call `draw_fullscreen_image` from `draw`**

In `shore-tui/src/ui.rs`, modify the `draw` function (line 12). Add an early return for fullscreen mode at the top, after `let size = frame.area();`:

```rust
if app.fullscreen.is_some() {
    // Still need to run draw_conversation to rebuild image_index,
    // but render to a zero-height area so nothing is visible.
    let hidden = Rect::new(0, 0, size.width, 0);
    draw_conversation(frame, &mut *app, hidden);
    draw_fullscreen_image(frame, app, size);
    return;
}
```

Wait — rendering `draw_conversation` with a zero-height area would break the line count / scroll calculations. Instead, we should rebuild the image_index separately. Let's extract the image index building.

Actually, the simplest approach: just call `draw_conversation` on the real area (it builds image_index as a side effect), then overdraw with the fullscreen image. Ratatui draws widgets in order — later widgets overwrite earlier ones.

Replace the approach. In the `draw` function, after the existing code that draws everything (after the `if app.show_help` block, ~line 49), add:

```rust
if app.fullscreen.is_some() {
    draw_fullscreen_image(frame, app, size);
}
```

This is clean: the normal conversation renders (building image_index), then the fullscreen overlay paints over the entire terminal.

- [ ] **Step 4: Build to verify**

Run: `cargo build -p shore-tui`
Expected: compiles cleanly

- [ ] **Step 5: Commit**

```bash
git add shore-tui/src/images.rs shore-tui/src/ui.rs
git commit -m "feat(tui): add fullscreen image viewer overlay"
```

---

### Task 5: Add `o` keybinding and fullscreen input handling

**Files:**
- Modify: `shore-tui/src/input.rs:39-64` (handle_key), `shore-tui/src/input.rs:66-169` (handle_normal_mode)

- [ ] **Step 1: Add fullscreen input handling in `handle_key`**

In `shore-tui/src/input.rs`, in `handle_key` (line 39), add a fullscreen intercept after the help overlay check (after line 44) and before the global shortcuts (before line 46):

```rust
// Fullscreen image viewer handles its own keys
if app.fullscreen.is_some() {
    return handle_fullscreen(app, key);
}
```

- [ ] **Step 2: Add `handle_fullscreen` function**

In `shore-tui/src/input.rs`, add after `handle_normal_mode` (after line 169):

```rust
fn handle_fullscreen(app: &mut App, key: KeyEvent) -> Action {
    match (key.modifiers, key.code) {
        // Exit fullscreen
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('o')) => {
            app.fullscreen = None;
            Action::Redraw
        }
        // Next image
        (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
            if let Some(ref mut idx) = app.fullscreen {
                let total = app.image_index.len();
                if total > 0 {
                    *idx = (*idx + 1) % total;
                }
            }
            Action::Redraw
        }
        // Previous image
        (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
            if let Some(ref mut idx) = app.fullscreen {
                let total = app.image_index.len();
                if total > 0 {
                    *idx = (*idx + total - 1) % total;
                }
            }
            Action::Redraw
        }
        _ => Action::None,
    }
}
```

- [ ] **Step 3: Add `o` keybinding in `handle_normal_mode`**

In `shore-tui/src/input.rs`, in `handle_normal_mode`, add before the `_ => Action::None` fallthrough (before line 167):

```rust
// Fullscreen image viewer
(KeyModifiers::NONE, KeyCode::Char('o')) => {
    if app.image_index.is_empty() {
        return Action::None;
    }
    // Find image closest to the center of the visible viewport.
    // scroll_offset is distance from bottom. The visible range in
    // the raw line list is [scroll .. scroll + visible_height].
    // We approximate: the app's draw_conversation already computed
    // image_index line positions. Use scroll_offset to estimate
    // which lines are visible.
    let term_height = crossterm::terminal::size()
        .map(|(_, h)| h)
        .unwrap_or(24);
    // The conversation area is roughly term_height minus input/thinking.
    // Use 80% as a rough estimate of conversation area height.
    let visible_h = (term_height * 80 / 100).max(1) as usize;
    // Total content lines approximated by the last image's line position
    // plus some buffer. We don't have the exact total here, so use a
    // heuristic: the image_index entries are in line order.
    // For scroll_offset=0 (bottom), visible center is near the end.
    // For scroll_offset=N, visible center is N lines up from the end.
    let last_line = app.image_index.last().map(|e| e.line).unwrap_or(0);
    let total_approx = last_line + visible_h;
    let center = if app.auto_scroll {
        total_approx.saturating_sub(visible_h / 2)
    } else {
        total_approx
            .saturating_sub(app.scroll_offset as usize)
            .saturating_sub(visible_h / 2)
    };
    // Find the image with line position closest to center
    let best = app
        .image_index
        .iter()
        .enumerate()
        .min_by_key(|(_, e)| (e.line as isize - center as isize).unsigned_abs())
        .map(|(i, _)| i)
        .unwrap_or(0);
    app.fullscreen = Some(best);
    Action::Redraw
}
```

- [ ] **Step 4: Add `crossterm` import if not already present**

The file already imports `crossterm::event::*` on line 1. We need `crossterm::terminal::size` which is in the `crossterm` crate — it's already a dependency. The `crossterm::terminal::size()` call doesn't need an additional import since we use the fully qualified path.

- [ ] **Step 5: Add `o` to help overlay**

In `shore-tui/src/ui.rs`, in `draw_help`, add after the `p toggle inline images` line:

```rust
Line::from(Span::styled(
    "    o               fullscreen image viewer",
    Style::default().fg(Color::White),
)),
```

- [ ] **Step 6: Build to verify**

Run: `cargo build -p shore-tui`
Expected: compiles cleanly

- [ ] **Step 7: Commit**

```bash
git add shore-tui/src/input.rs shore-tui/src/ui.rs
git commit -m "feat(tui): add 'o' keybinding for fullscreen image viewer with j/k navigation"
```

---

### Task 6: Final verification

- [ ] **Step 1: Run full workspace build**

Run: `cargo build --workspace`
Expected: compiles cleanly

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings

- [ ] **Step 3: Run tests**

Run: `cargo test --workspace`
Expected: all pass

- [ ] **Step 4: Run cargo fmt**

Run: `cargo fmt --all -- --check`
Expected: no formatting issues (fix with `cargo fmt --all` if needed)
