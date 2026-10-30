//! Rocket.Chat JSON to the common model, and Rocket.Chat Markdown to and from
//! standard Markdown (`*bold*` / `~strike~` there, `**bold**` / `~~strike~~` here).

use agora_core::model::*;
use serde_json::Value;
use std::collections::HashMap;

pub fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn non_empty(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(str::to_string)
}

/// REST sends ISO 8601 strings, the realtime socket `{"$date": millis}`.
pub fn ts(v: &Value) -> Option<Millis> {
    if let Some(n) = v["$date"].as_i64() {
        return Some(n);
    }
    chrono::DateTime::parse_from_rfc3339(v.as_str()?).ok().map(|d| d.timestamp_millis())
}

pub fn iso(ms: Millis) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// ------------------------------------------------------------ Markdown

/// Rocket.Chat's `*bold*` / `~strike~` to standard Markdown.
pub fn from_rc(md: &str) -> String {
    agora_core::emphasis::from_single(md)
}

pub fn to_rc(md: &str) -> String {
    agora_core::emphasis::to_single(md)
}

// ------------------------------------------------------------ users and rooms

pub fn user(acc: &str, id: &str, username: &str, name: &str) -> User {
    User {
        account_id: acc.to_string(),
        id: id.to_string(),
        name: username.to_string(),
        display_name: if name == username { String::new() } else { name.to_string() },
        avatar_url: (!username.is_empty()).then(|| media_url(acc, &format!("/avatar/{username}"))),
        is_bot: false,
    }
}

/// The author embedded in every message (`u`), so no lookup is needed.
pub fn author(acc: &str, m: &Value) -> Option<User> {
    let u = &m["u"];
    let id = u["_id"].as_str().filter(|x| !x.is_empty())?;
    let username = s(&u["username"]);
    Some(user(acc, id, &username, u["name"].as_str().unwrap_or(&username)))
}

/// Team id -> team name, from the rooms that are a team's main room.
pub fn team_names(rooms: &[Value]) -> HashMap<String, String> {
    rooms
        .iter()
        .filter(|r| r["teamMain"].as_bool() == Some(true))
        .filter_map(|r| Some((non_empty(&r["teamId"])?, non_empty(&r["fname"]).or_else(|| non_empty(&r["name"]))?)))
        .collect()
}

/// One conversation from its subscription (our unread state, favorite) and its room
/// (topic, last message, team, members). `None` for omnichannel rooms.
pub fn channel(acc: &str, sub: &Value, room: Option<&Value>, me: (&str, &str), teams: &HashMap<String, String>) -> Option<Channel> {
    let (self_id, self_name) = me;
    let null = Value::Null;
    let room = room.unwrap_or(&null);
    let id = non_empty(&sub["rid"]).or_else(|| non_empty(&room["_id"]))?;
    let uids: Vec<String> = room["uids"].as_array().into_iter().flatten().filter_map(|u| non_empty(u)).collect();
    let kind = match sub["t"].as_str().or(room["t"].as_str()).unwrap_or("c") {
        "c" => ChannelKind::Public,
        "p" => ChannelKind::Private,
        "d" if uids.len() > 2 => ChannelKind::Group,
        "d" => ChannelKind::Dm,
        _ => return None,
    };
    let mut name = non_empty(&sub["fname"]).or_else(|| non_empty(&sub["name"])).or_else(|| non_empty(&room["fname"])).unwrap_or_default();
    let mut c = Channel::new(acc, &id, kind, &name);
    if kind.is_direct() {
        c.members = uids.iter().filter(|u| *u != self_id).cloned().collect();
        if c.members.is_empty() {
            c.members.push(self_id.to_string());
        }
        if kind == ChannelKind::Group {
            let names: Vec<&str> = room["usernames"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|n| *n != self_name).collect();
            if !names.is_empty() {
                name = names.join(", ");
                c.name = name;
            }
        }
    } else {
        c.group = if room["teamMain"].as_bool() == Some(true) {
            c.name.clone()
        } else {
            room["teamId"].as_str().and_then(|t| teams.get(t)).cloned().unwrap_or_default()
        };
        if room["prid"].as_str().is_some_and(|p| !p.is_empty()) {
            c.category = "Discussions".into();
        }
    }
    c.topic = s(&room["topic"]);
    c.last_activity = ts(&room["lm"]).or_else(|| ts(&sub["ts"])).unwrap_or(0);
    c.last_read = ts(&sub["ls"]).unwrap_or(0);
    c.unread = sub["unread"].as_u64().unwrap_or(0) as u32;
    c.mentions = (sub["userMentions"].as_u64().unwrap_or(0) + sub["groupMentions"].as_u64().unwrap_or(0)) as u32;
    c.starred = sub["f"].as_bool().unwrap_or(false);
    c.starred_synced = true;
    Some(c)
}

