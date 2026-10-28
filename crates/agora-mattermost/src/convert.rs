//! Mattermost JSON to the common model. Mattermost already speaks standard
//! Markdown, so message text passes through unchanged.

use agora_core::model::*;
use agora_core::text::contains_word;
use serde_json::Value;

pub fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn non_empty(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(str::to_string)
}

/// Some websocket fields carry JSON encoded as a string.
pub fn embedded(v: &Value) -> Value {
    match v.as_str() {
        Some(raw) => serde_json::from_str(raw).unwrap_or(Value::Null),
        None => v.clone(),
    }
}

/// What counts as mentioning the signed-in user (from their notify_props).
#[derive(Debug, Clone, Default)]
pub struct MentionKeys {
    pub self_id: String,
    pub words: Vec<String>,
    pub channel_wide: bool,
}

impl MentionKeys {
    pub fn from_me(me: &Value) -> Self {
        let props = &me["notify_props"];
        let mut words = vec![format!("@{}", s(&me["username"]))];
        for k in props["mention_keys"].as_str().unwrap_or_default().split(',') {
            let k = k.trim();
            if !k.is_empty() && !words.iter().any(|w| w.eq_ignore_ascii_case(k)) {
                words.push(k.to_string());
            }
        }
        if props["first_name"].as_str() == Some("true") {
            if let Some(first) = non_empty(&me["first_name"]) {
                words.push(first);
            }
        }
        Self { self_id: s(&me["id"]), words, channel_wide: props["channel"].as_str() != Some("false") }
    }
}

/// `GET /users/{id}`: position, e-mail (when the server shows it), time zone, custom status
/// (a JSON string in `props.customStatus`), roles, nickname and locale.
pub fn profile(acc: &str, v: &Value) -> Profile {
    let u = user(acc, v);
    let tz = &v["timezone"];
    let timezone = if tz["useAutomaticTimezone"].as_str() == Some("false") || tz["useAutomaticTimezone"] == false {
        s(&tz["manualTimezone"])
    } else {
        s(&tz["automaticTimezone"])
    };
    let custom: Value = v["props"]["customStatus"].as_str().and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
    let roles = v["roles"].as_str().unwrap_or_default();
    let mut extra = Vec::new();
    let full = format!("{} {}", s(&v["first_name"]), s(&v["last_name"]));
    if !s(&v["nickname"]).is_empty() && s(&v["nickname"]) != full.trim() {
        extra.push(("Nickname".to_string(), s(&v["nickname"])));
    }
    if !s(&v["bot_description"]).is_empty() {
        extra.push(("About".to_string(), s(&v["bot_description"])));
    }
    if !s(&v["locale"]).is_empty() {
        extra.push(("Language".to_string(), s(&v["locale"])));
    }
    Profile {
        title: s(&v["position"]),
        email: s(&v["email"]),
        timezone,
        status_text: s(&custom["text"]),
        status_emoji: s(&custom["emoji"]),
        roles: [("system_admin", "System admin"), ("system_guest", "Guest")]
            .iter()
            .filter(|(r, _)| roles.split_whitespace().any(|x| x == *r))
            .map(|(_, l)| l.to_string())
            .collect(),
        inactive: v["delete_at"].as_i64().unwrap_or(0) > 0,
        extra,
        ..Profile::from_user(&u)
    }
}

pub fn user(acc: &str, v: &Value) -> User {
    let id = s(&v["id"]);
    let full = format!("{} {}", v["first_name"].as_str().unwrap_or(""), v["last_name"].as_str().unwrap_or(""));
    let full = full.trim();
    let display_name = if full.is_empty() { s(&v["nickname"]) } else { full.to_string() };
    User {
        account_id: acc.to_string(),
        name: s(&v["username"]),
        display_name,
        avatar_url: Some(media_url(acc, &format!("/users/{id}/image"))),
        is_bot: v["is_bot"].as_bool().unwrap_or(false),
        id,
    }
}

