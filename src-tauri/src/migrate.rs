//! Moves what the app saved under its earlier name (Armos) to the current one, once:
//! the data folder with the archive and boot file, and the WebView folder (layout,
//! collapsed sections). Saved sign-ins move in `agora_core::secrets` when first read.

use std::path::Path;

const LEGACY_ID: &str = "app.armos.desktop";
const LEGACY_FILES: [(&str, &str); 4] =
    [("armos.db", "agora.db"), ("armos.db-wal", "agora.db-wal"), ("armos.db-shm", "agora.db-shm"), ("armos.json", "agora.json")];

/// `dir` is the current app folder (data or local data); its legacy sibling moves into its
/// place when only the legacy one exists.
pub fn legacy_folder(dir: &Path) {
    let Some(old) = dir.parent().map(|p| p.join(LEGACY_ID)) else { return };
    if old.is_dir() && !dir.exists() {
        match std::fs::rename(&old, dir) {
            Ok(()) => tracing::info!("moved {} to {}", old.display(), dir.display()),
            Err(e) => tracing::warn!("could not move {} to {}: {e}", old.display(), dir.display()),
        }
    }
}

/// The archive and boot file under their earlier names.
pub fn legacy_files(dir: &Path) {
    for (old, new) in LEGACY_FILES {
        let (old, new) = (dir.join(old), dir.join(new));
        if old.exists() && !new.exists() {
            if let Err(e) = std::fs::rename(&old, &new) {
                tracing::warn!("could not rename {}: {e}", old.display());
            }
        }
    }
}
