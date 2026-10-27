//! An offline fake workspace. Exercises the whole pipeline (engine, archive,
//! notifications, tray, UI) before any real service is wired in.

use crate::engine::now_ms;
use crate::model::*;
use crate::service::*;
use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const SELF_ID: &str = "u0";
const PEOPLE: &[(&str, &str, &str)] = &[
    ("u0", "you", "You"),
    ("u1", "alex", "Alex Morgan"),
    ("u2", "sam", "Sam Rivera"),
    ("u3", "jordan", "Jordan Lee"),
    ("u4", "taylor", "Taylor Kim"),
    ("u5", "casey", "Casey Brooks"),
];
/// (id, kind, name, participants other than you)
const CHANNELS: &[(&str, ChannelKind, &str, &[&str])] = &[
    ("general", ChannelKind::Public, "general", &["u1", "u2", "u3", "u4", "u5"]),
    ("project", ChannelKind::Public, "project-updates", &["u2", "u4", "u5"]),
    ("ops", ChannelKind::Private, "team-private", &["u3", "u5"]),
    ("dm-alex", ChannelKind::Dm, "alex", &["u1"]),
    ("dm-taylor", ChannelKind::Dm, "taylor", &["u4"]),
    ("grp", ChannelKind::Group, "sam, jordan", &["u2", "u3"]),
];
const LINES: &[&str] = &[
    "Has anyone looked at the **latest results** from last night?",
    "The nightly build finished, see https://example.org/runs/42",
    "Quick snippet for the summary:\n```\nimport statistics\nvalues = [3, 1, 4, 1, 5]\nprint(statistics.mean(values))\n```",
    "> the error rate went up after the update\n\nAgreed, probably the new configuration.",
    "Version 3.0 of the library is out, worth upgrading before the next release.",
    "@you can you check the list before the call?",
    "Meeting moved to 15:00, same link.",
    "Urgent: the job queue is stuck again.",
    "Chart attached in the wiki, the _response time_ is ~~fixed~~ much better.",
    "Rust version of the helper:\n```rust\nfn every_nth(x: &[f64], k: usize) -> Vec<f64> {\n    x.iter().step_by(k).copied().collect()\n}\n```",
    "Thanks, looks good to me.",
    "Can we keep the review list short this week?\n- budget\n- schedule\n- open issues",
];

pub struct DemoService {
    account_id: String,
    seq: AtomicU64,
    sink: Mutex<Option<EventSink>>,
}

impl DemoService {
    pub fn new(account_id: &str) -> Self {
        Self { account_id: account_id.to_string(), seq: AtomicU64::new(1), sink: Mutex::new(None) }
    }

    fn next(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }
}

fn participants(channel_id: &str) -> &'static [&'static str] {
    CHANNELS.iter().find(|c| c.0 == channel_id).map(|c| c.3).unwrap_or(&["u1"])
}

fn message(acc: &str, ch: &str, id: String, author: &str, ts: Millis, text: &str) -> Message {
    let mut mentions = Vec::new();
    if text.contains("@you") {
        mentions.push(SELF_ID.to_string());
    }
    if text.contains("@channel") {
        mentions.push(MENTION_ALL.to_string());
    }
    Message {
        account_id: acc.to_string(),
        channel_id: ch.to_string(),
        id,
        author_id: author.to_string(),
        ts,
        text: text.to_string(),
        thread_root: None,
        reply_count: 0,
        edited_at: None,
        deleted: false,
        mentions,
        reactions: vec![],
        attachments: vec![],
        links: vec![],
    }
}

fn user(acc: &str, (id, name, display): (&str, &str, &str)) -> User {
    User {
        account_id: acc.to_string(),
        id: id.to_string(),
        name: name.to_string(),
        display_name: display.to_string(),
        avatar_url: None,
        is_bot: false,
    }
}

#[async_trait]
impl Service for DemoService {
    async fn bootstrap(&self) -> SResult<Bootstrap> {
        let now = now_ms();
        let acc = self.account_id.as_str();
        let channels = CHANNELS
            .iter()
            .enumerate()
            .map(|(i, (id, kind, name, members))| {
                let mut c = Channel::new(acc, id, *kind, name);
                if *id == "general" {
                    c.topic = "Everything that fits nowhere else | [Agenda and minutes](https://example.org/agenda)".into();
                }
                if kind.is_direct() {
                    c.members = members.iter().map(|m| m.to_string()).collect();
                }
                c.last_activity = now - (i as i64 + 1) * 7 * 60_000;
                c
            })
            .collect();
        Ok(Bootstrap {
            self_user: user(acc, PEOPLE[0]),
            users: PEOPLE[1..].iter().map(|p| user(acc, *p)).collect(),
            channels,
        })
    }

