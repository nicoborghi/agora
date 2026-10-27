//! Windows Credential Manager access. Tokens and the database key never touch disk in clear.

const SERVICE: &str = "Agora";
/// The app's earlier name: entries saved under it are moved to `SERVICE` when first read.
const LEGACY_SERVICE: &str = "Armos";
const DB_KEY: &str = "db-key";

fn entry(key: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, key).map_err(|e| e.to_string())
}

/// A secret still under the earlier name: copied to the current one, then removed there.
fn take_legacy(key: &str) -> Result<Option<String>, String> {
    let old = keyring::Entry::new(LEGACY_SERVICE, key).map_err(|e| e.to_string())?;
    match old.get_password() {
        Ok(v) => {
            set(key, &v)?;
            let _ = old.delete_credential();
            Ok(Some(v))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

pub fn account_key(account_id: &str) -> String {
    format!("account:{account_id}")
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    entry(key)?.set_password(value).map_err(|e| e.to_string())
}

pub fn get(key: &str) -> Result<Option<String>, String> {
    match entry(key)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => take_legacy(key),
        Err(e) => Err(e.to_string()),
    }
}

pub fn delete(key: &str) -> Result<(), String> {
    if let Ok(old) = keyring::Entry::new(LEGACY_SERVICE, key) {
        let _ = old.delete_credential();
    }
    match entry(key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// The SQLCipher key (64 hex chars), created on first use.
pub fn db_key() -> Result<String, String> {
    if let Some(k) = get(DB_KEY)? {
        return Ok(k);
    }
    let mut raw = [0u8; 32];
    getrandom::getrandom(&mut raw).map_err(|e| e.to_string())?;
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    set(DB_KEY, &hex)?;
    Ok(hex)
}
