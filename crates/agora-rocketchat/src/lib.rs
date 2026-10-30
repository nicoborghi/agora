//! Rocket.Chat connector: REST API v1 plus the realtime DDP websocket.
//!
//! Auth is a user id + login token pair, from the sign-in window (SSO, 2FA and
//! passwords as in the browser) or a personal access token, sent as
//! `X-User-Id` / `X-Auth-Token`. Teams become sidebar sections and discussions
//! a "Discussions" category; favorites are the subscriptions' `f` flag.

mod convert;

use agora_core::engine::now_ms;
use agora_core::model::*;
use agora_core::service::*;
use async_trait::async_trait;
use convert::{iso, s};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Cookies the web client sets after login (used by the server for file access).
pub const COOKIE_UID: &str = "rc_uid";
pub const COOKIE_TOKEN: &str = "rc_token";
/// Copies written by [`SIGNIN_SCRIPT`], for servers that keep the token only in localStorage.
pub const COOKIE_UID_COPY: &str = "agora_rc_uid";
pub const COOKIE_TOKEN_COPY: &str = "agora_rc_token";
/// Runs in the sign-in window only: copies the Meteor login token from localStorage into
/// first-party cookies, where the app can read it. Pages without a token are left alone.
pub const SIGNIN_SCRIPT: &str = r#"setInterval(function () {
  try {
    var t = localStorage.getItem('Meteor.loginToken'), u = localStorage.getItem('Meteor.userId');
    if (t && u) {
      document.cookie = 'agora_rc_token=' + encodeURIComponent(t) + '; path=/; SameSite=Strict';
      document.cookie = 'agora_rc_uid=' + encodeURIComponent(u) + '; path=/; SameSite=Strict';
    }
  } catch (e) {}
}, 700);"#;

const PING_EVERY: Duration = Duration::from_secs(30);
/// No frame at all for this long (pings included) means the socket is dead.
const SILENCE: Duration = Duration::from_secs(75);
const MAX_MEDIA: u64 = 25 * 1024 * 1024;
/// Rooms whose deletions are followed live; the rest is corrected by the next catch-up.
const MAX_ROOM_STREAMS: usize = 300;
/// DM partners whose status is asked for at connect (later changes arrive anyway).
const MAX_PRESENCE: usize = 30;
/// The server's default upper limit for `count`.
const PAGE: usize = 100;

/// What is kept in Windows Credential Manager for a Rocket.Chat account.
pub struct Credentials {
    pub user_id: String,
    pub token: String,
}

impl Credentials {
    pub fn to_json(&self) -> String {
        json!({ "user_id": self.user_id, "token": self.token }).to_string()
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(raw).ok()?;
        let c = Self { user_id: s(&v["user_id"]), token: s(&v["token"]) };
        (!c.user_id.is_empty() && !c.token.is_empty()).then_some(c)
    }
}

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

fn expired() -> ServiceError {
    ServiceError::Auth("Rocket.Chat session expired or was revoked".into())
}

async fn check(resp: Response) -> SResult<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        let header = |h: &str| resp.headers().get(h)?.to_str().ok()?.parse::<i64>().ok();
        // X-RateLimit-Reset is an epoch in milliseconds.
        let secs = header("Retry-After")
            .or_else(|| header("X-RateLimit-Reset").map(|reset| (reset - now_ms()) / 1000))
            .unwrap_or(10);
        return Err(ServiceError::RateLimited(Duration::from_secs(secs.clamp(1, 300) as u64)));
    }
    if status == StatusCode::UNAUTHORIZED {
        return Err(expired());
    }
    let body: Value = resp.json().await.unwrap_or_default();
    let detail = body["error"].as_str().or(body["message"].as_str()).unwrap_or("").to_string();
    Err(if status.is_server_error() {
        ServiceError::Network(format!("server error {status}: {detail}"))
    } else {
        ServiceError::Other(format!("Rocket.Chat: {detail} ({status})"))
    })
}

fn presence(status: &Value) -> Presence {
    match status.as_str().map(str::to_string).unwrap_or_else(|| status.to_string()).as_str() {
        "online" | "1" => Presence::Online,
        "away" | "2" => Presence::Away,
        "busy" | "3" => Presence::Dnd,
        "offline" | "0" => Presence::Offline,
        _ => Presence::Unknown,
    }
}

