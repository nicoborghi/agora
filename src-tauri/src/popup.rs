//! Custom notification popups: one small borderless window in a corner of the screen (bottom right;
//! top right on macOS), created on demand and destroyed as soon as it is empty, so it costs
//! no memory while nothing is shown.

use agora_core::engine::PopupNote;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "popup";
const WIDTH: f64 = 340.0;
const NOTE_HEIGHT: f64 = 76.0;
const MARGIN: f64 = 12.0;
const MAX_PENDING: usize = 3;

#[derive(Default)]
pub struct Popups {
    pending: Mutex<Vec<PopupNote>>,
    /// The popup page has loaded and listens for events.
    ready: AtomicBool,
}

impl Popups {
    pub fn push(&self, app: &AppHandle, note: PopupNote) {
        let window = app.get_webview_window(LABEL);
        if window.is_some() && self.ready.load(Ordering::SeqCst) {
            let _ = app.emit_to(LABEL, "popup-note", note);
            return;
        }
        let count = {
            let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            p.push(note);
            let excess = p.len().saturating_sub(MAX_PENDING);
            p.drain(..excess);
            p.len()
        };
        if window.is_none() {
            if let Err(e) = create(app, count) {
                tracing::warn!("cannot open the notification window: {e}");
            }
        }
    }

    /// Called by the popup page once loaded: hands over what arrived meanwhile.
    pub fn take_pending(&self) -> Vec<PopupNote> {
        self.ready.store(true, Ordering::SeqCst);
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn closed(&self, app: &AppHandle) {
        self.ready.store(false, Ordering::SeqCst);
        if let Some(w) = app.get_webview_window(LABEL) {
            let _ = w.destroy();
        }
    }
}

fn create(app: &AppHandle, count: usize) -> tauri::Result<()> {
    let height = NOTE_HEIGHT * count.max(1) as f64;
    let (x, y) = corner(app, height);
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("popup.html".into()))
        .title("Agora")
        .inner_size(WIDTH, height)
        .position(x, y)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .shadow(false)
        .build()?;
    Ok(())
}

/// Inside the screen's work area (without the taskbar, Dock or top bar): bottom right, except on
/// macOS, where notifications appear at the top right under the menu bar.
fn corner(app: &AppHandle, height: f64) -> (f64, f64) {
    let monitor = app
        .get_webview_window("main")
        .and_then(|w| w.primary_monitor().ok().flatten());
    match monitor {
        Some(m) => {
            let scale = m.scale_factor();
            let area = m.work_area();
            let pos = area.position.to_logical::<f64>(scale);
            let size = area.size.to_logical::<f64>(scale);
            let x = pos.x + size.width - WIDTH - MARGIN;
            let y = if cfg!(target_os = "macos") { pos.y + MARGIN } else { pos.y + size.height - height - MARGIN };
            (x, y)
        }
        None => (100.0, 100.0),
    }
}

pub fn resize(app: &AppHandle, height: f64) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window(LABEL) {
        let height = height.clamp(40.0, NOTE_HEIGHT * MAX_PENDING as f64 + 24.0);
        w.set_size(LogicalSize::new(WIDTH, height))?;
        let (x, y) = corner(app, height);
        w.set_position(LogicalPosition::new(x, y))?;
    }
    Ok(())
}
