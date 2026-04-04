//! Kitty graphics protocol diagnostic tool.
//!
//! Tests each layer of the image pipeline independently:
//!   1. Environment detection
//!   2. Cell size query
//!   3. Terminal probe (kitty query)
//!   4. Direct placement (a=T)
//!   5. Unicode placeholders (raw stdout — known working)
//!   6. Ratatui Paragraph WITHOUT wrap
//!   7. Ratatui Paragraph WITH wrap (matches TUI config)
//!   8. Ratatui in alternate screen (matches TUI)
//!
//! Run: cargo run -p shore-tui --bin kitty_diag

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Terminal;

fn main() {
    println!("=== Kitty Graphics Diagnostic ===\n");

    // --- Step 1: Environment ---
    println!("[1] Environment variables:");
    for var in &[
        "TERM",
        "TERM_PROGRAM",
        "SHORE_IMAGES",
        "KITTY_WINDOW_ID",
        "KITTY_PID",
    ] {
        match std::env::var(var) {
            Ok(v) => println!("    {var} = {v}"),
            Err(_) => println!("    {var} = (unset)"),
        }
    }
    let ghostty_vars: Vec<_> = std::env::vars()
        .filter(|(k, _)| k.starts_with("GHOSTTY_"))
        .collect();
    if ghostty_vars.is_empty() {
        println!("    GHOSTTY_* = (none)");
    } else {
        for (k, v) in &ghostty_vars {
            println!("    {k} = {v}");
        }
    }

    // --- Step 2: Cell size ---
    println!("\n[2] Cell size (TIOCGWINSZ):");
    match query_cell_size_full() {
        Some((cols, rows, xpix, ypix)) => {
            let cw = xpix / cols;
            let ch = ypix / rows;
            println!("    terminal: {cols}x{rows} cells, {xpix}x{ypix} pixels");
            println!("    cell size: {cw}x{ch} pixels");
        }
        None => println!("    FAILED — ioctl returned zero or error"),
    }

    // --- Step 3: Kitty probe ---
    println!("\n[3] Kitty graphics probe:");
    let _ = enable_raw_mode();
    let probe_result = probe_kitty_verbose();
    println!("    {probe_result}\r");

    // --- Step 4: Direct placement ---
    println!("\n\r[4] Direct placement test (a=T — transmit+display):\r");
    let test_png = make_test_png(10, 10, [255, 0, 0]);
    let b64 = base64_encode(&test_png);
    {
        let mut tty = open_tty();
        let _ = write!(tty, "\x1b_Ga=T,f=100,i=99,q=2;{b64}\x1b\\");
        let _ = tty.flush();
    }
    println!("    ^ red square = direct placement works\r");

    // --- Step 5: Raw Unicode placeholders (known working from last run) ---
    println!("\n\r[5] Raw Unicode placeholders (stdout, no ratatui):\r");
    let test_png2 = make_test_png(40, 20, [0, 128, 255]);
    let b64_2 = base64_encode(&test_png2);
    transmit_and_place(200, &b64_2, 5, 2);
    {
        let mut out = io::stdout();
        let _ = write!(out, "\x1b[38;5;200m");
        for row in 0u8..2 {
            for col in 0u8..5 {
                let _ = write!(out, "\u{10EEEE}{}{}", diacritic(row), diacritic(col));
            }
            let _ = write!(out, "\x1b[0m\r\n\x1b[38;5;200m");
        }
        let _ = write!(out, "\x1b[0m");
        let _ = out.flush();
    }
    println!("    ^ blue rect = Unicode placeholders work\r");

    // --- Step 6: Ratatui Paragraph WITHOUT wrap ---
    println!("\n\r[6] Ratatui Paragraph (NO wrap, no alt screen):\r");
    transmit_and_place(201, &b64_2, 5, 2);
    {
        let style = Style::default().fg(Color::Indexed(201));
        let lines = make_placeholder_lines(201, 5, 2, style);
        let text = Text::from(lines);
        let para = Paragraph::new(text);

        // Render to a test buffer, then dump the cells manually
        let area = Rect::new(0, 0, 40, 2);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(para, area, &mut buf);

        // Dump what ratatui produced to stdout
        let mut out = io::stdout();
        for y in 0..2u16 {
            for x in 0..40u16 {
                let cell = &buf[(x, y)];
                if cell.symbol() == " " && cell.fg == Color::Reset {
                    continue;
                }
                // Position cursor and set style
                let _ = write!(out, "\x1b[{};{}H", y + cursor_row(), x + 1);
                if let Color::Indexed(idx) = cell.fg {
                    let _ = write!(out, "\x1b[38;5;{idx}m");
                }
                let _ = write!(out, "{}", cell.symbol());
                let _ = write!(out, "\x1b[0m");
            }
        }
        let _ = write!(out, "\x1b[{};1H", cursor_row() + 2);
        let _ = out.flush();
    }
    println!("    ^ blue rect = ratatui buffer is correct\r");

    // --- Step 7: Ratatui Paragraph WITH wrap (matches TUI) ---
    println!("\n\r[7] Ratatui Paragraph (WITH Wrap {{ trim: false }}):\r");
    transmit_and_place(202, &b64_2, 5, 2);
    {
        let style = Style::default().fg(Color::Indexed(202));
        let lines = make_placeholder_lines(202, 5, 2, style);
        let text = Text::from(lines);
        let para = Paragraph::new(text).wrap(Wrap { trim: false });

        let area = Rect::new(0, 0, 40, 2);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(para, area, &mut buf);

        let mut out = io::stdout();
        for y in 0..2u16 {
            for x in 0..40u16 {
                let cell = &buf[(x, y)];
                if cell.symbol() == " " && cell.fg == Color::Reset {
                    continue;
                }
                let _ = write!(out, "\x1b[{};{}H", y + cursor_row(), x + 1);
                if let Color::Indexed(idx) = cell.fg {
                    let _ = write!(out, "\x1b[38;5;{idx}m");
                }
                let _ = write!(out, "{}", cell.symbol());
                let _ = write!(out, "\x1b[0m");
            }
        }
        let _ = write!(out, "\x1b[{};1H", cursor_row() + 2);
        let _ = out.flush();
    }
    println!("    ^ blue rect = wrap doesn't break placeholders\r");

    // --- Step 8: Raw placeholders ON alt screen (isolate alt-screen issue) ---
    println!("\n\r[8] Raw placeholders on ALT SCREEN (no ratatui) — Enter to start:\r");
    let _ = disable_raw_mode();
    wait_enter();
    let _ = enable_raw_mode();
    {
        io::stdout().execute(EnterAlternateScreen).unwrap();
        // Transmit AFTER entering alt screen
        transmit_and_place(203, &b64_2, 5, 2);
        let mut out = io::stdout();
        let _ = write!(out, "\x1b[1;1H=== [8] Raw placeholders on alt screen ===\r\n");
        let _ = write!(out, "Image should appear below:\r\n");
        let _ = write!(out, "\x1b[38;5;203m");
        for row in 0u8..2 {
            for col in 0u8..5 {
                let _ = write!(out, "\u{10EEEE}{}{}", diacritic(row), diacritic(col));
            }
            let _ = write!(out, "\x1b[0m\r\n\x1b[38;5;203m");
        }
        let _ = write!(out, "\x1b[0m\r\n");
        let _ = write!(out, "If blue rect above: alt screen works.\r\n");
        let _ = write!(out, "Press Enter to continue.\r\n");
        let _ = out.flush();

        let mut b = [0u8; 1];
        let _ = io::stdin().read(&mut b);
        io::stdout().execute(LeaveAlternateScreen).unwrap();
    }

    // --- Step 9: Ratatui + alt screen + FIXED approach (transmit inside alt screen) ---
    {
        io::stdout().execute(EnterAlternateScreen).unwrap();
        // Transmit AFTER entering alt screen
        transmit_and_place(204, &b64_2, 5, 2);

        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend).unwrap();

        let style = Style::default().fg(Color::Indexed(204));
        let mut lines = vec![
            Line::from("=== [9] Ratatui alt-screen (FIXED + transmit inside alt) ==="),
            Line::from("Image should appear below:"),
        ];
        lines.extend(make_placeholder_lines_fixed(204, 5, 2, style));
        lines.push(Line::from(""));
        lines.push(Line::from("If blue rect above: THE FIX WORKS!"));
        lines.push(Line::from("Press Enter to exit."));

        terminal
            .draw(|frame| {
                let area = frame.area();
                let para = Paragraph::new(Text::from(lines.clone()))
                    .wrap(Wrap { trim: false });
                frame.render_widget(para, area);
                fixup_placeholder_cells(frame.buffer_mut(), area);
            })
            .unwrap();

        let mut b = [0u8; 1];
        let _ = io::stdin().read(&mut b);
        io::stdout().execute(LeaveAlternateScreen).unwrap();
    }

    let _ = disable_raw_mode();

    println!("\n[done] Results summary:");
    println!("  [4] direct placement — check red square");
    println!("  [5] raw placeholders — check blue rect");
    println!("  [6] ratatui no-wrap (broken) — scattered cells");
    println!("  [7] ratatui w/ wrap (broken) — nothing visible");
    println!("  [8] raw placeholders on alt screen — check blue rect");
    println!("  [9] ratatui alt screen (FIXED) — check blue rect");
    println!("\nPress Enter to clean up.");
    wait_enter();

    // Clean up all test images
    {
        let mut tty = open_tty();
        for id in [99, 200, 201, 202, 203, 204] {
            let _ = write!(tty, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\");
        }
        let _ = tty.flush();
    }
}