/// People of a direct conversation, from the room's parallel `uids` / `usernames`.
pub fn dm_users(acc: &str, sub: &Value, room: &Value, self_id: &str) -> Vec<User> {
    let uids = room["uids"].as_array().cloned().unwrap_or_default();
    let names = room["usernames"].as_array().cloned().unwrap_or_default();
    let one_to_one = uids.len() == 2;
    uids.iter()
        .zip(names.iter())
        .filter(|(id, _)| id.as_str() != Some(self_id))
        .map(|(id, n)| {
            let username = s(n);
            // In a 1:1 conversation the subscription's fname is the peer's real name.
            let display = if one_to_one { sub["fname"].as_str().unwrap_or(&username).to_string() } else { username.clone() };
            user(acc, &s(id), &username, &display)
        })
        .collect()
}

// ------------------------------------------------------------ messages

/// Text for system messages (joins, topic changes...); `None` hides the ones nobody reads.
fn system_text(t: &str, who: &str, msg: &str) -> Option<String> {
    let text = match t {
        "uj" | "ujt" => format!("{who} joined"),
        "ul" | "ult" => format!("{who} left"),
        "au" => format!("{who} added {msg}"),
        "ru" => format!("{who} removed {msg}"),
        "room_changed_topic" => format!("{who} changed the topic to: {msg}"),
        "room_changed_description" => format!("{who} changed the description to: {msg}"),
        "r" => format!("{who} renamed the conversation to {msg}"),
        "message_pinned" => format!("{who} pinned a message"),
        "discussion-created" => format!("{who} started a discussion: {msg}"),
        "e2e" | "otr" | "user-muted" | "user-unmuted" | "subscription-role-added" | "subscription-role-removed" => return None,
        _ if !msg.trim().is_empty() => msg.trim().to_string(),
        _ => return None,
    };
    Some(format!("_{}_", text.replace('_', "\\_")))
}

/// Quotes and integration posts (GitLab, Jira, alerts) as Markdown.
fn attachment_text(a: &Value) -> String {
    let mut part = Vec::new();
    if a["message_link"].is_string() {
        let who = s(&a["author_name"]);
        let text = from_rc(a["text"].as_str().unwrap_or_default());
        let quoted: Vec<String> = text.lines().map(|l| format!("> {l}")).collect();
        return if who.is_empty() { quoted.join("\n") } else { format!("> **{who}**\n{}", quoted.join("\n")) };
    }
    part.extend(non_empty(&a["pretext"]).map(|t| from_rc(&t)));
    if let Some(title) = non_empty(&a["title"]) {
        part.push(match non_empty(&a["title_link"]) {
            Some(link) => format!("**[{title}]({link})**"),
            None => format!("**{title}**"),
        });
    }
    part.extend(non_empty(&a["text"]).map(|t| from_rc(&t)));
    for f in a["fields"].as_array().into_iter().flatten() {
        part.push(format!("**{}**: {}", s(&f["title"]), s(&f["value"])));
    }
    part.join("\n\n")
}

