//! The Android versions of the desktop-only parts (tray, notification popups, sign-in window),
//! so the shared commands compile unchanged. Android has no tray or popups.

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

/// Sign-in on Android: Android apps get one WebView, so the service's login page opens in
/// Agora's own window and Agora comes back when the session is captured (or the user goes
/// back). Same functions as the desktop sign-in window, so the shared commands are unchanged.
pub mod login {
    use std::future::Future;
    use std::sync::Mutex;
    use std::time::Duration;
    use tauri::{AppHandle, Manager, Url, WebviewWindow};

    const TIMEOUT: Duration = Duration::from_secs(15 * 60);
    const POLL: Duration = Duration::from_millis(800);

    /// Where to come back to, and the script each sign-in page gets.
    struct Session {
        home: Url,
        script: Option<String>,
    }
    static SESSION: Mutex<Option<Session>> = Mutex::new(None);

    fn on_app_page(w: &WebviewWindow, home: &Url) -> bool {
        w.url().map(|u| u.host_str() == home.host_str()).unwrap_or(true)
    }

    /// Leaves Agora's page for `start`. `script` is run in each sign-in page while polling.
    pub fn open(app: &AppHandle, _title: &str, start: &Url, script: Option<&str>) -> Result<WebviewWindow, String> {
        let w = app.get_webview_window("main").ok_or("No main window")?;
        let home = w.url().map_err(|e| e.to_string())?;
        *SESSION.lock().unwrap() = Some(Session { home, script: script.map(str::to_string) });
        crate::android::set_user_agent(Some(crate::android::DESKTOP_AGENT));
        w.navigate(start.clone()).map_err(|e| format!("Cannot open the sign-in page: {e}"))?;
        Ok(w)
    }

    /// Calls `step` about once a second until it returns a value, then forgets the sign-in
    /// page's cookies and returns to Agora. Going back to Agora cancels.
    pub async fn poll<T, F, Fut>(app: &AppHandle, window: WebviewWindow, step: F) -> Result<T, String>
    where
        F: FnMut(WebviewWindow) -> Fut,
        Fut: Future<Output = Option<T>>,
    {
        poll_until(app, window, step, || None).await
    }

    /// [`poll`], but when the user goes back (or time runs out), `on_close` can still give a
    /// result: what was collected so far.
    pub async fn poll_until<T, F, Fut>(
        _app: &AppHandle,
        window: WebviewWindow,
        mut step: F,
        on_close: impl FnOnce() -> Option<T>,
    ) -> Result<T, String>
    where
        F: FnMut(WebviewWindow) -> Fut,
        Fut: Future<Output = Option<T>>,
    {
        let (home, script) = SESSION
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| (s.home.clone(), s.script.clone()))
            .ok_or("Sign-in was not started")?;
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        let mut left = false;
        let result = loop {
            tokio::time::sleep(POLL).await;
            let here = on_app_page(&window, &home);
            if !here {
                left = true;
                if let Some(js) = &script {
                    let _ = window.eval(js);
                }
            } else if left {
                break Err("Sign-in cancelled".to_string());
            }
            if tokio::time::Instant::now() > deadline {
                break Err("Sign-in timed out".to_string());
            }
            if let Some(v) = step(window.clone()).await {
                break Ok(v);
            }
        };
        *SESSION.lock().unwrap() = None;
        crate::android::clear_cookies();
        crate::android::set_user_agent(None);
        if on_app_page(&window, &home) {
            // Went back by hand: that page loaded as a desktop browser, so it would lay out for one.
            let _ = window.eval("location.reload()");
        } else {
            let _ = window.navigate(home);
        }
        result.or_else(|e| on_close().ok_or(e))
    }

    /// Value of a non-empty cookie visible at `url`.
    pub fn cookie(_window: &WebviewWindow, url: &Url, name: &str) -> Option<String> {
        crate::android::cookies(url.as_str()).into_iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    /// Every non-empty cookie visible at `url`, as (name, value).
    pub fn all_cookies(_window: &WebviewWindow, url: &Url) -> Vec<(String, String)> {
        crate::android::cookies(url.as_str())
    }

    /// Opens `start` and waits until every cookie of one of `sets` (tried in order) is set
    /// for `start`, returning their values in that set's order.
    pub async fn capture_cookies(
        app: &AppHandle,
        title: &str,
        start: &str,
        sets: &[&[&str]],
        script: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let url: Url = start.parse().map_err(|_| format!("Invalid address: {start}"))?;
        let window = open(app, title, &url, script)?;
        poll(app, window, |w| {
            let found = sets.iter().find_map(|set| set.iter().map(|n| cookie(&w, &url, n)).collect::<Option<Vec<_>>>());
            async move { found }
        })
        .await
    }
}