#[derive(Default)]
struct State {
    self_id: String,
    self_name: String,
    self_status: Presence,
    /// Room id -> "c" | "p" | "d": picks the history endpoint.
    room_types: HashMap<String, String>,
    team_names: HashMap<String, String>,
    known_users: HashSet<String>,
    /// Username -> user id (reactions list usernames).
    names: HashMap<String, String>,
    /// Authors met in history, handed to the engine by `users()` without a request.
    pending_users: HashMap<String, User>,
    favorites: HashSet<String>,
    /// DM partners, whose status is shown.
    peers: Vec<String>,
}

pub struct RocketChatService {
    account_id: String,
    server: String,
    creds: Credentials,
    http: reqwest::Client,
    state: Mutex<State>,
}

impl RocketChatService {
    pub fn new(account: &Account, creds: Credentials) -> Self {
        Self {
            account_id: account.id.clone(),
            server: account.server_url.trim_end_matches('/').to_string(),
            creds,
            http: client(),
            state: Mutex::default(),
        }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn authed(&self, rb: RequestBuilder) -> RequestBuilder {
        rb.header("X-User-Id", &self.creds.user_id).header("X-Auth-Token", &self.creds.token)
    }

    fn req(&self, method: Method, path: &str) -> RequestBuilder {
        self.authed(self.http.request(method, format!("{}/api/v1/{path}", self.server)))
    }

    async fn call(&self, rb: RequestBuilder) -> SResult<Value> {
        let resp = check(rb.send().await.map_err(net)?).await?;
        let v: Value = resp.json().await.map_err(net)?;
        if v["success"].as_bool() == Some(false) {
            return Err(ServiceError::Other(format!("Rocket.Chat: {}", s(&v["error"]))));
        }
        Ok(v)
    }

    async fn get(&self, path: &str) -> SResult<Value> {
        self.call(self.req(Method::GET, path)).await
    }

    async fn post(&self, path: &str, body: Value) -> SResult<Value> {
        self.call(self.req(Method::POST, path).json(&body)).await
    }

    fn history_endpoint(&self, rid: &str) -> &'static str {
        match self.st().room_types.get(rid).map(String::as_str) {
            Some("p") => "groups.history",
            Some("d") => "im.history",
            _ => "channels.history",
        }
    }

    /// Converts messages, learning their authors on the way (names for reactions,
    /// profiles for the engine).
    fn convert(&self, msgs: &[Value]) -> Vec<Message> {
        let mut st = self.st();
        if st.pending_users.len() > 500 {
            st.pending_users.clear();
        }
        for m in msgs {
            if let Some(u) = convert::author(&self.account_id, m) {
                st.names.insert(u.name.clone(), u.id.clone());
                if !st.known_users.contains(&u.id) {
                    st.pending_users.insert(u.id.clone(), u);
                }
            }
        }
        let uid = |n: &str| st.names.get(n).cloned().unwrap_or_else(|| n.to_string());
        msgs.iter().filter_map(|m| convert::message(&self.account_id, &self.server, m, &uid)).collect()
    }

    /// Remembers what the realtime handlers need to know about a conversation.
    fn remember(&self, ch: &Channel, kind: &str, users: &[User]) {
        let mut st = self.st();
        st.room_types.insert(ch.id.clone(), kind.to_string());
        if ch.starred {
            st.favorites.insert(ch.id.clone());
        } else {
            st.favorites.remove(&ch.id);
        }
        if ch.kind == ChannelKind::Dm {
            for m in &ch.members {
                if !st.peers.contains(m) {
                    st.peers.push(m.clone());
                }
            }
        }
        for u in users {
            st.names.insert(u.name.clone(), u.id.clone());
            st.known_users.insert(u.id.clone());
        }
    }