fn wait_enter() {
    let mut buf = [0u8; 64];
    let _ = io::stdin().read(&mut buf);
}

/// Get approximate current cursor row for manual cell dumping.
fn cursor_row() -> u16 {
    // Use crossterm to query cursor position
    crossterm::cursor::position().map(|(_, y)| y + 1).unwrap_or(1)
}

fn transmit_and_place(id: u32, b64: &str, cols: u16, rows: u16) {
    let mut tty = open_tty();
    let _ = write!(tty, "\x1b_Ga=t,f=100,i={id},q=2;{b64}\x1b\\");
    let _ = write!(tty, "\x1b_Ga=p,U=1,q=2,i={id},c={cols},r={rows}\x1b\\");
    let _ = tty.flush();
}

fn make_placeholder_lines(
    _id: u32,
    cols: u16,
    rows: u16,
    style: Style,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(rows as usize);
    for row in 0..rows {
        let mut text = String::with_capacity(cols as usize * 12);
        for col in 0..cols {
            text.push('\u{10EEEE}');
            text.push(diacritic(row as u8));
            text.push(diacritic(col as u8));
        }
        lines.push(Line::from(Span::styled(text, style)));
    }
    lines
}

/// Same as make_placeholder_lines but uses U+2800 (width-1 stand-in).
fn make_placeholder_lines_fixed(
    _id: u32,
    cols: u16,
    rows: u16,
    style: Style,
) -> Vec<Line<'static>> {
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

