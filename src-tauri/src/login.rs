//! Sign-in window: loads the service's own login page in a private webview, so
//! SSO, two-factor and passwords all work exactly as in the browser, then hands
//! back the session. The window exists only during sign-in, is in private mode
//! (nothing kept on disk) and has no access to the app's commands.

use std::future::Future;
use std::time::Duration;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const LABEL: &str = "signin";
const TIMEOUT: Duration = Duration::from_secs(15 * 60);
const POLL: Duration = Duration::from_millis(800);

/// Opens the sign-in window on `start`. `script` runs in each page (used to expose a
/// token kept in localStorage). The title says "loading" while a page loads, so a
/// slow login page does not look like a blank, stuck window.
pub fn open(app: &AppHandle, title: &str, start: &Url, script: Option<&str>) -> Result<WebviewWindow, String> {
    if let Some(old) = app.get_webview_window(LABEL) {
        let _ = old.destroy();
    }
    let base = title.to_string();
    let mut builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External(start.clone()))
        .title(format!("{title} (loading...)"))
        .inner_size(520.0, 720.0)
        .center()
        .incognito(true)
        .on_page_load(move |w, p| {
            let t = match p.event() {
                PageLoadEvent::Started => format!("{base} (loading...)"),
                PageLoadEvent::Finished => base.clone(),
            };
            let _ = w.set_title(&t);
        });
    if let Some(js) = script {
        builder = builder.initialization_script(js);
    }
    builder.build().map_err(|e| format!("Cannot open the sign-in window: {e}"))
}

/// Calls `step` about once a second until it returns a value, then closes the window.
/// Fails when the user closes the window or after 15 minutes.
/// Must run in an async command: reading cookies on the main thread deadlocks WebView2.
pub async fn poll<T, F, Fut>(app: &AppHandle, window: WebviewWindow, mut step: F) -> Result<T, String>
where
    F: FnMut(WebviewWindow) -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let result = loop {
        tokio::time::sleep(POLL).await;
        if app.get_webview_window(LABEL).is_none() {
            break Err("Sign-in window closed".to_string());
        }
        if tokio::time::Instant::now() > deadline {
            break Err("Sign-in timed out".to_string());
        }
        if let Some(v) = step(window.clone()).await {
            break Ok(v);
        }
    };
    let _ = window.destroy();
    result
}

/// Value of a non-empty cookie visible at `url`.
pub fn cookie(window: &WebviewWindow, url: &Url, name: &str) -> Option<String> {
    let cookies = window.cookies_for_url(url.clone()).map_err(|e| tracing::debug!("cookies not readable yet: {e}")).ok()?;
    cookies.iter().find(|c| c.name() == name && !c.value().is_empty()).map(|c| c.value().to_string())
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
