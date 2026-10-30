//! Slack connector: the Web API plus the RTM websocket (`rtm.connect`), with the
//! web client's own session: an `xoxc-` token and the `d` cookie, both taken from
//! the sign-in window. No Slack app has to be installed in the workspace.
//!
//! One account per workspace. Favorites and custom sections come from the
//! sidebar sections (`users.channelSections.*`), as in the Slack client.

mod convert;

use agora_core::engine::now_ms;
use agora_core::model::*;
use agora_core::service::*;
use async_trait::async_trait;
use convert::{s, slack_ts, ts_ms};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Response, StatusCode};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message as WsMessage};

/// The web session cookie (HttpOnly, read by the sign-in window).
pub const COOKIE_D: &str = "d";
/// Written by [`SIGNIN_SCRIPT`]: one line per signed-in workspace, tab-separated
/// `team id, token, url, name`.
pub const TEAMS_COOKIE: &str = "agora_slack_teams";
/// Written by [`SIGNIN_SCRIPT`]: workspace URLs seen in the window (one per line), e.g.
/// from the list Slack shows after an email sign-in. Set on `.slack.com`.
pub const WORKSPACES_COOKIE: &str = "agora_slack_ws";
/// Runs in the sign-in window only:
/// - links and pop-ups that would open a new window (or the desktop app) open in this window,
///   which otherwise ignores them;
/// - workspace URLs on the page (links, embedded data) are collected into [`WORKSPACES_COOKIE`];
/// - once the Slack client runs, its workspaces and `xoxc-` tokens are copied from
///   localStorage into [`TEAMS_COOKIE`].
pub const SIGNIN_SCRIPT: &str = r#"(function () {
  var generic = ['app', 'www', 'api', 'edgeapi', 'status', 'files', 'slack', 'a', 'slackhq', 'downloads'];
  function cookie(name) {
    var m = document.cookie.match(new RegExp('(?:^|; )' + name + '=([^;]*)'));
    return m ? decodeURIComponent(m[1]) : '';
  }
  window.open = function (u) { if (u) location.href = u; return null; };
  document.addEventListener('click', function (e) {
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (!a) return;
    var href = a.getAttribute('href') || '';
    if (/^slack:/i.test(href)) { e.preventDefault(); return; }
    if (a.target && a.target !== '_self' && /^https?:/i.test(a.href)) { e.preventDefault(); location.href = a.href; }
  }, true);
  setInterval(function () {
    try {
      var found = {};
      cookie('agora_slack_ws').split('\n').forEach(function (w) { if (w) found[w] = 1; });
      var html = (document.documentElement && document.documentElement.innerHTML || '') + ' ' + location.href;
      var re = /https?:(?:\\?\/){2}([a-z0-9-]+)\.slack\.com/gi, m;
      while ((m = re.exec(html))) {
        if (generic.indexOf(m[1].toLowerCase()) < 0) found['https://' + m[1].toLowerCase() + '.slack.com'] = 1;
      }
      var list = Object.keys(found).slice(0, 20);
      if (list.length) document.cookie = 'agora_slack_ws=' + encodeURIComponent(list.join('\n')) + '; domain=.slack.com; path=/; SameSite=Lax';
      var c = JSON.parse(localStorage.getItem('localConfig_v2') || 'null');
      if (!c || !c.teams) return;
      var rows = Object.keys(c.teams).map(function (k) {
        var t = c.teams[k];
        return [t.id, t.token, t.url, t.name].join('\t');
      }).filter(function (r) { return r.indexOf('xoxc-') >= 0; });
      if (rows.length) document.cookie = 'agora_slack_teams=' + encodeURIComponent(rows.join('\n')) + '; path=/; SameSite=Strict';
    } catch (e) {}
  }, 700);
})();"#;
/// Where the sign-in window ends up, and where both cookies are visible.
pub const CLIENT_URL: &str = "https://app.slack.com/";

const PING_EVERY: Duration = Duration::from_secs(30);
const SILENCE: Duration = Duration::from_secs(75);
const MAX_MEDIA: u64 = 25 * 1024 * 1024;
/// User id -> name cache for `<@U…>` mentions; cleared beyond this (about 1 MB).
const MAX_NAMES: usize = 20_000;
const PAGE: usize = 100;
const AUTH_ERRORS: &[&str] = &["invalid_auth", "not_authed", "token_revoked", "account_inactive", "token_expired"];

/// What is kept in Windows Credential Manager for a Slack workspace.
pub struct Credentials {
    pub token: String,
    /// The `d` cookie, URL-encoded as Slack expects it in the header.
    pub cookie: String,
}

impl Credentials {
    pub fn new(token: &str, cookie: &str) -> Self {
        Self { token: token.trim().to_string(), cookie: convert::pct_encode_cookie(cookie.trim()) }
    }

