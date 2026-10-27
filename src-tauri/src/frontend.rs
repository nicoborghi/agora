//! Bridges engine output to the webviews, the popup window and the tray.

use crate::{popup::Popups, tray};
use agora_core::engine::{Frontend, PopupNote, UiEvent};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

pub struct TauriFrontend {
    app: AppHandle,
    popups: Arc<Popups>,
}

impl TauriFrontend {
    pub fn new(app: AppHandle, popups: Arc<Popups>) -> Self {
        Self { app, popups }
    }
}

impl Frontend for TauriFrontend {
    fn emit(&self, event: UiEvent) {
        let _ = self.app.emit_to("main", "agora", event);
    }

    fn popup(&self, note: PopupNote) {
        self.popups.push(&self.app, note);
    }

    fn play_sound(&self, sound: &str) {
        let _ = self.app.emit_to("main", "agora-sound", sound.to_string());
    }

    fn set_tray(&self, dots: Vec<String>) {
        tray::set_dots(&self.app, &dots);
    }
}
