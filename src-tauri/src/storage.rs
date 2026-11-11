//! What Agora keeps on disk, by kind, and removing it (Settings → Privacy & storage). Sizes are
//! measured when asked, never kept: nothing here costs memory while Agora runs.

use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

/// Set by "Clear and restart": the webview's cache is emptied at the next start, before the
/// webview opens it (it cannot be removed while in use).
const CLEAR_WEBVIEW_FLAG: &str = "clear-webview-cache";
/// The app's folders under its earlier name (Armos), left behind when the new ones existed first.
const LEGACY_ID: &str = "app.armos.desktop";
/// The webview's own caches; cookies and site storage (sign-in windows) stay.
#[cfg(windows)]
const WEBVIEW_CACHES: [&str; 4] = ["Cache", "Code Cache", "GPUCache", "Service Worker/CacheStorage"];

#[derive(Serialize)]
pub struct Download {
    /// The folder it is kept in (a hash), to delete it by.
    pub id: String,
    pub name: String,
    pub bytes: u64,
    pub modified: i64,
}

#[derive(Serialize)]
pub struct CacheKind {
    /// "images", "pdfs", "videos", "audio" or "other".
    pub kind: &'static str,
    pub bytes: u64,
    pub count: u32,
}

#[derive(Serialize)]
pub struct Usage {
    /// The message archive (database and its journal).
    pub archive: u64,
    pub downloads: Vec<Download>,
    pub cache: Vec<CacheKind>,
    /// The webview's cache, where it can be measured (Windows: WebView2).
    pub webview: Option<u64>,
    /// Leftovers from before the rename.
    pub legacy: u64,
}

pub fn files_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_cache_dir().map_err(|e| e.to_string())?.join("files"))
}

pub fn media_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_cache_dir().map_err(|e| e.to_string())?.join("media"))
}

fn size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).map(|d| d.flatten().map(|e| size(&e.path())).sum()).unwrap_or(0)
}

/// A cached file's kind, from the type on its first line (see `media::store`).
fn cache_kind(file: &Path) -> &'static str {
    use std::io::Read;
    let mut head = [0u8; 128];
    let n = std::fs::File::open(file).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let mime = String::from_utf8_lossy(&head[..n]);
    let mime = mime.split('\n').next().unwrap_or_default();
    match mime {
        m if m.starts_with("image/") => "images",
        "application/pdf" => "pdfs",
        m if m.starts_with("video/") => "videos",
        m if m.starts_with("audio/") => "audio",
        _ => "other",
    }
}

fn legacy_dirs(app: &AppHandle) -> Vec<PathBuf> {
    let p = app.path();
    let mut out: Vec<PathBuf> = [p.app_data_dir(), p.app_local_data_dir(), p.app_cache_dir()]
        .into_iter()
        .flatten()
        .filter_map(|d| d.parent().map(|parent| parent.join(LEGACY_ID)))
        .filter(|d| d.is_dir())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(windows)]
fn webview_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_local_data_dir().ok().map(|d| d.join("EBWebView"))
}

pub fn usage(app: &AppHandle, data_dir: &Path) -> Result<Usage, String> {
    let archive = ["agora.db", "agora.db-wal", "agora.db-shm"].iter().map(|f| size(&data_dir.join(f))).sum();
    let mut downloads = Vec::new();
    for folder in std::fs::read_dir(files_dir(app)?).into_iter().flatten().flatten() {
        // One file per folder; a `.download` is one still arriving.
        for f in std::fs::read_dir(folder.path()).into_iter().flatten().flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            let Ok(meta) = f.metadata() else { continue };
            if name == ".download" || !meta.is_file() {
                continue;
            }
            let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64);
            downloads.push(Download { id: folder.file_name().to_string_lossy().to_string(), name, bytes: meta.len(), modified });
        }
    }
    downloads.sort_by_key(|d| std::cmp::Reverse(d.bytes));
    let mut cache: Vec<CacheKind> = ["images", "pdfs", "videos", "audio", "other"].iter().map(|k| CacheKind { kind: k, bytes: 0, count: 0 }).collect();
    for f in std::fs::read_dir(media_dir(app)?).into_iter().flatten().flatten() {
        let Ok(meta) = f.metadata() else { continue };
        let kind = cache_kind(&f.path());
        if let Some(c) = cache.iter_mut().find(|c| c.kind == kind) {
            c.bytes += meta.len();
            c.count += 1;
        }
    }
    #[cfg(windows)]
    let webview = webview_dir(app).map(|d| WEBVIEW_CACHES.iter().map(|c| size(&d.join("Default").join(c))).sum());
    #[cfg(not(windows))]
    let webview = None;
    let legacy = legacy_dirs(app).iter().map(|d| size(d)).sum();
    Ok(Usage { archive, downloads, cache, webview, legacy })
}