    pub fn to_json(&self) -> String {
        json!({ "token": self.token, "cookie": self.cookie }).to_string()
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(raw).ok()?;
        let c = Self { token: s(&v["token"]), cookie: s(&v["cookie"]) };
        (!c.token.is_empty() && !c.cookie.is_empty()).then_some(c)
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

fn retry_after(resp: &Response) -> Duration {
    let secs = resp.headers().get("Retry-After").and_then(|v| v.to_str().ok()?.parse::<u64>().ok()).unwrap_or(30);
    Duration::from_secs(secs.clamp(1, 300))
}

/// Slack answers HTTP 200 with `ok: false` for most errors.
async fn check(resp: Response) -> SResult<Value> {
    let status = resp.status();
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(ServiceError::RateLimited(retry_after(&resp)));
    }
    if status.is_server_error() {
        return Err(ServiceError::Network(format!("Slack server error {status}")));
    }
    let v: Value = resp.json().await.map_err(net)?;
    if v["ok"].as_bool() == Some(true) {
        return Ok(v);
    }
    let error = v["error"].as_str().unwrap_or("unknown_error");
    Err(match error {
        e if AUTH_ERRORS.contains(&e) => ServiceError::Auth(format!("Slack session ended ({e})")),
        "ratelimited" => ServiceError::RateLimited(Duration::from_secs(30)),
        e => ServiceError::Other(format!("Slack: {e}")),
    })
}

fn presence(v: &str) -> Presence {
    match v {
        "active" => Presence::Online,
        "away" => Presence::Away,
        _ => Presence::Unknown,
    }
}

#[derive(Default)]
struct State {
    self_id: String,
    self_name: String,
    team_id: String,
    /// User id -> display name, for mentions and system messages.
    names: HashMap<String, String>,
    known_users: HashSet<String>,
    /// Profiles fetched while converting, handed to the engine by `users()`.
    pending_users: HashMap<String, User>,
    known_channels: HashSet<String>,
    /// Conversation id -> name, for `<#C123>` references without a label.
    channel_names: HashMap<String, String>,
    peers: Vec<String>,
    /// The "Starred" sidebar section, when the workspace has sections.
    stars_section: Option<String>,
    /// No sidebar sections: favorites come from (and go to) the older Stars API.
    legacy_stars: bool,
    favorites: HashSet<String>,
    categories: HashMap<String, String>,
}

pub struct SlackService {
    account_id: String,
    /// Workspace URL, e.g. https://acme.slack.com
    base: String,
    creds: Credentials,
    http: reqwest::Client,
    state: Mutex<State>,
}

impl SlackService {
    pub fn new(account: &Account, creds: Credentials) -> Self {
        let base = account.server_url.trim_end_matches('/').to_string();
        let state = State { team_id: account.team_id.clone().unwrap_or_default(), ..State::default() };
        Self { account_id: account.id.clone(), base, creds, http: client(), state: Mutex::new(state) }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cookie(&self) -> String {
        format!("d={}", self.creds.cookie)
    }

    async fn api(&self, method: &str, params: &[(&str, String)]) -> SResult<Value> {
        let mut form: Vec<(&str, &str)> = vec![("token", self.creds.token.as_str())];
        form.extend(params.iter().map(|(k, v)| (*k, v.as_str())));
        let resp = self
            .http
            .post(format!("{}/api/{method}", self.base))
            .header("Cookie", self.cookie())
            .form(&form)
            .send()
            .await
            .map_err(net)?;
        check(resp).await
    }

    /// A user's shown name, or a conversation's name (ids never collide: U/W vs C/G/D).
    fn name_of(&self, id: &str) -> Option<String> {
        let st = self.st();
        st.names.get(id).or_else(|| st.channel_names.get(id)).cloned()
    }

    fn learn(&self, users: &[User]) {
        let mut st = self.st();
        if st.names.len() > MAX_NAMES {
            st.names.clear();
        }
        for u in users {
            let shown = if u.display_name.is_empty() { u.name.clone() } else { u.display_name.clone() };
            st.names.insert(u.id.clone(), shown);
        }
    }

    /// Profiles of these users; the web client's batch endpoint first, `users.info` as fallback.
    async fn fetch_users(&self, ids: &[String]) -> Vec<User> {
        if ids.is_empty() {
            return vec![];
        }
        let team = self.st().team_id.clone();
        let updated: serde_json::Map<String, Value> = ids.iter().map(|id| (id.clone(), json!(0))).collect();
        let body = json!({ "token": self.creds.token, "check_interaction": true, "include_profile_only_users": true, "updated_ids": updated });
        let batch = self
            .http
            .post(format!("https://edgeapi.slack.com/cache/{team}/users/info"))
            .header("Cookie", self.cookie())
            .json(&body)
            .send()
            .await;
        let mut users: Vec<User> = match batch {
            Ok(r) => match check(r).await {
                Ok(v) => v["results"].as_array().into_iter().flatten().map(|u| convert::user(&self.account_id, u)).collect(),
                Err(_) => vec![],
            },
            Err(_) => vec![],
        };
        if users.is_empty() {
            for id in ids.iter().take(20) {
                if let Ok(v) = self.api("users.info", &[("user", id.clone())]).await {
                    users.push(convert::user(&self.account_id, &v["user"]));
                }
            }
        }
        self.learn(&users);
        users
    }

    /// Converts messages after resolving the names they mention; new profiles wait in
    /// `pending_users` for the engine.
    async fn convert(&self, channel_id: &str, msgs: &[Value]) -> Vec<Message> {
        let mut ids = Vec::new();
        for m in msgs {
            convert::referenced_users(m, &mut ids);
        }
        ids.sort();
        ids.dedup();
        let missing: Vec<String> = {
            let st = self.st();
            ids.into_iter().filter(|id| !st.names.contains_key(id)).collect()
        };
        let fetched = self.fetch_users(&missing).await;
        let bots: Vec<User> = msgs.iter().filter_map(|m| convert::bot_user(&self.account_id, m)).collect();
        self.learn(&bots);
        {
            let mut st = self.st();
            if st.pending_users.len() > 500 {
                st.pending_users.clear();
            }
            for u in fetched.into_iter().chain(bots) {
                if !st.known_users.contains(&u.id) {
                    st.pending_users.insert(u.id.clone(), u);
                }
            }
        }
        let name_of = |id: &str| self.name_of(id);
        msgs.iter().filter_map(|m| convert::message(&self.account_id, channel_id, m, &name_of)).collect()
    }

    /// Favorites and custom sections. `None` when the workspace has no sidebar sections API.
    /// Favorites ("Starred") and custom sections. The sidebar sections API first; workspaces
    /// without it fall back to the older Stars list. `None` when neither answers.
    async fn load_sections(&self) -> Option<()> {
        let mut stars = None;
        let mut favorites = HashSet::new();
        let mut categories = HashMap::new();
        match self.api("users.channelSections.list", &[]).await {
            Ok(v) => {
                for sec in v["channel_sections"].as_array().into_iter().flatten() {
                    let ids = sec["channel_ids_page"]["channel_ids"].as_array().cloned().unwrap_or_default();
                    let ids = ids.iter().filter_map(Value::as_str).map(str::to_string);
                    let name = s(&sec["name"]);
                    match sec["type"].as_str().unwrap_or_default() {
                        // The type has been "stars"; accept the visible name too in case it changes.
                        "stars" | "starred" | "favorites" => {
                            stars = Some(s(&sec["channel_section_id"]));
                            favorites.extend(ids);
                        }
                        _ if name.eq_ignore_ascii_case("starred") && stars.is_none() => {
                            stars = Some(s(&sec["channel_section_id"]));
                            favorites.extend(ids);
                        }
                        "standard" => categories.extend(ids.map(|id| (id, name.clone()))),
                        _ => {}
                    }
                }
                if stars.is_none() {
                    tracing::warn!("Slack sidebar sections have no Starred section; trying the Stars list");
                }
            }
            Err(e) => tracing::warn!("Slack sidebar sections unavailable ({e}); trying the Stars list"),
        }
        let mut legacy = false;
        if stars.is_none() {
            match self.api("stars.list", &[("limit", "1000".into())]).await {
                Ok(v) => {
                    legacy = true;
                    favorites.extend(
                        v["items"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|i| matches!(i["type"].as_str(), Some("channel" | "im" | "group" | "mpim")))
                            .filter_map(|i| i["channel"].as_str().map(str::to_string)),
                    );
                }
                Err(e) => {
                    tracing::warn!("Slack stars unavailable too ({e}); favorites stay local");
                    if categories.is_empty() {
                        return None;
                    }
                }
            }
        }
        let mut st = self.st();
        st.stars_section = stars;
        st.legacy_stars = legacy;
        st.favorites = favorites;
        st.categories = categories;
        Some(())
    }

    /// Whether the workspace tells us its favorites (so they are synced both ways).
    fn stars_known(st: &State) -> bool {
        st.stars_section.is_some() || st.legacy_stars
    }

    /// Slack's Starred conversations become favorites; the others keep the star they have in the
    /// archive, because Slack may refuse Agora's own stars (see `set_starred`), which then stay local.
    fn apply_sections(&self, ch: &mut Channel) {
        let st = self.st();
        if Self::stars_known(&st) && st.favorites.contains(&ch.id) {
            ch.starred = true;
            ch.starred_synced = true;
        }
        ch.category = st.categories.get(&ch.id).cloned().unwrap_or_default();
    }

    fn remember(&self, ch: &Channel) {
        let mut st = self.st();
        st.known_channels.insert(ch.id.clone());
        if matches!(ch.kind, ChannelKind::Public | ChannelKind::Private) {
            st.channel_names.insert(ch.id.clone(), ch.name.clone());
        }
        if ch.kind == ChannelKind::Dm {
            for m in &ch.members {
                if !st.peers.contains(m) {
                    st.peers.push(m.clone());
                }
            }
        }
    }

    async fn fetch_channel(&self, id: &str) -> SResult<Channel> {
        let v = self.api("conversations.info", &[("channel", id.to_string())]).await?;
        let (me, my_name) = {
            let st = self.st();
            (st.self_id.clone(), st.self_name.clone())
        };
        let mut ch = convert::channel(&self.account_id, &v["channel"], None, &me, &my_name)
            .ok_or_else(|| ServiceError::Other("archived conversation".into()))?;
        self.apply_sections(&mut ch);
        self.remember(&ch);
        Ok(ch)
    }

    /// Sends a conversation we did not know, with its people. `false` once the engine is gone.
    async fn introduce(&self, sink: &EventSink, id: &str) -> bool {
        if id.is_empty() || self.st().known_channels.contains(id) {
            return true;
        }
        let Ok(ch) = self.fetch_channel(id).await else { return true };
        let missing: Vec<String> = {
            let st = self.st();
            ch.members.iter().filter(|u| !st.known_users.contains(*u)).cloned().collect()
        };
        for u in self.fetch_users(&missing).await {
            self.st().known_users.insert(u.id.clone());
            if !sink.send(ServiceEvent::UserUpsert(u)).await {
                return false;
            }
        }
        sink.send(ServiceEvent::ChannelUpsert(ch)).await
    }

    /// Sends profiles that conversion fetched, before the messages that need them.
    async fn flush_users(&self, sink: &EventSink) -> bool {
        let users: Vec<User> = {
            let mut st = self.st();
            let users: Vec<User> = st.pending_users.drain().map(|(_, u)| u).collect();
            st.known_users.extend(users.iter().map(|u| u.id.clone()));
            users
        };
        for u in users {
            if !sink.send(ServiceEvent::UserUpsert(u)).await {
                return false;
            }
        }
        true
    }

    async fn sections_event(&self) -> Option<ServiceEvent> {
        self.load_sections().await?;
        let st = self.st();
        let sections = st
            .known_channels
            .iter()
            .map(|id| ChannelSection {
                channel_id: id.clone(),
                // Only adds: a star Slack refused is kept in Agora (see `apply_sections`).
                starred: (Self::stars_known(&st) && st.favorites.contains(id)).then_some(true),
                category: Some(st.categories.get(id).cloned().unwrap_or_default()),
            })
            .collect();
        Some(ServiceEvent::Sections(sections))
    }

    /// Handles one RTM event. `false` once the engine is gone.
    async fn on_event(&self, sink: &EventSink, ev: &Value) -> bool {
        let channel = s(&ev["channel"]);
        let me = self.st().self_id.clone();
        let mut out: Vec<ServiceEvent> = Vec::new();
        match ev["type"].as_str().unwrap_or_default() {
            "message" => {
                if !self.introduce(sink, &channel).await {
                    return false;
                }
                match ev["subtype"].as_str().unwrap_or_default() {
                    "message_deleted" => out.push(ServiceEvent::MessageDeleted { channel_id: channel, id: s(&ev["deleted_ts"]) }),
                    "message_changed" | "message_replied" => {
                        for m in self.convert(&channel, std::slice::from_ref(&ev["message"])).await {
                            out.push(if m.deleted {
                                ServiceEvent::MessageDeleted { channel_id: m.channel_id, id: m.id }
                            } else {
                                ServiceEvent::MessageUpsert(m)
                            });
                        }
                    }
                    subtype => {
                        out.extend(self.convert(&channel, std::slice::from_ref(ev)).await.into_iter().map(ServiceEvent::MessageUpsert));
                        if matches!(subtype, "channel_topic" | "group_topic" | "channel_name" | "group_name") {
                            if let Ok(ch) = self.fetch_channel(&channel).await {
                                out.push(ServiceEvent::ChannelUpsert(ch));
                            }
                        }
                    }
                }
                if !self.flush_users(sink).await {
                    return false;
                }
            }
            e @ ("reaction_added" | "reaction_removed") => {
                let item = &ev["item"];
                if item["type"].as_str() == Some("message") {
                    out.push(ServiceEvent::Reaction {
                        channel_id: s(&item["channel"]),
                        message_id: s(&item["ts"]),
                        emoji: s(&ev["reaction"]),
                        user_id: s(&ev["user"]),
                        added: e == "reaction_added",
                    });
                }
            }
            "channel_marked" | "im_marked" | "group_marked" | "mpim_marked" => {
                out.push(ServiceEvent::ChannelRead { channel_id: channel, ts: ts_ms(ev["ts"].as_str().unwrap_or("0")) });
            }
            "user_typing" => {
                let user = s(&ev["user"]);
                if user != me {
                    let root = ev["thread_ts"].as_str().map(str::to_string);
                    out.push(ServiceEvent::Typing { channel_id: channel, user_id: user, thread_root: root });
                }
            }
            "presence_change" => {
                let p = presence(ev["presence"].as_str().unwrap_or_default());
                let users: Vec<String> = match ev["users"].as_array() {
                    Some(list) => list.iter().map(s).collect(),
                    None => vec![s(&ev["user"])],
                };
                out.extend(users.into_iter().map(|user_id| ServiceEvent::Presence { user_id, presence: p }));
            }
            "dnd_updated_user" if s(&ev["user"]) == me => {
                if ev["dnd_status"]["snooze_enabled"].as_bool() == Some(true) {
                    out.push(ServiceEvent::Presence { user_id: me, presence: Presence::Dnd });
                }
            }
            "channel_joined" | "group_joined" | "im_created" | "mpim_joined" | "im_open" | "mpim_open" | "group_open" => {
                let id = ev["channel"]["id"].as_str().map(str::to_string).unwrap_or(channel);
                return self.introduce(sink, &id).await;
            }
            "channel_rename" | "group_rename" => {
                if let Ok(ch) = self.fetch_channel(&s(&ev["channel"]["id"])).await {
                    out.push(ServiceEvent::ChannelUpsert(ch));
                }
            }
            "user_change" => {
                let u = convert::user(&self.account_id, &ev["user"]);
                self.learn(std::slice::from_ref(&u));
                out.push(ServiceEvent::UserUpsert(u));
            }
            e @ ("star_added" | "star_removed") => {
                if let Some(id) = ev["item"]["channel"].as_str().filter(|_| ev["item"]["type"].as_str() != Some("message")) {
                    let added = e == "star_added";
                    let mut st = self.st();
                    if added { st.favorites.insert(id.to_string()); } else { st.favorites.remove(id); }
                    drop(st);
                    out.push(ServiceEvent::Sections(vec![ChannelSection { channel_id: id.to_string(), starred: Some(added), category: None }]));
                }
            }
            t if t.starts_with("channel_section") => {
                out.extend(self.sections_event().await);
            }
            _ => {}
        }
        for e in out {
            if !sink.send(e).await {
                return false;
            }
        }
        true
    }

    async fn fetch_url(&self, url: &str, auth: bool) -> SResult<Media> {
        let mut rb = self.http.get(url);
        if auth {
            rb = rb.bearer_auth(&self.creds.token).header("Cookie", self.cookie());
        }
        let resp = rb.send().await.map_err(net)?;
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(ServiceError::RateLimited(retry_after(&resp)));
        }
        if !resp.status().is_success() {
            return Err(ServiceError::Other(format!("HTTP {}", resp.status())));
        }
        if resp.content_length().is_some_and(|n| n > MAX_MEDIA) {
            return Err(ServiceError::Other("file too large to preview".into()));
        }
        let mime = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
        // An expired session gets the sign-in page instead of the file.
        if auth && mime.starts_with("text/html") {
            return Err(ServiceError::Other("not an image".into()));
        }
        let bytes = resp.bytes().await.map_err(net)?.to_vec();
        Ok(Media { mime, bytes })
    }
}

#[async_trait]
impl Service for SlackService {
    async fn bootstrap(&self) -> SResult<Bootstrap> {
        let auth = self.api("auth.test", &[]).await?;
        let self_id = s(&auth["user_id"]);
        {
            let mut st = self.st();
            st.self_id = self_id.clone();
            st.team_id = s(&auth["team_id"]);
        }
        let me = self.api("users.info", &[("user", self_id.clone())]).await?;
        let self_user = convert::user(&self.account_id, &me["user"]);
        let self_name = s(&me["user"]["name"]);
        {
            let mut st = self.st();
            st.self_name = self_name.clone();
            st.known_users.insert(self_id.clone());
        }
        self.learn(std::slice::from_ref(&self_user));

        // Unread state, as the Slack client shows it. Optional: older workspaces may lack it.
        let counts = self.api("client.counts", &[("thread_counts_by_channel", "false".into())]).await.ok();
        let by_id: HashMap<String, Value> = counts
            .iter()
            .flat_map(|c| ["channels", "mpims", "ims"].into_iter().flat_map(move |k| c[k].as_array().cloned().unwrap_or_default()))
            .filter_map(|e| Some((e["id"].as_str()?.to_string(), e)))
            .collect();
        let sections_ok = self.load_sections().await.is_some();

        let mut convos: Vec<Value> = Vec::new();
        let mut cursor = String::new();
        for _ in 0..20 {
            let mut params = vec![
                ("types", "public_channel,private_channel,mpim,im".to_string()),
                ("exclude_archived", "true".to_string()),
                ("limit", "999".to_string()),
            ];
            if !cursor.is_empty() {
                params.push(("cursor", cursor.clone()));
            }
            let v = self.api("users.conversations", &params).await?;
            convos.extend(v["channels"].as_array().cloned().unwrap_or_default());
            cursor = s(&v["response_metadata"]["next_cursor"]);
            if cursor.is_empty() {
                break;
            }
        }

        let mut channels = Vec::new();
        for c in &convos {
            let id = c["id"].as_str().unwrap_or_default();
            // DMs closed in Slack stay out, like in its sidebar (only when counts tell which are open).
            if c["is_im"].as_bool() == Some(true) && counts.is_some() && !by_id.contains_key(id) {
                continue;
            }
            let Some(mut ch) = convert::channel(&self.account_id, c, by_id.get(id), &self_id, &self_name) else { continue };
            self.apply_sections(&mut ch);
            if !sections_ok {
                ch.starred_synced = false;
            }
            self.remember(&ch);
            channels.push(ch);
        }

        let peers: Vec<String> = self.st().peers.iter().filter(|p| **p != self_id).cloned().collect();
        let mut users = Vec::new();
        for chunk in peers.chunks(PAGE) {
            users.extend(self.fetch_users(chunk).await);
        }
        self.st().known_users.extend(users.iter().map(|u| u.id.clone()));
        Ok(Bootstrap { self_user, users, channels })
    }

