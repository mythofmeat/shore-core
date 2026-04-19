use ksni::{menu::StandardItem, Icon, MenuItem, Tray, TrayMethods};
use shore_client::ConnCommand;
use std::io::Cursor;
use tauri::{AppHandle, Manager};
use tracing::warn;

use crate::AppState;

struct ShoreTray {
    app: AppHandle,
    icons: Vec<Icon>,
}

impl Tray for ShoreTray {
    fn id(&self) -> String {
        "shore-gui".into()
    }

    fn title(&self) -> String {
        "Shore".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icons.clone()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        toggle_main_window(&self.app);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Show Shore".into(),
                activate: Box::new(|this: &mut Self| show_main_window(&this.app)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Disconnect".into(),
                activate: Box::new(|this: &mut Self| disconnect(this.app.clone())),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|this: &mut Self| this.app.exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn toggle_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let visible = window.is_visible().unwrap_or(false);
    let focused = window.is_focused().unwrap_or(false);
    if visible && focused {
        let _ = window.hide();
    } else {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn disconnect(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let mut guard = state.connection.lock().await;
        if let Some(tx) = guard.take() {
            let _ = tx.send(ConnCommand::Shutdown).await;
        }
    });
}

pub fn spawn(app: AppHandle) {
    // ARGB32, network byte order — SNI spec. Source PNGs are 8-bit RGBA, so
    // decode_png_to_icon rotates each pixel one byte right to move A first.
    let icons: Vec<Icon> = [
        &include_bytes!("../icons/32x32.png")[..],
        &include_bytes!("../icons/128x128.png")[..],
    ]
    .into_iter()
    .filter_map(decode_png_to_icon)
    .collect();

    tauri::async_runtime::spawn(async move {
        match (ShoreTray { app, icons }).spawn().await {
            Ok(handle) => {
                // Keep the tray service alive for the program's lifetime.
                std::mem::forget(handle);
            }
            Err(e) => warn!(error = %e, "failed to spawn ksni tray"),
        }
    });
}

fn decode_png_to_icon(bytes: &[u8]) -> Option<Icon> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    for pixel in buf.chunks_exact_mut(4) {
        pixel.rotate_right(1);
    }
    Some(Icon {
        width: info.width as i32,
        height: info.height as i32,
        data: buf,
    })
}