/// `team` is the team name, given when the account spans several teams: it becomes the
/// channel's sidebar section.
pub fn channel(acc: &str, c: &Value, member: Option<&Value>, self_id: &str, team: Option<&str>) -> Channel {
    let id = s(&c["id"]);
    let kind = match c["type"].as_str() {
        Some("P") => ChannelKind::Private,
        Some("D") => ChannelKind::Dm,
        Some("G") => ChannelKind::Group,
        _ => ChannelKind::Public,
    };
    let mut members = Vec::new();
    let name = non_empty(&c["display_name"]).unwrap_or_else(|| s(&c["name"]));
    let mut group = String::new();
    match kind {
        // DM names are "<user id>__<user id>"; the engine shows the peer's name.
        ChannelKind::Dm => {
            let peer = s(&c["name"]).split("__").find(|u| *u != self_id).unwrap_or(self_id).to_string();
            members.push(peer);
        }
        // Group names list every username, ours included.
        ChannelKind::Group => {}
        _ => group = team.unwrap_or_default().to_string(),
    }
    let (mut unread, mut mentions, mut last_read) = (0, 0, 0);
    if let Some(m) = member {
        // Count root posts when the server tracks them (collapsed threads), like the web client.
        let (total, seen) = match (c["total_msg_count_root"].as_i64(), m["msg_count_root"].as_i64()) {
            (Some(t), Some(s)) => (t, s),
            _ => (c["total_msg_count"].as_i64().unwrap_or(0), m["msg_count"].as_i64().unwrap_or(0)),
        };
        unread = (total - seen).max(0) as u32;
        mentions = m["mention_count"].as_u64().unwrap_or(0) as u32;
        last_read = m["last_viewed_at"].as_i64().unwrap_or(0);
    }
    let mut ch = Channel::new(acc, &id, kind, &name);
    ch.topic = s(&c["header"]);
    ch.members = members;
    ch.last_activity = c["last_post_at"].as_i64().unwrap_or(0);
    ch.last_read = last_read;
    ch.unread = unread;
    ch.mentions = mentions;
    ch.group = group;
    // Favorites come from the sidebar categories and are synced both ways.
    ch.starred_synced = true;
    ch
}

/// Favorites and custom categories of one team, from `/channels/categories`.
/// Every channel sits in exactly one category, so this covers them all.
pub fn categories(v: &Value) -> Vec<ChannelSection> {
    let mut out = Vec::new();
    for cat in v["categories"].as_array().into_iter().flatten() {
        let kind = cat["type"].as_str().unwrap_or_default();
        let custom = if kind == "custom" { s(&cat["display_name"]) } else { String::new() };
        for id in cat["channel_ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            out.push(ChannelSection {
                channel_id: id.to_string(),
                starred: Some(kind == "favorites"),
                category: Some(custom.clone()),
            });
        }
    }
    out
}

/// Favorite changes from a `preferences_changed` event (or the legacy preference list).
pub fn favorite_prefs(prefs: &Value) -> Vec<ChannelSection> {
    prefs
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["category"].as_str() == Some("favorite_channel"))
        .map(|p| ChannelSection {
            channel_id: s(&p["name"]),
            starred: Some(p["value"].as_str() == Some("true")),
            category: None,
        })
        .collect()
}

/// Group-message display names include our own username; drop it.
pub fn group_name(display: &str, self_name: &str) -> String {
    let others: Vec<&str> = display.split(',').map(str::trim).filter(|n| !n.is_empty() && *n != self_name).collect();
    if others.is_empty() { display.to_string() } else { others.join(", ") }
}

fn mentions(text: &str, post: &Value, keys: &MentionKeys) -> Vec<String> {
    let mut out = Vec::new();
    if !keys.self_id.is_empty() && keys.words.iter().any(|w| contains_word(text, w)) {
        out.push(keys.self_id.clone());
    }
    let highlight_off = post["props"]["mentionHighlightDisabled"].as_bool() == Some(true);
    if keys.channel_wide && !highlight_off && ["@channel", "@all", "@here"].iter().any(|w| contains_word(text, w)) {
        out.push(MENTION_ALL.to_string());
    }
    out
}

/// Message attachments posted by integrations (GitLab, Jira, alert bots), as Markdown.
fn integration_text(post: &Value) -> String {
    let mut out = String::new();
    for a in post["props"]["attachments"].as_array().into_iter().flatten() {
        let mut part = Vec::new();
        if let Some(t) = non_empty(&a["pretext"]) {
            part.push(t);
        }
        if let Some(title) = non_empty(&a["title"]) {
            part.push(match non_empty(&a["title_link"]) {
                Some(link) => format!("**[{title}]({link})**"),
                None => format!("**{title}**"),
            });
        }
        if let Some(t) = non_empty(&a["text"]) {
            part.push(t);
        }
        for f in a["fields"].as_array().into_iter().flatten() {
            let value = match &f["value"] {
                Value::String(v) => v.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            part.push(format!("**{}**: {value}", s(&f["title"])));
        }
        if part.is_empty() {
            part.extend(non_empty(&a["fallback"]));
        }
        if !part.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&part.join("\n\n"));
        }
    }
    out
}