    async fn run(&self, sink: EventSink) -> SResult<()> {
        let v = self.api("rtm.connect", &[("batch_presence_aware", "1".into())]).await?;
        let url = v["url"].as_str().ok_or_else(|| ServiceError::Other("rtm.connect returned no url".into()))?;
        let mut req = url.into_client_request().map_err(net)?;
        req.headers_mut().insert("Cookie", HeaderValue::from_str(&self.cookie()).map_err(net)?);
        let (ws, _) = tokio_tungstenite::connect_async(req).await.map_err(net)?;
        let (mut tx, mut rx) = ws.split();
        let mut seq = 1u64;
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.reset();
        loop {
            tokio::select! {
                frame = tokio::time::timeout(SILENCE, rx.next()) => {
                    let text = match frame {
                        Err(_) => return Err(ServiceError::Network("no data from Slack for 75 s".into())),
                        Ok(None) | Ok(Some(Ok(WsMessage::Close(_)))) => return Err(ServiceError::Network("socket closed".into())),
                        Ok(Some(Err(e))) => return Err(net(e)),
                        Ok(Some(Ok(WsMessage::Text(t)))) => t,
                        Ok(Some(Ok(_))) => continue,
                    };
                    let ev: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    match ev["type"].as_str().unwrap_or_default() {
                        "hello" => {
                            if !sink.send(ServiceEvent::Connection(ConnState::Online)).await {
                                return Ok(());
                            }
                            // Presence only for DM partners and ourselves; Slack then sends their current state.
                            let ids = {
                                let st = self.st();
                                let mut ids = st.peers.clone();
                                ids.push(st.self_id.clone());
                                ids
                            };
                            let sub = json!({ "type": "presence_sub", "ids": ids });
                            tx.send(WsMessage::Text(sub.to_string())).await.map_err(net)?;
                        }
                        "goodbye" => return Err(ServiceError::Network("Slack asked to reconnect".into())),
                        "pong" => {}
                        _ => {
                            if !self.on_event(&sink, &ev).await {
                                return Ok(());
                            }
                        }
                    }
                }
                _ = ping.tick() => {
                    seq += 1;
                    tx.send(WsMessage::Text(json!({ "id": seq, "type": "ping" }).to_string())).await.map_err(net)?;
                }
            }
        }
    }