    /// A conversation we did not know (new DM, invited to a room), with our subscription.
    async fn fetch_channel(&self, rid: &str) -> SResult<(Channel, Vec<User>)> {
        let room = self.get(&format!("rooms.info?roomId={rid}")).await?["room"].clone();
        let sub = self.get(&format!("subscriptions.getOne?roomId={rid}")).await?["subscription"].clone();
        let (ch, users) = {
            let mut st = self.st();
            if room["teamMain"].as_bool() == Some(true) {
                if let (Some(t), Some(n)) = (room["teamId"].as_str(), room["fname"].as_str().or(room["name"].as_str())) {
                    st.team_names.insert(t.to_string(), n.to_string());
                }
            }
            let ch = convert::channel(&self.account_id, &sub, Some(&room), (st.self_id.as_str(), st.self_name.as_str()), &st.team_names)
                .ok_or_else(|| ServiceError::Other("unsupported room type".into()))?;
            let users = if ch.kind.is_direct() { convert::dm_users(&self.account_id, &sub, &room, &st.self_id) } else { vec![] };
            (ch, users)
        };
        self.remember(&ch, sub["t"].as_str().unwrap_or("c"), &users);
        Ok((ch, users))
    }

    /// Sends a newly met conversation with its people. `false` once the engine is gone.
    async fn introduce(&self, sink: &EventSink, rid: &str, watch: &mut Vec<String>) -> bool {
        if rid.is_empty() || self.st().room_types.contains_key(rid) {
            return true;
        }
        match self.fetch_channel(rid).await {
            Ok((ch, users)) => {
                for u in users {
                    if !sink.send(ServiceEvent::UserUpsert(u)).await {
                        return false;
                    }
                }
                watch.push(rid.to_string());
                sink.send(ServiceEvent::ChannelUpsert(ch)).await
            }
            Err(e) => {
                tracing::debug!("room {rid} not loaded: {e}");
                true
            }
        }
    }

    async fn send_presence(&self, sink: &EventSink) -> bool {
        let (me, mine, peers) = {
            let st = self.st();
            (st.self_id.clone(), st.self_status, st.peers.iter().take(MAX_PRESENCE).cloned().collect::<Vec<_>>())
        };
        if !sink.send(ServiceEvent::Presence { user_id: me, presence: mine }).await {
            return false;
        }
        for id in peers {
            if let Ok(v) = self.get(&format!("users.getPresence?userId={id}")).await {
                if !sink.send(ServiceEvent::Presence { user_id: id, presence: presence(&v["presence"]) }).await {
                    return false;
                }
            }
        }
        true
    }

    /// Handles one DDP `changed` frame. Returns `None` once the engine is gone,
    /// otherwise the ids of new rooms to follow.
    async fn on_changed(&self, sink: &EventSink, v: &Value) -> Option<Vec<String>> {
        let event = v["fields"]["eventName"].as_str().unwrap_or_default();
        let args = &v["fields"]["args"];
        let mut watch = Vec::new();
        let mut out: Vec<ServiceEvent> = Vec::new();
        match v["collection"].as_str().unwrap_or_default() {
            "stream-room-messages" => {
                let m = &args[0];
                let rid = s(&m["rid"]);
                if !self.introduce(sink, &rid, &mut watch).await {
                    return None;
                }
                if let Some(u) = convert::author(&self.account_id, m) {
                    let new = self.st().known_users.insert(u.id.clone());
                    if new {
                        out.push(ServiceEvent::UserUpsert(u));
                    }
                }
                for msg in self.convert(std::slice::from_ref(m)) {
                    out.push(if msg.deleted {
                        ServiceEvent::MessageDeleted { channel_id: msg.channel_id, id: msg.id }
                    } else {
                        ServiceEvent::MessageUpsert(msg)
                    });
                }
                // Topic and name changes only arrive as system messages.
                if matches!(m["t"].as_str(), Some("room_changed_topic" | "r")) {
                    if let Ok((ch, _)) = self.fetch_channel(&rid).await {
                        out.push(ServiceEvent::ChannelUpsert(ch));
                    }
                }
            }
            "stream-notify-user" if event.ends_with("/subscriptions-changed") => {
                let sub = &args[1];
                let rid = s(&sub["rid"]);
                if args[0].as_str() == Some("removed") || rid.is_empty() {
                    return Some(watch);
                }
                if !self.st().room_types.contains_key(&rid) {
                    return self.introduce(sink, &rid, &mut watch).await.then_some(watch);
                }
                // Read on another device (or by us): no unread count and no alert left.
                if sub["unread"].as_u64() == Some(0) && sub["alert"].as_bool() != Some(true) {
                    let ts = convert::ts(&sub["ls"]).unwrap_or_else(now_ms);
                    out.push(ServiceEvent::ChannelRead { channel_id: rid.clone(), ts });
                }
                let fav = sub["f"].as_bool().unwrap_or(false);
                let changed = {
                    let mut st = self.st();
                    if fav { st.favorites.insert(rid.clone()) } else { st.favorites.remove(&rid) }
                };
                if changed {
                    out.push(ServiceEvent::Sections(vec![ChannelSection { channel_id: rid, starred: Some(fav), category: None }]));
                }
            }
            "stream-notify-user" if event.ends_with("/rooms-changed") => {
                if args[0].as_str() == Some("inserted") {
                    let rid = s(&args[1]["_id"]);
                    return self.introduce(sink, &rid, &mut watch).await.then_some(watch);
                }
            }
            "stream-notify-logged" if event == "user-status" => {
                let u = &args[0];
                let id = s(&u[0]);
                let wanted = {
                    let st = self.st();
                    id == st.self_id || st.peers.contains(&id)
                };
                if wanted {
                    out.push(ServiceEvent::Presence { user_id: id, presence: presence(&u[2]) });
                }
            }
            "stream-notify-room" if event.ends_with("/deleteMessage") => {
                let rid = event.split('/').next().unwrap_or_default().to_string();
                out.push(ServiceEvent::MessageDeleted { channel_id: rid, id: s(&args[0]["_id"]) });
            }
            _ => {}
        }
        for e in out {
            if !sink.send(e).await {
                return None;
            }
        }
        Some(watch)
    }
}