/// `uid_of` maps a username to its user id (reactions list usernames).
pub fn message(acc: &str, server: &str, m: &Value, uid_of: &dyn Fn(&str) -> String) -> Option<Message> {
    if m["_hidden"].as_bool() == Some(true) {
        return None;
    }
    let id = non_empty(&m["_id"])?;
    let t = m["t"].as_str().unwrap_or_default();
    let raw = m["msg"].as_str().unwrap_or_default();
    let who = m["u"]["name"].as_str().or(m["u"]["username"].as_str()).unwrap_or("Someone");
    let deleted = t == "rm";
    let mut text = if t.is_empty() || deleted {
        from_rc(raw)
    } else {
        system_text(t, who, raw)?
    };

    let mut attachments = Vec::new();
    for a in m["attachments"].as_array().into_iter().flatten() {
        let link = a["title_link"].as_str().filter(|l| l.starts_with("/file-upload/"));
        if let Some(link) = link {
            let preview = a["image_url"].as_str();
            let name = s(&a["title"]);
            // A PDF can come with a preview picture: the file's own type decides, not the preview.
            let file_type = m["files"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(std::iter::once(&m["file"]))
                .find(|f| f["name"].as_str() == Some(name.as_str()))
                .and_then(|f| f["type"].as_str());
            let mime = a["image_type"]
                .as_str()
                .or(file_type)
                .map(str::to_string)
                .unwrap_or_else(|| if preview.is_some() { "image/*".into() } else { String::new() });
            let image = mime.starts_with("image/");
            attachments.push(Attachment {
                id: link.split('/').nth(2).unwrap_or_default().to_string(),
                path: Some(link.to_string()),
                url: if image { media_url(acc, link) } else { format!("{server}{link}?download") },
                thumb_url: preview.filter(|u| u.starts_with('/') && *u != link).map(|u| media_url(acc, u)),
                mime,
                size: a["image_size"].as_u64().unwrap_or(0),
                width: a["image_dimensions"]["width"].as_u64().map(|w| w as u32),
                height: a["image_dimensions"]["height"].as_u64().map(|h| h as u32),
                name,
            });
            if let Some(desc) = non_empty(&a["description"]) {
                text = if text.is_empty() { from_rc(&desc) } else { format!("{text}\n\n{}", from_rc(&desc)) };
            }
        } else {
            let extra = attachment_text(a);
            if !extra.is_empty() {
                text = if text.is_empty() { extra } else { format!("{text}\n\n{extra}") };
            }
        }
    }

    let mut reactions: Vec<Reaction> = m["reactions"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(emoji, r)| Reaction {
            emoji: emoji.trim_matches(':').to_string(),
            users: r["usernames"].as_array().into_iter().flatten().filter_map(Value::as_str).map(uid_of).collect(),
        })
        .collect();
    reactions.sort_by(|a, b| a.emoji.cmp(&b.emoji));

    let links = m["urls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| {
            let meta = &u["meta"];
            let title = non_empty(&meta["ogTitle"]).or_else(|| non_empty(&meta["pageTitle"]))?;
            let image = non_empty(&meta["ogImage"]).filter(|i| i.starts_with("https://"));
            Some(LinkPreview {
                url: non_empty(&u["url"])?,
                title,
                description: non_empty(&meta["ogDescription"]).or_else(|| non_empty(&meta["description"])).unwrap_or_default(),
                image_url: image,
            })
        })
        .collect();

    let mentions = m["mentions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| match u["username"].as_str() {
            Some("all" | "here") => Some(MENTION_ALL.to_string()),
            _ => non_empty(&u["_id"]),
        })
        .collect();

    Some(Message {
        account_id: acc.to_string(),
        channel_id: s(&m["rid"]),
        id,
        author_id: s(&m["u"]["_id"]),
        ts: ts(&m["ts"]).unwrap_or(0),
        text: if deleted { String::new() } else { text },
        thread_root: non_empty(&m["tmid"]),
        reply_count: m["tcount"].as_u64().unwrap_or(0) as u32,
        edited_at: ts(&m["editedAt"]),
        deleted,
        in_channel: m["tshow"].as_bool().unwrap_or(false),
        mentions,
        reactions,
        attachments,
        links,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn markdown_both_ways() {
        assert_eq!(from_rc("*bold* and _it_ and ~gone~"), "**bold** and _it_ and ~~gone~~");
        assert_eq!(from_rc("**already** standard"), "**already** standard");
        assert_eq!(from_rc("* list item\n2*3*4 `*code*`"), "* list item\n2*3*4 `*code*`");
        assert_eq!(from_rc("```\n*x*\n```\n*y*"), "```\n*x*\n```\n**y**");
        assert_eq!(to_rc("**bold** *it* ~~gone~~"), "*bold* _it_ ~gone~");
        assert_eq!(to_rc("- item\n`**raw**`"), "- item\n`**raw**`");
        assert_eq!(from_rc(&to_rc("**a** _b_ ~~c~~")), "**a** _b_ ~~c~~");
    }

    #[test]
    fn timestamps() {
        assert_eq!(ts(&json!({ "$date": 1700000000123i64 })), Some(1700000000123));
        let t = ts(&json!("2023-11-14T22:13:20.123Z")).unwrap();
        assert_eq!(t, 1700000000123);
        assert_eq!(iso(t), "2023-11-14T22:13:20.123Z");
    }

    #[test]
    fn message_with_file_reactions_and_thread() {
        let m = json!({
            "_id": "m1", "rid": "r1", "msg": "see *plot*", "ts": "2023-11-14T22:13:20.000Z",
            "u": { "_id": "u1", "username": "ada", "name": "Ada" }, "tmid": "root", "editedAt": { "$date": 5 },
            "reactions": { ":+1:": { "usernames": ["ada", "bob"] } },
            "mentions": [{ "_id": "me", "username": "nico" }, { "_id": "here", "username": "here" }],
            "attachments": [{ "title": "plot.png", "title_link": "/file-upload/f1/plot.png", "image_url": "/file-upload/t1/thumb.png", "image_type": "image/png" }],
            "urls": [{ "url": "https://example.org", "meta": { "ogTitle": "Example" } }]
        });
        let uid = |n: &str| if n == "ada" { "u1".to_string() } else { n.to_string() };
        let msg = message("acc", "https://rc", &m, &uid).unwrap();
        assert_eq!(msg.text, "see **plot**");
        assert_eq!(msg.thread_root.as_deref(), Some("root"));
        assert_eq!(msg.edited_at, Some(5));
        assert_eq!(msg.reactions[0].emoji, "+1");
        assert_eq!(msg.reactions[0].users, vec!["u1", "bob"]);
        assert_eq!(msg.mentions, vec!["me".to_string(), MENTION_ALL.to_string()]);
        assert_eq!(msg.attachments[0].url, format!("{MEDIA_ORIGIN}/acc/file-upload/f1/plot.png"));
        assert_eq!(msg.attachments[0].id, "f1");
        assert_eq!(msg.links[0].title, "Example");
    }

    #[test]
    fn system_messages() {
        let uid = |n: &str| n.to_string();
        let join = json!({ "_id": "x", "rid": "r", "t": "uj", "msg": "ada", "u": { "_id": "u1", "username": "ada" } });
        assert_eq!(message("a", "s", &join, &uid).unwrap().text, "_ada joined_");
        let muted = json!({ "_id": "y", "rid": "r", "t": "user-muted", "msg": "", "u": { "_id": "u1" } });
        assert!(message("a", "s", &muted, &uid).is_none());
        let removed = json!({ "_id": "z", "rid": "r", "t": "rm", "msg": "Message removed", "u": { "_id": "u1" } });
        assert!(message("a", "s", &removed, &uid).unwrap().deleted);
    }

    #[test]
    fn rooms_and_teams() {
        let rooms = vec![
            json!({ "_id": "t", "t": "c", "teamMain": true, "teamId": "T1", "fname": "Cosmology WG" }),
            json!({ "_id": "c1", "t": "p", "teamId": "T1", "topic": "[Agenda](https://x)", "prid": "t" }),
            json!({ "_id": "d1", "t": "d", "uids": ["me", "u1"], "usernames": ["nico", "ada"] }),
        ];
        let teams = team_names(&rooms);
        let me = ("me", "nico");
        let main = channel("a", &json!({ "rid": "t", "t": "c", "fname": "Cosmology WG", "f": true }), Some(&rooms[0]), me, &teams).unwrap();
        assert_eq!((main.group.as_str(), main.starred), ("Cosmology WG", true));
        let sub = json!({ "rid": "c1", "t": "p", "name": "sims", "unread": 3, "userMentions": 1, "groupMentions": 1 });
        let c = channel("a", &sub, Some(&rooms[1]), me, &teams).unwrap();
        assert_eq!((c.kind, c.group.as_str(), c.category.as_str(), c.unread, c.mentions), (ChannelKind::Private, "Cosmology WG", "Discussions", 3, 2));
        let dm_sub = json!({ "rid": "d1", "t": "d", "name": "ada", "fname": "Alex Morgan" });
        let dm = channel("a", &dm_sub, Some(&rooms[2]), me, &teams).unwrap();
        assert_eq!((dm.kind, dm.members.clone()), (ChannelKind::Dm, vec!["u1".to_string()]));
        let people = dm_users("a", &dm_sub, &rooms[2], "me");
        assert_eq!((people[0].name.as_str(), people[0].display_name.as_str()), ("ada", "Alex Morgan"));
        assert!(channel("a", &json!({ "rid": "l", "t": "l" }), None, me, &teams).is_none());
    }
}
