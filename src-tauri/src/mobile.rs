//! Stand-ins for the desktop-only parts (tray, notification popups, sign-in window) in the
//! Android build, so the shared commands compile unchanged. Android has no tray or popups;
//! its own sign-in comes with the Keystore (step 3 of MOBILE.md).

pub mod tray {
    use tauri::AppHandle;

    pub fn create(_app: &AppHandle) -> tauri::Result<()> {
        Ok(())
    }

    pub fn show_main(_app: &AppHandle) {}

    pub fn set_dots(_app: &AppHandle, _colors: &[String], _focus: bool) {}
}

pub mod popup {
    use agora_core::engine::PopupNote;
    use tauri::AppHandle;

    /// No popups on Android: nothing is kept.
    #[derive(Default)]
    pub struct Popups;

    impl Popups {
        pub fn push(&self, _app: &AppHandle, _note: PopupNote) {}

        pub fn take_pending(&self) -> Vec<PopupNote> {
            Vec::new()
        }

        pub fn closed(&self, _app: &AppHandle) {}
    }

    pub fn resize(_app: &AppHandle, _height: f64) -> tauri::Result<()> {
        Ok(())
    }
}

pub mod login {
    use std::future::Future;
    use tauri::{AppHandle, Url, WebviewWindow};

    const NOT_YET: &str = "Signing in on Android is not available yet";

    pub fn open(_app: &AppHandle, _title: &str, _start: &Url, _script: Option<&str>) -> Result<WebviewWindow, String> {
        Err(NOT_YET.into())
    }

    pub async fn poll<T, F, Fut>(_app: &AppHandle, _window: WebviewWindow, _step: F) -> Result<T, String>
    where
        F: FnMut(WebviewWindow) -> Fut,
        Fut: Future<Output = Option<T>>,
    {
        Err(NOT_YET.into())
    }

    pub fn cookie(_window: &WebviewWindow, _url: &Url, _name: &str) -> Option<String> {
        None
    }

    pub fn all_cookies(_window: &WebviewWindow, _url: &Url) -> Vec<(String, String)> {
        Vec::new()
    }

    pub async fn capture_cookies(
        _app: &AppHandle,
        _title: &str,
        _start: &str,
        _sets: &[&[&str]],
        _script: Option<&str>,
    ) -> Result<Vec<String>, String> {
        Err(NOT_YET.into())
    }
}
