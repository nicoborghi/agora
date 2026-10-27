//! The local archive (SQLite, encrypted with SQLCipher when enabled).
//!
//! The database *is* the message cache: the engine keeps no message lists in
//! memory, and the UI asks for one page at a time. SQLite's own page cache is
//! capped at 4 MB.

use crate::model::*;
use crate::settings::{ChannelPrefs, Settings};
use crate::search::{fts_query, like_escape, SearchFilter};
use crate::sync::ChannelSyncState;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{0}")]
    Other(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS accounts (
    id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, server_url TEXT NOT NULL,
    team_id TEXT, self_user_id TEXT, icon TEXT, color TEXT NOT NULL,
    position INTEGER NOT NULL, enabled INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS channels (
    account_id TEXT NOT NULL, id TEXT NOT NULL, kind TEXT NOT NULL, name TEXT NOT NULL,
    topic TEXT NOT NULL DEFAULT '', members TEXT NOT NULL DEFAULT '[]',
    last_activity INTEGER NOT NULL DEFAULT 0, last_read INTEGER NOT NULL DEFAULT 0,
    unread INTEGER NOT NULL DEFAULT 0, mentions INTEGER NOT NULL DEFAULT 0,
    starred INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, id)
);
CREATE TABLE IF NOT EXISTS users (
    account_id TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '', avatar_url TEXT, is_bot INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, id)
);
CREATE TABLE IF NOT EXISTS messages (
    account_id TEXT NOT NULL, channel_id TEXT NOT NULL, id TEXT NOT NULL,
    author_id TEXT NOT NULL, ts INTEGER NOT NULL, text TEXT NOT NULL,
    thread_root TEXT, reply_count INTEGER NOT NULL DEFAULT 0, edited_at INTEGER,
    deleted INTEGER NOT NULL DEFAULT 0, extra TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (account_id, channel_id, id)
);
CREATE INDEX IF NOT EXISTS messages_ts ON messages (account_id, channel_id, ts);
CREATE INDEX IF NOT EXISTS messages_thread ON messages (account_id, channel_id, thread_root, ts);
CREATE TABLE IF NOT EXISTS message_versions (
    account_id TEXT NOT NULL, channel_id TEXT NOT NULL, id TEXT NOT NULL,
    edited_at INTEGER NOT NULL, text TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS channel_prefs (
    account_id TEXT NOT NULL, channel_id TEXT NOT NULL, prefs TEXT NOT NULL,
    PRIMARY KEY (account_id, channel_id)
);
";

const MSG_COLS: &str = "m.account_id, m.channel_id, m.id, m.author_id, m.ts, m.text, m.thread_root, \
                        m.reply_count, m.edited_at, m.deleted, m.extra";
const AUTHOR_JOIN: &str = "LEFT JOIN users u ON u.account_id = m.account_id AND u.id = m.author_id";
const AUTHOR_NAME: &str = "COALESCE(NULLIF(u.display_name, ''), u.name, m.author_id)";
const CHANNEL_COLS: &str =
    "account_id, id, kind, name, topic, members, last_activity, last_read, unread, mentions, starred, \
     grp, category, local_group";

/// Parts of a message that are only ever read back whole.
#[derive(Default, Deserialize)]
struct Extra {
    #[serde(default)]
    mentions: Vec<String>,
    #[serde(default)]
    reactions: Vec<Reaction>,
    #[serde(default)]
    attachments: Vec<Attachment>,
    #[serde(default)]
    links: Vec<LinkPreview>,
}

#[derive(Serialize)]
struct ExtraRef<'a> {
    mentions: &'a [String],
    reactions: &'a [Reaction],
    attachments: &'a [Attachment],
    links: &'a [LinkPreview],
}

pub struct Store {
    conn: Mutex<Connection>,
    path: PathBuf,
}

/// Runs a statement that may or may not return rows (PRAGMAs, sqlcipher_export).
fn run(conn: &Connection, sql: &str) -> rusqlite::Result<()> {
    let mut st = conn.prepare(sql)?;
    let mut rows = st.query([])?;
    while rows.next()?.is_some() {}
    Ok(())
}

fn open_conn(path: &Path, key: Option<&str>) -> StoreResult<Connection> {
    let conn = Connection::open(path)?;
    if let Some(k) = key {
        run(&conn, &format!("PRAGMA key = \"x'{k}'\""))?;
    }
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))
        .map_err(|_| {
            StoreError::Other(
                "cannot read the archive: wrong key, or encryption setting does not match the file".into(),
            )
        })?;
    run(&conn, "PRAGMA journal_mode = WAL")?;
    run(&conn, "PRAGMA synchronous = NORMAL")?;
    // 4 MB page cache: the archive is read a page at a time, it needs no big cache.
    run(&conn, "PRAGMA cache_size = -4000")?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Columns added after the first release. Each step is idempotent.
