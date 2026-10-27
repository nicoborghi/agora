//! Search syntax shared by every service, modelled on Mattermost's:
//! words, "exact phrases", `from:user`, `in:channel`, `on:` / `before:` / `after:` `YYYY-MM-DD`.

use crate::model::Millis;
use chrono::{Days, Local, NaiveDate, TimeZone};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParsedQuery {
    pub words: Vec<String>,
    pub phrases: Vec<String>,
    pub from: Vec<String>,
    pub in_: Vec<String>,
    /// Inclusive lower bound.
    pub after: Option<Millis>,
    /// Exclusive upper bound.
    pub before: Option<Millis>,
}

impl ParsedQuery {
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
            && self.phrases.is_empty()
            && self.from.is_empty()
            && self.in_.is_empty()
            && self.after.is_none()
            && self.before.is_none()
    }
}

/// What the archive is asked for, after the engine resolved `in:` to conversations.
#[derive(Debug, Default, Clone)]
pub struct SearchFilter {
    pub words: Vec<String>,
    pub phrases: Vec<String>,
    pub from: Vec<String>,
    pub account: Option<String>,
    /// `Some` restricts to these (account, channel) pairs.
    pub channels: Option<Vec<(String, String)>>,
    pub after: Option<Millis>,
    pub before: Option<Millis>,
    pub limit: u32,
}

/// Splits on whitespace, keeping `"quoted text"` (also after `key:`) together.
fn tokens(q: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in q.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                cur.push(ch);
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn local_midnight(d: NaiveDate) -> Option<Millis> {
    let dt = d.and_hms_opt(0, 0, 0)?;
    Some(Local.from_local_datetime(&dt).earliest()?.timestamp_millis())
}

fn day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

pub fn parse(q: &str) -> ParsedQuery {
    let mut p = ParsedQuery::default();
    for tok in tokens(q) {
        if let Some((key, value)) = tok.split_once(':') {
            let value = value.trim_matches('"');
            let next_day = |d: NaiveDate| d.checked_add_days(Days::new(1)).and_then(local_midnight);
            let handled = match key.to_ascii_lowercase().as_str() {
                "from" => {
                    let v = value.trim_start_matches('@');
                    if !v.is_empty() {
                        p.from.push(v.to_string());
                    }
                    true
                }
                "in" | "channel" => {
                    let v = value.trim_start_matches(['#', '~', '@']);
                    if !v.is_empty() {
                        p.in_.push(v.to_string());
                    }
                    true
                }
                "on" => {
                    if let Some(d) = day(value) {
                        p.after = local_midnight(d);
                        p.before = next_day(d);
                    }
                    true
                }
                // Like Mattermost: "after:" excludes that day, "before:" excludes that day.
                "after" => {
                    if let Some(ts) = day(value).and_then(next_day) {
                        p.after = Some(p.after.map_or(ts, |a| a.max(ts)));
                    }
                    true
                }
                "before" => {
                    if let Some(ts) = day(value).and_then(local_midnight) {
                        p.before = Some(p.before.map_or(ts, |b| b.min(ts)));
                    }
                    true
                }
                _ => false,
            };
            if handled {
                continue;
            }
        }
        if tok.len() > 1 && tok.starts_with('"') {
            let phrase = tok.trim_matches('"').trim();
            if !phrase.is_empty() {
                p.phrases.push(phrase.to_string());
            }
        } else {
            let word = tok.trim_matches(|c: char| c == '"' || c == '*');
            if !word.is_empty() {
                p.words.push(word.to_string());
            }
        }
    }
    p
}

/// FTS5 query: every word as a prefix, every phrase exact, all required.
pub fn fts_query(words: &[String], phrases: &[String]) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    words
        .iter()
        .map(|w| format!("{}*", quote(w)))
        .chain(phrases.iter().map(|p| quote(p)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Escapes `%`, `_` and `\` for `LIKE ... ESCAPE '\'`.
pub fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_phrases() {
        let p = parse(r#"strain "noise budget" from:@ada in:#detchar"#);
        assert_eq!(p.words, vec!["strain"]);
        assert_eq!(p.phrases, vec!["noise budget"]);
        assert_eq!(p.from, vec!["ada"]);
        assert_eq!(p.in_, vec!["detchar"]);
        assert!(p.after.is_none());
    }

    #[test]
    fn dates() {
        let on = parse("on:2026-03-04");
        let d = NaiveDate::from_ymd_opt(2026, 3, 4).unwrap();
        assert_eq!(on.after, local_midnight(d));
        assert_eq!(on.before.unwrap() - on.after.unwrap(), 24 * 3600 * 1000);
        let range = parse("after:2026-03-01 before:2026-03-10");
        assert!(range.after.unwrap() < range.before.unwrap());
        assert_eq!(parse("on:yesterday").after, None);
    }

    #[test]
    fn unknown_keys_are_words() {
        assert_eq!(parse("https://x.org ratio:3").words, vec!["https://x.org", "ratio:3"]);
    }

    #[test]
    fn fts_quoting() {
        let q = fts_query(&["gw".into(), "a\"b".into()], &["noise budget".into()]);
        assert_eq!(q, r#""gw"* "a""b"* "noise budget""#);
        assert_eq!(like_escape("50%_x"), "50\\%\\_x");
    }
}
