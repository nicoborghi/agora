//! Mattermost connector: REST API v4 plus the realtime websocket.
//!
//! Auth is a session token (from the SSO login window or a personal access
//! token), sent as `Authorization: Bearer`. One account covers every team the
//! user belongs to; channel names get a team prefix when there is more than one.

mod convert;

use agora_core::engine::now_ms;
use agora_core::model::*;
use agora_core::service::*;
use async_trait::async_trait;
use convert::{embedded, s, MentionKeys};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message as WsMessage};

/// The web session cookie that the login window captures.
pub const SESSION_COOKIE: &str = "MMAUTHTOKEN";
const PING_EVERY: Duration = Duration::from_secs(30);
/// No frame at all for this long (pings included) means the socket is dead.
const SILENCE: Duration = Duration::from_secs(75);
/// Images are proxied whole; anything bigger opens in the browser instead.
const MAX_MEDIA: u64 = 25 * 1024 * 1024;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("Agora/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap_or_default()
}

fn net(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::Network(e.to_string())
}

/// Maps HTTP failures to the errors the engine understands.
async fn check(resp: Response) -> SResult<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        let secs = ["Retry-After", "X-Ratelimit-Reset"]
            .iter()
            .find_map(|h| resp.headers().get(*h)?.to_str().ok()?.parse::<u64>().ok())
            .unwrap_or(5);
        return Err(ServiceError::RateLimited(Duration::from_secs(secs.clamp(1, 300))));
    }
    let body: Value = resp.json().await.unwrap_or_default();
    let detail = body["message"].as_str().unwrap_or("").to_string();
    Err(match status {
        StatusCode::UNAUTHORIZED => ServiceError::Auth("Mattermost session expired or was revoked".into()),
        s if s.is_server_error() => ServiceError::Network(format!("server error {s}: {detail}")),
        s => ServiceError::Other(format!("Mattermost: {detail} ({s})")),
    })
}

#[derive(Default)]
struct State {
    keys: MentionKeys,
    self_name: String,
    /// Team names by id, only filled when the account spans several teams.
    team_prefix: HashMap<String, String>,
    /// Ids of the teams this account covers.
    teams: Vec<String>,
    /// Favorite flag and custom category per channel, from the sidebar categories.
    sections: HashMap<String, (bool, String)>,
    known_users: HashSet<String>,
    known_channels: HashSet<String>,
    /// DM partners, whose presence is shown.
    peers: Vec<String>,
}

pub struct MattermostService {
    account_id: String,
    server: String,
    token: String,
    team_filter: Option<String>,
    http: reqwest::Client,
    state: Mutex<State>,
}

impl MattermostService {
    pub fn new(account: &Account, token: String) -> Self {
        Self {
            account_id: account.id.clone(),
            server: account.server_url.trim_end_matches('/').to_string(),
            token,
            team_filter: account.team_id.clone(),
            http: client(),
            state: Mutex::default(),
        }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn req(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, format!("{}/api/v4{path}", self.server)).bearer_auth(&self.token)
    }

