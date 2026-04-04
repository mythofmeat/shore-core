# Image Display Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Improve TUI image display with better spacing, viewport-percentage caps, and an inline toggle.

**Architecture:** Three independent changes to the shore-tui crate: (1) add vertical spacing around images, (2) replace the width-only cap with width+height percentage-of-viewport caps that preserve aspect ratio, (3) add a `p` keybinding to toggle inline image rendering, persisted alongside existing prefs.

**Tech Stack:** Rust, ratatui, crossterm, kitty graphics protocol

---

### Task 1: Add `show_images` field to App state

**Files:**
- Modify: `shore-tui/src/app.rs:362-419`

- [ ] **Step 1: Add `show_images` field to `App` struct**

In `shore-tui/src/app.rs`, add the field after `show_tools`:

```rust
pub show_images: bool,
```

And in the `Default` impl, after `show_tools: true,`:

```rust
show_images: true,
```

- [ ] **Step 2: Build to verify**

Run: `cargo build -p shore-tui`
Expected: compiles cleanly

- [ ] **Step 3: Commit**

```bash
git add shore-tui/src/app.rs
git commit -m "feat(tui): add show_images toggle field to App"
```

---

### Task 2: Add `p` keybinding and persist preference

**Files:**
- Modify: `shore-tui/src/input.rs:141-145`
- Modify: `shore-tui/src/main.rs:85-104`

- [ ] **Step 1: Add keybinding in `handle_normal_mode`**

In `shore-tui/src/input.rs`, add a new arm after the `show_tools` toggle (after line 145):

```rust
// Toggle inline images in history
(KeyModifiers::NONE, KeyCode::Char('p')) => {
    app.show_images = !app.show_images;
    Action::Redraw
}
```

- [ ] **Step 2: Persist `show_images` in prefs**

In `shore-tui/src/main.rs`, update `load_prefs` — add after the `show_tools` block (after line 93):

```rust
if let Some(b) = v.get("show_images").and_then(|v| v.as_bool()) {
    app.show_images = b;
}
```

Update `save_prefs` — add `show_images` to the JSON value (line 99-102):

```rust
let v = serde_json::json!({
    "show_thinking": app.show_thinking,
    "show_tools": app.show_tools,
    "show_images": app.show_images,
});
```

- [ ] **Step 3: Add `p` to help overlay**

In `shore-tui/src/ui.rs`, in `draw_help`, add after the `T toggle tool-use blocks` line (after line 824):

```rust
Line::from(Span::styled(
    "    p               toggle inline images",
    Style::default().fg(Color::White),
)),
```

- [ ] **Step 4: Build to verify**

Run: `cargo build -p shore-tui`
Expected: compiles cleanly

- [ ] **Step 5: Commit**

```bash
git add shore-tui/src/input.rs shore-tui/src/main.rs shore-tui/src/ui.rs
git commit -m "feat(tui): add 'p' keybinding to toggle inline images"
```

---

### Task 3: Add spacing around images in `render_images`

**Files:**
- Modify: `shore-tui/src/ui.rs:584-611`

- [ ] **Step 1: Add blank line before images and pass `show_images`**

Replace the `render_images` function with:

