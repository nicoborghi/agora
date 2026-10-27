//! The only plain-text config file: settings that must be known before the
//! (possibly encrypted) archive can be opened.

use agora_core::store::Store;
use serde::{Deserialize, Serialize};
use std::path::Path;

const FILE: &str = "agora.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootConfig {
    pub db_encrypted: bool,
}

impl BootConfig {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            // First run: encrypt by default whenever this build has SQLCipher.
            .unwrap_or_else(|| Self { db_encrypted: Store::cipher_available() })
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(dir.join(FILE), json)
    }
}