    async fn history(&self, channel_id: &str, q: HistoryQuery) -> SResult<Vec<Message>> {
        let mut raw: Vec<Value> = Vec::new();
        if let Some(after) = q.after {
            // Pages run newest first; follow the cursor back to `after` (at most 10 pages).
            let mut cursor = String::new();
            for _ in 0..10 {
                let mut params = vec![("channel", channel_id.to_string()), ("limit", PAGE.to_string()), ("oldest", slack_ts(after))];
                if !cursor.is_empty() {
                    params.push(("cursor", cursor.clone()));
                }
                let v = self.api("conversations.history", &params).await?;
                raw.extend(v["messages"].as_array().cloned().unwrap_or_default());
                cursor = s(&v["response_metadata"]["next_cursor"]);
                if cursor.is_empty() || v["has_more"].as_bool() != Some(true) {
                    break;
                }
            }
        } else {
            let mut params = vec![("channel", channel_id.to_string()), ("limit", (q.limit as usize).min(PAGE).to_string())];
            match (&q.before_id, q.before) {
                (Some(id), _) => params.push(("latest", id.clone())),
                (None, Some(ms)) => params.push(("latest", slack_ts(ms))),
                _ => {}
            }
            raw = self.api("conversations.history", &params).await?["messages"].as_array().cloned().unwrap_or_default();
        }
        let mut msgs = self.convert(channel_id, &raw).await;
        if let Some(after) = q.after {
            msgs.retain(|m| m.ts > after);
        }
        msgs.sort_by_key(|m| m.ts);
        Ok(msgs)
    }