fn migrate(conn: &Connection) -> StoreResult<()> {
    add_column(conn, "channels", "grp", "TEXT NOT NULL DEFAULT ''")?;
    add_column(conn, "channels", "category", "TEXT NOT NULL DEFAULT ''")?;
    add_column(conn, "channels", "local_group", "TEXT")?;
    add_column(conn, "accounts", "icon_image", "TEXT")?;
    if let Err(e) = ensure_fts(conn) {
        // Search then falls back to LIKE, slower but correct.
        tracing::warn!("full-text index unavailable: {e}");
    }
    Ok(())
}

fn add_column(conn: &Connection, table: &str, column: &str, decl: &str) -> StoreResult<()> {
    let exists = conn
        .prepare(&format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"))?
        .exists([column])?;
    if !exists {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

fn has_fts(conn: &Connection) -> bool {
    conn.prepare("SELECT 1 FROM sqlite_master WHERE name = 'messages_fts'")
        .and_then(|mut st| st.exists([]))
        .unwrap_or(false)
}

/// Full-text index over message text. It lives on disk only (about a third of
/// the archive size); triggers keep it in step with every insert, edit and delete.
fn ensure_fts(conn: &Connection) -> rusqlite::Result<()> {
    if has_fts(conn) {
        return Ok(());
    }
    conn.execute_batch(
        "BEGIN;
         CREATE VIRTUAL TABLE messages_fts USING fts5(
             text, content='messages', content_rowid='rowid', tokenize='unicode61 remove_diacritics 2');
         CREATE TRIGGER messages_fts_ai AFTER INSERT ON messages BEGIN
             INSERT INTO messages_fts (rowid, text) VALUES (new.rowid, new.text);
         END;
         CREATE TRIGGER messages_fts_ad AFTER DELETE ON messages BEGIN
             INSERT INTO messages_fts (messages_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
         END;
         CREATE TRIGGER messages_fts_au AFTER UPDATE OF text ON messages BEGIN
             INSERT INTO messages_fts (messages_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
             INSERT INTO messages_fts (rowid, text) VALUES (new.rowid, new.text);
         END;
         INSERT INTO messages_fts (messages_fts) VALUES ('rebuild');
         COMMIT;",
    )
    .inspect_err(|_| {
        let _ = conn.execute_batch("ROLLBACK");
    })
}

/// The index refers to rowids, which an export renumbers: drop it before, rebuild after.
fn drop_fts(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS messages_fts_ai;
         DROP TRIGGER IF EXISTS messages_fts_ad;
         DROP TRIGGER IF EXISTS messages_fts_au;
         DROP TABLE IF EXISTS messages_fts;",
    )
}

/// A message with its author's display name and picture (columns 11 and 12).
pub struct Authored {
    pub message: Message,
    pub name: String,
    pub avatar: Option<String>,
}

fn read_authored(r: &Row) -> rusqlite::Result<Authored> {
    Ok(Authored { message: read_message(r)?, name: r.get(11)?, avatar: r.get(12)? })
}

fn read_message(r: &Row) -> rusqlite::Result<Message> {
    let extra: String = r.get(10)?;
    let extra: Extra = serde_json::from_str(&extra).unwrap_or_default();
    Ok(Message {
        account_id: r.get(0)?,
        channel_id: r.get(1)?,
        id: r.get(2)?,
        author_id: r.get(3)?,
        ts: r.get(4)?,
        text: r.get(5)?,
        thread_root: r.get(6)?,
        reply_count: r.get(7)?,
        edited_at: r.get(8)?,
        deleted: r.get(9)?,
        mentions: extra.mentions,
        reactions: extra.reactions,
        attachments: extra.attachments,
        links: extra.links,
    })
}

fn read_channel(r: &Row) -> rusqlite::Result<Channel> {
    let kind: String = r.get(2)?;
    let members: String = r.get(5)?;
    Ok(Channel {
        account_id: r.get(0)?,
        id: r.get(1)?,
        kind: ChannelKind::parse(&kind),
        name: r.get(3)?,
        topic: r.get(4)?,
        members: serde_json::from_str(&members).unwrap_or_default(),
        last_activity: r.get(6)?,
        last_read: r.get(7)?,
        unread: r.get(8)?,
        mentions: r.get(9)?,
        starred: r.get(10)?,
        starred_synced: false,
        group: r.get(11)?,
        category: r.get(12)?,
        local_group: r.get(13)?,
    })
}

fn read_account(r: &Row) -> rusqlite::Result<Account> {
    let kind: String = r.get(1)?;
    Ok(Account {
        id: r.get(0)?,
        kind: ServiceKind::parse(&kind).unwrap_or(ServiceKind::Demo),
        name: r.get(2)?,
        server_url: r.get(3)?,
        team_id: r.get(4)?,
        self_user_id: r.get(5)?,
        icon: r.get(6)?,
        icon_image: r.get(10)?,
        color: r.get(7)?,
        position: r.get(8)?,
        enabled: r.get(9)?,
    })
}

fn upsert_message_in(db: &Connection, m: &Message) -> StoreResult<()> {
    let extra = serde_json::to_string(&ExtraRef {
        mentions: &m.mentions,
        reactions: &m.reactions,
        attachments: &m.attachments,
        links: &m.links,
    })
    .unwrap_or_else(|_| "{}".into());
    db.prepare_cached(
        "INSERT INTO messages (account_id, channel_id, id, author_id, ts, text, thread_root,
                               reply_count, edited_at, deleted, extra)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (account_id, channel_id, id) DO UPDATE SET
             text = excluded.text,
             edited_at = excluded.edited_at,
             deleted = excluded.deleted,
             reply_count = MAX(reply_count, excluded.reply_count),
             thread_root = COALESCE(excluded.thread_root, thread_root),
             extra = excluded.extra",
    )?
    .execute(params![
        m.account_id,
        m.channel_id,
        m.id,
        m.author_id,
        m.ts,
        m.text,
        m.thread_root,
        m.reply_count,
        m.edited_at,
        m.deleted,
        extra
    ])?;
    Ok(())
}

fn get_message_in(db: &Connection, acc: &str, ch: &str, id: &str) -> StoreResult<Option<Message>> {
    Ok(db
        .query_row(
            &format!("SELECT {MSG_COLS} FROM messages m WHERE m.account_id = ?1 AND m.channel_id = ?2 AND m.id = ?3"),
            params![acc, ch, id],
            read_message,
        )
        .optional()?)
}

impl Store {
    /// `key` is 64 hex chars; `None` opens the archive unencrypted.
    pub fn open(path: &Path, key: Option<&str>) -> StoreResult<Self> {
        Ok(Self { conn: Mutex::new(open_conn(path, key)?), path: path.to_path_buf() })
    }

    /// Whether this build links SQLCipher (false in a `plain-sqlite` build).
    pub fn cipher_available() -> bool {
        Connection::open_in_memory()
            .and_then(|c| c.query_row("PRAGMA cipher_version", [], |r| r.get::<_, String>(0)).optional())
            .ok()
            .flatten()
            .is_some()
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Re-encrypts (or decrypts) the archive in place with `sqlcipher_export`.
    pub fn convert(&self, old_key: Option<&str>, new_key: Option<&str>) -> StoreResult<()> {
        let mut db = self.db();
        let tmp = self.path.with_extension("db.convert");
        let _ = std::fs::remove_file(&tmp);
        let target = tmp.to_string_lossy().replace('\'', "''");
        let key_sql = match new_key {
            Some(k) => format!("\"x'{k}'\""),
            None => "''".into(),
        };
        // Rebuilt by `open_conn` on the new file (or right below if the export fails).
        drop_fts(&db)?;
        db.execute_batch(&format!("ATTACH DATABASE '{target}' AS conv KEY {key_sql};"))?;
        let exported = run(&db, "SELECT sqlcipher_export('conv')");
        db.execute_batch("DETACH DATABASE conv;")?;
        if let Err(e) = exported {
            let _ = ensure_fts(&db);
            return Err(e.into());
        }

        // Close the live connection before swapping files (WAL is checkpointed on close).
        drop(std::mem::replace(&mut *db, Connection::open_in_memory()?));
        for ext in ["db-wal", "db-shm"] {
            let _ = std::fs::remove_file(self.path.with_extension(ext));
        }
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            *db = open_conn(&self.path, old_key)?;
            return Err(StoreError::Other(format!("could not replace the archive: {e}")));
        }
        *db = open_conn(&self.path, new_key)?;
        Ok(())
    }

    // ------------------------------------------------------------ settings

    pub fn get_kv(&self, key: &str) -> StoreResult<Option<String>> {
        Ok(self
            .db()
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_kv(&self, key: &str, value: &str) -> StoreResult<()> {
        self.db().execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn load_settings(&self) -> Settings {
        let mut s: Settings = self
            .get_kv("settings")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Builds before the repository existed saved a placeholder feed.
        if s.update_url.contains("/OWNER/") {
            s.update_url = Settings::default().update_url;
        }
        s
    }

    pub fn save_settings(&self, s: &Settings) -> StoreResult<()> {
        let json = serde_json::to_string(s).map_err(|e| StoreError::Other(e.to_string()))?;
        self.set_kv("settings", &json)
    }

    // ------------------------------------------------------------ accounts

    pub fn list_accounts(&self) -> StoreResult<Vec<Account>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT id, kind, name, server_url, team_id, self_user_id, icon, color, position, enabled, icon_image
             FROM accounts ORDER BY position, rowid",
        )?;
        let rows = st.query_map([], read_account)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_account(&self, id: &str) -> StoreResult<Option<Account>> {
        Ok(self
            .db()
            .query_row(
                "SELECT id, kind, name, server_url, team_id, self_user_id, icon, color, position, enabled, icon_image
                 FROM accounts WHERE id = ?1",
                [id],
                read_account,
            )
            .optional()?)
    }

    pub fn upsert_account(&self, a: &Account) -> StoreResult<()> {
        self.db().execute(
            "INSERT INTO accounts (id, kind, name, server_url, team_id, self_user_id, icon, color, position, enabled,
                                   icon_image)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (id) DO UPDATE SET
                 name = excluded.name, server_url = excluded.server_url, team_id = excluded.team_id,
                 self_user_id = COALESCE(excluded.self_user_id, self_user_id), icon = excluded.icon,
                 color = excluded.color, position = excluded.position, enabled = excluded.enabled,
                 icon_image = excluded.icon_image",
            params![
                a.id,
                a.kind.as_str(),
                a.name,
                a.server_url,
                a.team_id,
                a.self_user_id,
                a.icon,
                a.color,
                a.position,
                a.enabled,
                a.icon_image
            ],
        )?;
        Ok(())
    }

    /// Stores an automatically fetched logo unless the user picked one meanwhile.
    pub fn set_auto_icon(&self, account_id: &str, data_url: &str) -> StoreResult<bool> {
        let n = self.db().execute(
            "UPDATE accounts SET icon_image = ?2 WHERE id = ?1 AND icon_image IS NULL",
            params![account_id, data_url],
        )?;
        Ok(n > 0)
    }

    pub fn set_self_user(&self, account_id: &str, user_id: &str) -> StoreResult<()> {
        self.db()
            .execute("UPDATE accounts SET self_user_id = ?2 WHERE id = ?1", params![account_id, user_id])?;
        Ok(())
    }

    pub fn set_account_positions(&self, ids: &[String]) -> StoreResult<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for (i, id) in ids.iter().enumerate() {
            tx.execute("UPDATE accounts SET position = ?2 WHERE id = ?1", params![id, i as i64])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes the account; with `purge` its archived conversations go too.
    pub fn delete_account(&self, id: &str, purge: bool) -> StoreResult<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM accounts WHERE id = ?1", [id])?;
        if purge {
            for table in ["channels", "users", "messages", "message_versions", "channel_prefs"] {
                tx.execute(&format!("DELETE FROM {table} WHERE account_id = ?1"), [id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ------------------------------------------------------------ channels

    pub fn upsert_channels(&self, channels: &[Channel]) -> StoreResult<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for c in channels {
            let members = serde_json::to_string(&c.members).unwrap_or_else(|_| "[]".into());
            // `starred` is overwritten only when the service syncs favorites (?11);
            // `local_group` is Agora-only and never touched here.
            tx.prepare_cached(
                "INSERT INTO channels (account_id, id, kind, name, topic, members, last_activity,
                                       last_read, unread, mentions, starred, grp, category)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11 AND ?12, ?13, ?14)
                 ON CONFLICT (account_id, id) DO UPDATE SET
                     kind = excluded.kind, name = excluded.name, topic = excluded.topic,
                     members = excluded.members,
                     last_activity = MAX(last_activity, excluded.last_activity),
                     last_read = MAX(last_read, excluded.last_read),
                     unread = excluded.unread, mentions = excluded.mentions,
                     starred = CASE WHEN ?11 THEN excluded.starred ELSE starred END,
                     grp = excluded.grp, category = excluded.category",
            )?
            .execute(params![
                c.account_id,
                c.id,
                c.kind.as_str(),
                c.name,
                c.topic,
                members,
                c.last_activity,
                c.last_read,
                c.unread,
                c.mentions,
                c.starred_synced,
                c.starred,
                c.group,
                c.category
            ])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_channels(&self, account_id: Option<&str>) -> StoreResult<Vec<Channel>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {CHANNEL_COLS} FROM channels
             WHERE ?1 IS NULL OR account_id = ?1
             ORDER BY last_activity DESC"
        ))?;
        let rows = st.query_map([account_id], read_channel)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn unread_channels(&self) -> StoreResult<Vec<Channel>> {
        let db = self.db();
        let mut st = db.prepare(&format!("SELECT {CHANNEL_COLS} FROM channels WHERE unread > 0 OR mentions > 0"))?;
        let rows = st.query_map([], read_channel)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_channel(&self, acc: &str, id: &str) -> StoreResult<Option<Channel>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {CHANNEL_COLS} FROM channels WHERE account_id = ?1 AND id = ?2"),
                params![acc, id],
                read_channel,
            )
            .optional()?)
    }

    pub fn set_starred(&self, acc: &str, id: &str, starred: bool) -> StoreResult<()> {
        self.db().execute(
            "UPDATE channels SET starred = ?3 WHERE account_id = ?1 AND id = ?2",
            params![acc, id, starred],
        )?;
        Ok(())
    }

    pub fn set_local_group(&self, acc: &str, id: &str, group: Option<&str>) -> StoreResult<()> {
        self.db().execute(
            "UPDATE channels SET local_group = ?3 WHERE account_id = ?1 AND id = ?2",
            params![acc, id, group],
        )?;
        Ok(())
    }

    /// Conversations of a deleted Agora group go back to their normal section.
    pub fn clear_missing_groups(&self, keep: &[String]) -> StoreResult<usize> {
        let keep = serde_json::to_string(keep).unwrap_or_else(|_| "[]".into());
        Ok(self.db().execute(
            "UPDATE channels SET local_group = NULL
             WHERE local_group IS NOT NULL AND local_group NOT IN (SELECT value FROM json_each(?1))",
            [keep],
        )?)
    }

    pub fn apply_sections(&self, acc: &str, sections: &[ChannelSection]) -> StoreResult<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for s in sections {
            tx.prepare_cached(
                "UPDATE channels SET starred = COALESCE(?3, starred), category = COALESCE(?4, category)
                 WHERE account_id = ?1 AND id = ?2",
            )?
            .execute(params![acc, s.channel_id, s.starred, s.category])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn touch_channel(&self, acc: &str, id: &str, ts: Millis) -> StoreResult<()> {
        self.db().execute(
            "UPDATE channels SET last_activity = MAX(last_activity, ?3) WHERE account_id = ?1 AND id = ?2",
            params![acc, id, ts],
        )?;
        Ok(())
    }

    pub fn bump_unread(&self, acc: &str, id: &str, mention: bool) -> StoreResult<()> {
        self.db().execute(
            "UPDATE channels SET unread = unread + 1, mentions = mentions + ?3
             WHERE account_id = ?1 AND id = ?2",
            params![acc, id, mention as i64],
        )?;
        Ok(())
    }

    pub fn mark_read_local(&self, acc: &str, id: &str, ts: Millis) -> StoreResult<()> {
        self.db().execute(
            "UPDATE channels SET unread = 0, mentions = 0, last_read = MAX(last_read, ?3)
             WHERE account_id = ?1 AND id = ?2",
            params![acc, id, ts],
        )?;
        Ok(())
    }

    pub fn sync_states(&self, acc: &str) -> StoreResult<Vec<ChannelSyncState>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT c.id, c.kind, c.starred, c.unread, c.last_activity,
                    (SELECT MAX(m.ts) FROM messages m WHERE m.account_id = c.account_id AND m.channel_id = c.id)
             FROM channels c WHERE c.account_id = ?1",
        )?;
        let rows = st.query_map([acc], |r| {
            let kind: String = r.get(1)?;
            Ok(ChannelSyncState {
                id: r.get(0)?,
                kind: ChannelKind::parse(&kind),
                starred: r.get(2)?,
                unread: r.get(3)?,
                last_activity: r.get(4)?,
                local_latest: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ------------------------------------------------------------ users

    pub fn upsert_users(&self, users: &[User]) -> StoreResult<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for u in users {
            tx.prepare_cached(
                "INSERT INTO users (account_id, id, name, display_name, avatar_url, is_bot)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (account_id, id) DO UPDATE SET
                     name = excluded.name, display_name = excluded.display_name,
                     avatar_url = excluded.avatar_url, is_bot = excluded.is_bot",
            )?
            .execute(params![u.account_id, u.id, u.name, u.display_name, u.avatar_url, u.is_bot])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn user_name(&self, acc: &str, id: &str) -> StoreResult<String> {
        Ok(self
            .db()
            .query_row(
                "SELECT COALESCE(NULLIF(display_name, ''), name) FROM users WHERE account_id = ?1 AND id = ?2",
                params![acc, id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| id.to_string()))
    }

    pub fn get_user(&self, acc: &str, id: &str) -> StoreResult<Option<User>> {
        Ok(self
            .db()
            .query_row(
                "SELECT account_id, id, name, display_name, avatar_url, is_bot FROM users
                 WHERE account_id = ?1 AND id = ?2",
                params![acc, id],
                |r| {
                    Ok(User {
                        account_id: r.get(0)?,
                        id: r.get(1)?,
                        name: r.get(2)?,
                        display_name: r.get(3)?,
                        avatar_url: r.get(4)?,
                        is_bot: r.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// People whose handle starts with `query` or whose name contains it; handles first.
    pub fn find_users(&self, acc: &str, query: &str, limit: u32) -> StoreResult<Vec<User>> {
        let db = self.db();
        let q = like_escape(query.trim_start_matches('@'));
        let mut st = db.prepare_cached(
            "SELECT account_id, id, name, display_name, avatar_url, is_bot FROM users
             WHERE account_id = ?1 AND (name LIKE ?2 ESCAPE '\\' OR display_name LIKE ?3 ESCAPE '\\')
             ORDER BY (name LIKE ?2 ESCAPE '\\') DESC, is_bot, name LIMIT ?4",
        )?;
        let rows = st.query_map(params![acc, format!("{q}%"), format!("%{q}%"), limit], |r| {
            Ok(User {
                account_id: r.get(0)?,
                id: r.get(1)?,
                name: r.get(2)?,
                display_name: r.get(3)?,
                avatar_url: r.get(4)?,
                is_bot: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Messages matching a parsed search, newest first, with author display names.
    pub fn search(&self, q: &SearchFilter) -> StoreResult<Vec<Authored>> {
        use rusqlite::types::Value as V;
        let db = self.db();
        let fts = has_fts(&db);
        let mut sql = format!("SELECT {MSG_COLS}, {AUTHOR_NAME}, u.avatar_url FROM messages m {AUTHOR_JOIN}");
        let mut args: Vec<V> = Vec::new();
        let mut cond = vec!["m.deleted = 0".to_string()];
        if !q.words.is_empty() || !q.phrases.is_empty() {
            if fts {
                sql.push_str(" JOIN messages_fts f ON f.rowid = m.rowid");
                cond.push("messages_fts MATCH ?".into());
                args.push(V::Text(fts_query(&q.words, &q.phrases)));
            } else {
                for t in q.words.iter().chain(&q.phrases) {
                    cond.push("m.text LIKE ? ESCAPE '\\'".into());
                    args.push(V::Text(format!("%{}%", like_escape(t))));
                }
            }
        }
        if let Some(acc) = &q.account {
            cond.push("m.account_id = ?".into());
            args.push(V::Text(acc.clone()));
        }
        if let Some(channels) = &q.channels {
            let keys: Vec<String> = channels.iter().map(|(a, c)| format!("{a}\u{1}{c}")).collect();
            cond.push("(m.account_id || char(1) || m.channel_id) IN (SELECT value FROM json_each(?))".into());
            args.push(V::Text(serde_json::to_string(&keys).unwrap_or_else(|_| "[]".into())));
        }
        if !q.from.is_empty() {
            let mut any = Vec::new();
            for f in &q.from {
                any.push("u.name LIKE ? ESCAPE '\\' OR u.display_name LIKE ? ESCAPE '\\'");
                let pat = format!("%{}%", like_escape(f));
                args.push(V::Text(pat.clone()));
                args.push(V::Text(pat));
            }
            cond.push(format!("({})", any.join(" OR ")));
        }
        if let Some(after) = q.after {
            cond.push("m.ts >= ?".into());
            args.push(V::Integer(after));
        }
        if let Some(before) = q.before {
            cond.push("m.ts < ?".into());
            args.push(V::Integer(before));
        }
        sql.push_str(&format!(" WHERE {} ORDER BY m.ts DESC LIMIT {}", cond.join(" AND "), q.limit.min(200)));
        let mut st = db.prepare(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(args), |r| read_authored(r))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Id of the top-level message at exactly `ts` (for services that page by id).
    pub fn message_id_at(&self, acc: &str, ch: &str, ts: Millis) -> StoreResult<Option<String>> {
        Ok(self
            .db()
            .query_row(
                "SELECT id FROM messages
                 WHERE account_id = ?1 AND channel_id = ?2 AND ts = ?3 AND thread_root IS NULL LIMIT 1",
                params![acc, ch, ts],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The ids in `ids` that have no user row yet, without duplicates.
    pub fn missing_users(&self, acc: &str, ids: &[String]) -> StoreResult<Vec<String>> {
        let db = self.db();
        let mut st = db.prepare_cached("SELECT 1 FROM users WHERE account_id = ?1 AND id = ?2")?;
        let mut out: Vec<String> = Vec::new();
        for id in ids {
            if !out.contains(id) && !st.exists(params![acc, id])? {
                out.push(id.clone());
            }
        }
        Ok(out)
    }

    /// (id, display name, picture URL) for each id; unknown people get their id as name.
    pub fn user_cards(&self, acc: &str, ids: &[String]) -> StoreResult<Vec<(String, String, Option<String>)>> {
        let db = self.db();
        let mut st = db.prepare_cached(
            "SELECT COALESCE(NULLIF(display_name, ''), name), avatar_url FROM users WHERE account_id = ?1 AND id = ?2",
        )?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let row: Option<(String, Option<String>)> =
                st.query_row(params![acc, id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let (name, avatar) = row.unwrap_or_else(|| (id.clone(), None));
            out.push((id.clone(), name, avatar));
        }
        Ok(out)
    }

    // ------------------------------------------------------------ messages

    pub fn get_message(&self, acc: &str, ch: &str, id: &str) -> StoreResult<Option<Message>> {
        get_message_in(&self.db(), acc, ch, id)
    }

    pub fn upsert_message(&self, m: &Message) -> StoreResult<()> {
        upsert_message_in(&self.db(), m)
    }

    pub fn upsert_messages(&self, msgs: &[Message]) -> StoreResult<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for m in msgs {
            upsert_message_in(&tx, m)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Top-level messages older than `before`, oldest first, with author display names.
    pub fn messages(
        &self,
        acc: &str,
        ch: &str,
        before: Option<Millis>,
        limit: u32,
    ) -> StoreResult<Vec<Authored>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {MSG_COLS}, {AUTHOR_NAME}, u.avatar_url FROM messages m {AUTHOR_JOIN}
             WHERE m.account_id = ?1 AND m.channel_id = ?2 AND m.thread_root IS NULL AND m.ts < ?3
             ORDER BY m.ts DESC LIMIT ?4"
        ))?;
        let rows = st.query_map(params![acc, ch, before.unwrap_or(i64::MAX), limit], |r| {
            read_authored(r)
        })?;
        let mut out = rows.collect::<Result<Vec<_>, _>>()?;
        out.reverse();
        Ok(out)
    }

    /// Root message followed by its replies, oldest first.
    pub fn thread(&self, acc: &str, ch: &str, root: &str) -> StoreResult<Vec<Authored>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {MSG_COLS}, {AUTHOR_NAME}, u.avatar_url FROM messages m {AUTHOR_JOIN}
             WHERE m.account_id = ?1 AND m.channel_id = ?2 AND (m.id = ?3 OR m.thread_root = ?3)
             ORDER BY (m.id = ?3) DESC, m.ts"
        ))?;
        let rows = st.query_map(params![acc, ch, root], |r| read_authored(r))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn latest_message(&self, acc: &str, ch: &str) -> StoreResult<Option<Message>> {
        Ok(self
            .db()
            .query_row(
                &format!(
                    "SELECT {MSG_COLS} FROM messages m WHERE m.account_id = ?1 AND m.channel_id = ?2
                     ORDER BY m.ts DESC LIMIT 1"
                ),
                params![acc, ch],
                read_message,
            )
            .optional()?)
    }

    /// The service deleted it: drop the content, keep a tombstone so the gap is visible.
    pub fn delete_message(&self, acc: &str, ch: &str, id: &str) -> StoreResult<()> {
        self.db().execute(
            "UPDATE messages SET deleted = 1, text = '', extra = '{}'
             WHERE account_id = ?1 AND channel_id = ?2 AND id = ?3",
            params![acc, ch, id],
        )?;
        Ok(())
    }

    pub fn bump_reply_count(&self, acc: &str, ch: &str, root: &str) -> StoreResult<()> {
        self.db().execute(
            "UPDATE messages SET reply_count = reply_count + 1
             WHERE account_id = ?1 AND channel_id = ?2 AND id = ?3",
            params![acc, ch, root],
        )?;
        Ok(())
    }

    pub fn apply_reaction(
        &self,
        acc: &str,
        ch: &str,
        id: &str,
        emoji: &str,
        user: &str,
        add: bool,
    ) -> StoreResult<Option<Message>> {
        let db = self.db();
        let Some(mut m) = get_message_in(&db, acc, ch, id)? else { return Ok(None) };
        m.apply_reaction(emoji, user, add);
        upsert_message_in(&db, &m)?;
        Ok(Some(m))
    }

    /// Only called when "keep pre-edit versions" is on.
    pub fn save_version(&self, old: &Message) -> StoreResult<()> {
        self.db().execute(
            "INSERT INTO message_versions (account_id, channel_id, id, edited_at, text)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![old.account_id, old.channel_id, old.id, old.edited_at.unwrap_or(old.ts), old.text],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------ per-conversation prefs

    pub fn channel_prefs(&self, acc: &str, ch: &str) -> StoreResult<ChannelPrefs> {
        let json: Option<String> = self
            .db()
            .query_row(
                "SELECT prefs FROM channel_prefs WHERE account_id = ?1 AND channel_id = ?2",
                params![acc, ch],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default())
    }

    pub fn set_channel_prefs(&self, acc: &str, ch: &str, prefs: &ChannelPrefs) -> StoreResult<()> {
        let db = self.db();
        if prefs.is_default() {
            db.execute(
                "DELETE FROM channel_prefs WHERE account_id = ?1 AND channel_id = ?2",
                params![acc, ch],
            )?;
        } else {
            let json = serde_json::to_string(prefs).map_err(|e| StoreError::Other(e.to_string()))?;
            db.execute(
                "INSERT INTO channel_prefs (account_id, channel_id, prefs) VALUES (?1, ?2, ?3)
                 ON CONFLICT (account_id, channel_id) DO UPDATE SET prefs = excluded.prefs",
                params![acc, ch, json],
            )?;
        }
        Ok(())
    }

    pub fn all_channel_prefs(&self) -> StoreResult<HashMap<(String, String), ChannelPrefs>> {
        let db = self.db();
        let mut st = db.prepare("SELECT account_id, channel_id, prefs FROM channel_prefs")?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (a, c, j) = row?;
            out.insert((a, c), serde_json::from_str(&j).unwrap_or_default());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Store {
        let path = std::env::temp_dir().join(format!("agora-test-{:x}.db", fastrand::u64(..)));
        Store::open(&path, None).unwrap()
    }

    fn msg(id: &str, ts: Millis, root: Option<&str>) -> Message {
        Message {
            account_id: "a".into(),
            channel_id: "c".into(),
            id: id.into(),
            author_id: "u1".into(),
            ts,
            text: format!("text {id}"),
            thread_root: root.map(str::to_string),
            reply_count: 0,
            edited_at: None,
            deleted: false,
            mentions: vec![],
            reactions: vec![],
            attachments: vec![],
            links: vec![],
        }
    }

    #[test]
    fn messages_page_and_thread() {
        let s = temp_store();
        s.upsert_messages(&[msg("1", 10, None), msg("2", 20, None), msg("3", 30, Some("1")), msg("4", 40, None)])
            .unwrap();
        let page: Vec<String> = s.messages("a", "c", None, 2).unwrap().into_iter().map(|a| a.message.id).collect();
        assert_eq!(page, vec!["2", "4"]);
        let older: Vec<String> = s.messages("a", "c", Some(20), 10).unwrap().into_iter().map(|a| a.message.id).collect();
        assert_eq!(older, vec!["1"]);
        let thread: Vec<String> = s.thread("a", "c", "1").unwrap().into_iter().map(|a| a.message.id).collect();
        assert_eq!(thread, vec!["1", "3"]);
    }

    #[test]
    fn starred_survives_service_upsert() {
        let s = temp_store();
        let mut c = Channel::new("a", "c", ChannelKind::Public, "general");
        c.last_activity = 5;
        c.unread = 1;
        s.upsert_channels(&[c.clone()]).unwrap();
        s.set_starred("a", "c", true).unwrap();
        s.set_local_group("a", "c", Some("g1")).unwrap();
        s.upsert_channels(&[Channel { last_activity: 3, ..c.clone() }]).unwrap();
        let got = s.get_channel("a", "c").unwrap().unwrap();
        assert!(got.starred);
        assert_eq!(got.last_activity, 5);
        assert_eq!(got.local_group.as_deref(), Some("g1"));

        // A service that syncs favorites wins over the archive.
        s.upsert_channels(&[Channel { starred_synced: true, starred: false, ..c }]).unwrap();
        assert!(!s.get_channel("a", "c").unwrap().unwrap().starred);
        s.apply_sections("a", &[ChannelSection { channel_id: "c".into(), starred: Some(true), category: None }])
            .unwrap();
        assert!(s.get_channel("a", "c").unwrap().unwrap().starred);

        s.clear_missing_groups(&["other".into()]).unwrap();
        assert_eq!(s.get_channel("a", "c").unwrap().unwrap().local_group, None);
    }

    #[test]
    fn search_words_phrases_and_filters() {
        let s = temp_store();
        let mut a = msg("1", 10, None);
        a.text = "The calibration pipeline finished".into();
        let mut b = msg("2", 20, None);
        b.text = "noise budget review tomorrow".into();
        let mut c = msg("3", 30, None);
        c.text = "budget noise is different".into();
        s.upsert_messages(&[a, b, c]).unwrap();
        let ids = |f: SearchFilter| -> Vec<String> {
            s.search(&SearchFilter { limit: 50, ..f }).unwrap().into_iter().map(|a| a.message.id).collect()
        };
        assert_eq!(ids(SearchFilter { words: vec!["calib".into()], ..Default::default() }), vec!["1"]);
        assert_eq!(ids(SearchFilter { phrases: vec!["noise budget".into()], ..Default::default() }), vec!["2"]);
        assert_eq!(ids(SearchFilter { words: vec!["budget".into()], ..Default::default() }), vec!["3", "2"]);
        assert_eq!(ids(SearchFilter { words: vec!["budget".into()], before: Some(25), ..Default::default() }), vec!["2"]);
        let only = Some(vec![("a".to_string(), "other".to_string())]);
        assert!(ids(SearchFilter { words: vec!["budget".into()], channels: only, ..Default::default() }).is_empty());
        // Edits and deletions reach the index through the triggers.
        s.delete_message("a", "c", "2").unwrap();
        assert_eq!(ids(SearchFilter { words: vec!["budget".into()], ..Default::default() }), vec!["3"]);
    }
}
