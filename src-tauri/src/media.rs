//! The `agora` URI scheme: serves images that need the account's credentials
//! (`http://agora.localhost/<account id>/<service path>`). Bytes pass straight
//! through, with no cache in Rust; WebView2 keeps its own disk cache.

use crate::AppState;
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{AppHandle, Manager, UriSchemeContext, UriSchemeResponder};

pub const SCHEME: &str = "agora";
/// The earlier name's scheme: picture links saved in the archive before the rename still load.
pub const LEGACY_SCHEME: &str = "armos";

pub fn handle(ctx: UriSchemeContext<'_, tauri::Wry>, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    // Only the main window shows messages; the sign-in window loads third-party pages.
    if ctx.webview_label() != "main" {
        responder.respond(status(StatusCode::FORBIDDEN, "forbidden"));
        return;
    }
    let app = ctx.app_handle().clone();
    let path = request.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
    tauri::async_runtime::spawn(async move { responder.respond(serve(&app, &path).await) });
}

async fn serve(app: &AppHandle, path: &str) -> Response<Vec<u8>> {
    let Some((account, rest)) = path.trim_start_matches('/').split_once('/') else {
        return status(StatusCode::NOT_FOUND, "not found");
    };
    let Some(state) = app.try_state::<AppState>() else {
        return status(StatusCode::SERVICE_UNAVAILABLE, "starting");
    };
    match state.engine.fetch_media(account, &format!("/{rest}")).await {
        Ok(media) => Response::builder()
            .header(header::CONTENT_TYPE, media.mime)
            .header(header::CACHE_CONTROL, "private, max-age=604800")
            .body(media.bytes)
            .unwrap_or_else(|_| status(StatusCode::INTERNAL_SERVER_ERROR, "error")),
        Err(e) => status(StatusCode::BAD_GATEWAY, &e),
    }
}

fn status(code: StatusCode, text: &str) -> Response<Vec<u8>> {
    let mut r = Response::new(text.as_bytes().to_vec());
    *r.status_mut() = code;
    r
}