    async fn thread(&self, channel_id: &str, root_id: &str) -> SResult<Vec<Message>> {
        let params = [("channel", channel_id.to_string()), ("ts", root_id.to_string()), ("limit", "200".to_string())];
        let raw = self.api("conversations.replies", &params).await?["messages"].as_array().cloned().unwrap_or_default();
        let mut msgs = self.convert(channel_id, &raw).await;
        msgs.sort_by_key(|m| m.ts);
        Ok(msgs)
    }

    async fn send(&self, channel_id: &str, text: &str, thread_root: Option<&str>) -> SResult<Message> {
        let mut params = vec![("channel", channel_id.to_string()), ("text", convert::to_slack(text)), ("link_names", "true".into())];
        if let Some(root) = thread_root {
            params.push(("thread_ts", root.to_string()));
        }
        let v = self.api("chat.postMessage", &params).await?;
        self.convert(channel_id, std::slice::from_ref(&v["message"]))
            .await
            .pop()
            .ok_or_else(|| ServiceError::Other("Slack returned no message".into()))
    }

    async fn edit(&self, channel_id: &str, message_id: &str, text: &str) -> SResult<()> {
        let params = [
            ("channel", channel_id.to_string()),
            ("ts", message_id.to_string()),
            ("text", convert::to_slack(text)),
            ("link_names", "true".into()),
        ];
        self.api("chat.update", &params).await.map(drop)
    }

    async fn delete(&self, channel_id: &str, message_id: &str) -> SResult<()> {
        self.api("chat.delete", &[("channel", channel_id.to_string()), ("ts", message_id.to_string())]).await.map(drop)
    }