    async fn call(&self, rb: RequestBuilder) -> SResult<Value> {
        let resp = check(rb.send().await.map_err(net)?).await?;
        let bytes = resp.bytes().await.map_err(net)?;
        Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> SResult<Value> {
        self.call(self.req(Method::GET, path)).await
    }

    async fn post_json(&self, path: &str, body: Value) -> SResult<Value> {
        self.call(self.req(Method::POST, path).json(&body)).await
    }

    fn keys(&self) -> MentionKeys {
        self.st().keys.clone()
    }

    fn self_id(&self) -> String {
        self.st().keys.self_id.clone()
    }

    fn to_channel(&self, c: &Value, member: Option<&Value>) -> Channel {
        let st = self.st();
        let team = st.team_prefix.get(c["team_id"].as_str().unwrap_or_default()).map(String::as_str);
        let mut ch = convert::channel(&self.account_id, c, member, &st.keys.self_id, team);
        if ch.kind == ChannelKind::Group {
            ch.name = convert::group_name(&ch.name, &st.self_name);
        }
        if let Some((starred, category)) = st.sections.get(&ch.id) {
            ch.starred = *starred;
            ch.category = category.clone();
        }
        ch
    }

    /// Remembers sections (so later channel fetches keep them) and returns them.
    fn remember_sections(&self, sections: Vec<ChannelSection>) -> Vec<ChannelSection> {
        let mut st = self.st();
        for s in &sections {
            let entry = st.sections.entry(s.channel_id.clone()).or_default();
            if let Some(f) = s.starred {
                entry.0 = f;
            }
            if let Some(c) = &s.category {
                entry.1 = c.clone();
            }
        }
        sections
    }

    /// Favorites and custom categories of one team; falls back to the favorite
    /// preferences on servers older than sidebar categories (5.32).
    async fn team_sections(&self, team_id: &str) -> SResult<Vec<ChannelSection>> {
        match self.get(&format!("/users/me/teams/{team_id}/channels/categories")).await {
            Ok(v) => Ok(convert::categories(&v)),
            Err(ServiceError::Auth(e)) => Err(ServiceError::Auth(e)),
            Err(_) => Ok(convert::favorite_prefs(&self.get("/users/me/preferences/favorite_channel").await?)),
        }
    }

    /// GET on an absolute URL (team icon, favicon), as an image.
    async fn fetch_url(&self, url: &str, auth: bool) -> SResult<Media> {
        let mut rb = self.http.get(url);
        if auth {
            rb = rb.bearer_auth(&self.token);
        }
        let resp = check(rb.send().await.map_err(net)?).await?;
        if resp.content_length().is_some_and(|n| n > MAX_MEDIA) {
            return Err(ServiceError::Other("file too large to preview".into()));
        }
        let mime = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = resp.bytes().await.map_err(net)?.to_vec();
        Ok(Media { mime, bytes })
    }

    async fn fetch_users(&self, ids: &[String]) -> SResult<Vec<User>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let v = self.post_json("/users/ids", json!(ids)).await?;
        let users: Vec<User> =
            v.as_array().into_iter().flatten().map(|u| convert::user(&self.account_id, u)).collect();
        self.st().known_users.extend(users.iter().map(|u| u.id.clone()));
        Ok(users)
    }

    /// A conversation we did not know (new DM, added to a channel): fetch it with our membership.
    async fn fetch_channel(&self, id: &str) -> SResult<Channel> {
        let c = self.get(&format!("/channels/{id}")).await?;
        let member = self.get(&format!("/channels/{id}/members/me")).await.ok();
        let ch = self.to_channel(&c, member.as_ref());
        let mut st = self.st();
        st.known_channels.insert(ch.id.clone());
        if ch.kind == ChannelKind::Dm {
            st.peers.extend(ch.members.iter().cloned());
        }
        Ok(ch)
    }

    /// Sends the users and channel a new post refers to before the post itself,
    /// so the engine never shows a raw id.
    async fn introduce(&self, sink: &EventSink, channel_id: &str, user_id: &str) -> bool {
        let channel_known = self.st().known_channels.contains(channel_id);
        if !channel_known {
            if let Ok(ch) = self.fetch_channel(channel_id).await {
                let mut ids = ch.members.clone();
                ids.retain(|u| !self.st().known_users.contains(u));
                for u in self.fetch_users(&ids).await.unwrap_or_default() {
                    if !sink.send(ServiceEvent::UserUpsert(u)).await {
                        return false;
                    }
                }
                if !sink.send(ServiceEvent::ChannelUpsert(ch)).await {
                    return false;
                }
            }
        }
        let user_known = self.st().known_users.contains(user_id);
        if !user_known && !user_id.is_empty() {
            for u in self.fetch_users(&[user_id.to_string()]).await.unwrap_or_default() {
                if !sink.send(ServiceEvent::UserUpsert(u)).await {
                    return false;
                }
            }
        }
        true
    }

    /// Status of DM partners and of ourselves (for the status bar).
    async fn send_presence(&self, sink: &EventSink) -> bool {
        let mut peers = self.st().peers.clone();
        peers.push(self.self_id());
        let Ok(v) = self.post_json("/users/status/ids", json!(peers)).await else { return true };
        for st in v.as_array().into_iter().flatten() {
            let presence = presence(st["status"].as_str().unwrap_or_default());
            if !sink.send(ServiceEvent::Presence { user_id: s(&st["user_id"]), presence }).await {
                return false;
            }
        }
        true
    }