/// Replace U+2800 with U+10EEEE in buffer cells.
fn fixup_placeholder_cells(buf: &mut ratatui::buffer::Buffer, area: Rect) {
    for y in area.y..(area.y + area.height) {
        for x in area.x..(area.x + area.width) {
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            if let Some(rest) = sym.strip_prefix('\u{2800}') {
                if !rest.is_empty() {
                    let mut new_sym = String::with_capacity(4 + rest.len());
                    new_sym.push('\u{10EEEE}');
                    new_sym.push_str(rest);
                    buf[(x, y)].set_symbol(&new_sym);
                }
            }
        }
    }
}

fn open_tty() -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .expect("cannot open /dev/tty")
}

fn probe_kitty_verbose() -> String {
    let mut tty = open_tty();
    let query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
    if tty.write_all(query).is_err() {
        return "FAILED — could not write probe".into();
    }
    if tty.flush().is_err() {
        return "FAILED — could not flush probe".into();
    }

    let deadline = Instant::now() + Duration::from_millis(500);
    let mut response = Vec::with_capacity(128);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut buf = [0u8; 128];
        let n = read_with_timeout(&mut tty, &mut buf, remaining);
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buf[..n]);
        if response.windows(2).any(|w| w == b"\x1b\\") {
            break;
        }
    }

    if response.is_empty() {
        return "NO RESPONSE — terminal may not support kitty graphics".into();
    }

    let text = String::from_utf8_lossy(&response);
    let escaped: String = text
        .chars()
        .map(|c| {
            if c == '\x1b' {
                "ESC".to_string()
            } else if c.is_control() {
                format!("\\x{:02x}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect();

    if text.contains("OK") {
        format!("OK — kitty graphics supported (raw: {escaped})")
    } else {
        format!("UNEXPECTED — response: {escaped}")
    }
}

#[cfg(unix)]
fn read_with_timeout(file: &mut std::fs::File, buf: &mut [u8], timeout: Duration) -> usize {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let timeout_ms = timeout.as_millis() as i32;
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    if ready <= 0 {
        return 0;
    }
    file.read(buf).unwrap_or(0)
}

#[cfg(unix)]
fn query_cell_size_full() -> Option<(u16, u16, u16, u16)> {
    use std::os::unix::io::AsRawFd;
    let tty = std::fs::File::open("/dev/tty").ok()?;
    let fd = tty.as_raw_fd();
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if ret != 0 {
        return None;
    }
    Some((ws.ws_col, ws.ws_row, ws.ws_xpixel, ws.ws_ypixel))
}

#[cfg(not(unix))]
fn query_cell_size_full() -> Option<(u16, u16, u16, u16)> {
    None
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn make_test_png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr_data = Vec::new();
    ihdr_data.extend_from_slice(&width.to_be_bytes());
    ihdr_data.extend_from_slice(&height.to_be_bytes());
    ihdr_data.push(8);
    ihdr_data.push(2);
    ihdr_data.push(0);
    ihdr_data.push(0);
    ihdr_data.push(0);
    write_png_chunk(&mut buf, b"IHDR", &ihdr_data);
    let mut raw = Vec::new();
    #[allow(clippy::same_item_push)]
    for _ in 0..height {
        raw.push(0u8); // PNG filter byte (none)
        for _ in 0..width {
            raw.extend_from_slice(&rgb);
        }
    }
    let compressed = deflate_uncompressed(&raw);
    write_png_chunk(&mut buf, b"IDAT", &compressed);
    write_png_chunk(&mut buf, b"IEND", &[]);
    buf
}

fn write_png_chunk(buf: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
    buf.extend_from_slice(chunk_type);
    buf.extend_from_slice(data);
    let mut crc_data = Vec::with_capacity(4 + data.len());
    crc_data.extend_from_slice(chunk_type);
    crc_data.extend_from_slice(data);
    buf.extend_from_slice(&png_crc32(&crc_data).to_be_bytes());
}

fn png_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
        }
    }
    crc ^ 0xFFFFFFFF
}