    async fn react(&self, channel_id: &str, message_id: &str, emoji: &str, add: bool) -> SResult<()> {
        let method = if add { "reactions.add" } else { "reactions.remove" };
        let params = [("channel", channel_id.to_string()), ("timestamp", message_id.to_string()), ("name", emoji.to_string())];
        self.api(method, &params).await.map(drop)
    }

    async fn mark_read(&self, channel_id: &str, latest: Option<&Message>) -> SResult<()> {
        let ts = latest.map(|m| m.id.clone()).unwrap_or_else(|| slack_ts(now_ms()));
        self.api("conversations.mark", &[("channel", channel_id.to_string()), ("ts", ts)]).await.map(drop)
    }

    /// Not in Slack's public API, but what its web client calls; a failure only leaves the
    /// thread unread in Slack's own Threads view.
    async fn mark_thread_read(&self, channel_id: &str, root: &str, latest: &Message) -> SResult<()> {
        let params = [("channel", channel_id.to_string()), ("thread_ts", root.to_string()), ("ts", latest.id.clone()), ("read", "1".into())];
        self.api("subscriptions.thread.mark", &params).await.map(drop)
    }

    /// The current three-step upload (files.upload is retired).
    async fn upload(&self, channel_id: &str, file_name: &str, bytes: Vec<u8>, thread_root: Option<&str>) -> SResult<()> {
        let params = [("filename", file_name.to_string()), ("length", bytes.len().to_string())];
        let v = self.api("files.getUploadURLExternal", &params).await?;
        let (url, file_id) = (s(&v["upload_url"]), s(&v["file_id"]));
        let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name.to_string());
        let resp = self.http.post(&url).multipart(reqwest::multipart::Form::new().part("file", part)).send().await.map_err(net)?;
        if !resp.status().is_success() {
            return Err(ServiceError::Other(format!("upload failed: HTTP {}", resp.status())));
        }
        let mut params = vec![("files", json!([{ "id": file_id, "title": file_name }]).to_string()), ("channel_id", channel_id.to_string())];
        if let Some(root) = thread_root {
            params.push(("thread_ts", root.to_string()));
        }
        self.api("files.completeUploadExternal", &params).await.map(drop)
    }

    async fn set_starred(&self, channel_id: &str, starred: bool) -> SResult<()> {
        let (section, legacy) = {
            let st = self.st();
            (st.stars_section.clone(), st.legacy_stars)
        };
        let result = match section {
            Some(section) => {
                let change = json!([{ "channel_section_id": section, "channel_ids": [channel_id] }]).to_string();
                let key = if starred { "insert" } else { "remove" };
                self.api("users.channelSections.channels.bulkUpdate", &[(key, change)]).await.map(drop)
            }
            None if legacy => {
                let method = if starred { "stars.add" } else { "stars.remove" };
                match self.api(method, &[("channel", channel_id.to_string())]).await {
                    // Already in the wanted state on Slack's side.
                    Err(ServiceError::Other(e)) if e.contains("already_starred") || e.contains("not_starred") => Ok(()),
                    r => r.map(drop),
                }
            }
            None => return Err(ServiceError::Unsupported),
        };
        // Slack lets only its own client change the Starred section, and many workspaces have the
        // older Stars API switched off: the star then stays in Agora only.
        if let Err(ServiceError::Other(e)) = &result {
            if e.contains("cannot_modify_special_sections") || e.contains("feature_not_enabled") || e.contains("not_allowed") {
                if !starred && self.st().favorites.contains(channel_id) {
                    return Err(ServiceError::Other(
                        "Slack does not let other apps change its Starred section: unstar this conversation in Slack".into(),
                    ));
                }
                return Err(ServiceError::Unsupported);
            }
        }
        result?;
        let mut st = self.st();
        if starred {
            st.favorites.insert(channel_id.to_string());
        } else {
            st.favorites.remove(channel_id);
        }
        Ok(())
    }

    async fn search(&self, query: &str, limit: u32) -> SResult<Vec<Message>> {
        let params = [("query", query.to_string()), ("count", limit.min(100).to_string()), ("sort", "timestamp".into())];
        let v = self.api("search.messages", &params).await?;
        let mut out = Vec::new();
        for m in v["messages"]["matches"].as_array().into_iter().flatten() {
            let channel = s(&m["channel"]["id"]);
            out.extend(self.convert(&channel, std::slice::from_ref(m)).await);
        }
        out.sort_by(|a, b| b.ts.cmp(&a.ts));
        Ok(out)
    }

    /// Slack has no "offline": Offline and Away both set away. DND snoozes notifications for an hour.
    async fn set_presence(&self, p: Presence) -> SResult<()> {
        match p {
            Presence::Online => {
                let _ = self.api("dnd.endSnooze", &[]).await;
                self.api("users.setPresence", &[("presence", "auto".into())]).await.map(drop)
            }
            Presence::Away | Presence::Offline => self.api("users.setPresence", &[("presence", "away".into())]).await.map(drop),
            Presence::Dnd => self.api("dnd.setSnooze", &[("num_minutes", "60".into())]).await.map(drop),
            Presence::Unknown => Err(ServiceError::Unsupported),
        }
    }

    async fn workspace_icon(&self) -> SResult<Option<Media>> {
        let v = self.api("team.info", &[]).await?;
        let icon = &v["team"]["icon"];
        if icon["image_default"].as_bool() == Some(true) {
            return Ok(None);
        }
        match icon["image_88"].as_str().or(icon["image_68"].as_str()) {
            Some(url) => Ok(self.fetch_url(url, false).await.ok()),
            None => Ok(None),
        }
    }

    async fn users(&self, ids: &[String]) -> SResult<Vec<User>> {
        let mut out = Vec::new();
        let mut missing = Vec::new();
        {
            let mut st = self.st();
            for id in ids {
                match st.pending_users.remove(id) {
                    Some(u) => out.push(u),
                    None => missing.push(id.clone()),
                }
            }
        }
        out.extend(self.fetch_users(&missing).await);
        self.st().known_users.extend(out.iter().map(|u| u.id.clone()));
        Ok(out)
    }