    async fn run(&self, sink: EventSink) -> SResult<()> {
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink.clone());
        if !sink.send(ServiceEvent::Connection(ConnState::Online)).await {
            return Ok(());
        }
        for (id, _, _) in &PEOPLE[1..] {
            let presence = if fastrand::bool() { Presence::Online } else { Presence::Away };
            sink.send(ServiceEvent::Presence { user_id: id.to_string(), presence }).await;
        }
        loop {
            tokio::time::sleep(Duration::from_millis(fastrand::u64(8_000..20_000))).await;
            let (ch, ..) = CHANNELS[fastrand::usize(..CHANNELS.len())];
            let people = participants(ch);
            let author = people[fastrand::usize(..people.len())];
            let typing = ServiceEvent::Typing { channel_id: ch.into(), user_id: author.into(), thread_root: None };
            if !sink.send(typing).await {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let text = LINES[fastrand::usize(..LINES.len())];
            let m = message(&self.account_id, ch, format!("l{}", self.next()), author, now_ms(), text);
            if !sink.send(ServiceEvent::MessageUpsert(m)).await {
                return Ok(());
            }
            if fastrand::u8(..6) == 0 {
                let (id, _, _) = PEOPLE[1 + fastrand::usize(..PEOPLE.len() - 1)];
                let presence = if fastrand::bool() { Presence::Online } else { Presence::Away };
                sink.send(ServiceEvent::Presence { user_id: id.into(), presence }).await;
            }
        }
    }

    /// Deterministic fake history (same ids on every call) going back three days,
    /// like a service with a limited retention window.
    async fn history(&self, channel_id: &str, q: HistoryQuery) -> SResult<Vec<Message>> {
        if q.after.is_some() {
            return Ok(vec![]);
        }
        let step: Millis = 9 * 60_000;
        let floor = now_ms() - 3 * 24 * 3600 * 1000;
        let mut ts = q.before.unwrap_or_else(now_ms) - 1;
        ts -= ts.rem_euclid(step);
        let people = participants(channel_id);
        let mut out = Vec::new();
        while out.len() < q.limit as usize && ts > floor {
            let n = (ts / step) as usize;
            let author = if n % 4 == 0 { SELF_ID } else { people[n % people.len()] };
            out.push(message(&self.account_id, channel_id, format!("h{ts}"), author, ts, LINES[n % LINES.len()]));
            ts -= step;
        }
        out.reverse();
        Ok(out)
    }

    async fn thread(&self, _channel_id: &str, _root_id: &str) -> SResult<Vec<Message>> {
        Ok(vec![])
    }

    async fn send(&self, channel_id: &str, text: &str, thread_root: Option<&str>) -> SResult<Message> {
        let mut m = message(&self.account_id, channel_id, format!("s{}", self.next()), SELF_ID, now_ms(), text);
        m.thread_root = thread_root.map(str::to_string);
        // In DMs the other person answers, so popups and the tray can be tried out.
        if channel_id.starts_with("dm-") {
            let sink = self.sink.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(sink) = sink {
                let acc = self.account_id.clone();
                let ch = channel_id.to_string();
                let peer = participants(channel_id)[0];
                let id = format!("r{}", self.next());
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    let typing = ServiceEvent::Typing { channel_id: ch.clone(), user_id: peer.into(), thread_root: None };
                    sink.send(typing).await;
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    let reply = message(&acc, &ch, id, peer, now_ms(), "Got it, I'll take a look.");
                    sink.send(ServiceEvent::MessageUpsert(reply)).await;
                });
            }
        }
        Ok(m)
    }

    async fn edit(&self, _channel_id: &str, _message_id: &str, _text: &str) -> SResult<()> {
        Ok(())
    }

    async fn delete(&self, _channel_id: &str, _message_id: &str) -> SResult<()> {
        Ok(())
    }

    async fn react(&self, _channel_id: &str, _message_id: &str, _emoji: &str, _add: bool) -> SResult<()> {
        Ok(())
    }

    async fn mark_read(&self, _channel_id: &str, _latest: Option<&Message>) -> SResult<()> {
        Ok(())
    }
}
