//! The `agora` URI scheme: serves images that need the account's credentials
//! (`http://agora.localhost/<account id>/<service path>`). Each picture is kept in a disk cache
//! (Settings → Privacy & storage → Image cache limit), so it is downloaded once; the oldest
//! pictures go when the cache is over its limit.

use crate::AppState;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{AppHandle, Manager, UriSchemeContext, UriSchemeResponder};

pub const SCHEME: &str = "agora";
/// The earlier name's scheme: picture links saved in the archive before the rename still load.
pub const LEGACY_SCHEME: &str = "armos";

/// Bytes in the cache folder, counted once at the first picture and kept up to date.
static CACHED: OnceLock<AtomicU64> = OnceLock::new();

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
    match load(app, account, &format!("/{rest}")).await {
        Ok((mime, bytes)) => ok(mime, bytes),
        Err(e) => status(StatusCode::BAD_GATEWAY, &e),
    }
}

/// A service file (`path` starts with "/"), from the disk cache or fetched signed in and cached.
pub async fn load(app: &AppHandle, account: &str, path: &str) -> Result<(String, Vec<u8>), String> {
    let state = app.try_state::<AppState>().ok_or("starting")?;
    let file = cache_dir(app).map(|d| d.join(cache_key(account, path.trim_start_matches('/'))));
    if let Some(found) = file.as_deref().and_then(read_cached) {
        return Ok(found);
    }
    let media = state.engine.fetch_media(account, path).await?;
    let Some(file) = file else { return Ok((media.mime, media.bytes)) };
    // Written to the cache first, then the same bytes answer: no second copy of large files.
    let limit = state.engine.settings().image_cache_mb.saturating_mul(1_000_000);
    let mime = media.mime.clone();
    let bytes = media.bytes;
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        store(&file, &mime, &bytes, limit);
        bytes
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok((media.mime, bytes))
}

fn cache_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_cache_dir().ok().map(|d| d.join("media"))
}

fn cache_key(account: &str, path: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (account, path).hash(&mut h);
    format!("{:016x}", h.finish())
}

/// A cached picture: its type on the first line, then the bytes. Reading it marks it as used.
fn read_cached(file: &Path) -> Option<(String, Vec<u8>)> {
    let data = std::fs::read(file).ok()?;
    let split = data.iter().position(|b| *b == b'\n')?;
    let mime = String::from_utf8(data[..split].to_vec()).ok()?;
    if let Ok(f) = std::fs::File::options().write(true).open(file) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    Some((mime, data[split + 1..].to_vec()))
}

fn store(file: &Path, mime: &str, bytes: &[u8], limit: u64) {
    let Some(dir) = file.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let total = CACHED.get_or_init(|| AtomicU64::new(folder_size(dir)));
    let mut data = Vec::with_capacity(mime.len() + 1 + bytes.len());
    data.extend_from_slice(mime.as_bytes());
    data.push(b'\n');
    data.extend_from_slice(bytes);
    if std::fs::write(file, &data).is_ok() && total.fetch_add(data.len() as u64, Ordering::Relaxed) + data.len() as u64 > limit {
        prune(dir, limit, total);
    }
}

fn folder_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir).map(|d| d.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0)
}

/// Removes the least recently used pictures until the cache is at 90% of its limit.
fn prune(dir: &Path, limit: u64, total: &AtomicU64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    files.sort_by_key(|(t, ..)| *t);
    let mut size: u64 = files.iter().map(|(_, n, _)| n).sum();
    for (_, n, path) in files {
        if size <= limit / 10 * 9 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            size -= n;
        }
    }
    total.store(size, Ordering::Relaxed);
}

fn ok(mime: String, bytes: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "private, max-age=604800")
        .body(bytes)
        .unwrap_or_else(|_| status(StatusCode::INTERNAL_SERVER_ERROR, "error"))
}

fn status(code: StatusCode, text: &str) -> Response<Vec<u8>> {
    let mut r = Response::new(text.as_bytes().to_vec());
    *r.status_mut() = code;
    r
}