    /// Handles one websocket event. Returns `false` once the engine is gone.
    async fn on_event(&self, sink: &EventSink, ev: &Value) -> bool {
        let data = &ev["data"];
        let bc = &ev["broadcast"];
        let events: Vec<ServiceEvent> = match ev["event"].as_str().unwrap_or_default() {
            "hello" => {
                return sink.send(ServiceEvent::Connection(ConnState::Online)).await && self.send_presence(sink).await;
            }
            "posted" => {
                let p = embedded(&data["post"]);
                if !self.introduce(sink, p["channel_id"].as_str().unwrap_or_default(), p["user_id"].as_str().unwrap_or_default()).await {
                    return false;
                }
                let mut m = convert::post(&self.account_id, &self.server, &p, &self.keys());
                // The server's own mention list also covers keywords we may not know about.
                let me = self.self_id();
                let server_mentions = embedded(&data["mentions"]);
                if server_mentions.as_array().is_some_and(|a| a.iter().any(|u| u.as_str() == Some(me.as_str())))
                    && !m.mentions.contains(&me)
                {
                    m.mentions.push(me);
                }
                vec![ServiceEvent::MessageUpsert(m)]
            }
            "post_edited" => vec![ServiceEvent::MessageUpsert(convert::post(&self.account_id, &self.server, &embedded(&data["post"]), &self.keys()))],
            "post_deleted" => {
                let p = embedded(&data["post"]);
                vec![ServiceEvent::MessageDeleted { channel_id: s(&p["channel_id"]), id: s(&p["id"]) }]
            }
            e @ ("reaction_added" | "reaction_removed") => {
                let r = embedded(&data["reaction"]);
                let channel_id = convert::s(&bc["channel_id"]);
                let channel_id = if channel_id.is_empty() { s(&r["channel_id"]) } else { channel_id };
                vec![ServiceEvent::Reaction {
                    channel_id,
                    message_id: s(&r["post_id"]),
                    emoji: s(&r["emoji_name"]),
                    user_id: s(&r["user_id"]),
                    added: e == "reaction_added",
                }]
            }
            "channel_viewed" => vec![ServiceEvent::ChannelRead { channel_id: s(&data["channel_id"]), ts: now_ms() }],
            "multiple_channels_viewed" => embedded(&data["channel_times"])
                .as_object()
                .into_iter()
                .flatten()
                .map(|(id, ts)| ServiceEvent::ChannelRead { channel_id: id.clone(), ts: ts.as_i64().unwrap_or_else(now_ms) })
                .collect(),
            "typing" => {
                let user_id = s(&data["user_id"]);
                if user_id == self.self_id() {
                    return true;
                }
                vec![ServiceEvent::Typing {
                    channel_id: s(&bc["channel_id"]),
                    user_id,
                    thread_root: data["parent_id"].as_str().filter(|p| !p.is_empty()).map(str::to_string),
                }]
            }
            "status_change" => vec![ServiceEvent::Presence {
                user_id: s(&data["user_id"]),
                presence: presence(data["status"].as_str().unwrap_or_default()),
            }],
            "user_updated" => vec![ServiceEvent::UserUpsert(convert::user(&self.account_id, &embedded(&data["user"])))],
            e @ ("channel_created" | "channel_updated" | "channel_converted" | "channel_restored" | "direct_added"
            | "group_added" | "user_added") => {
                if e == "user_added" && data["user_id"].as_str() != Some(self.self_id().as_str()) {
                    return true;
                }
                let id = [&data["channel_id"], &bc["channel_id"], &embedded(&data["channel"])["id"]]
                    .iter()
                    .find_map(|v| v.as_str().filter(|x| !x.is_empty()).map(str::to_string));
                match id {
                    Some(id) => match self.fetch_channel(&id).await {
                        Ok(ch) => vec![ServiceEvent::ChannelUpsert(ch)],
                        Err(e) => {
                            tracing::debug!("channel {id} not loaded: {e}");
                            vec![]
                        }
                    },
                    None => vec![],
                }
            }
            // Categories created, renamed, reordered or channels moved (favorites included).
            e if e.starts_with("sidebar_category_") => {
                let team = [&bc["team_id"], &data["team_id"]]
                    .iter()
                    .find_map(|v| v.as_str().filter(|x| !x.is_empty()).map(str::to_string));
                let teams = match team {
                    Some(t) => vec![t],
                    None => self.st().teams.clone(),
                };
                let mut out = Vec::new();
                for t in teams {
                    match self.team_sections(&t).await {
                        Ok(list) => out.extend(self.remember_sections(list)),
                        Err(e) => tracing::debug!("categories of team {t} not reloaded: {e}"),
                    }
                }
                if out.is_empty() { vec![] } else { vec![ServiceEvent::Sections(out)] }
            }
            e @ ("preferences_changed" | "preference_changed" | "preferences_deleted") => {
                let prefs = match data.get("preferences") {
                    Some(p) => embedded(p),
                    None => json!([embedded(&data["preference"])]),
                };
                let mut list = convert::favorite_prefs(&prefs);
                if e == "preferences_deleted" {
                    list.iter_mut().for_each(|x| x.starred = Some(false));
                }
                if list.is_empty() { vec![] } else { vec![ServiceEvent::Sections(self.remember_sections(list))] }
            }
            _ => vec![],
        };
        for e in events {
            if !sink.send(e).await {
                return false;
            }
        }
        true
    }
}