fn deflate_uncompressed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x78);
    out.push(0x01);
    let chunks: Vec<&[u8]> = data.chunks(65535).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let is_last = i + 1 == chunks.len();
        out.push(if is_last { 0x01 } else { 0x00 });
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    let adler = (b << 16) | a;
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

#[rustfmt::skip]
const DIACRITICS: [u32; 256] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F,
    0x0346, 0x034A, 0x034B, 0x034C, 0x0350, 0x0351, 0x0352, 0x0357,
    0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484,
    0x0485, 0x0486, 0x0487, 0x0592, 0x0593, 0x0594, 0x0595, 0x0597,
    0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611,
    0x0612, 0x0613, 0x0614, 0x0615, 0x0616, 0x0617, 0x0657, 0x0658,
    0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2,
    0x06E4, 0x06E7, 0x06E8, 0x06EB, 0x06EC, 0x0730, 0x0732, 0x0733,
    0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE,
    0x07EF, 0x07F0, 0x07F1, 0x07F3, 0x0816, 0x0817, 0x0818, 0x0819,
    0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C,
    0x082D, 0x0951, 0x0953, 0x0954, 0x0F82, 0x0F83, 0x0F86, 0x0F87,
    0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D,
    0x1B6E, 0x1B6F, 0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1,
    0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1,
    0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5, 0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9,
    0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1,
    0x20D4, 0x20D5, 0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7,
    0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA,
    0x2DEB, 0x2DEC, 0x2DED, 0x2DEE, 0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2,
    0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D,
    0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5,
];

fn diacritic(value: u8) -> char {
    char::from_u32(DIACRITICS[value as usize]).unwrap()
}
