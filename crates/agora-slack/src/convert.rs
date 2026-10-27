//! Slack JSON to the common model, and Slack mrkdwn to and from standard Markdown.
//!
//! mrkdwn differs in three ways: `*bold*` / `~strike~`, links and mentions in
//! angle brackets (`<url|label>`, `<@U123>`, `<!here>`), and `&`, `<`, `>`
//! escaped as HTML entities.

use agora_core::emphasis;
use agora_core::model::*;
use serde_json::Value;

pub fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn non_empty(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(str::to_string)
}

/// Slack timestamps ("1700000000.123456") are also message ids.
pub fn ts_ms(ts: &str) -> Millis {
    let (secs, frac) = ts.split_once('.').unwrap_or((ts, "0"));
    let ms: String = frac.chars().chain("000".chars()).take(3).collect();
    secs.parse::<i64>().unwrap_or(0) * 1000 + ms.parse::<i64>().unwrap_or(0)
}

pub fn slack_ts(ms: Millis) -> String {
    format!("{}.{:03}000", ms / 1000, ms % 1000)
}

pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = hex {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Browsers show the `d` cookie URL-encoded; Slack expects it that way in the header.
pub fn pct_encode_cookie(raw: &str) -> String {
    if raw.contains('%') {
        return raw.to_string();
    }
    raw.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

// ------------------------------------------------------------ mrkdwn

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// mrkdwn → Markdown. `name_of` resolves user ids for `<@U123>` and conversation ids for
/// `<#C123>`; returns the text and the
/// user ids it mentions ([`MENTION_ALL`] for @here / @channel / @everyone).
pub fn from_slack(text: &str, name_of: &dyn Fn(&str) -> Option<String>) -> (String, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut mentions = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>').map(|c| open + c) else { break };
        out.push_str(&rest[..open]);
        let inner = &rest[open + 1..close];
        let (target, label) = match inner.split_once('|') {
            Some((t, l)) => (t, Some(l)),
            None => (inner, None),
        };
        if let Some(id) = target.strip_prefix('@') {
            let name = label.map(|l| l.trim_start_matches('@').to_string()).or_else(|| name_of(id)).unwrap_or_else(|| id.to_string());
            out.push('@');
            out.push_str(&name);
            mentions.push(id.to_string());
        } else if let Some(id) = target.strip_prefix('#') {
            // Slack often sends `<#C123|>` with an empty label: look the name up.
            let name = label.filter(|l| !l.is_empty()).map(str::to_string).or_else(|| name_of(id)).unwrap_or_else(|| id.to_string());
            out.push('#');
            out.push_str(&name);
        } else if let Some(special) = target.strip_prefix('!') {
            match special {
                "here" | "channel" | "everyone" => {
                    out.push('@');
                    out.push_str(special);
                    mentions.push(MENTION_ALL.to_string());
                }
                _ => out.push_str(label.unwrap_or(special)),
            }
        } else {
            let url = unescape(target);
            let shown = url.trim_start_matches("mailto:");
            match label.map(unescape) {
                Some(l) if l != url && l != shown => out.push_str(&format!("[{l}]({url})")),
                _ => out.push_str(shown),
            }
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    (emphasis::from_single(&unescape(&out)), mentions)
}

/// Markdown → mrkdwn: entities escaped, `[label](url)` as `<url|label>`, single-char emphasis.
pub fn to_slack(md: &str) -> String {
    let escaped = escape(md);
    let linked = emphasis::map_text(&escaped, |t| {
        let mut out = String::with_capacity(t.len());
        let mut rest = t;
        while let Some(open) = rest.find('[') {
            let tail = &rest[open..];
            let parsed = tail.find("](").and_then(|mid| {
                let end = tail[mid + 2..].find(')')? + mid + 2;
                let url = &tail[mid + 2..end];
                (url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:"))
                    .then(|| (format!("<{url}|{}>", &tail[1..mid]), end + 1))
            });
            match parsed {
                Some((link, len)) => {
                    out.push_str(&rest[..open]);
                    out.push_str(&link);
                    rest = &tail[len..];
                }
                None => {
                    out.push_str(&rest[..=open]);
                    rest = &rest[open + 1..];
                }
            }
        }
        out.push_str(rest);
        out
    });
    emphasis::to_single(&linked)
}

// ------------------------------------------------------------ users

/// `users.info` (with `fields`, the labelled custom profile fields from `users.profile.get`).
pub fn profile(acc: &str, u: &Value, fields: &Value) -> Profile {
    let p = &u["profile"];
    let mut roles = Vec::new();
    for (flag, label) in [("is_primary_owner", "Primary owner"), ("is_owner", "Owner"), ("is_admin", "Admin")] {
        if u[flag].as_bool() == Some(true) {
            roles.push(label.to_string());
            break;
        }
    }
    if u["is_ultra_restricted"].as_bool() == Some(true) {
        roles.push("Single-channel guest".into());
    } else if u["is_restricted"].as_bool() == Some(true) {
        roles.push("Guest".into());
    }
    let mut extra = Vec::new();
    if let Some(pronouns) = non_empty(&p["pronouns"]) {
        extra.push(("Pronouns".to_string(), pronouns));
    }
    for f in fields.as_object().into_iter().flatten().map(|(_, f)| f) {
        let value = non_empty(&f["alt"]).or_else(|| non_empty(&f["value"]));
        if let (Some(label), Some(value)) = (non_empty(&f["label"]), value) {
            extra.push((label, value));
        }
    }
    Profile {
        title: s(&p["title"]),
        email: s(&p["email"]),
        phone: s(&p["phone"]),
        timezone: s(&u["tz"]),
        utc_offset_min: u["tz_offset"].as_i64().map(|sec| (sec / 60) as i32),
        status_text: s(&p["status_text"]),
        status_emoji: s(&p["status_emoji"]).trim_matches(':').to_string(),
        roles,
        inactive: u["deleted"].as_bool() == Some(true),
        extra,
        ..Profile::from_user(&user(acc, u))
    }
}

pub fn user(acc: &str, u: &Value) -> User {
    let p = &u["profile"];
    let handle = non_empty(&p["display_name"]).unwrap_or_else(|| s(&u["name"]));
    let real = non_empty(&p["real_name"]).or_else(|| non_empty(&u["real_name"])).unwrap_or_default();
    User {
        account_id: acc.to_string(),
        id: s(&u["id"]),
        display_name: if real == handle { String::new() } else { real },
        name: handle,
        // Sizes vary by account type and API; the batch endpoint sometimes omits the small ones.
        avatar_url: ["image_72", "image_48", "image_192", "image_32", "image_512", "image_original"]
            .iter()
            .find_map(|k| non_empty(&p[*k])),
        is_bot: u["is_bot"].as_bool().unwrap_or(false),
    }
}

/// Bots post without a user; their profile travels with the message.
pub fn bot_user(acc: &str, m: &Value) -> Option<User> {
    let id = non_empty(&m["bot_id"])?;
    let p = &m["bot_profile"];
    let name = non_empty(&m["username"]).or_else(|| non_empty(&p["name"])).unwrap_or_else(|| "bot".into());
    Some(User {
        account_id: acc.to_string(),
        id,
        name,
        display_name: String::new(),
        avatar_url: non_empty(&p["icons"]["image_48"]),
        is_bot: true,
    })
}

pub fn author_id(m: &Value) -> String {
    non_empty(&m["user"]).or_else(|| non_empty(&m["bot_id"])).unwrap_or_default()
}

/// User ids a message refers to (author and `<@U…>` mentions), to resolve names first.
pub fn referenced_users(m: &Value, out: &mut Vec<String>) {
    if let Some(u) = non_empty(&m["user"]) {
        out.push(u);
    }
    let text = m["text"].as_str().unwrap_or_default();
    for part in text.split("<@").skip(1) {
        let id: String = part.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        if !id.is_empty() {
            out.push(id);
        }
    }
}

// ------------------------------------------------------------ channels

/// `counts` is the conversation's entry in `client.counts` (unread state), when known.
pub fn channel(acc: &str, c: &Value, counts: Option<&Value>, self_id: &str, self_name: &str) -> Option<Channel> {
    let id = non_empty(&c["id"])?;
    if c["is_archived"].as_bool() == Some(true) {
        return None;
    }
    let kind = if c["is_im"].as_bool() == Some(true) {
        ChannelKind::Dm
    } else if c["is_mpim"].as_bool() == Some(true) {
        ChannelKind::Group
    } else if c["is_private"].as_bool() == Some(true) || c["is_group"].as_bool() == Some(true) {
        ChannelKind::Private
    } else {
        ChannelKind::Public
    };
    let name = match kind {
        ChannelKind::Dm => s(&c["user"]),
        // "mpdm-ada--bob--nico-1": the people, without us.
        ChannelKind::Group => {
            let raw = s(&c["name"]);
            let core = raw.trim_start_matches("mpdm-");
            let core = core.rsplit_once('-').map(|(a, _)| a).unwrap_or(core);
            core.split("--").filter(|n| *n != self_name).collect::<Vec<_>>().join(", ")
        }
        _ => s(&c["name"]),
    };
    let mut ch = Channel::new(acc, &id, kind, &name);
    if kind == ChannelKind::Dm {
        let peer = s(&c["user"]);
        ch.members.push(if peer.is_empty() { self_id.to_string() } else { peer });
    }
    ch.topic = from_slack(c["topic"]["value"].as_str().unwrap_or_default(), &|_| None).0;
    // `updated` is in milliseconds, `created` in seconds.
    ch.last_activity = c["updated"].as_i64().unwrap_or_else(|| c["created"].as_i64().unwrap_or(0) * 1000);
    if let Some(n) = counts {
        if let Some(latest) = n["latest"].as_str() {
            ch.last_activity = ch.last_activity.max(ts_ms(latest));
        }
        ch.last_read = n["last_read"].as_str().map(ts_ms).unwrap_or(0);
        ch.mentions = n["mention_count"].as_u64().unwrap_or(0) as u32;
        // Slack gives a count only for mentions (every DM message counts as one).
        ch.unread = if n["has_unreads"].as_bool() == Some(true) { ch.mentions.max(1) } else { 0 };
    }
    Some(ch)
}

// ------------------------------------------------------------ messages

fn system_text(subtype: &str, who: &str, text: &str) -> Option<String> {
    let t = match subtype {
        "channel_join" | "group_join" => format!("{who} joined"),
        "channel_leave" | "group_leave" => format!("{who} left"),
        "channel_topic" | "group_topic" => format!("{who} changed the topic: {text}"),
        "channel_purpose" | "group_purpose" => format!("{who} changed the description: {text}"),
        "channel_name" | "group_name" => format!("{who} renamed the channel: {text}"),
        "pinned_item" => format!("{who} pinned a message"),
        "channel_archive" | "group_archive" => format!("{who} archived the channel"),
        "tombstone" => "This message was deleted.".to_string(),
        _ => return None,
    };
    Some(format!("_{}_", t.replace('_', "\\_")))
}

/// Unfurls and bot attachments: link previews for the former, Markdown for the latter.
fn attachments(m: &Value, text: &mut String, links: &mut Vec<LinkPreview>) {
    for a in m["attachments"].as_array().into_iter().flatten() {
        let unfurl = non_empty(&a["from_url"]).or_else(|| non_empty(&a["original_url"]));
        if let Some(url) = unfurl {
            if let Some(title) = non_empty(&a["title"]).or_else(|| non_empty(&a["service_name"])) {
                links.push(LinkPreview {
                    url,
                    title,
                    description: from_slack(a["text"].as_str().unwrap_or_default(), &|_| None).0,
                    image_url: non_empty(&a["image_url"]).or_else(|| non_empty(&a["thumb_url"])),
                });
            }
            continue;
        }
        let mut part = Vec::new();
        let md = |v: &Value| from_slack(v.as_str().unwrap_or_default(), &|_| None).0;
        if non_empty(&a["pretext"]).is_some() {
            part.push(md(&a["pretext"]));
        }
        if let Some(title) = non_empty(&a["title"]) {
            part.push(match non_empty(&a["title_link"]) {
                Some(link) => format!("**[{title}]({link})**"),
                None => format!("**{title}**"),
            });
        }
        if non_empty(&a["text"]).is_some() {
            part.push(md(&a["text"]));
        }
        for f in a["fields"].as_array().into_iter().flatten() {
            part.push(format!("**{}**: {}", s(&f["title"]), md(&f["value"])));
        }
        if part.is_empty() {
            part.extend(non_empty(&a["fallback"]));
        }
        if !part.is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&part.join("\n\n"));
        }
    }
}

pub fn message(acc: &str, channel_id: &str, m: &Value, name_of: &dyn Fn(&str) -> Option<String>) -> Option<Message> {
    let id = non_empty(&m["ts"])?;
    let subtype = m["subtype"].as_str().unwrap_or_default();
    let author = author_id(m);
    let (mut text, mentions) = from_slack(m["text"].as_str().unwrap_or_default(), name_of);
    match subtype {
        "" | "bot_message" | "thread_broadcast" | "file_share" | "me_message" | "reply_broadcast" => {}
        other => {
            let who = name_of(&author).unwrap_or_else(|| "Someone".into());
            text = system_text(other, &who, &text)?;
        }
    }
    let mut links = Vec::new();
    attachments(m, &mut text, &mut links);

    let files = m["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["mode"].as_str() != Some("tombstone"))
        .map(|f| {
            let mime = s(&f["mimetype"]);
            let private = s(&f["url_private"]);
            let path = |u: &str| u.strip_prefix("https://files.slack.com").map(|p| media_url(acc, p));
            let image = mime.starts_with("image/");
            Attachment {
                id: s(&f["id"]),
                name: non_empty(&f["name"]).or_else(|| non_empty(&f["title"])).unwrap_or_default(),
                size: f["size"].as_u64().unwrap_or(0),
                url: if image { path(&private).unwrap_or(private.clone()) } else { non_empty(&f["permalink"]).unwrap_or(private) },
                thumb_url: if image {
                    non_empty(&f["thumb_720"]).or_else(|| non_empty(&f["thumb_360"])).and_then(|t| path(&t))
                } else {
                    None
                },
                width: f["original_w"].as_u64().map(|w| w as u32),
                height: f["original_h"].as_u64().map(|h| h as u32),
                mime,
            }
        })
        .collect();

    let reactions = m["reactions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Reaction {
            emoji: s(&r["name"]),
            users: r["users"].as_array().into_iter().flatten().filter_map(|u| non_empty(u)).collect(),
        })
        .collect();

    let thread = non_empty(&m["thread_ts"]).filter(|t| *t != id);
    Some(Message {
        account_id: acc.to_string(),
        channel_id: non_empty(&m["channel"]).unwrap_or_else(|| channel_id.to_string()),
        ts: ts_ms(&id),
        author_id: author,
        text,
        thread_root: thread,
        reply_count: m["reply_count"].as_u64().unwrap_or(0) as u32,
        edited_at: m["edited"]["ts"].as_str().map(ts_ms),
        deleted: subtype == "tombstone",
        mentions,
        reactions,
        attachments: files,
        links,
        id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names(id: &str) -> Option<String> {
        (id == "U1").then(|| "ada".to_string())
    }

    #[test]
    fn mrkdwn_in() {
        let (t, m) = from_slack("hi <@U1> and <!here>: *bold* ~x~ <https://a.org?x=1&amp;y=2|docs> &lt;3 <#C9|general>", &names);
        assert_eq!(t, "hi @ada and @here: **bold** ~~x~~ [docs](https://a.org?x=1&y=2) <3 #general");
        assert_eq!(m, vec!["U1".to_string(), MENTION_ALL.to_string()]);
        assert_eq!(from_slack("<https://x.org>", &names).0, "https://x.org");
        assert_eq!(from_slack("<@U7>", &names).0, "@U7");
    }

    #[test]
    fn mrkdwn_out() {
        assert_eq!(to_slack("**bold** *it* a < b & [docs](https://x.org/a?b=1&c=2)"), "*bold* _it_ a &lt; b &amp; <https://x.org/a?b=1&amp;c=2|docs>");
        assert_eq!(to_slack("`**raw** [x](https://y)`"), "`**raw** [x](https://y)`");
        assert_eq!(to_slack("[not a link](file)"), "[not a link](file)");
    }

    #[test]
    fn timestamps() {
        assert_eq!(ts_ms("1700000000.123456"), 1700000000123);
        assert_eq!(ts_ms("1700000000"), 1700000000000);
        assert_eq!(slack_ts(1700000000123), "1700000000.123000");
        assert_eq!(pct_decode("a%2Fb%3D"), "a/b=");
        assert_eq!(pct_encode_cookie("xoxd-a/b+c="), "xoxd-a%2Fb%2Bc%3D");
        assert_eq!(pct_encode_cookie("xoxd-a%2F"), "xoxd-a%2F");
    }

    #[test]
    fn messages() {
        let m = json!({
            "ts": "1700000000.000200", "user": "U1", "text": "see <@U1>", "thread_ts": "1700000000.000100",
            "edited": { "ts": "1700000001.000000" }, "reactions": [{ "name": "+1", "users": ["U1", "U2"], "count": 2 }],
            "files": [{ "id": "F1", "name": "plot.png", "mimetype": "image/png", "url_private": "https://files.slack.com/files-pri/T-F1/plot.png",
                        "thumb_360": "https://files.slack.com/files-tmb/T-F1/plot_360.png" }],
            "attachments": [{ "from_url": "https://example.org", "title": "Example", "text": "news" }]
        });
        let msg = message("acc", "C1", &m, &names).unwrap();
        assert_eq!((msg.id.as_str(), msg.channel_id.as_str(), msg.text.as_str()), ("1700000000.000200", "C1", "see @ada"));
        assert_eq!(msg.thread_root.as_deref(), Some("1700000000.000100"));
        assert_eq!(msg.edited_at, Some(1700000001000));
        assert_eq!(msg.reactions[0].users, vec!["U1", "U2"]);
        assert_eq!(msg.attachments[0].url, format!("{MEDIA_ORIGIN}/acc/files-pri/T-F1/plot.png"));
        assert_eq!(msg.links[0].title, "Example");
        let join = json!({ "ts": "1.0", "subtype": "channel_join", "user": "U1", "text": "<@U1> has joined" });
        assert_eq!(message("a", "C", &join, &names).unwrap().text, "_ada joined_");
        assert!(message("a", "C", &json!({ "ts": "1.0", "subtype": "huddle_thread", "text": "x" }), &names).is_none());
    }

    #[test]
    fn channels() {
        let mpim = json!({ "id": "G1", "is_mpim": true, "name": "mpdm-ada--bob--nico-1" });
        assert_eq!(channel("a", &mpim, None, "U0", "nico").unwrap().name, "ada, bob");
        let im = json!({ "id": "D1", "is_im": true, "user": "U1" });
        let counts = json!({ "has_unreads": true, "mention_count": 2, "last_read": "1700000000.000000", "latest": "1700000005.000000" });
        let c = channel("a", &im, Some(&counts), "U0", "nico").unwrap();
        assert_eq!((c.kind, c.members.clone(), c.unread, c.mentions, c.last_activity), (ChannelKind::Dm, vec!["U1".to_string()], 2, 2, 1700000005000));
        let ch = json!({ "id": "C1", "is_channel": true, "is_private": true, "name": "ops", "topic": { "value": "<https://x|Agenda>" } });
        let c = channel("a", &ch, Some(&json!({ "has_unreads": true })), "U0", "nico").unwrap();
        assert_eq!((c.kind, c.topic.as_str(), c.unread), (ChannelKind::Private, "[Agenda](https://x)", 1));
        assert!(channel("a", &json!({ "id": "C2", "is_archived": true }), None, "U0", "nico").is_none());
    }
}