    async fn profile(&self, user_id: &str) -> SResult<Profile> {
        let v = self.api("users.info", &[("user", user_id.to_string()), ("include_locale", "true".into())]).await?;
        // Custom fields carry their labels only here; optional, so failures just leave them out.
        let fields = match self.api("users.profile.get", &[("user", user_id.to_string()), ("include_labels", "true".into())]).await {
            Ok(p) => p["profile"]["fields"].clone(),
            Err(_) => Value::Null,
        };
        let mut p = convert::profile(&self.account_id, &v["user"], &fields);
        if let Ok(pr) = self.api("users.getPresence", &[("user", user_id.to_string())]).await {
            p.presence = presence(pr["presence"].as_str().unwrap_or_default());
        }
        self.learn(&[p.to_user(&self.account_id)]);
        Ok(p)
    }

    async fn channel_details(&self, channel_id: &str) -> SResult<ChannelDetails> {
        let info = self.api("conversations.info", &[("channel", channel_id.to_string())]).await?;
        let c = &info["channel"];
        let mut d = ChannelDetails {
            purpose: convert::from_slack(c["purpose"]["value"].as_str().unwrap_or_default(), &|_| None).0,
            created: c["created"].as_i64().filter(|t| *t > 0).map(|t| t * 1000),
            creator: c["creator"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
            ..Default::default()
        };
        // The channel's own canvas, then canvas tabs (each once).
        let props = &c["properties"];
        let mut seen = std::collections::HashSet::new();
        if let Some(id) = props["canvas"]["file_id"].as_str().filter(|_| props["canvas"]["is_empty"].as_bool() != Some(true)) {
            seen.insert(id.to_string());
            d.canvases.push(CanvasRef { id: id.to_string(), title: "Channel canvas".into() });
        }
        for t in props["tabs"].as_array().into_iter().flatten().filter(|t| t["type"].as_str() == Some("canvas")) {
            if let Some(id) = t["data"]["file_id"].as_str().filter(|id| seen.insert(id.to_string())) {
                let title = t["label"].as_str().filter(|l| !l.is_empty()).unwrap_or("Canvas");
                d.canvases.push(CanvasRef { id: id.to_string(), title: title.to_string() });
            }
        }
        // Bookmarks and pins are extras: a workspace that refuses them still shows the rest.
        if let Ok(v) = self.api("bookmarks.list", &[("channel_id", channel_id.to_string())]).await {
            d.bookmarks = v["bookmarks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| {
                    let url = b["link"].as_str().filter(|u| !u.is_empty())?.to_string();
                    let title = b["title"].as_str().filter(|t| !t.is_empty()).unwrap_or(&url).to_string();
                    Some(Bookmark { title, url })
                })
                .collect();
        }
        if let Ok(v) = self.api("pins.list", &[("channel", channel_id.to_string())]).await {
            let msgs: Vec<Value> = v["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|i| i["type"].as_str() == Some("message"))
                .map(|i| i["message"].clone())
                .collect();
            d.pins = self.convert(channel_id, &msgs).await;
            d.pins.sort_by_key(|m| std::cmp::Reverse(m.ts));
        }
        Ok(d)
    }

    async fn canvas(&self, id: &str) -> SResult<Canvas> {
        let v = self.api("files.info", &[("file", id.to_string())]).await?;
        let f = &v["file"];
        let title = s(&f["title"]);
        let url = s(&f["permalink"]);
        let source = convert::non_empty(&f["url_private_download"]).or_else(|| convert::non_empty(&f["url_private"]));
        let Some(source) = source else { return Ok(Canvas { id: id.into(), title, html: String::new(), url }) };
        // Canvases download as an HTML document (unlike images, which `fetch_url` handles).
        let resp = self.http.get(&source).bearer_auth(&self.creds.token).header("Cookie", self.cookie()).send().await.map_err(net)?;
        if !resp.status().is_success() {
            return Err(ServiceError::Other(format!("HTTP {}", resp.status())));
        }
        let html = resp.text().await.map_err(net)?;
        Ok(Canvas { id: id.into(), title, html, url })
    }

    async fn members(&self, channel_id: &str, cursor: Option<&str>) -> SResult<MemberPage> {
        let mut params = vec![("channel", channel_id.to_string()), ("limit", "200".to_string())];
        if let Some(c) = cursor {
            params.push(("cursor", c.to_string()));
        }
        let v = self.api("conversations.members", &params).await?;
        let ids: Vec<String> = v["members"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
        let users = self.fetch_users(&ids).await;
        self.st().known_users.extend(users.iter().map(|u| u.id.clone()));
        let next = v["response_metadata"]["next_cursor"].as_str().filter(|c| !c.is_empty()).map(str::to_string);
        let total = if cursor.is_none() {
            self.api("conversations.info", &[("channel", channel_id.to_string()), ("include_num_members", "true".into())])
                .await
                .ok()
                .and_then(|i| i["channel"]["num_members"].as_u64())
                .or_else(|| next.is_none().then_some(ids.len() as u64))
        } else {
            None
        };
        Ok(MemberPage { users, presence: vec![], next, total })
    }

    async fn fetch(&self, path: &str) -> SResult<Media> {
        let allowed = path.starts_with("/files-pri/") || path.starts_with("/files-tmb/");
        if !allowed || path.contains("..") {
            return Err(ServiceError::Other("path not allowed".into()));
        }
        self.fetch_url(&format!("https://files.slack.com{path}"), true).await
    }

    async fn download(&self, path: &str, dest: &mut Download) -> SResult<()> {
        if !path.starts_with("/files-pri/") || path.contains("..") {
            return Err(ServiceError::Other("path not allowed".into()));
        }
        let url = format!("https://files.slack.com{path}");
        let mut resp = self.http.get(&url).bearer_auth(&self.creds.token).header("Cookie", self.cookie()).send().await.map_err(net)?;
        if !resp.status().is_success() {
            return Err(ServiceError::Other(format!("HTTP {}", resp.status())));
        }
        dest.mime = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        // An expired session gets the sign-in page instead of the file.
        if dest.mime.starts_with("text/html") {
            return Err(ServiceError::Auth("Slack sent its sign-in page: sign in again".into()));
        }
        while let Some(chunk) = resp.chunk().await.map_err(net)? {
            dest.write(&chunk)?;
        }
        Ok(())
    }

    fn to_remote_markdown(&self, md: &str) -> String {
        convert::to_slack(md)
    }
}

// ------------------------------------------------------------ sign-in helpers

/// One workspace found in the sign-in window.
pub struct TeamLogin {
    pub id: String,
    pub token: String,
    pub url: String,
    pub name: String,
}

/// Parses [`TEAMS_COOKIE`] (URL-encoded, one tab-separated line per workspace).
pub fn parse_teams(raw: &str) -> Vec<TeamLogin> {
    convert::pct_decode(raw)
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let t = TeamLogin {
                id: f.next()?.to_string(),
                token: f.next()?.to_string(),
                url: f.next()?.trim_end_matches('/').to_string(),
                name: f.next().unwrap_or_default().to_string(),
            };
            t.token.starts_with("xoxc-").then_some(t)
        })
        .collect()
}