fn presence(status: &str) -> Presence {
    match status {
        "online" => Presence::Online,
        "away" => Presence::Away,
        "dnd" => Presence::Dnd,
        "offline" => Presence::Offline,
        _ => Presence::Unknown,
    }
}

#[async_trait]
impl Service for MattermostService {
    async fn bootstrap(&self) -> SResult<Bootstrap> {
        let me = self.get("/users/me").await?;
        let self_user = convert::user(&self.account_id, &me);
        let mut teams: Vec<Value> = self.get("/users/me/teams").await?.as_array().cloned().unwrap_or_default();
        if let Some(only) = &self.team_filter {
            teams.retain(|t| t["id"].as_str() == Some(only.as_str()));
        }

        // Favorites and custom categories first, so every channel below gets them.
        // A DM can sit in several teams' categories: a favorite anywhere counts.
        let mut sections: HashMap<String, (bool, String)> = HashMap::new();
        let mut sections_ok = true;
        for t in &teams {
            match self.team_sections(&s(&t["id"])).await {
                Ok(list) => {
                    for x in list {
                        let e = sections.entry(x.channel_id).or_default();
                        e.0 |= x.starred.unwrap_or(false);
                        if let Some(c) = x.category.filter(|c| !c.is_empty()) {
                            e.1 = c;
                        }
                    }
                }
                Err(ServiceError::Auth(e)) => return Err(ServiceError::Auth(e)),
                Err(e) => {
                    tracing::warn!("favorites unavailable, keeping the local ones: {e}");
                    sections_ok = false;
                }
            }
        }
        {
            let mut st = self.st();
            st.keys = MentionKeys::from_me(&me);
            st.self_name = self_user.name.clone();
            st.team_prefix = if teams.len() > 1 {
                teams.iter().map(|t| (s(&t["id"]), s(&t["display_name"]))).collect()
            } else {
                HashMap::new()
            };
            st.teams = teams.iter().map(|t| s(&t["id"])).collect();
            st.sections = sections;
        }

        let mut channels: Vec<Channel> = Vec::new();
        let mut seen = HashSet::new();
        for t in &teams {
            let tid = s(&t["id"]);
            let list = self.get(&format!("/users/me/teams/{tid}/channels")).await?;
            let members = self.get(&format!("/users/me/teams/{tid}/channels/members")).await?;
            let by_channel: HashMap<&str, &Value> = members
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| Some((m["channel_id"].as_str()?, m)))
                .collect();
            for c in list.as_array().into_iter().flatten() {
                let id = c["id"].as_str().unwrap_or_default();
                // DMs and groups are listed again under every team.
                if c["delete_at"].as_i64().unwrap_or(0) > 0 || !seen.insert(id.to_string()) {
                    continue;
                }
                let mut ch = self.to_channel(c, by_channel.get(id).copied());
                ch.starred_synced = sections_ok;
                channels.push(ch);
            }
        }

        let peers: Vec<String> = channels
            .iter()
            .filter(|c| c.kind == ChannelKind::Dm)
            .flat_map(|c| c.members.iter().cloned())
            .filter(|u| *u != self_user.id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let mut users = Vec::new();
        for chunk in peers.chunks(200) {
            users.extend(self.fetch_users(chunk).await?);
        }
        {
            let mut st = self.st();
            st.known_users.insert(self_user.id.clone());
            st.known_channels = channels.iter().map(|c| c.id.clone()).collect();
            st.peers = peers;
        }
        Ok(Bootstrap { self_user, users, channels })
    }