```rust
/// Render image entries — kitty placeholders when available, text fallback otherwise.
fn render_images(
    lines: &mut Vec<Line<'static>>,
    img_refs: &[shore_protocol::types::ImageRef],
    cache: &images::ImageCache,
    show_inline: bool,
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
                lines.extend(images::placeholder_lines(transmitted));
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

- [ ] **Step 2: Update both call sites to pass `app.show_images`**

In `shore-tui/src/ui.rs`, the two calls to `render_images` (User entry ~line 413, Assistant entry ~line 451) both need the extra argument:

```rust
render_images(&mut lines, images, &app.image_cache, app.show_images);
```

- [ ] **Step 3: Build to verify**

Run: `cargo build -p shore-tui`
Expected: compiles cleanly

- [ ] **Step 4: Commit**

```bash
git add shore-tui/src/ui.rs
git commit -m "feat(tui): add spacing around images and support show_images toggle"
```

---

### Task 4: Implement width/height viewport-percentage caps

**Files:**
- Modify: `shore-tui/src/main.rs:506-510` (replace `image_max_cols`)
- Modify: `shore-tui/src/images.rs:125-237` (update `calculate_cells`, `ensure_transmitted`, `ensure_transmitted_from_b64`)

- [ ] **Step 1: Replace `image_max_cols` with `image_max_cells`**

In `shore-tui/src/main.rs`, replace:

```rust
/// Max display columns for images (terminal width minus borders/indent).
fn image_max_cols() -> u16 {
    crossterm::terminal::size()
        .map(|(w, _)| w.saturating_sub(4))
        .unwrap_or(76)
}
```

With:

```rust
/// Max display cells for images: 80% terminal width (minus indent), 50% terminal height.
fn image_max_cells() -> (u16, u16) {
    let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
    let max_cols = (w * 80 / 100).saturating_sub(4).max(1);
    let max_rows = (h * 50 / 100).max(1);
    (max_cols, max_rows)
}
```

- [ ] **Step 2: Update all call sites in `main.rs`**

In `transmit_entry_images` (~line 514-526), replace:

```rust
let max_cols = image_max_cols();
```

with:

```rust
let (max_cols, max_rows) = image_max_cells();
```

And update the inner call:

```rust
transmit_image_ref(&mut app.image_cache, img, max_cols, max_rows);
```

Update `transmit_image_ref` (~line 529-539):

```rust
fn transmit_image_ref(
    cache: &mut images::ImageCache,
    img: &shore_protocol::types::ImageRef,
    max_cols: u16,
    max_rows: u16,
) {
    if let Some(b64) = &img.data {
        cache.ensure_transmitted_from_b64(&img.path, b64, max_cols, max_rows);
    } else {
        cache.ensure_transmitted(&img.path, max_cols, max_rows);
    }
}
```

In `handle_server_message`, the `NewMessage` handler (~line 647-650), replace:

```rust
let max_cols = image_max_cols();
for img in &new_msg.message.images {
    transmit_image_ref(&mut app.image_cache, img, max_cols);
}
```

with:

```rust
let (max_cols, max_rows) = image_max_cells();
for img in &new_msg.message.images {
    transmit_image_ref(&mut app.image_cache, img, max_cols, max_rows);
}
```

In the `SendImage` handler (~line 709-715), replace:

```rust
let max_cols = image_max_cols();
if let Some(b64) = &img.data {
    app.image_cache
        .ensure_transmitted_from_b64(&img.path, b64, max_cols);
} else {
    app.image_cache.ensure_transmitted(&img.path, max_cols);
}
```

with:

```rust
let (max_cols, max_rows) = image_max_cells();
if let Some(b64) = &img.data {
    app.image_cache
        .ensure_transmitted_from_b64(&img.path, b64, max_cols, max_rows);
} else {
    app.image_cache.ensure_transmitted(&img.path, max_cols, max_rows);
}
```

- [ ] **Step 3: Update `calculate_cells` to enforce both caps**

In `shore-tui/src/images.rs`, replace:

```rust
fn calculate_cells(&self, pw: u32, ph: u32, max_cols: u16) -> (u16, u16) {
    let natural_cols = pw / self.cell_width as u32;
    let cols = (natural_cols as u16).min(max_cols).max(1);
    let scale = (cols as f64 * self.cell_width as f64) / pw as f64;
    let rows = ((ph as f64 * scale) / self.cell_height as f64).ceil() as u16;
    (cols, rows.clamp(1, 255))
}
```

With:

```rust
fn calculate_cells(&self, pw: u32, ph: u32, max_cols: u16, max_rows: u16) -> (u16, u16) {
    let cw = self.cell_width as f64;
    let ch = self.cell_height as f64;

    // Scale to fit width
    let natural_cols = pw as f64 / cw;
    let cols_f = natural_cols.min(max_cols as f64).max(1.0);
    let scale_w = (cols_f * cw) / pw as f64;
    let rows_from_w = (ph as f64 * scale_w / ch).ceil();

    // If height exceeds cap, scale to fit height instead
    let (cols, rows) = if rows_from_w > max_rows as f64 {
        let scale_h = (max_rows as f64 * ch) / ph as f64;
        let cols_from_h = (pw as f64 * scale_h / cw).floor().max(1.0);
        (cols_from_h as u16, max_rows)
    } else {
        (cols_f as u16, rows_from_w as u16)
    };

    (cols.max(1), rows.clamp(1, 255))
}
```

- [ ] **Step 4: Update `ensure_transmitted` signature and call**

In `shore-tui/src/images.rs`, update `ensure_transmitted` (~line 151):

```rust
pub fn ensure_transmitted(&mut self, path: &str, max_cols: u16, max_rows: u16) -> Option<&TransmittedImage> {
```

And its inner call (~line 161):

```rust
let (cols, rows) = self.calculate_cells(pw, ph, max_cols, max_rows);
```

- [ ] **Step 5: Update `ensure_transmitted_from_b64` signature and call**

In `shore-tui/src/images.rs`, update `ensure_transmitted_from_b64` (~line 180):

```rust
pub fn ensure_transmitted_from_b64(
    &mut self,
    key: &str,
    b64_data: &str,
    max_cols: u16,
    max_rows: u16,
) -> Option<&TransmittedImage> {
```

And its inner call (~line 198):

```rust
let (cols, rows) = self.calculate_cells(pw, ph, max_cols, max_rows);
```

- [ ] **Step 6: Build to verify**

Run: `cargo build -p shore-tui`
Expected: compiles cleanly

- [ ] **Step 7: Commit**

```bash
git add shore-tui/src/main.rs shore-tui/src/images.rs
git commit -m "feat(tui): cap images to 80% width / 50% height of viewport"
```

---

### Task 5: Final verification

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