#[async_trait]
impl Service for RocketChatService {
    async fn bootstrap(&self) -> SResult<Bootstrap> {
        let me = self.get("me").await?;
        let self_id = s(&me["_id"]);
        let self_name = s(&me["username"]);
        let self_user = convert::user(&self.account_id, &self_id, &self_name, me["name"].as_str().unwrap_or(&self_name));
        let subs = self.get("subscriptions.get").await?["update"].as_array().cloned().unwrap_or_default();
        let rooms = self.get("rooms.get").await?["update"].as_array().cloned().unwrap_or_default();
        let by_id: HashMap<&str, &Value> = rooms.iter().filter_map(|r| Some((r["_id"].as_str()?, r))).collect();
        let teams = convert::team_names(&rooms);
        {
            let mut st = self.st();
            st.self_id = self_id.clone();
            st.self_name = self_name.clone();
            st.self_status = presence(&me["status"]);
            st.team_names = teams.clone();
            st.names.insert(self_name.clone(), self_id.clone());
            st.known_users.insert(self_id.clone());
        }

        let mut channels = Vec::new();
        let mut users: HashMap<String, User> = HashMap::new();
        for sub in &subs {
            // Archived rooms and DMs hidden from the sidebar stay out, like in the web app.
            if sub["archived"].as_bool() == Some(true) || sub["open"].as_bool() == Some(false) {
                continue;
            }
            let room = sub["rid"].as_str().and_then(|r| by_id.get(r).copied());
            let Some(ch) = convert::channel(&self.account_id, sub, room, (self_id.as_str(), self_name.as_str()), &teams) else { continue };
            let people = match room {
                Some(r) if ch.kind.is_direct() => convert::dm_users(&self.account_id, sub, r, &self_id),
                _ => vec![],
            };
            self.remember(&ch, sub["t"].as_str().unwrap_or("c"), &people);
            users.extend(people.into_iter().map(|u| (u.id.clone(), u)));
            channels.push(ch);
        }
        Ok(Bootstrap { self_user, users: users.into_values().collect(), channels })
    }

    async fn run(&self, sink: EventSink) -> SResult<()> {
        let url = format!("{}/websocket", self.server.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1));
        let (ws, _) = tokio_tungstenite::connect_async(url).await.map_err(net)?;
        let (mut tx, mut rx) = ws.split();
        let frame = |v: Value| WsMessage::Text(v.to_string());
        tx.send(frame(json!({ "msg": "connect", "version": "1", "support": ["1"] }))).await.map_err(net)?;
        let login = json!({ "msg": "method", "method": "login", "id": "login", "params": [{ "resume": self.creds.token }] });
        tx.send(frame(login)).await.map_err(net)?;