    async fn run(&self, sink: EventSink) -> SResult<()> {
        let url = format!(
            "{}/api/v4/websocket",
            self.server.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1)
        );
        let mut req = url.into_client_request().map_err(net)?;
        let bearer = HeaderValue::from_str(&format!("Bearer {}", self.token)).map_err(net)?;
        req.headers_mut().insert("Authorization", bearer);
        let (ws, _) = tokio_tungstenite::connect_async(req).await.map_err(|e| match &e {
            tokio_tungstenite::tungstenite::Error::Http(r) if r.status() == StatusCode::UNAUTHORIZED.as_u16() => {
                ServiceError::Auth("Mattermost session expired or was revoked".into())
            }
            _ => net(e),
        })?;
        let (mut tx, mut rx) = ws.split();
        // Header auth is enough on current servers; the challenge covers older ones.
        let challenge = json!({ "seq": 1, "action": "authentication_challenge", "data": { "token": self.token } });
        tx.send(WsMessage::Text(challenge.to_string())).await.map_err(net)?;

        let mut seq = 2u64;
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.reset();
        loop {
            tokio::select! {
                frame = tokio::time::timeout(SILENCE, rx.next()) => {
                    let text = match frame {
                        Err(_) => return Err(ServiceError::Network("no data from the server for 75 s".into())),
                        Ok(None) | Ok(Some(Ok(WsMessage::Close(_)))) => return Err(ServiceError::Network("socket closed".into())),
                        Ok(Some(Err(e))) => return Err(net(e)),
                        Ok(Some(Ok(WsMessage::Text(t)))) => t,
                        Ok(Some(Ok(_))) => continue,
                    };
                    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    if v["seq_reply"] == 1 && v["status"].as_str() == Some("FAIL") {
                        return Err(ServiceError::Auth("Mattermost refused the session".into()));
                    }
                    if v["event"].is_string() && !self.on_event(&sink, &v).await {
                        return Ok(());
                    }
                }
                _ = ping.tick() => {
                    let msg = json!({ "seq": seq, "action": "ping" });
                    seq += 1;
                    tx.send(WsMessage::Text(msg.to_string())).await.map_err(net)?;
                }
            }
        }
    }

    async fn history(&self, channel_id: &str, q: HistoryQuery) -> SResult<Vec<Message>> {
        let path = match (q.after, &q.before_id) {
            // Everything created or changed since then, edits and deletions included.
            (Some(after), _) => format!("/channels/{channel_id}/posts?since={after}"),
            (None, Some(before)) => {
                format!("/channels/{channel_id}/posts?per_page={}&before={before}&collapsedThreads=true", q.limit.min(200))
            }
            (None, None) if q.before.is_some() => return Ok(vec![]),
            (None, None) => format!("/channels/{channel_id}/posts?per_page={}&collapsedThreads=true", q.limit.min(200)),
        };
        let v = self.get(&path).await?;
        let mut msgs = convert::post_list(&self.account_id, &self.server, &v, &self.keys());
        if let Some(after) = q.after {
            // `since` also returns older posts that were edited; keep those (they update the
            // archive) but never return a page the engine would read as "nothing newer".
            msgs.retain(|m| m.ts > after || m.edited_at.is_some() || m.deleted);
        }
        Ok(msgs)
    }

    async fn thread(&self, _channel_id: &str, root_id: &str) -> SResult<Vec<Message>> {
        let v = self.get(&format!("/posts/{root_id}/thread")).await?;
        Ok(convert::post_list(&self.account_id, &self.server, &json!({ "posts": v["posts"] }), &self.keys()))
    }

    async fn send(&self, channel_id: &str, text: &str, thread_root: Option<&str>) -> SResult<Message> {
        let body = json!({ "channel_id": channel_id, "message": text, "root_id": thread_root.unwrap_or("") });
        let p = self.post_json("/posts", body).await?;
        Ok(convert::post(&self.account_id, &self.server, &p, &self.keys()))
    }

    async fn edit(&self, _channel_id: &str, message_id: &str, text: &str) -> SResult<()> {
        self.call(self.req(Method::PUT, &format!("/posts/{message_id}/patch")).json(&json!({ "message": text })))
            .await
            .map(drop)
    }

    async fn delete(&self, _channel_id: &str, message_id: &str) -> SResult<()> {
        self.call(self.req(Method::DELETE, &format!("/posts/{message_id}"))).await.map(drop)
    }

    async fn react(&self, _channel_id: &str, message_id: &str, emoji: &str, add: bool) -> SResult<()> {
        let me = self.self_id();
        if add {
            let body = json!({ "user_id": me, "post_id": message_id, "emoji_name": emoji });
            self.post_json("/reactions", body).await.map(drop)
        } else {
            let path = format!("/users/{me}/posts/{message_id}/reactions/{emoji}");
            self.call(self.req(Method::DELETE, &path)).await.map(drop)
        }
    }

    async fn mark_read(&self, channel_id: &str, _latest: Option<&Message>) -> SResult<()> {
        self.post_json("/channels/members/me/view", json!({ "channel_id": channel_id, "prev_channel_id": "" }))
            .await
            .map(drop)
    }

    async fn upload(&self, channel_id: &str, file_name: &str, bytes: Vec<u8>, thread_root: Option<&str>) -> SResult<()> {
        let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name.to_string());
        let form = reqwest::multipart::Form::new().text("channel_id", channel_id.to_string()).part("files", part);
        let v = self.call(self.req(Method::POST, "/files").multipart(form)).await?;
        let file_id = s(&v["file_infos"][0]["id"]);
        if file_id.is_empty() {
            return Err(ServiceError::Other("upload returned no file".into()));
        }
        let body = json!({ "channel_id": channel_id, "message": "", "root_id": thread_root.unwrap_or(""), "file_ids": [file_id] });
        self.post_json("/posts", body).await.map(drop)
    }

    async fn users(&self, ids: &[String]) -> SResult<Vec<User>> {
        self.fetch_users(ids).await
    }

    async fn profile(&self, user_id: &str) -> SResult<Profile> {
        let v = self.get(&format!("/users/{user_id}")).await?;
        let mut p = convert::profile(&self.account_id, &v);
        if let Ok(st) = self.get(&format!("/users/{user_id}/status")).await {
            p.presence = presence(st["status"].as_str().unwrap_or_default());
        }
        Ok(p)
    }

    async fn members(&self, channel_id: &str, cursor: Option<&str>) -> SResult<MemberPage> {
        const PER_PAGE: usize = 100;
        let page: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
        let v = self
            .get(&format!("/users?in_channel={channel_id}&page={page}&per_page={PER_PAGE}&sort=status&active=true"))
            .await?;
        let users: Vec<User> = v.as_array().into_iter().flatten().map(|u| convert::user(&self.account_id, u)).collect();
        self.st().known_users.extend(users.iter().map(|u| u.id.clone()));
        let ids: Vec<&str> = users.iter().map(|u| u.id.as_str()).collect();
        let presence = match self.post_json("/users/status/ids", json!(ids)).await {
            Ok(v) => v
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| (s(&x["user_id"]), presence(x["status"].as_str().unwrap_or_default())))
                .collect(),
            Err(_) => vec![],
        };
        let total = if page == 0 {
            self.get(&format!("/channels/{channel_id}/stats")).await.ok().and_then(|v| v["member_count"].as_u64())
        } else {
            None
        };
        let next = (users.len() == PER_PAGE).then(|| (page + 1).to_string());
        Ok(MemberPage { users, presence, next, total })
    }

    async fn fetch(&self, path: &str) -> SResult<Media> {
        // Only what the UI needs: files, previews, avatars and the server's image proxy.
        let allowed = path.starts_with("/files/")
            || (path.starts_with("/users/") && path.ends_with("/image"))
            || path.starts_with("/image?");
        if !allowed || path.contains("..") {
            return Err(ServiceError::Other("path not allowed".into()));
        }
        self.fetch_url(&format!("{}/api/v4{path}", self.server), true).await
    }

    /// Favorites go through the `favorite_channel` preference, which the server keeps in
    /// step with the Favorites category of every team (and the phone and web apps follow).
    async fn set_starred(&self, channel_id: &str, starred: bool) -> SResult<()> {
        let me = self.self_id();
        let prefs = json!([{ "user_id": me, "category": "favorite_channel", "name": channel_id, "value": "true" }]);
        let path = if starred { format!("/users/{me}/preferences") } else { format!("/users/{me}/preferences/delete") };
        let method = if starred { Method::PUT } else { Method::POST };
        self.call(self.req(method, &path).json(&prefs)).await?;
        self.remember_sections(vec![ChannelSection { channel_id: channel_id.to_string(), starred: Some(starred), category: None }]);
        Ok(())
    }

    /// Searches every team of the account, like the search box of the web app.
    async fn search(&self, query: &str, limit: u32) -> SResult<Vec<Message>> {
        let teams = self.st().teams.clone();
        let body = json!({
            "terms": query, "is_or_search": false, "include_deleted_channels": false,
            "time_zone_offset": 0, "page": 0, "per_page": limit.min(100)
        });
        let keys = self.keys();
        let mut out: Vec<Message> = Vec::new();
        for t in teams {
            let v = self.post_json(&format!("/teams/{t}/posts/search"), body.clone()).await?;
            for m in convert::post_list(&self.account_id, &self.server, &v, &keys) {
                if !out.iter().any(|x| x.id == m.id) {
                    out.push(m);
                }
            }
        }
        out.sort_by(|a, b| b.ts.cmp(&a.ts));
        out.truncate(limit as usize);
        Ok(out)
    }

    async fn set_presence(&self, p: Presence) -> SResult<()> {
        let status = match p {
            Presence::Online => "online",
            Presence::Away => "away",
            Presence::Dnd => "dnd",
            Presence::Offline => "offline",
            Presence::Unknown => return Err(ServiceError::Unsupported),
        };
        let me = self.self_id();
        self.call(self.req(Method::PUT, "/users/me/status").json(&json!({ "user_id": me, "status": status })))
            .await
            .map(drop)
    }

    /// The team icon for a single-team account, otherwise the server's favicon
    /// (the Mattermost logo unless the admin replaced it).
    async fn workspace_icon(&self) -> SResult<Option<Media>> {
        let teams = self.st().teams.clone();
        if let [team] = teams.as_slice() {
            let t = self.get(&format!("/teams/{team}")).await?;
            if t["last_team_icon_update"].as_i64().unwrap_or(0) > 0 {
                if let Ok(m) = self.fetch_url(&format!("{}/api/v4/teams/{team}/image", self.server), true).await {
                    return Ok(Some(m));
                }
            }
        }
        for path in ["/static/images/favicon/favicon-default-64x64.png", "/favicon.ico"] {
            if let Ok(m) = self.fetch_url(&format!("{}{path}", self.server), false).await {
                if m.mime.starts_with("image/") {
                    return Ok(Some(m));
                }
            }
        }
        Ok(None)
    }
}

