//! Notification rules: level per conversation, mention/keyword overrides, DND.
//! Pure functions, so every combination can be unit-tested.

use crate::settings::{ChannelPrefs, Level, NotificationSettings};
use crate::text::contains_word;

pub struct NotifyInput<'a> {
    pub settings: &'a NotificationSettings,
    pub prefs: &'a ChannelPrefs,
    pub account_id: &'a str,
    pub is_direct: bool,
    pub is_self: bool,
    pub mentions_me: bool,
    /// The conversation is open and the window has focus.
    pub viewing: bool,
    pub text: &'a str,
    /// 0 = Monday.
    pub weekday: u8,
    /// Minutes after local midnight.
    pub minute: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
    pub popup: bool,
    pub tray_dot: bool,
    pub sound: bool,
    pub keyword: Option<String>,
}

pub fn matched_keyword(s: &NotificationSettings, account_id: &str, text: &str) -> Option<String> {
    s.keywords
        .iter()
        .filter(|k| k.accounts.is_empty() || k.accounts.iter().any(|a| a == account_id))
        .find(|k| contains_word(text, &k.word))
        .map(|k| k.word.clone())
}

pub fn decide(i: &NotifyInput) -> Decision {
    if i.is_self || i.viewing {
        return Decision::default();
    }
    let s = i.settings;
    let level = i.prefs.level.unwrap_or_else(|| s.default_level(i.account_id, i.is_direct));
    let mut popup = level == Level::Alert;
    let mut tray_dot = level != Level::Silent;
    if let Some(p) = i.prefs.popup {
        popup = p;
    }
    if let Some(t) = i.prefs.tray {
        tray_dot = t;
    }

    let keyword = matched_keyword(s, i.account_id, i.text);
    if (i.mentions_me && s.mentions_override) || (keyword.is_some() && s.keywords_override) {
        popup = true;
        tray_dot = true;
    }

    if s.dnd.enabled && s.dnd.ranges.iter().any(|r| r.contains(i.weekday, i.minute)) {
        let allowed = (i.is_direct && s.dnd.allow_dm)
            || (i.mentions_me && s.dnd.allow_mentions)
            || (keyword.is_some() && s.dnd.allow_keywords);
        if !allowed {
            // Quiet hours: no popup, no sound. The tray dot stays: it is silent.
            popup = false;
        }
    }

    let sound_name = i.prefs.sound.as_deref().unwrap_or(&s.sound);
    Decision { popup, tray_dot, sound: popup && sound_name != "none", keyword }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Keyword, TimeRange};

    fn input<'a>(s: &'a NotificationSettings, p: &'a ChannelPrefs, text: &'a str) -> NotifyInput<'a> {
        NotifyInput {
            settings: s,
            prefs: p,
            account_id: "acc",
            is_direct: false,
            is_self: false,
            mentions_me: false,
            viewing: false,
            text,
            weekday: 2,
            minute: 12 * 60,
        }
    }

    #[test]
    fn defaults() {
        let s = NotificationSettings::default();
        let p = ChannelPrefs::default();
        let d = decide(&input(&s, &p, "hi"));
        assert!(!d.popup && d.tray_dot);
        let d = decide(&NotifyInput { is_direct: true, ..input(&s, &p, "hi") });
        assert!(d.popup && d.tray_dot && d.sound);
    }

    #[test]
    fn own_and_viewed_messages_are_ignored() {
        let s = NotificationSettings::default();
        let p = ChannelPrefs::default();
        assert_eq!(decide(&NotifyInput { is_self: true, ..input(&s, &p, "x") }), Decision::default());
        assert_eq!(decide(&NotifyInput { viewing: true, ..input(&s, &p, "x") }), Decision::default());
    }

    #[test]
    fn mention_overrides_silent() {
        let s = NotificationSettings::default();
        let p = ChannelPrefs { level: Some(Level::Silent), ..Default::default() };
        assert!(!decide(&input(&s, &p, "x")).tray_dot);
        let d = decide(&NotifyInput { mentions_me: true, ..input(&s, &p, "x") });
        assert!(d.popup && d.tray_dot);
    }

    #[test]
    fn keyword_scoped_to_account() {
        let mut s = NotificationSettings::default();
        s.keywords.push(Keyword { word: "urgent".into(), accounts: vec!["other".into()] });
        let p = ChannelPrefs { level: Some(Level::Silent), ..Default::default() };
        assert!(!decide(&input(&s, &p, "URGENT please")).popup);
        s.keywords[0].accounts.clear();
        let d = decide(&input(&s, &p, "URGENT please"));
        assert!(d.popup);
        assert_eq!(d.keyword.as_deref(), Some("urgent"));
    }

    #[test]
    fn independent_popup_and_tray() {
        let s = NotificationSettings::default();
        let p = ChannelPrefs { level: Some(Level::Alert), tray: Some(false), ..Default::default() };
        let d = decide(&input(&s, &p, "x"));
        assert!(d.popup && !d.tray_dot);
    }

    #[test]
    fn dnd_blocks_unless_allowed() {
        let mut s = NotificationSettings::default();
        s.dnd.enabled = true;
        s.dnd.ranges = vec![TimeRange { days: (0..7).collect(), start: 11 * 60, end: 13 * 60 }];
        let p = ChannelPrefs::default();
        let dm = NotifyInput { is_direct: true, ..input(&s, &p, "x") };
        let d = decide(&dm);
        assert!(!d.popup && !d.sound && d.tray_dot);
        s.dnd.allow_dm = true;
        assert!(decide(&NotifyInput { is_direct: true, ..input(&s, &p, "x") }).popup);
    }

    #[test]
    fn muted_sound() {
        let s = NotificationSettings::default();
        let p = ChannelPrefs { sound: Some("none".into()), ..Default::default() };
        let d = decide(&NotifyInput { is_direct: true, ..input(&s, &p, "x") });
        assert!(d.popup && !d.sound);
    }
}