        let mut next_sub = 0u64;
        let mut sub = |name: &str, param: String| {
            next_sub += 1;
            frame(json!({ "msg": "sub", "id": format!("s{next_sub}"), "name": name, "params": [param, false] }))
        };
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.reset();
        loop {
            tokio::select! {
                frame_in = tokio::time::timeout(SILENCE, rx.next()) => {
                    let text = match frame_in {
                        Err(_) => return Err(ServiceError::Network("no data from the server for 75 s".into())),
                        Ok(None) | Ok(Some(Ok(WsMessage::Close(_)))) => return Err(ServiceError::Network("socket closed".into())),
                        Ok(Some(Err(e))) => return Err(net(e)),
                        Ok(Some(Ok(WsMessage::Text(t)))) => t,
                        Ok(Some(Ok(_))) => continue,
                    };
                    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    match v["msg"].as_str().unwrap_or_default() {
                        "ping" => tx.send(frame(json!({ "msg": "pong" }))).await.map_err(net)?,
                        "result" if v["id"] == "login" => {
                            if v.get("error").is_some_and(|e| !e.is_null()) {
                                return Err(expired());
                            }
                            if !sink.send(ServiceEvent::Connection(ConnState::Online)).await {
                                return Ok(());
                            }
                            let (me, rooms) = {
                                let st = self.st();
                                (st.self_id.clone(), st.room_types.keys().take(MAX_ROOM_STREAMS).cloned().collect::<Vec<_>>())
                            };
                            let mut frames = vec![
                                sub("stream-room-messages", "__my_messages__".into()),
                                sub("stream-notify-user", format!("{me}/subscriptions-changed")),
                                sub("stream-notify-user", format!("{me}/rooms-changed")),
                                sub("stream-notify-logged", "user-status".into()),
                            ];
                            frames.extend(rooms.into_iter().map(|rid| sub("stream-notify-room", format!("{rid}/deleteMessage"))));
                            for f in frames {
                                tx.send(f).await.map_err(net)?;
                            }
                            if !self.send_presence(&sink).await {
                                return Ok(());
                            }
                        }
                        "changed" => match self.on_changed(&sink, &v).await {
                            None => return Ok(()),
                            Some(new_rooms) => {
                                for rid in new_rooms {
                                    tx.send(sub("stream-notify-room", format!("{rid}/deleteMessage"))).await.map_err(net)?;
                                }
                            }
                        },
                        "nosub" => tracing::debug!("subscription refused: {}", v["error"]),
                        "failed" => return Err(ServiceError::Network("the server refused the realtime protocol".into())),
                        _ => {}
                    }
                }
                _ = ping.tick() => tx.send(frame(json!({ "msg": "ping" }))).await.map_err(net)?,
            }
        }
    }

    async fn history(&self, channel_id: &str, q: HistoryQuery) -> SResult<Vec<Message>> {
        let ep = self.history_endpoint(channel_id);
        let mut raw: Vec<Value> = Vec::new();
        if let Some(after) = q.after {
            // History pages run newest first: walk back from now until `after` (at most 10 pages).
            let mut latest: Option<Millis> = None;
            for _ in 0..10 {
                let mut path = format!("{ep}?roomId={channel_id}&count={PAGE}&oldest={}", iso(after));
                if let Some(l) = latest {
                    path.push_str(&format!("&latest={}", iso(l)));
                }
                let batch = self.get(&path).await?["messages"].as_array().cloned().unwrap_or_default();
                let full = batch.len() >= PAGE;
                latest = batch.last().and_then(|m| convert::ts(&m["ts"]));
                raw.extend(batch);
                if !full || latest.is_none() {
                    break;
                }
            }
        } else {
            let mut path = format!("{ep}?roomId={channel_id}&count={}", (q.limit as usize).min(PAGE));
            if let Some(before) = q.before {
                path.push_str(&format!("&latest={}", iso(before)));
            }
            raw = self.get(&path).await?["messages"].as_array().cloned().unwrap_or_default();
        }
        let mut msgs = self.convert(&raw);
        if let Some(after) = q.after {
            msgs.retain(|m| m.ts > after);
        }
        msgs.sort_by_key(|m| m.ts);
        msgs.dedup_by(|a, b| a.id == b.id);
        Ok(msgs)
    }

    async fn thread(&self, _channel_id: &str, root_id: &str) -> SResult<Vec<Message>> {
        let mut raw = self.get(&format!("chat.getThreadMessages?tmid={root_id}&count=200")).await?["messages"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Ok(v) = self.get(&format!("chat.getMessage?msgId={root_id}")).await {
            raw.push(v["message"].clone());
        }
        let mut msgs = self.convert(&raw);
        msgs.sort_by_key(|m| m.ts);
        Ok(msgs)
    }

    async fn send(&self, channel_id: &str, text: &str, thread_root: Option<&str>) -> SResult<Message> {
        let mut message = json!({ "rid": channel_id, "msg": convert::to_rc(text) });
        if let Some(root) = thread_root {
            message["tmid"] = json!(root);
        }
        let v = self.post("chat.sendMessage", json!({ "message": message })).await?;
        self.convert(std::slice::from_ref(&v["message"]))
            .pop()
            .ok_or_else(|| ServiceError::Other("the server returned no message".into()))
    }

    async fn edit(&self, channel_id: &str, message_id: &str, text: &str) -> SResult<()> {
        let body = json!({ "roomId": channel_id, "msgId": message_id, "text": convert::to_rc(text) });
        self.post("chat.update", body).await.map(drop)
    }

    async fn delete(&self, channel_id: &str, message_id: &str) -> SResult<()> {
        self.post("chat.delete", json!({ "roomId": channel_id, "msgId": message_id })).await.map(drop)
    }

    async fn react(&self, _channel_id: &str, message_id: &str, emoji: &str, add: bool) -> SResult<()> {
        let body = json!({ "messageId": message_id, "emoji": format!(":{emoji}:"), "shouldReact": add });
        self.post("chat.react", body).await.map(drop)
    }

    async fn mark_read(&self, channel_id: &str, _latest: Option<&Message>) -> SResult<()> {
        // Threads too: their replies are on screen with the conversation (as thread notes).
        self.post("subscriptions.read", json!({ "rid": channel_id, "readThreads": true })).await.map(drop)
    }

    /// `rooms.media` + `rooms.mediaConfirm` (6.8+), falling back to the older `rooms.upload`.
    async fn upload(&self, channel_id: &str, file_name: &str, bytes: Vec<u8>, thread_root: Option<&str>) -> SResult<()> {
        let form = |bytes: Vec<u8>| {
            let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name.to_string());
            reqwest::multipart::Form::new().part("file", part)
        };
        let media = self.call(self.req(Method::POST, &format!("rooms.media/{channel_id}")).multipart(form(bytes.clone()))).await;
        match media {
            Ok(v) => {
                let file_id = s(&v["file"]["_id"]);
                let mut body = json!({ "msg": "" });
                if let Some(root) = thread_root {
                    body["tmid"] = json!(root);
                }
                self.post(&format!("rooms.mediaConfirm/{channel_id}/{file_id}"), body).await.map(drop)
            }
            Err(ServiceError::Other(_)) => {
                let mut f = form(bytes);
                if let Some(root) = thread_root {
                    f = f.text("tmid", root.to_string());
                }
                self.call(self.req(Method::POST, &format!("rooms.upload/{channel_id}")).multipart(f)).await.map(drop)
            }
            Err(e) => Err(e),
        }
    }

    async fn set_starred(&self, channel_id: &str, starred: bool) -> SResult<()> {
        self.post("rooms.favorite", json!({ "roomId": channel_id, "favorite": starred })).await?;
        let mut st = self.st();
        if starred {
            st.favorites.insert(channel_id.to_string());
        } else {
            st.favorites.remove(channel_id);
        }
        Ok(())
    }

    /// Server-wide search goes through the `messageSearch` method; it returns results only
    /// when the admin enabled global search, otherwise nothing.
    async fn search(&self, query: &str, limit: u32) -> SResult<Vec<Message>> {
        let call = json!({ "msg": "method", "method": "messageSearch", "params": [query, null, limit.min(100)], "id": "1" });
        let v = self.post("method.call/messageSearch", json!({ "message": call.to_string() })).await?;
        let inner: Value = serde_json::from_str(v["message"].as_str().unwrap_or("{}")).unwrap_or_default();
        if inner.get("error").is_some_and(|e| !e.is_null()) {
            return Err(ServiceError::Unsupported);
        }
        let docs = inner["result"]["message"]["docs"].as_array().cloned().unwrap_or_default();
        let mut msgs = self.convert(&docs);
        msgs.sort_by(|a, b| b.ts.cmp(&a.ts));
        Ok(msgs)
    }

    async fn set_presence(&self, p: Presence) -> SResult<()> {
        let status = match p {
            Presence::Online => "online",
            Presence::Away => "away",
            Presence::Dnd => "busy",
            Presence::Offline => "offline",
            Presence::Unknown => return Err(ServiceError::Unsupported),
        };
        self.post("users.setStatus", json!({ "status": status, "message": "" })).await.map(drop)
    }

    /// The server's own icon (the Rocket.Chat logo unless the admin replaced it).
    async fn workspace_icon(&self) -> SResult<Option<Media>> {
        for path in ["/assets/favicon_192.png", "/assets/favicon_512.png", "/assets/favicon.ico", "/favicon.ico"] {
            if let Ok(m) = self.fetch_url(path).await {
                if m.mime.starts_with("image/") {
                    return Ok(Some(m));
                }
            }
        }
        Ok(None)
    }

    async fn users(&self, ids: &[String]) -> SResult<Vec<User>> {
        let mut out = Vec::new();
        let mut missing = Vec::new();
        {
            let mut st = self.st();
            for id in ids {
                match st.pending_users.remove(id) {
                    Some(u) => {
                        st.known_users.insert(u.id.clone());
                        out.push(u);
                    }
                    None => missing.push(id.clone()),
                }
            }
        }
        for id in missing.into_iter().take(30) {
            if let Ok(v) = self.get(&format!("users.info?userId={id}")).await {
                let u = &v["user"];
                let username = s(&u["username"]);
                out.push(convert::user(&self.account_id, &id, &username, u["name"].as_str().unwrap_or(&username)));
            }
        }
        let mut st = self.st();
        for u in &out {
            st.names.insert(u.name.clone(), u.id.clone());
        }
        Ok(out)
    }

    async fn profile(&self, user_id: &str) -> SResult<Profile> {
        let v = self.get(&format!("users.info?userId={user_id}")).await?;
        let u = &v["user"];
        let username = s(&u["username"]);
        let base = convert::user(&self.account_id, user_id, &username, u["name"].as_str().unwrap_or(&username));
        let mut extra = Vec::new();
        for (label, key) in [("About", "bio"), ("Nickname", "nickname")] {
            if !s(&u[key]).is_empty() {
                extra.push((label.to_string(), s(&u[key])));
            }
        }
        // Admin-defined profile fields (department, office...).
        for (k, val) in u["customFields"].as_object().into_iter().flatten() {
            if let Some(t) = val.as_str().filter(|t| !t.is_empty()) {
                extra.push((k.clone(), t.to_string()));
            }
        }
        let roles = u["roles"].as_array().into_iter().flatten().filter_map(Value::as_str);
        Ok(Profile {
            email: s(&u["emails"][0]["address"]),
            // Hours, possibly fractional (5.5 for India).
            utc_offset_min: u["utcOffset"].as_f64().map(|h| (h * 60.0).round() as i32),
            status_text: s(&u["statusText"]),
            presence: presence(&u["status"]),
            is_bot: u["type"].as_str() == Some("bot"),
            roles: roles
                .filter_map(|r| match r {
                    "admin" => Some("Admin"),
                    "guest" => Some("Guest"),
                    _ => None,
                })
                .map(str::to_string)
                .collect(),
            inactive: u["active"].as_bool() == Some(false),
            extra,
            ..Profile::from_user(&base)
        })
    }

    async fn members(&self, channel_id: &str, cursor: Option<&str>) -> SResult<MemberPage> {
        const COUNT: u64 = 100;
        let offset: u64 = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
        let method = match self.st().room_types.get(channel_id).map(String::as_str) {
            Some("p") => "groups.members",
            Some("d") => "im.members",
            _ => "channels.members",
        };
        let v = self.get(&format!("{method}?roomId={channel_id}&offset={offset}&count={COUNT}")).await?;
        let list = v["members"].as_array().cloned().unwrap_or_default();
        let users: Vec<User> = list
            .iter()
            .map(|m| {
                let username = s(&m["username"]);
                convert::user(&self.account_id, &s(&m["_id"]), &username, m["name"].as_str().unwrap_or(&username))
            })
            .collect();
        let presence = list.iter().map(|m| (s(&m["_id"]), presence(&m["status"]))).collect();
        {
            let mut st = self.st();
            for u in &users {
                st.known_users.insert(u.id.clone());
                st.names.insert(u.name.clone(), u.id.clone());
            }
        }
        let total = v["total"].as_u64();
        let seen = offset + list.len() as u64;
        let next = (list.len() as u64 == COUNT && total.map_or(true, |t| seen < t)).then(|| seen.to_string());
        Ok(MemberPage { users, presence, next, total })
    }

    async fn fetch(&self, path: &str) -> SResult<Media> {
        // Only what the UI shows: uploaded files, thumbnails and avatars.
        let allowed = path.starts_with("/file-upload/") || path.starts_with("/avatar/");
        if !allowed || path.contains("..") {
            return Err(ServiceError::Other("path not allowed".into()));
        }
        self.fetch_url(path).await
    }

    async fn download(&self, path: &str, dest: &mut Download) -> SResult<()> {
        if !path.starts_with("/file-upload/") || path.contains("..") {
            return Err(ServiceError::Other("path not allowed".into()));
        }
        let cookie = format!("{COOKIE_UID}={}; {COOKIE_TOKEN}={}", self.creds.user_id, self.creds.token);
        let rb = self.authed(self.http.get(format!("{}{path}", self.server))).header("Cookie", cookie);
        let mut resp = check(rb.send().await.map_err(net)?).await?;
        dest.mime = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        while let Some(chunk) = resp.chunk().await.map_err(net)? {
            dest.write(&chunk)?;
        }
        Ok(())
    }

    fn to_remote_markdown(&self, md: &str) -> String {
        convert::to_rc(md)
    }
}