pub fn post(acc: &str, server: &str, p: &Value, keys: &MentionKeys) -> Message {
    let mut text = s(&p["message"]);
    if p["type"].as_str().is_some_and(|t| t.starts_with("system_")) && !text.trim().is_empty() {
        text = format!("_{}_", text.trim());
    }
    let extra = integration_text(p);
    if !extra.is_empty() {
        text = if text.is_empty() { extra } else { format!("{text}\n\n{extra}") };
    }
    let meta = &p["metadata"];

    let mut reactions: Vec<Reaction> = Vec::new();
    for r in meta["reactions"].as_array().into_iter().flatten() {
        let (emoji, user) = (s(&r["emoji_name"]), s(&r["user_id"]));
        match reactions.iter_mut().find(|x| x.emoji == emoji) {
            Some(x) => x.users.push(user),
            None => reactions.push(Reaction { emoji, users: vec![user] }),
        }
    }

    let attachments = meta["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| {
            let id = s(&f["id"]);
            let mime = s(&f["mime_type"]);
            // The preview is a resized JPEG; the thumbnail is too small for the timeline.
            let thumb = f["has_preview_image"]
                .as_bool()
                .unwrap_or(false)
                .then(|| media_url(acc, &format!("/files/{id}/preview")));
            // Images are shown in the app; other files open in the browser, which is
            // normally signed in to the same server.
            let url = if mime.starts_with("image/") {
                media_url(acc, &format!("/files/{id}"))
            } else {
                format!("{server}/api/v4/files/{id}?download=1")
            };
            Attachment {
                url,
                thumb_url: thumb,
                name: s(&f["name"]),
                mime,
                size: f["size"].as_u64().unwrap_or(0),
                width: f["width"].as_u64().filter(|w| *w > 0).map(|w| w as u32),
                height: f["height"].as_u64().filter(|h| *h > 0).map(|h| h as u32),
                id,
            }
        })
        .collect();

    let links = meta["embeds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["type"].as_str() == Some("opengraph"))
        .filter_map(|e| {
            let url = non_empty(&e["url"])?;
            let og = &e["data"];
            let image = og["images"].get(0).and_then(|i| non_empty(&i["secure_url"]).or_else(|| non_empty(&i["url"])));
            Some(LinkPreview {
                title: s(&og["title"]),
                description: s(&og["description"]),
                image_url: image.map(|u| if u.starts_with('/') { media_url(acc, &u) } else { u }),
                url,
            })
        })
        .collect();

    let edit = p["edit_at"].as_i64().unwrap_or(0);
    Message {
        account_id: acc.to_string(),
        channel_id: s(&p["channel_id"]),
        id: s(&p["id"]),
        author_id: s(&p["user_id"]),
        ts: p["create_at"].as_i64().unwrap_or(0),
        mentions: mentions(&text, p, keys),
        text,
        thread_root: non_empty(&p["root_id"]),
        reply_count: p["reply_count"].as_u64().unwrap_or(0) as u32,
        edited_at: (edit > 0).then_some(edit),
        deleted: p["delete_at"].as_i64().unwrap_or(0) > 0,
        // Mattermost shows replies in the channel too (unless the reader collapses threads).
        in_channel: true,
        reactions,
        attachments,
        links,
    }
}

