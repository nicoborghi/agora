//! User settings. Stored as JSON in the archive; every field has a default so
//! older settings files keep loading after new options are added.

use crate::model::AccountId;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Popup + tray dot.
    Alert,
    /// Tray dot only.
    Dot,
    /// Nothing; read it when you open the app.
    Silent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// "system" | "light" | "dark"
    pub theme: String,
    /// Accent colour preset the UI offers ("blue", "violet", "teal"...).
    pub accent: String,
    /// "mono" (Noto Emoji line drawings, in the text colour) | "system" (the system's colour emoji).
    pub emoji_style: String,
    /// Text size of the whole window (applied as a zoom): "small" | "normal" (Medium) | "large".
    /// The name predates the option covering more than the sidebar.
    pub sidebar_size: String,
    /// "Spacing" in the UI: sidebar width and the space around its rows, "compact" | "wide".
    pub sidebar_width: String,
    /// Interface font the UI offers ("system" = the default, Lato; "segoe" = Segoe UI).
    pub font: String,
    pub link_previews: bool,
    pub image_cache_mb: u64,
    pub autostart: bool,
    pub update_url: String,
    /// Keep the text a message had before it was edited (off by default: workspace policies).
    pub keep_edit_history: bool,
    /// Marks the build as not yet reviewed for public release (Slack session tokens, archive).
    pub prepublication: bool,
    pub notifications: NotificationSettings,
    /// Agora-only sidebar groups, in display order. They can mix conversations from any account.
    pub groups: Vec<LocalGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalGroup {
    pub id: String,
    pub name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            accent: "blue".into(),
            emoji_style: "mono".into(),
            sidebar_size: "normal".into(),
            sidebar_width: "compact".into(),
            font: "system".into(),
            link_previews: true,
            image_cache_mb: 1024,
            autostart: false,
            update_url: "https://github.com/nicoborghi/agora/releases/latest/download/latest.json".into(),
            keep_edit_history: false,
            prepublication: true,
            notifications: NotificationSettings::default(),
            groups: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationSettings {
    pub dm_level: Level,
    pub channel_level: Level,
    pub popup_seconds: u32,
    /// "default" (gravitational-wave chirp), "beep" or "none".
    pub sound: String,
    /// Notification sound volume, 0–100.
    pub volume: u8,
    /// @mentions (and @channel) notify even in Dot/Silent conversations.
    pub mentions_override: bool,
    pub keywords_override: bool,
    pub keywords: Vec<Keyword>,
    pub dnd: Dnd,
    /// Per-workspace defaults that replace `dm_level` / `channel_level` for that account.
    /// A conversation's own level still wins.
    pub accounts: HashMap<AccountId, AccountLevels>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountLevels {
    pub dm_level: Option<Level>,
    pub channel_level: Option<Level>,
}

impl NotificationSettings {
    /// The level a conversation gets when it has none of its own.
    pub fn default_level(&self, account_id: &str, is_direct: bool) -> Level {
        let own = self.accounts.get(account_id);
        if is_direct {
            own.and_then(|a| a.dm_level).unwrap_or(self.dm_level)
        } else {
            own.and_then(|a| a.channel_level).unwrap_or(self.channel_level)
        }
    }
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            dm_level: Level::Alert,
            channel_level: Level::Dot,
            popup_seconds: 6,
            sound: "default".into(),
            volume: 70,
            mentions_override: true,
            keywords_override: true,
            keywords: Vec::new(),
            dnd: Dnd::default(),
            accounts: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keyword {
    pub word: String,
    /// Empty = every account.
    #[serde(default)]
    pub accounts: Vec<AccountId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Dnd {
    pub enabled: bool,
    pub ranges: Vec<TimeRange>,
    pub allow_dm: bool,
    pub allow_mentions: bool,
    pub allow_keywords: bool,
}

impl Default for Dnd {
    fn default() -> Self {
        Self {
            enabled: false,
            ranges: vec![TimeRange { days: (0..7).collect(), start: 22 * 60, end: 7 * 60 }],
            allow_dm: false,
            allow_mentions: false,
            allow_keywords: false,
        }
    }
}

/// A quiet period. `days` are 0 = Monday … 6 = Sunday and refer to the day the
/// period starts; `start`/`end` are minutes after midnight. `end < start` crosses midnight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub days: Vec<u8>,
    pub start: u16,
    pub end: u16,
}

impl TimeRange {
    pub fn contains(&self, weekday: u8, minute: u16) -> bool {
        let on = |d: u8| self.days.contains(&d);
        let yesterday = (weekday + 6) % 7;
        if self.start == self.end {
            on(weekday)
        } else if self.start < self.end {
            on(weekday) && minute >= self.start && minute < self.end
        } else {
            (on(weekday) && minute >= self.start) || (on(yesterday) && minute < self.end)
        }
    }
}

/// Per-conversation overrides. `None` = follow the global setting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelPrefs {
    pub level: Option<Level>,
    pub tray: Option<bool>,
    pub popup: Option<bool>,
    /// "default" | "none"
    pub sound: Option<String>,
    /// Tray dot colour for this conversation only.
    pub color: Option<String>,
}

impl ChannelPrefs {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overnight_range() {
        // Friday 22:00 → Saturday 07:00
        let r = TimeRange { days: vec![4], start: 22 * 60, end: 7 * 60 };
        assert!(r.contains(4, 23 * 60));
        assert!(r.contains(5, 6 * 60));
        assert!(!r.contains(5, 8 * 60));
        assert!(!r.contains(3, 23 * 60));
    }

    #[test]
    fn day_range() {
        let r = TimeRange { days: vec![0], start: 9 * 60, end: 12 * 60 };
        assert!(r.contains(0, 10 * 60));
        assert!(!r.contains(0, 12 * 60));
    }

    #[test]
    fn old_json_still_loads() {
        let s: Settings = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert_eq!(s.theme, "dark");
        assert!(s.notifications.mentions_override);
    }

    #[test]
    fn workspace_levels_override_globals() {
        let mut n = NotificationSettings::default();
        n.accounts.insert("slack-1".into(), AccountLevels { dm_level: None, channel_level: Some(Level::Silent) });
        assert_eq!(n.default_level("slack-1", false), Level::Silent);
        assert_eq!(n.default_level("slack-1", true), n.dm_level);
        assert_eq!(n.default_level("other", false), n.channel_level);
    }
}