impl RocketChatService {
    /// GET on a server path outside the API. File access checks the `rc_uid` / `rc_token`
    /// cookies on most versions and the auth headers on newer ones, so both are sent.
    async fn fetch_url(&self, path: &str) -> SResult<Media> {
        let cookie = format!("{COOKIE_UID}={}; {COOKIE_TOKEN}={}", self.creds.user_id, self.creds.token);
        let rb = self.authed(self.http.get(format!("{}{path}", self.server))).header("Cookie", cookie);
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
}

/// Finds the Rocket.Chat base URL in whatever the user pasted (a room link, the
/// login page, a bare host) by asking `/api/info` on successively shorter paths.
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
        if let Ok(r) = http.get(format!("{candidate}/api/info")).send().await {
            if r.status().is_success() && r.json::<Value>().await.is_ok_and(|v| v["success"] == true) {
                let site: Value = match http.get(format!("{candidate}/api/v1/settings.public?_id=Site_Name")).send().await {
                    Ok(r) => r.json().await.unwrap_or_default(),
                    Err(_) => Value::Null,
                };
                let site_name = site["settings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|x| x["_id"] == "Site_Name")
                    .and_then(|x| x["value"].as_str())
                    .filter(|n| !n.is_empty())
                    .unwrap_or("Rocket.Chat")
                    .to_string();
                return Ok(ServerInfo { url: candidate, site_name });
            }
        }
        match candidate.rfind('/') {
            Some(i) if i >= scheme_end => candidate.truncate(i),
            _ => return Err(format!("No Rocket.Chat server found at {raw}")),
        }
    }
}

/// Checks a user id + token pair and returns who it belongs to.
pub async fn whoami(server: &str, creds: &Credentials) -> Result<WhoAmI, String> {
    let r = client()
        .get(format!("{server}/api/v1/me"))
        .header("X-User-Id", &creds.user_id)
        .header("X-Auth-Token", &creds.token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let me: Value = check(r).await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    Ok(WhoAmI { user_id: s(&me["_id"]), username: s(&me["username"]) })
}