// ------------------------------------------------------------ sign-in helpers

pub struct ServerInfo {
    /// Base URL without trailing slash.
    pub url: String,
    pub site_name: String,
}

pub struct WhoAmI {
    pub user_id: String,
    pub username: String,
    pub teams: Vec<String>,
}

/// Finds the Mattermost base URL in whatever the user pasted (a channel link,
/// a login page, a bare host) by pinging successively shorter paths.
pub async fn probe_server(raw: &str) -> Result<ServerInfo, String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() {
        return Err("Enter the server address".into());
    }
    let full = if raw.starts_with("http://") || raw.starts_with("https://") { raw.to_string() } else { format!("https://{raw}") };
    let scheme_end = full.find("://").map(|i| i + 3).unwrap_or(0);
    let mut candidate = full.split(['?', '#']).next().unwrap_or(&full).to_string();
    let http = client();
    loop {
        let ping = http.get(format!("{candidate}/api/v4/system/ping")).send().await;
        if let Ok(r) = ping {
            if r.status().is_success() && r.json::<Value>().await.is_ok_and(|v| v["status"] == "OK") {
                let cfg: Value = match http.get(format!("{candidate}/api/v4/config/client?format=old")).send().await {
                    Ok(r) => r.json().await.unwrap_or_default(),
                    Err(_) => Value::Null,
                };
                let site_name = cfg["SiteName"].as_str().unwrap_or("Mattermost").to_string();
                return Ok(ServerInfo { url: candidate, site_name });
            }
        }
        match candidate.rfind('/') {
            Some(i) if i >= scheme_end => candidate.truncate(i),
            _ => return Err(format!("No Mattermost server found at {raw}")),
        }
    }
}

/// Checks a token and returns who it belongs to.
pub async fn whoami(server: &str, token: &str) -> Result<WhoAmI, String> {
    let http = client();
    let get = |path: &str| http.get(format!("{server}/api/v4{path}")).bearer_auth(token).send();
    let me: Value = check(get("/users/me").await.map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let teams: Value = match get("/users/me/teams").await {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(_) => Value::Null,
    };
    Ok(WhoAmI {
        user_id: s(&me["id"]),
        username: s(&me["username"]),
        teams: teams.as_array().into_iter().flatten().map(|t| s(&t["display_name"])).collect(),
    })
}