/// Where the sign-in window always starts: Slack's light, general page (email, Google, Apple,
/// workspace list). `app.slack.com` would first download the whole web
/// client (tens of MB in an empty private profile) just to redirect here.
pub const SIGNIN_URL: &str = "https://slack.com/signin";

/// "acme", "acme.slack.com" or any link into it → the workspace URL (no trailing slash).
pub fn workspace_url(workspace: &str) -> Option<String> {
    let w = workspace.trim().trim_end_matches('/');
    if w.is_empty() {
        return None;
    }
    let host = w.trim_start_matches("https://").trim_start_matches("http://");
    let host = host.split('/').next().unwrap_or(host);
    Some(if host.contains('.') { format!("https://{host}") } else { format!("https://{host}.slack.com") })
}


/// Parses [`WORKSPACES_COOKIE`]; only `*.slack.com` workspace hosts are kept, so the
/// session cookie is never sent anywhere else.
pub fn parse_workspaces(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in convert::pct_decode(raw).lines() {
        if let Some(ws) = workspace_of(line.trim()) {
            if !out.contains(&ws) {
                out.push(ws);
            }
        }
    }
    out.truncate(20);
    out
}

/// The workspace a sign-in window is on (`https://acme.slack.com/...`), if any.
pub fn workspace_of(url: &str) -> Option<String> {
    let host = url.split("://").nth(1)?.split('/').next()?;
    let sub = host.strip_suffix(".slack.com")?;
    let generic = ["app", "www", "api", "edgeapi", "status", "files", "slack", "a"];
    (!sub.is_empty() && !sub.contains('.') && !generic.contains(&sub)).then(|| format!("https://{host}"))
}

fn find_token(html: &str) -> Option<String> {
    const KEY: &str = "\"api_token\":\"";
    let start = html.match_indices(KEY).map(|(i, _)| i + KEY.len()).find(|&i| html[i..].starts_with("xoxc-"))?;
    let len = html[start..].find('"')?;
    Some(html[start..start + len].to_string())
}

/// The web client's `xoxc-` token for a workspace, read from the page the browser loads
/// right after sign-in, using only the `d` cookie. Lets the sign-in window close as soon
/// as the login is done, without waiting for the full Slack client to start.
pub async fn token_from_cookie(workspace_url: &str, cookie: &str) -> Option<String> {
    let cookie = convert::pct_encode_cookie(cookie);
    // Slack serves its boot data only to browsers it supports.
    const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0";
    let http = reqwest::Client::builder().user_agent(UA).timeout(Duration::from_secs(20)).build().ok()?;
    for path in ["/ssb/redirect", "/"] {
        let resp = http.get(format!("{workspace_url}{path}")).header("Cookie", format!("d={cookie}")).send().await;
        let Ok(resp) = resp else { continue };
        if let Some(t) = resp.text().await.ok().and_then(|html| find_token(&html)) {
            return Some(t);
        }
    }
    None
}

pub struct WhoAmI {
    pub user_id: String,
    pub username: String,
    pub team_id: String,
    pub team: String,
    /// Workspace URL without trailing slash.
    pub url: String,
}

/// Checks a token + cookie pair against `auth.test`.
pub async fn whoami(creds: &Credentials) -> Result<WhoAmI, String> {
    let resp = client()
        .post("https://slack.com/api/auth.test")
        .header("Cookie", format!("d={}", creds.cookie))
        .form(&[("token", creds.token.as_str())])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v = check(resp).await.map_err(|e| e.to_string())?;
    Ok(WhoAmI {
        user_id: s(&v["user_id"]),
        username: s(&v["user"]),
        team_id: s(&v["team_id"]),
        team: s(&v["team"]),
        url: s(&v["url"]).trim_end_matches('/').to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_in_helpers() {
        let raw = "T1%09xoxc-abc%09https%3A%2F%2Facme.slack.com%2F%09Acme%0AT2%09xoxe-no%09https%3A%2F%2Fx.slack.com%2F%09X";
        let teams = parse_teams(raw);
        assert_eq!(teams.len(), 1);
        assert_eq!((teams[0].id.as_str(), teams[0].url.as_str(), teams[0].name.as_str()), ("T1", "https://acme.slack.com", "Acme"));
        assert_eq!(workspace_of("https://acme.slack.com/ssb/redirect").as_deref(), Some("https://acme.slack.com"));
        assert_eq!(workspace_of("https://app.slack.com/client/T1"), None);
        assert_eq!(workspace_of("https://slack.com/signin"), None);
        let ws = parse_workspaces("https%3A%2F%2Facme.slack.com%0Ahttps%3A%2F%2Fevil.example.com%0Ahttps%3A%2F%2Facme.slack.com");
        assert_eq!(ws, vec!["https://acme.slack.com".to_string()]);
        let html = r#"x "api_token":"xoxe-no" y "api_token":"xoxc-123-abc" z"#;
        assert_eq!(find_token(html).as_deref(), Some("xoxc-123-abc"));
        let c = Credentials::new(" xoxc-1 ", "xoxd-a/b");
        assert_eq!(Credentials::parse(&c.to_json()).unwrap().cookie, "xoxd-a%2Fb");
    }
}