/// One downloaded file (`id`: its folder), or all of them (`None`).
pub fn delete_downloads(app: &AppHandle, id: Option<&str>) -> Result<(), String> {
    let dir = files_dir(app)?;
    match id {
        // Only Agora's own folder names: never a path.
        Some(id) if id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit()) => {
            std::fs::remove_dir_all(dir.join(id)).map_err(|e| e.to_string())
        }
        Some(_) => Err("not an Agora download".into()),
        None if dir.exists() => std::fs::remove_dir_all(&dir).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Cached files of one kind, or all of them (`None`).
pub fn clear_cache(app: &AppHandle, kind: Option<&str>) -> Result<(), String> {
    let dir = media_dir(app)?;
    for f in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if kind.map_or(true, |k| cache_kind(&f.path()) == k) {
            let _ = std::fs::remove_file(f.path());
        }
    }
    crate::media::recount(&dir);
    Ok(())
}

/// Marks the webview's cache for removal at the next start.
pub fn clear_webview_next_start(data_dir: &Path) -> Result<(), String> {
    std::fs::write(data_dir.join(CLEAR_WEBVIEW_FLAG), b"").map_err(|e| e.to_string())
}

/// First thing in `main`: empties the webview's cache when asked to. Tauri opens the window
/// (and the webview its cache) before the app's setup runs, so this cannot wait for the app's
/// paths: these are Tauri's folders on Windows (`identifier` under %APPDATA% and %LOCALAPPDATA%).
#[cfg(windows)]
pub fn clear_webview_if_asked(identifier: &str) {
    let dir = |var: &str| std::env::var_os(var).map(|v| PathBuf::from(v).join(identifier));
    let Some(flag) = dir("APPDATA").map(|d| d.join(CLEAR_WEBVIEW_FLAG)).filter(|f| f.exists()) else { return };
    if let Some(webview) = dir("LOCALAPPDATA").map(|d| d.join("EBWebView").join("Default")) {
        for c in WEBVIEW_CACHES {
            let _ = std::fs::remove_dir_all(webview.join(c));
        }
    }
    let _ = std::fs::remove_file(flag);
}

pub fn delete_legacy(app: &AppHandle) -> Result<(), String> {
    for d in legacy_dirs(app) {
        std::fs::remove_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_files_sorted_by_their_type() {
        let dir = std::env::temp_dir().join(format!("agora-storage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files = [
            ("a", &b"image/png\n\x89PNG"[..], "images"),
            ("b", b"application/pdf\n%PDF", "pdfs"),
            ("c", b"video/mp4\n....", "videos"),
            ("d", b"audio/ogg\nOggS", "audio"),
            ("e", b"text/plain\nhi", "other"),
            ("f", b"", "other"),
        ];
        for (name, data, kind) in files {
            std::fs::write(dir.join(name), data).unwrap();
            assert_eq!(cache_kind(&dir.join(name)), kind, "{name}");
        }
        assert_eq!(size(&dir), files.iter().map(|(_, d, _)| d.len() as u64).sum::<u64>());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