/// Posts from a `PostList` (`order` + `posts`), oldest first. Uses `order` when present,
/// because `posts` may also hold thread roots outside the requested page.
pub fn post_list(acc: &str, server: &str, v: &Value, keys: &MentionKeys) -> Vec<Message> {
    let posts = &v["posts"];
    let mut out: Vec<Message> = match v["order"].as_array().filter(|o| !o.is_empty()) {
        Some(order) => order.iter().filter_map(|id| posts.get(id.as_str()?)).map(|p| post(acc, server, p, keys)).collect(),
        None => posts.as_object().into_iter().flatten().map(|(_, p)| post(acc, server, p, keys)).collect(),
    };
    out.sort_by_key(|m| m.ts);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn keys() -> MentionKeys {
        MentionKeys::from_me(&json!({
            "id": "me", "username": "nico", "first_name": "Nicola",
            "notify_props": { "mention_keys": "nico,@nico,urgent-ops", "first_name": "true", "channel": "true" }
        }))
    }

    #[test]
    fn mention_detection() {
        let k = keys();
        let p = json!({});
        assert_eq!(mentions("ping @nico please", &p, &k), vec!["me"]);
        assert_eq!(mentions("Nicola, look", &p, &k), vec!["me"]);
        assert!(mentions("@nicolas is someone else", &p, &k).is_empty());
        assert_eq!(mentions("@here meeting", &p, &k), vec![MENTION_ALL]);
        let off = json!({ "props": { "mentionHighlightDisabled": true } });
        assert!(mentions("@channel", &off, &k).is_empty());
    }

    #[test]
    fn post_with_files_reactions_and_thread() {
        let p = json!({
            "id": "p1", "channel_id": "c1", "user_id": "u1", "create_at": 1000, "edit_at": 2000,
            "delete_at": 0, "root_id": "r1", "message": "hi @nico", "reply_count": 0,
            "metadata": {
                "files": [{ "id": "f1", "name": "plot.png", "mime_type": "image/png", "size": 10, "has_preview_image": true }],
                "reactions": [{ "user_id": "u1", "emoji_name": "+1" }, { "user_id": "u2", "emoji_name": "+1" }],
                "embeds": [{ "type": "opengraph", "url": "https://example.org", "data": { "title": "Example" } }]
            }
        });
        let m = post("acc", "https://mm", &p, &keys());
        assert_eq!(m.thread_root.as_deref(), Some("r1"));
        assert_eq!(m.edited_at, Some(2000));
        assert_eq!(m.mentions, vec!["me"]);
        assert_eq!(m.reactions[0].users, vec!["u1", "u2"]);
        assert_eq!(m.attachments[0].url, format!("{MEDIA_ORIGIN}/acc/files/f1"));
        assert_eq!(m.attachments[0].thumb_url.as_deref(), Some(format!("{MEDIA_ORIGIN}/acc/files/f1/preview").as_str()));
        assert_eq!(m.links[0].title, "Example");
    }

    #[test]
    fn integration_attachments_become_markdown() {
        let p = json!({ "id": "p", "message": "", "props": { "attachments": [
            { "title": "Build failed", "title_link": "https://ci", "text": "job 42", "fields": [{ "title": "Branch", "value": "main" }] }
        ]}});
        assert_eq!(post("a", "https://mm", &p, &keys()).text, "**[Build failed](https://ci)**\n\njob 42\n\n**Branch**: main");
    }

    #[test]
    fn channels() {
        let dm = json!({ "id": "d", "type": "D", "name": "me__u9", "total_msg_count": 5, "last_post_at": 7 });
        let c = channel("a", &dm, Some(&json!({ "msg_count": 3, "mention_count": 1 })), "me", Some("Team"));
        assert_eq!((c.kind, c.members.clone(), c.unread, c.mentions), (ChannelKind::Dm, vec!["u9".to_string()], 2, 1));
        assert_eq!(c.group, "", "DMs belong to no team");
        let open = json!({ "id": "o", "type": "O", "display_name": "Town Square" });
        let c = channel("a", &open, None, "me", Some("Research"));
        assert_eq!((c.name.as_str(), c.group.as_str()), ("Town Square", "Research"));
        assert_eq!(group_name("ada, nico, vera", "nico"), "ada, vera");
    }

    #[test]
    fn sidebar_categories() {
        let v = json!({ "categories": [
            { "type": "favorites", "display_name": "Favorites", "channel_ids": ["a"] },
            { "type": "custom", "display_name": "LISA", "channel_ids": ["b"] },
            { "type": "channels", "display_name": "Channels", "channel_ids": ["c"] }
        ]});
        let s: Vec<(String, Option<bool>, Option<String>)> =
            categories(&v).into_iter().map(|x| (x.channel_id, x.starred, x.category)).collect();
        assert_eq!(s[0], ("a".into(), Some(true), Some(String::new())));
        assert_eq!(s[1], ("b".into(), Some(false), Some("LISA".into())));
        assert_eq!(s[2], ("c".into(), Some(false), Some(String::new())));
        let prefs = json!([{ "category": "favorite_channel", "name": "x", "value": "false" }, { "category": "theme", "name": "y" }]);
        let f = favorite_prefs(&prefs);
        assert_eq!((f.len(), f[0].starred), (1, Some(false)));
    }

    #[test]
    fn post_list_follows_order() {
        let v = json!({ "order": ["b", "a"], "posts": {
            "a": { "id": "a", "create_at": 1 }, "b": { "id": "b", "create_at": 2 }, "root": { "id": "root", "create_at": 0 }
        }});
        let ids: Vec<String> = post_list("x", "https://mm", &v, &keys()).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }
}
