//! Which conversations to catch up after a (re)connect, and in what order.
//!
//! Conversations without news are skipped entirely; DMs, groups and starred
//! conversations go first so the important gaps close before rate limits bite.

use crate::model::{ChannelKind, Millis};

/// On the very first sync, only conversations active in this window are fetched;
/// older ones are loaded lazily when opened.
pub const FIRST_SYNC_WINDOW: Millis = 14 * 24 * 3600 * 1000;

#[derive(Debug, Clone)]
pub struct ChannelSyncState {
    pub id: String,
    pub kind: ChannelKind,
    pub starred: bool,
    pub unread: u32,
    /// Last activity as reported by the service (or seen locally).
    pub last_activity: Millis,
    /// Newest message already in the archive.
    pub local_latest: Option<Millis>,
}

pub fn has_news(c: &ChannelSyncState, now: Millis) -> bool {
    match c.local_latest {
        Some(local) => c.unread > 0 || c.last_activity > local,
        None => {
            c.unread > 0
                || c.starred
                || (c.last_activity > 0 && now - c.last_activity < FIRST_SYNC_WINDOW)
        }
    }
}

pub fn catch_up_order(states: &[ChannelSyncState], now: Millis) -> Vec<String> {
    let mut picked: Vec<(u8, Millis, &str)> = states
        .iter()
        .filter(|c| has_news(c, now))
        .map(|c| {
            let prio = if c.kind.is_direct() || c.starred { 0 } else { 1 };
            (prio, c.last_activity, c.id.as_str())
        })
        .collect();
    picked.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    picked.into_iter().map(|(_, _, id)| id.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(id: &str, kind: ChannelKind, starred: bool, last: Millis, local: Option<Millis>) -> ChannelSyncState {
        ChannelSyncState { id: id.into(), kind, starred, unread: 0, last_activity: last, local_latest: local }
    }

    #[test]
    fn order_and_skip() {
        let now = 1_000_000_000;
        let states = vec![
            st("quiet", ChannelKind::Public, false, 500, Some(500)),
            st("busy", ChannelKind::Public, false, 900, Some(100)),
            st("dm", ChannelKind::Dm, false, 800, Some(100)),
            st("star", ChannelKind::Public, true, 700, Some(100)),
            st("newer", ChannelKind::Public, false, 950, Some(100)),
        ];
        assert_eq!(catch_up_order(&states, now), vec!["dm", "star", "newer", "busy"]);
    }

    #[test]
    fn first_sync_window() {
        let now = 100 * 24 * 3600 * 1000;
        let old = st("old", ChannelKind::Public, false, now - FIRST_SYNC_WINDOW - 1, None);
        let recent = st("recent", ChannelKind::Public, false, now - 1000, None);
        assert!(!has_news(&old, now));
        assert!(has_news(&recent, now));
    }
}
