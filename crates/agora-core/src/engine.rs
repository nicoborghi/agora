//! The engine: one supervised, isolated connector per account, a single event
//! hub that writes to the archive and applies notification rules, and the API
//! the UI layer calls.
//!
//! Isolation: every connector runs in its own task. A panic, an auth failure or
//! a network outage in one account only greys out that account.

use crate::backoff::Backoff;
use crate::model::*;
use crate::notify::{self, NotifyInput};
use crate::service::*;
use crate::settings::{ChannelPrefs, Level, Settings};
use crate::store::{Authored, Store, StoreResult};
use crate::{search, secrets, sync, text};
use chrono::{Datelike, Timelike};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// Bounded so that a burst of events back-pressures the sockets instead of growing memory.
const EVENT_QUEUE: usize = 1024;
/// Workspace dots on the taskbar button (2×2 grid).
const MAX_TRAY_DOTS: usize = 4;
const MARK_READ_DELAY: Duration = Duration::from_millis(1500);
/// A wall-clock jump bigger than this between two 15 s ticks means the PC was asleep.
const RESUME_GAP: Duration = Duration::from_secs(45);
/// Workspace logos are small (team icons, favicons); anything bigger is not a logo.
const MAX_ICON_BYTES: usize = 96 * 1024;

pub trait Frontend: Send + Sync + 'static {
    fn emit(&self, event: UiEvent);
    fn popup(&self, note: PopupNote);
    fn play_sound(&self, sound: &str);
    /// Colours of the workspaces with unread news, in left-bar order (at most 4): one general dot
    /// on the tray icon, one dot per workspace on the taskbar button.
    fn set_tray(&self, dots: Vec<String>);
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageView {
    #[serde(flatten)]
    pub message: Message,
    pub author_name: String,
    /// Profile picture URL (https, or the `agora` scheme for pictures that need the account's token).
    pub author_avatar: Option<String>,
    pub is_self: bool,
    /// Who reacted (user id -> name and picture), so the UI can say who.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub reactors: HashMap<String, Reactor>,
    /// For a thread reply: who wrote the message it answers, and how it starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<RootPreview>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RootPreview {
    pub author_name: String,
    pub text: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Reactor {
    pub name: String,
    pub avatar: Option<String>,
}

impl MessageView {
    fn new(a: Authored, self_id: Option<&str>) -> Self {
        Self {
            is_self: self_id == Some(a.message.author_id.as_str()),
            author_name: a.name,
            author_avatar: a.avatar,
            message: a.message,
            reactors: HashMap::new(),
            reply_to: None,
        }
    }
}

/// One unread conversation on the Home view, with its newest unread messages.
#[derive(Debug, Clone, Serialize)]
pub struct HomeItem {
    pub channel: ChannelView,
    pub messages: Vec<MessageView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeerView {
    pub id: String,
    pub name: String,
    pub presence: Presence,
    /// Profile picture URL, when the service has one.
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelView {
    #[serde(flatten)]
    pub channel: Channel,
    pub display_name: String,
    pub peers: Vec<PeerView>,
    /// Notifications fully off (effective level Silent): shown dimmed with a crossed bell.
    pub muted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileView {
    pub account_id: AccountId,
    #[serde(flatten)]
    pub profile: Profile,
    /// The signed-in user of that account.
    pub is_self: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemberView {
    #[serde(flatten)]
    pub user: User,
    pub presence: Presence,
}

#[derive(Debug, Clone, Serialize)]
pub struct MembersView {
    pub members: Vec<MemberView>,
    /// Cursor for the next page, if there is one.
    pub next: Option<String>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SelfView {
    pub user: User,
    pub presence: Presence,
}

#[derive(Debug, Clone, Serialize)]
pub struct PopupNote {
    pub account_id: AccountId,
    pub channel_id: String,
    pub service: String,
    pub channel: String,
    pub author: String,
    pub preview: String,
    pub color: String,
    pub seconds: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    Connection { account_id: AccountId, state: ConnState },
    Message { message: MessageView },
    MessageDeleted { account_id: AccountId, channel_id: String, id: String },
    Channel { channel: ChannelView },
    ChannelsReloaded { account_id: AccountId },
    /// Catch-up stored new history for this conversation.
    History { account_id: AccountId, channel_id: String },
    Typing { account_id: AccountId, channel_id: String, user_name: String, thread_root: Option<String> },
    Presence { account_id: AccountId, user_id: String, presence: Presence },
    Accounts,
}

struct Running {
    service: Arc<dyn Service>,
    task: JoinHandle<()>,
}

/// Aborts the task when dropped, so cancelling a supervisor also closes its socket.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Default)]
struct View {
    open: Option<(AccountId, String)>,
    focused: bool,
}

type Key = (AccountId, String);

pub struct Engine {
    store: Arc<Store>,
    frontend: Arc<dyn Frontend>,
    factory: Arc<dyn ServiceFactory>,
    settings: RwLock<Settings>,
    rt: OnceLock<tokio::runtime::Handle>,
    tx: mpsc::Sender<(AccountId, ServiceEvent)>,
    rx: Mutex<Option<mpsc::Receiver<(AccountId, ServiceEvent)>>>,
    wake: watch::Sender<u64>,
    running: Mutex<HashMap<AccountId, Running>>,
    conn: Mutex<HashMap<AccountId, ConnState>>,
    /// Only for people we have a conversation with; a few hundred entries at most.
    presence: Mutex<HashMap<Key, Presence>>,
    dots: Mutex<HashMap<Key, String>>,
    view: Mutex<View>,
    pending_reads: Arc<Mutex<HashMap<Key, Message>>>,
    /// Conversations whose full server history is already local.
    exhausted: Mutex<HashSet<Key>>,
    /// Pages of the archive checked against the service recently (conversation, `before`),
    /// so reopening or reloading a conversation does not ask again within a short time.
    verified: Mutex<HashMap<(Key, Option<Millis>), Instant>>,
}

/// How often the same archive page is checked against the service.
const VERIFY_EVERY: Duration = Duration::from_secs(30);
/// Thread replies shown with one page of a conversation.
const REPLIES_PER_PAGE: u32 = 300;
/// Pages fetched to close one gap; older ones are filled while scrolling up.
const MAX_GAP_PAGES: usize = 5;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn now_ms() -> Millis {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as Millis)
        .unwrap_or(0)
}

fn local_clock() -> (u8, u16) {
    let now = chrono::Local::now();
    (now.weekday().num_days_from_monday() as u8, (now.hour() * 60 + now.minute()) as u16)
}

impl Engine {
    pub fn new(store: Arc<Store>, frontend: Arc<dyn Frontend>, factory: Arc<dyn ServiceFactory>) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(EVENT_QUEUE);
        let (wake, _) = watch::channel(0u64);
        let settings = store.load_settings();
        Arc::new(Self {
            store,
            frontend,
            factory,
            settings: RwLock::new(settings),
            rt: OnceLock::new(),
            tx,
            rx: Mutex::new(Some(rx)),
            wake,
            running: Mutex::default(),
            conn: Mutex::default(),
            presence: Mutex::default(),
            dots: Mutex::default(),
            view: Mutex::default(),
            pending_reads: Arc::default(),
            exhausted: Mutex::default(),
            verified: Mutex::default(),
        })
    }

    /// Must be called once, from inside the Tokio runtime.
    pub fn start(self: &Arc<Self>) {
        if self.rt.set(tokio::runtime::Handle::current()).is_err() {
            return;
        }
        if let Some(mut rx) = lock(&self.rx).take() {
            let me = self.clone();
            self.spawn(async move {
                while let Some((acc, ev)) = rx.recv().await {
                    me.handle_event(&acc, ev);
                }
            });
        }
        let me = self.clone();
        self.spawn(async move { me.watch_resume().await });
        self.recompute_tray();
        for a in self.accounts() {
            if a.enabled {
                self.start_account(a);
            }
        }
    }

    fn spawn<F: Future<Output = ()> + Send + 'static>(&self, f: F) -> Option<JoinHandle<()>> {
        self.rt.get().map(|h| h.spawn(f))
    }

    pub fn store(&self) -> Arc<Store> {
        self.store.clone()
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Detects sleep/resume through wall-clock jumps and makes every connector reconnect.
    async fn watch_resume(&self) {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last = SystemTime::now();
        loop {
            tick.tick().await;
            let now = SystemTime::now();
            if now.duration_since(last).unwrap_or_default() > RESUME_GAP {
                tracing::info!("system resume detected, reconnecting");
                self.reconnect_all();
            }
            last = now;
        }
    }

    pub fn reconnect_all(&self) {
        self.wake.send_modify(|g| *g = g.wrapping_add(1));
    }

    // ------------------------------------------------------------ supervision

    fn start_account(self: &Arc<Self>, account: Account) {
        self.stop_account(&account.id);
        let id = account.id.clone();
        let service = match self.factory.create(&account) {
            Ok(s) => s,
            Err(reason) => {
                self.set_conn(&id, ConnState::AuthExpired { reason });
                return;
            }
        };
        let me = self.clone();
        let svc = service.clone();
        if let Some(task) = self.spawn(async move { me.supervise(account, svc).await }) {
            lock(&self.running).insert(id, Running { service, task });
        }
    }

    fn stop_account(&self, id: &str) {
        if let Some(r) = lock(&self.running).remove(id) {
            r.task.abort();
        }
        lock(&self.conn).remove(id);
    }

    fn service(&self, acc: &str) -> Option<Arc<dyn Service>> {
        lock(&self.running).get(acc).map(|r| r.service.clone())
    }

    async fn supervise(self: Arc<Self>, account: Account, svc: Arc<dyn Service>) {
        let id = account.id;
        let mut backoff = Backoff::new(Duration::from_secs(2), Duration::from_secs(300));
        let mut wake = self.wake.subscribe();
        loop {
            self.set_conn(&id, ConnState::Connecting);
            let started = Instant::now();
            // The session runs in its own task so a panicking connector is caught here.
            let mut session = AbortOnDrop(tokio::spawn(self.clone().session(id.clone(), svc.clone(), wake.clone())));
            let outcome = match (&mut session.0).await {
                Ok(r) => r,
                Err(e) if e.is_panic() => Err(ServiceError::Other("connector crashed".into())),
                Err(_) => Ok(()),
            };
            if started.elapsed() > Duration::from_secs(120) {
                backoff.reset();
            }
            let delay = match outcome {
                Err(ServiceError::Auth(reason)) => {
                    tracing::warn!(account = %id, "signed out: {reason}");
                    self.set_conn(&id, ConnState::AuthExpired { reason });
                    return;
                }
                Err(ServiceError::RateLimited(d)) => d.max(backoff.next_delay()),
                Err(e) => {
                    tracing::warn!(account = %id, "connection lost: {e}");
                    backoff.next_delay()
                }
                Ok(()) => backoff.next_delay(),
            };
            self.set_conn(&id, ConnState::Offline { retry_in: delay.as_secs().max(1) });
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = wake.changed() => {}
            }
        }
    }

    /// One connected session: bootstrap, realtime loop and catch-up, until the socket drops.
    async fn session(
        self: Arc<Self>,
        id: AccountId,
        svc: Arc<dyn Service>,
        mut wake: watch::Receiver<u64>,
    ) -> SResult<()> {
        let boot = svc.bootstrap().await?;
        if let Err(e) = self.apply_bootstrap(&id, boot) {
            tracing::warn!(account = %id, "bootstrap not stored: {e}");
        }
        let wants_icon = self.store.get_account(&id).ok().flatten().is_some_and(|a| a.icon_image.is_none());
        if wants_icon {
            self.fetch_icon(&id, &svc).await;
        }
        let sink = EventSink::new(id.clone(), self.tx.clone());
        let run_svc = svc.clone();
        let mut run = AbortOnDrop(tokio::spawn(async move { run_svc.run(sink).await }));
        let _catch_up = AbortOnDrop(tokio::spawn(self.clone().catch_up(id, svc)));
        tokio::select! {
            r = &mut run.0 => match r {
                Ok(r) => r,
                Err(e) if e.is_panic() => Err(ServiceError::Other("connector crashed".into())),
                Err(_) => Ok(()),
            },
            _ = wake.changed() => Err(ServiceError::Network("system resumed, reconnecting".into())),
        }
    }

    fn apply_bootstrap(&self, acc: &str, b: Bootstrap) -> StoreResult<()> {
        self.store.set_self_user(acc, &b.self_user.id)?;
        let mut users = b.users;
        users.push(b.self_user);
        self.store.upsert_users(&users)?;
        drop(users);
        self.store.upsert_channels(&b.channels)?;
        self.recompute_tray();
        self.frontend.emit(UiEvent::ChannelsReloaded { account_id: acc.to_string() });
        Ok(())
    }

    async fn catch_up(self: Arc<Self>, acc: AccountId, svc: Arc<dyn Service>) {
        let states = match self.store.sync_states(&acc) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(account = %acc, "catch-up skipped: {e}");
                return;
            }
        };
        for ch in sync::catch_up_order(&states, now_ms()) {
            let mut after = states.iter().find(|s| s.id == ch).and_then(|s| s.local_latest);
            loop {
                let limit = if after.is_some() { 100 } else { 50 };
                match svc.history(&ch, HistoryQuery { after, limit, ..Default::default() }).await {
                    Ok(msgs) => {
                        let newest = msgs.iter().map(|m| m.ts).max();
                        let full = msgs.len() >= limit as usize;
                        if let Err(e) = self.store_history(&acc, &ch, &msgs) {
                            tracing::warn!(account = %acc, "history not stored: {e}");
                            break;
                        }
                        self.fill_users(&acc, &svc, &msgs).await;
                        // First sync: one page is enough, older pages load on scroll.
                        if after.is_none() || !full || newest <= after {
                            break;
                        }
                        after = newest;
                    }
                    Err(ServiceError::RateLimited(d)) => tokio::time::sleep(d).await,
                    Err(e) => {
                        tracing::debug!(account = %acc, channel = %ch, "catch-up stopped: {e}");
                        break;
                    }
                }
            }
            self.frontend.emit(UiEvent::History { account_id: acc.clone(), channel_id: ch });
        }
    }

    /// Fetches profiles of authors the archive has never seen (old messages, people who left).
    async fn fill_users(&self, acc: &str, svc: &Arc<dyn Service>, msgs: &[Message]) {
        let ids: Vec<String> = msgs.iter().map(|m| m.author_id.clone()).collect();
        let missing = match self.store.missing_users(acc, &ids) {
            Ok(m) if !m.is_empty() => m,
            _ => return,
        };
        match svc.users(&missing).await {
            Ok(mut users) => {
                users.iter_mut().for_each(|u| u.account_id = acc.to_string());
                if let Err(e) = self.store.upsert_users(&users) {
                    tracing::warn!(account = %acc, "users not stored: {e}");
                }
            }
            Err(e) => tracing::debug!(account = %acc, "user lookup failed: {e}"),
        }
    }

    fn store_history(&self, acc: &str, ch: &str, msgs: &[Message]) -> StoreResult<()> {
        if msgs.is_empty() {
            return Ok(());
        }
        self.store.upsert_messages(msgs)?;
        if let Some(ts) = msgs.iter().map(|m| m.ts).max() {
            self.store.touch_channel(acc, ch, ts)?;
        }
        Ok(())
    }

    fn set_conn(&self, acc: &str, state: ConnState) {
        lock(&self.conn).insert(acc.to_string(), state.clone());
        self.frontend.emit(UiEvent::Connection { account_id: acc.to_string(), state });
    }

    // ------------------------------------------------------------ event hub

    fn handle_event(&self, acc: &str, ev: ServiceEvent) {
        if let Err(e) = self.apply_event(acc, ev) {
            tracing::warn!(account = %acc, "event dropped: {e}");
        }
    }

    fn apply_event(&self, acc: &str, ev: ServiceEvent) -> StoreResult<()> {
        match ev {
            ServiceEvent::Connection(state) => self.set_conn(acc, state),
            ServiceEvent::MessageUpsert(m) => self.ingest(acc, m, true)?,
            ServiceEvent::MessageDeleted { channel_id, id } => {
                self.store.delete_message(acc, &channel_id, &id)?;
                self.frontend.emit(UiEvent::MessageDeleted { account_id: acc.to_string(), channel_id, id });
            }
            ServiceEvent::Reaction { channel_id, message_id, emoji, user_id, added } => {
                if let Some(m) = self.store.apply_reaction(acc, &channel_id, &message_id, &emoji, &user_id, added)? {
                    self.emit_message(m)?;
                }
            }
            ServiceEvent::ChannelUpsert(c) => {
                let id = c.id.clone();
                self.store.upsert_channels(std::slice::from_ref(&c))?;
                self.emit_channel(acc, &id)?;
            }
            ServiceEvent::ChannelRead { channel_id, ts } => {
                self.store.mark_read_local(acc, &channel_id, ts)?;
                self.clear_dot(acc, &channel_id);
                self.emit_channel(acc, &channel_id)?;
            }
            ServiceEvent::Sections(sections) => {
                self.store.apply_sections(acc, &sections)?;
                self.frontend.emit(UiEvent::ChannelsReloaded { account_id: acc.to_string() });
            }
            ServiceEvent::UserUpsert(u) => self.store.upsert_users(std::slice::from_ref(&u))?,
            ServiceEvent::Typing { channel_id, user_id, thread_root } => {
                let user_name = self.store.user_name(acc, &user_id)?;
                self.frontend.emit(UiEvent::Typing { account_id: acc.to_string(), channel_id, user_name, thread_root });
            }
            ServiceEvent::Presence { user_id, presence } => {
                lock(&self.presence).insert((acc.to_string(), user_id.clone()), presence);
                self.frontend.emit(UiEvent::Presence { account_id: acc.to_string(), user_id, presence });
            }
        }
        Ok(())
    }

    /// Stores a new or edited message. `notify` is false for our own sends.
    fn ingest(&self, acc: &str, mut m: Message, notify: bool) -> StoreResult<()> {
        m.account_id = acc.to_string();
        let previous = self.store.get_message(acc, &m.channel_id, &m.id)?;
        if let Some(old) = &previous {
            // Text-edit events from some services carry no reactions: keep the ones we know.
            // Other updates (Rocket.Chat sends the whole message when a reaction goes away)
            // are authoritative, so an emptied list stays empty.
            let text_edit = m.edited_at.is_some() && m.edited_at != old.edited_at;
            if m.reactions.is_empty() && text_edit {
                m.reactions = old.reactions.clone();
            }
            if m.edited_at.is_some() && old.text != m.text && self.settings().keep_edit_history {
                self.store.save_version(old)?;
            }
        }
        self.store.upsert_message(&m)?;
        if previous.is_none() {
            self.store.touch_channel(acc, &m.channel_id, m.ts)?;
            if let Some(root) = &m.thread_root {
                self.store.bump_reply_count(acc, &m.channel_id, root)?;
                if let Some(r) = self.store.get_message(acc, &m.channel_id, root)? {
                    self.emit_message(r)?;
                }
            }
            if notify {
                self.on_new_message(acc, &m)?;
            }
        }
        self.emit_message(m)
    }

    fn on_new_message(&self, acc: &str, m: &Message) -> StoreResult<()> {
        let Some(account) = self.store.get_account(acc)? else { return Ok(()) };
        let Some(channel) = self.store.get_channel(acc, &m.channel_id)? else { return Ok(()) };
        let self_id = account.self_user_id.clone().unwrap_or_default();
        let is_self = !self_id.is_empty() && m.author_id == self_id;
        let mentioned = m.mentions.iter().any(|u| u == MENTION_ALL || (!self_id.is_empty() && *u == self_id));
        // A reply in a thread we started or replied in is addressed to us, like a mention.
        let my_thread = !is_self
            && !self_id.is_empty()
            && match &m.thread_root {
                Some(root) => self.store.in_thread(acc, &m.channel_id, root, &self_id)?,
                None => false,
            };
        let mentions_me = mentioned || my_thread;
        let viewing = self.is_viewing(acc, &m.channel_id);
        // Thread replies only count as unread when they mention us or follow up on our thread.
        let counts = !is_self && !viewing && (m.thread_root.is_none() || mentions_me);
        if counts {
            self.store.bump_unread(acc, &m.channel_id, mentions_me)?;
        }
        if viewing && !is_self {
            self.queue_mark_read(acc, m.clone());
        }

        let prefs = self.store.channel_prefs(acc, &m.channel_id)?;
        let settings = self.settings();
        let (weekday, minute) = local_clock();
        let decision = notify::decide(&NotifyInput {
            settings: &settings.notifications,
            prefs: &prefs,
            account_id: acc,
            is_direct: channel.kind.is_direct(),
            is_self,
            mentions_me,
            viewing,
            text: &m.text,
            weekday,
            minute,
        });
        let color = prefs.color.clone().unwrap_or_else(|| account.color.clone());
        if counts && decision.tray_dot {
            lock(&self.dots).insert((acc.to_string(), m.channel_id.clone()), color.clone());
            self.push_tray();
        }
        if decision.popup {
            let view = self.channel_view(channel)?;
            self.frontend.popup(PopupNote {
                account_id: acc.to_string(),
                channel_id: m.channel_id.clone(),
                service: account.name.clone(),
                channel: if view.channel.kind.is_direct() { view.display_name } else { format!("#{}", view.display_name) },
                author: self.store.user_name(acc, &m.author_id)?,
                preview: text::preview(&m.text, 160),
                color,
                seconds: settings.notifications.popup_seconds,
            });
        }
        if decision.sound {
            let sound = prefs.sound.as_deref().unwrap_or(&settings.notifications.sound);
            self.frontend.play_sound(sound);
        }
        self.emit_channel(acc, &m.channel_id)
    }

    fn is_viewing(&self, acc: &str, ch: &str) -> bool {
        let v = lock(&self.view);
        v.focused && v.open.as_ref().is_some_and(|(a, c)| a == acc && c == ch)
    }

    /// Debounced: a busy open channel produces one mark-read call per 1.5 s, not one per message.
    fn queue_mark_read(&self, acc: &str, latest: Message) {
        let key = (acc.to_string(), latest.channel_id.clone());
        if lock(&self.pending_reads).insert(key.clone(), latest).is_some() {
            return;
        }
        let Some(svc) = self.service(acc) else {
            lock(&self.pending_reads).remove(&key);
            return;
        };
        let pending = self.pending_reads.clone();
        self.spawn(async move {
            tokio::time::sleep(MARK_READ_DELAY).await;
            let latest = lock(&pending).remove(&key);
            if let Some(m) = latest {
                if let Err(e) = svc.mark_read(&m.channel_id, Some(&m)).await {
                    tracing::debug!("mark read failed: {e}");
                }
            }
        });
    }

    // ------------------------------------------------------------ tray

    fn clear_dot(&self, acc: &str, ch: &str) {
        let removed = lock(&self.dots).remove(&(acc.to_string(), ch.to_string())).is_some();
        if removed {
            self.push_tray();
        }
    }

    /// One colour per workspace with something to see, in the left bar's order, so a
    /// workspace keeps its place on the taskbar button.
    fn push_tray(&self) {
        let dots = lock(&self.dots);
        let mut colors: Vec<String> = Vec::new();
        for a in self.accounts().into_iter().filter(|a| a.enabled) {
            if dots.keys().any(|(acc, _)| acc == &a.id) && !colors.contains(&a.color) {
                colors.push(a.color);
            }
        }
        drop(dots);
        colors.truncate(MAX_TRAY_DOTS);
        self.frontend.set_tray(colors);
    }

    /// Rebuilds tray dots from stored unread counts (start-up, settings or prefs change).
    fn recompute_tray(&self) {
        let n = self.settings().notifications;
        let accounts: HashMap<String, Account> =
            self.accounts().into_iter().filter(|a| a.enabled).map(|a| (a.id.clone(), a)).collect();
        let prefs = self.store.all_channel_prefs().unwrap_or_default();
        let mut dots = HashMap::new();
        for c in self.store.unread_channels().unwrap_or_default() {
            let Some(account) = accounts.get(&c.account_id) else { continue };
            let key = (c.account_id.clone(), c.id.clone());
            let p = prefs.get(&key).cloned().unwrap_or_default();
            let level = p.level.unwrap_or_else(|| n.default_level(&c.account_id, c.kind.is_direct()));
            let mut dot = level != Level::Silent;
            if let Some(t) = p.tray {
                dot = t;
            }
            if c.mentions > 0 && n.mentions_override {
                dot = true;
            }
            if dot {
                dots.insert(key, p.color.unwrap_or_else(|| account.color.clone()));
            }
        }
        *lock(&self.dots) = dots;
        self.push_tray();
    }

    // ------------------------------------------------------------ views

    fn self_id(&self, acc: &str) -> Option<String> {
        self.store.get_account(acc).ok().flatten().and_then(|a| a.self_user_id)
    }

    fn message_view(&self, m: Message) -> StoreResult<MessageView> {
        let user = self.store.get_user(&m.account_id, &m.author_id)?;
        let author_name = match &user {
            Some(u) if !u.display_name.is_empty() => u.display_name.clone(),
            Some(u) => u.name.clone(),
            None => m.author_id.clone(),
        };
        let is_self = self.self_id(&m.account_id).as_deref() == Some(m.author_id.as_str());
        let view = MessageView { author_avatar: user.and_then(|u| u.avatar_url), message: m, author_name, is_self, reactors: HashMap::new(), reply_to: None };
        Ok(self.with_reactors(vec![view]).pop().expect("one view in, one out"))
    }

    /// Fills in who reacted, with one archive lookup per account for the whole list, and for
    /// replies the message they answer.
    fn with_reactors(&self, mut views: Vec<MessageView>) -> Vec<MessageView> {
        let mut roots: HashMap<(String, String, String), Option<RootPreview>> = HashMap::new();
        for v in &mut views {
            let m = &v.message;
            let Some(root) = &m.thread_root else { continue };
            let k = (m.account_id.clone(), m.channel_id.clone(), root.clone());
            let preview = roots.entry(k).or_insert_with(|| {
                let r = self.store.get_message(&m.account_id, &m.channel_id, root).ok().flatten()?;
                Some(RootPreview {
                    author_name: self.store.user_name(&m.account_id, &r.author_id).unwrap_or_default(),
                    text: text::preview(&r.text, 140),
                    deleted: r.deleted,
                })
            });
            v.reply_to = preview.clone();
        }
        let mut wanted: HashMap<String, Vec<String>> = HashMap::new();
        for v in &views {
            for r in &v.message.reactions {
                wanted.entry(v.message.account_id.clone()).or_default().extend(r.users.iter().cloned());
            }
        }
        let mut cards: HashMap<(String, String), Reactor> = HashMap::new();
        for (acc, mut ids) in wanted {
            ids.sort();
            ids.dedup();
            for (id, name, avatar) in self.store.user_cards(&acc, &ids).unwrap_or_default() {
                cards.insert((acc.clone(), id), Reactor { name, avatar });
            }
        }
        for v in &mut views {
            for r in &v.message.reactions {
                for u in &r.users {
                    if let Some(c) = cards.get(&(v.message.account_id.clone(), u.clone())) {
                        v.reactors.insert(u.clone(), c.clone());
                    }
                }
            }
        }
        views
    }

    fn emit_message(&self, m: Message) -> StoreResult<()> {
        let message = self.message_view(m)?;
        self.frontend.emit(UiEvent::Message { message });
        Ok(())
    }

    fn channel_view(&self, c: Channel) -> StoreResult<ChannelView> {
        // Muted = effective level Silent: no popup, no dot, not in Home.
        let own = self.store.channel_prefs(&c.account_id, &c.id)?.level;
        let level = own.unwrap_or_else(|| self.settings().notifications.default_level(&c.account_id, c.kind.is_direct()));
        let muted = level == Level::Silent;
        if !c.kind.is_direct() {
            let display_name = c.name.clone();
            return Ok(ChannelView { channel: c, display_name, peers: vec![], muted });
        }
        let cards = self.store.user_cards(&c.account_id, &c.members)?;
        let presence = lock(&self.presence);
        let peers: Vec<PeerView> = cards
            .into_iter()
            .map(|(id, name, avatar)| PeerView {
                presence: presence.get(&(c.account_id.clone(), id.clone())).copied().unwrap_or_default(),
                id,
                name,
                avatar,
            })
            .collect();
        drop(presence);
        let display_name = if peers.is_empty() {
            c.name.clone()
        } else {
            peers.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
        };
        Ok(ChannelView { channel: c, display_name, peers, muted })
    }

    fn emit_channel(&self, acc: &str, ch: &str) -> StoreResult<()> {
        if let Some(c) = self.store.get_channel(acc, ch)? {
            let channel = self.channel_view(c)?;
            self.frontend.emit(UiEvent::Channel { channel });
        }
        Ok(())
    }

    // ------------------------------------------------------------ API used by the UI

    pub fn accounts(&self) -> Vec<Account> {
        self.store.list_accounts().unwrap_or_default()
    }

    pub fn connections(&self) -> HashMap<AccountId, ConnState> {
        lock(&self.conn).clone()
    }

    pub fn channels(&self, account: Option<&str>) -> Result<Vec<ChannelView>, String> {
        let chans = self.store.list_channels(account).map_err(err)?;
        chans.into_iter().map(|c| self.channel_view(c).map_err(err)).collect()
    }

    /// One page of top-level messages; falls back to the service when the archive runs out.
    /// A full page from the archive is shown at once and checked against the service in the
    /// background, because the archive can have gaps (see [`Self::fill_gap`]).
    pub async fn messages(
        self: &Arc<Self>,
        acc: &str,
        ch: &str,
        before: Option<Millis>,
        limit: u32,
    ) -> Result<Vec<MessageView>, String> {
        let mut local = self.store.messages(acc, ch, before, limit).map_err(err)?;
        let key = (acc.to_string(), ch.to_string());
        let exhausted = lock(&self.exhausted).contains(&key);
        if (local.len() as u32) >= limit || exhausted {
            if let Some(svc) = self.service(acc) {
                let fresh = {
                    let mut v = lock(&self.verified);
                    let page = (key.clone(), before);
                    let recent = v.get(&page).is_some_and(|t| t.elapsed() < VERIFY_EVERY);
                    if !recent {
                        v.insert(page, Instant::now());
                    }
                    !recent
                };
                if fresh {
                    let before_id = before.and_then(|ts| self.store.message_id_at(acc, ch, ts).ok().flatten());
                    let me = self.clone();
                    let (acc, ch) = key.clone();
                    self.spawn(async move { me.fill_gap(acc, ch, svc, before, before_id, limit).await });
                }
            }
        } else {
            if let Some(svc) = self.service(acc) {
                let (oldest, oldest_id) = match local.first() {
                    Some(a) => (Some(a.message.ts), Some(a.message.id.clone())),
                    None => (before, before.and_then(|ts| self.store.message_id_at(acc, ch, ts).ok().flatten())),
                };
                let query = HistoryQuery { before: oldest, before_id: oldest_id, limit, ..Default::default() };
                match svc.history(ch, query).await {
                    Ok(remote) => {
                        if (remote.len() as u32) < limit {
                            lock(&self.exhausted).insert(key);
                        }
                        if !remote.is_empty() {
                            self.store_history(acc, ch, &remote).map_err(err)?;
                            self.fill_users(acc, &svc, &remote).await;
                            local = self.store.messages(acc, ch, before, limit).map_err(err)?;
                        }
                    }
                    Err(e) => tracing::debug!(account = %acc, "remote history unavailable: {e}"),
                }
            }
        }
        // Thread replies from the same stretch of time, so the conversation shows what happened
        // in its threads (the UI folds the ones that stay in the thread into one line).
        let from = match local.first() {
            Some(a) if (local.len() as u32) >= limit => a.message.ts,
            _ => Millis::MIN,
        };
        local.extend(self.store.replies_between(acc, ch, from, before, REPLIES_PER_PAGE).map_err(err)?);
        local.sort_by_key(|a| a.message.ts);
        let self_id = self.self_id(acc);
        Ok(self.with_reactors(local.into_iter().map(|a| MessageView::new(a, self_id.as_deref())).collect()))
    }

    /// Fetches the page before `before` (the newest page when `None`) from the service and stores
    /// the messages the archive lacks. The archive can have gaps: conversations the catch-up
    /// skipped, or time Agora was closed, where live messages sit on top of older ones with
    /// nothing in between. While whole pages are new, keeps going back until it meets messages
    /// the archive has. Refreshes the open conversation when anything was added.
    async fn fill_gap(
        self: Arc<Self>,
        acc: String,
        ch: String,
        svc: Arc<dyn Service>,
        mut before: Option<Millis>,
        mut before_id: Option<String>,
        limit: u32,
    ) {
        let mut added = 0;
        let mut pages = 0;
        while pages < MAX_GAP_PAGES {
            let query = HistoryQuery { before, before_id: before_id.clone(), limit, ..Default::default() };
            let remote = match svc.history(&ch, query).await {
                Ok(r) => r,
                Err(ServiceError::RateLimited(d)) => {
                    tokio::time::sleep(d).await;
                    continue;
                }
                Err(e) => {
                    tracing::debug!(account = %acc, channel = %ch, "history check skipped: {e}");
                    break;
                }
            };
            pages += 1;
            let ids: Vec<String> = remote.iter().map(|m| m.id.clone()).collect();
            let missing = match self.store.missing_messages(&acc, &ch, &ids) {
                Ok(n) => n,
                Err(_) => break,
            };
            if missing == 0 {
                break;
            }
            if let Err(e) = self.store_history(&acc, &ch, &remote) {
                tracing::warn!(account = %acc, "history not stored: {e}");
                break;
            }
            self.fill_users(&acc, &svc, &remote).await;
            added += missing;
            // Part of the page was known (the gap is closed), or the service has nothing older.
            if missing < remote.len() || (remote.len() as u32) < limit {
                break;
            }
            // Oldest first: continue below the oldest message of this page.
            before = remote.first().map(|m| m.ts);
            before_id = remote.first().map(|m| m.id.clone());
        }
        if added > 0 {
            tracing::info!(account = %acc, channel = %ch, added, "filled a gap in the archive");
            self.frontend.emit(UiEvent::History { account_id: acc, channel_id: ch });
        }
    }

    /// Bookmarks are local to Agora (the archive), across every account.
    pub fn set_bookmark(&self, acc: &str, ch: &str, id: &str, on: bool) -> Result<(), String> {
        self.store.set_bookmark(acc, ch, id, on, now_ms()).map_err(err)
    }

    /// Bookmarked messages, most recently bookmarked first.
    pub fn bookmarks(&self) -> Result<Vec<MessageView>, String> {
        let mut self_ids: HashMap<String, Option<String>> = HashMap::new();
        Ok(self
            .store
            .bookmarks()
            .map_err(err)?
            .into_iter()
            .map(|a| {
                let me = self_ids.entry(a.message.account_id.clone()).or_insert_with(|| self.self_id(&a.message.account_id));
                MessageView::new(a, me.as_deref())
            })
            .collect::<Vec<_>>())
        .map(|views| self.with_reactors(views))
    }

    pub async fn thread(&self, acc: &str, ch: &str, root: &str) -> Result<Vec<MessageView>, String> {
        if let Some(svc) = self.service(acc) {
            match svc.thread(ch, root).await {
                Ok(remote) => {
                    self.store.upsert_messages(&remote).map_err(err)?;
                    self.fill_users(acc, &svc, &remote).await;
                }
                Err(e) => tracing::debug!(account = %acc, "remote thread unavailable: {e}"),
            }
        }
        let self_id = self.self_id(acc);
        Ok(self.with_reactors(
            self.store.thread(acc, ch, root).map_err(err)?.into_iter().map(|a| MessageView::new(a, self_id.as_deref())).collect(),
        ))
    }

    /// The UI opened a conversation (or closed all with `None`).
    pub fn open_channel(&self, target: Option<(AccountId, String)>) -> Result<(), String> {
        let focused = {
            let mut v = lock(&self.view);
            v.open = target.clone();
            v.focused
        };
        match target {
            Some((acc, ch)) if focused => self.mark_read(&acc, &ch),
            _ => Ok(()),
        }
    }

    pub fn set_focused(&self, focused: bool) {
        let open = {
            let mut v = lock(&self.view);
            v.focused = focused;
            v.open.clone()
        };
        if let (true, Some((acc, ch))) = (focused, open) {
            let _ = self.mark_read(&acc, &ch);
        }
        // Coming back to the window is a good moment to try again, without waiting out the backoff.
        if focused && lock(&self.conn).values().any(|s| matches!(s, ConnState::Offline { .. })) {
            self.reconnect_all();
        }
    }

    fn mark_read(&self, acc: &str, ch: &str) -> Result<(), String> {
        let Some(c) = self.store.get_channel(acc, ch).map_err(err)? else { return Ok(()) };
        let has_dot = lock(&self.dots).contains_key(&(acc.to_string(), ch.to_string()));
        if c.unread == 0 && c.mentions == 0 && !has_dot {
            return Ok(());
        }
        let latest = self.store.latest_message(acc, ch).map_err(err)?;
        let ts = latest.as_ref().map(|m| m.ts).unwrap_or_else(now_ms);
        self.store.mark_read_local(acc, ch, ts).map_err(err)?;
        self.clear_dot(acc, ch);
        if let Some(m) = latest {
            self.queue_mark_read(acc, m);
        }
        self.emit_channel(acc, ch).map_err(err)
    }

    fn connected(&self, acc: &str) -> Result<Arc<dyn Service>, String> {
        self.service(acc).ok_or_else(|| "this account is not connected".to_string())
    }

    pub async fn send(
        &self,
        acc: &str,
        ch: &str,
        text: &str,
        thread_root: Option<&str>,
    ) -> Result<MessageView, String> {
        let svc = self.connected(acc)?;
        let m = svc.send(ch, text, thread_root).await.map_err(err)?;
        self.ingest(acc, m.clone(), false).map_err(err)?;
        self.message_view(m).map_err(err)
    }

    pub async fn edit(&self, acc: &str, ch: &str, id: &str, text: &str) -> Result<(), String> {
        let svc = self.connected(acc)?;
        svc.edit(ch, id, text).await.map_err(err)?;
        if let Some(mut m) = self.store.get_message(acc, ch, id).map_err(err)? {
            m.text = text.to_string();
            m.edited_at = Some(now_ms());
            self.ingest(acc, m, false).map_err(err)?;
        }
        Ok(())
    }

    pub async fn delete(&self, acc: &str, ch: &str, id: &str) -> Result<(), String> {
        let svc = self.connected(acc)?;
        svc.delete(ch, id).await.map_err(err)?;
        self.store.delete_message(acc, ch, id).map_err(err)?;
        self.frontend.emit(UiEvent::MessageDeleted {
            account_id: acc.to_string(),
            channel_id: ch.to_string(),
            id: id.to_string(),
        });
        Ok(())
    }

    pub async fn react(&self, acc: &str, ch: &str, id: &str, emoji: &str, add: bool) -> Result<(), String> {
        let svc = self.connected(acc)?;
        svc.react(ch, id, emoji, add).await.map_err(err)?;
        let me = self.self_id(acc).unwrap_or_default();
        if let Some(m) = self.store.apply_reaction(acc, ch, id, emoji, &me, add).map_err(err)? {
            self.emit_message(m).map_err(err)?;
        }
        Ok(())
    }

    /// Unread conversations for the Home view, most important first (mentions and DMs),
    /// each with its newest unread messages. Bounded: 6 per workspace, 30 in all.
    pub fn home(&self) -> Result<Vec<HomeItem>, String> {
        let enabled: HashSet<String> = self.accounts().into_iter().filter(|a| a.enabled).map(|a| a.id).collect();
        // The notification level decides what reaches Home too: Silent conversations stay out,
        // unless they mention you and mentions override levels.
        let n = self.settings().notifications;
        let prefs = self.store.all_channel_prefs().unwrap_or_default();
        let shown = |c: &Channel| {
            let own = prefs.get(&(c.account_id.clone(), c.id.clone())).and_then(|p| p.level);
            let level = own.unwrap_or_else(|| n.default_level(&c.account_id, c.kind.is_direct()));
            level != Level::Silent || (c.mentions > 0 && n.mentions_override)
        };
        let mut chans: Vec<Channel> = self
            .store
            .unread_channels()
            .map_err(err)?
            .into_iter()
            .filter(|c| enabled.contains(&c.account_id) && shown(c))
            .collect();
        chans.sort_by_key(|c| (std::cmp::Reverse(c.mentions > 0 || c.kind.is_direct()), std::cmp::Reverse(c.last_activity)));
        let mut per_account: HashMap<String, usize> = HashMap::new();
        let mut out = Vec::new();
        for c in chans {
            let n = per_account.entry(c.account_id.clone()).or_default();
            if *n >= 6 || out.len() >= 30 {
                continue;
            }
            *n += 1;
            let me = self.self_id(&c.account_id);
            let messages = self
                .store
                .messages(&c.account_id, &c.id, None, 3)
                .map_err(err)?
                .into_iter()
                .filter(|a| a.message.ts > c.last_read && !a.message.deleted)
                .map(|a| MessageView::new(a, me.as_deref()))
                .collect();
            out.push(HomeItem { channel: self.channel_view(c).map_err(err)?, messages });
        }
        Ok(out)
    }

    /// Sends a file (pasted or dropped image, attachment) to a conversation.
    pub async fn upload(&self, acc: &str, ch: &str, name: &str, bytes: Vec<u8>, thread_root: Option<&str>) -> Result<(), String> {
        self.connected(acc)?.upload(ch, name, bytes, thread_root).await.map_err(|e| match e {
            ServiceError::Unsupported => "This account cannot receive files".to_string(),
            e => e.to_string(),
        })
    }

    /// People of an account whose handle or name contains `query`, for @mention completion.
    pub fn find_users(&self, acc: &str, query: &str, limit: u32) -> Result<Vec<User>, String> {
        self.store.find_users(acc, query, limit).map_err(err)
    }

    /// Serves a [`media_url`] path with the account's credentials.
    pub async fn fetch_media(&self, acc: &str, path: &str) -> Result<crate::service::Media, String> {
        self.connected(acc)?.fetch(path).await.map_err(err)
    }

    pub fn preview_remote(&self, acc: &str, text: &str) -> String {
        match self.service(acc) {
            Some(s) => s.to_remote_markdown(text),
            None => text.to_string(),
        }
    }

    /// Two-way favorites: the service first (so a refusal leaves both sides unchanged),
    /// then the archive. Services without favorites keep the star local.
    pub async fn set_starred(&self, acc: &str, ch: &str, starred: bool) -> Result<(), String> {
        if let Some(svc) = self.service(acc) {
            match svc.set_starred(ch, starred).await {
                Ok(()) | Err(ServiceError::Unsupported) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        self.store.set_starred(acc, ch, starred).map_err(err)?;
        self.emit_channel(acc, ch).map_err(err)
    }

    /// Moves a conversation into an Agora-only group (`None` = back to its normal section).
    pub fn set_local_group(&self, acc: &str, ch: &str, group: Option<&str>) -> Result<(), String> {
        self.store.set_local_group(acc, ch, group).map_err(err)?;
        self.emit_channel(acc, ch).map_err(err)
    }

    /// Local archive search, newest first. `account` limits it to one account.
    pub fn search(&self, query: &str, account: Option<&str>, limit: u32) -> Result<Vec<MessageView>, String> {
        let p = search::parse(query);
        if p.is_empty() {
            return Ok(vec![]);
        }
        let channels = if p.in_.is_empty() {
            None
        } else {
            let wanted: Vec<String> = p.in_.iter().map(|s| s.to_lowercase()).collect();
            let matching: Vec<(String, String)> = self
                .channels(account)?
                .into_iter()
                .filter(|c| {
                    let names = [c.display_name.to_lowercase(), c.channel.name.to_lowercase()];
                    wanted.iter().any(|w| names.iter().any(|n| n.contains(w.as_str())))
                })
                .map(|c| (c.channel.account_id, c.channel.id))
                .collect();
            if matching.is_empty() {
                return Ok(vec![]);
            }
            Some(matching)
        };
        let filter = search::SearchFilter {
            words: p.words,
            phrases: p.phrases,
            from: p.from,
            account: account.map(str::to_string),
            channels,
            after: p.after,
            before: p.before,
            limit,
        };
        let hits = self.store.search(&filter).map_err(err)?;
        Ok(self.with_reactors(
            hits.into_iter().map(|a| { let me = self.self_id(&a.message.account_id); MessageView::new(a, me.as_deref()) }).collect(),
        ))
    }

    /// Asks the services themselves (older history the archive does not have). Results are
    /// archived, so they are found locally next time. Accounts that cannot search are skipped.
    pub async fn search_remote(&self, query: &str, account: Option<&str>, limit: u32) -> Result<Vec<MessageView>, String> {
        let targets: Vec<String> = match account {
            Some(a) => vec![a.to_string()],
            None => self.accounts().into_iter().filter(|a| a.enabled).map(|a| a.id).collect(),
        };
        let mut out = Vec::new();
        let mut errors = Vec::new();
        for acc in targets {
            let Some(svc) = self.service(&acc) else { continue };
            match svc.search(query, limit).await {
                Ok(mut msgs) => {
                    msgs.iter_mut().for_each(|m| m.account_id = acc.clone());
                    self.store.upsert_messages(&msgs).map_err(err)?;
                    self.fill_users(&acc, &svc, &msgs).await;
                    for m in msgs {
                        out.push(self.message_view(m).map_err(err)?);
                    }
                }
                Err(ServiceError::Unsupported) => {}
                Err(e) => errors.push(e.to_string()),
            }
        }
        if out.is_empty() && !errors.is_empty() {
            return Err(errors.join("; "));
        }
        out.sort_by(|a, b| b.message.ts.cmp(&a.message.ts));
        Ok(out)
    }

    /// The signed-in user of an account, for the status bar.
    pub fn me(&self, acc: &str) -> Result<Option<SelfView>, String> {
        let Some(id) = self.self_id(acc) else { return Ok(None) };
        let Some(user) = self.store.get_user(acc, &id).map_err(err)? else { return Ok(None) };
        let presence = lock(&self.presence).get(&(acc.to_string(), id)).copied().unwrap_or_default();
        Ok(Some(SelfView { user, presence }))
    }

    /// A person's profile from the service, falling back to what the archive knows. The
    /// basic part (names, picture) is saved; the rest is shown and forgotten.
    pub async fn profile(&self, acc: &str, user_id: &str) -> Result<ProfileView, String> {
        let stored = self.store.get_user(acc, user_id).map_err(err)?;
        let fetched = match self.service(acc) {
            Some(svc) => match svc.profile(user_id).await {
                Ok(p) => Some(p),
                Err(ServiceError::Unsupported) => None,
                Err(e) if stored.is_some() => {
                    tracing::warn!("profile of {user_id} on {acc}: {e}");
                    None
                }
                Err(e) => return Err(e.to_string()),
            },
            None => None,
        };
        let mut profile = match fetched {
            Some(p) => {
                self.store.upsert_users(&[p.to_user(acc)]).map_err(err)?;
                p
            }
            None => Profile::from_user(&stored.ok_or("This person is not known to Agora yet")?),
        };
        let key = (acc.to_string(), user_id.to_string());
        if profile.presence == Presence::Unknown {
            profile.presence = lock(&self.presence).get(&key).copied().unwrap_or_default();
        } else {
            lock(&self.presence).insert(key, profile.presence);
        }
        let is_self = self.self_id(acc).as_deref() == Some(user_id);
        Ok(ProfileView { account_id: acc.to_string(), profile, is_self })
    }

    /// One page of a conversation's members (names saved to the archive so mentions resolve).
    /// Services without a members call use the list the conversation came with.
    pub async fn members(&self, acc: &str, ch: &str, cursor: Option<&str>) -> Result<MembersView, String> {
        let page = match self.service(acc) {
            Some(svc) => match svc.members(ch, cursor).await {
                Ok(p) => Some(p),
                Err(ServiceError::Unsupported) => None,
                Err(e) => return Err(e.to_string()),
            },
            None => None,
        };
        let page = match page {
            Some(p) => {
                self.store.upsert_users(&p.users).map_err(err)?;
                let mut presence = lock(&self.presence);
                for (id, pr) in &p.presence {
                    presence.insert((acc.to_string(), id.clone()), *pr);
                }
                p
            }
            None => {
                let c = self.store.get_channel(acc, ch).map_err(err)?.ok_or("Unknown conversation")?;
                let mut users = Vec::with_capacity(c.members.len());
                for id in &c.members {
                    users.push(self.store.get_user(acc, id).map_err(err)?.unwrap_or_else(|| User {
                        account_id: acc.to_string(),
                        id: id.clone(),
                        name: id.clone(),
                        display_name: String::new(),
                        avatar_url: None,
                        is_bot: false,
                    }));
                }
                MemberPage { total: Some(users.len() as u64), users, ..MemberPage::default() }
            }
        };
        let presence = lock(&self.presence);
        let members = page
            .users
            .into_iter()
            .map(|user| {
                let p = presence.get(&(acc.to_string(), user.id.clone())).copied().unwrap_or_default();
                MemberView { user, presence: p }
            })
            .collect();
        Ok(MembersView { members, next: page.next, total: page.total })
    }

    pub async fn set_presence(&self, acc: &str, presence: Presence) -> Result<(), String> {
        self.connected(acc)?.set_presence(presence).await.map_err(err)?;
        let me = self.self_id(acc).unwrap_or_default();
        lock(&self.presence).insert((acc.to_string(), me.clone()), presence);
        self.frontend.emit(UiEvent::Presence { account_id: acc.to_string(), user_id: me, presence });
        Ok(())
    }

    /// Fetches the workspace logo now (after the user asked for the service logo back).
    pub async fn refresh_icon(&self, acc: &str) -> Result<(), String> {
        let svc = self.connected(acc)?;
        self.fetch_icon(acc, &svc).await;
        Ok(())
    }

    /// Stores the service's logo as the account icon when the user has not chosen one.
    async fn fetch_icon(&self, acc: &str, svc: &Arc<dyn Service>) {
        match svc.workspace_icon().await {
            Ok(Some(media)) if media.mime.starts_with("image/") && media.bytes.len() <= MAX_ICON_BYTES => {
                let url = format!("data:{};base64,{}", media.mime, text::base64(&media.bytes));
                if self.store.set_auto_icon(acc, &url).unwrap_or(false) {
                    self.frontend.emit(UiEvent::Accounts);
                }
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(account = %acc, "no workspace logo: {e}"),
        }
    }

    pub fn channel_prefs(&self, acc: &str, ch: &str) -> Result<ChannelPrefs, String> {
        self.store.channel_prefs(acc, ch).map_err(err)
    }

    pub fn set_channel_prefs(&self, acc: &str, ch: &str, prefs: &ChannelPrefs) -> Result<(), String> {
        self.store.set_channel_prefs(acc, ch, prefs).map_err(err)?;
        self.recompute_tray();
        // The level may have muted or unmuted it in the sidebar.
        self.emit_channel(acc, ch).map_err(err)
    }

    pub fn save_settings(&self, s: Settings) -> Result<(), String> {
        self.store.save_settings(&s).map_err(err)?;
        let old = self.settings().notifications;
        let n = &s.notifications;
        let levels_changed = old.dm_level != n.dm_level || old.channel_level != n.channel_level || old.accounts != n.accounts;
        let groups: Vec<String> = s.groups.iter().map(|g| g.id.clone()).collect();
        let regrouped = self.store.clear_missing_groups(&groups).map_err(err)? > 0;
        *self.settings.write().unwrap_or_else(|e| e.into_inner()) = s;
        // Sidebar rows show who is muted, so new levels (or dissolved groups) reload them.
        if levels_changed || regrouped {
            for a in self.accounts() {
                self.frontend.emit(UiEvent::ChannelsReloaded { account_id: a.id });
            }
        }
        self.recompute_tray();
        Ok(())
    }

    pub fn add_account(self: &Arc<Self>, account: Account) -> Result<(), String> {
        self.store.upsert_account(&account).map_err(err)?;
        self.frontend.emit(UiEvent::Accounts);
        if account.enabled {
            self.start_account(account);
        }
        Ok(())
    }

    /// Name, icon, colour or enabled flag changed.
    pub fn update_account(self: &Arc<Self>, account: Account) -> Result<(), String> {
        let was_enabled = self.store.get_account(&account.id).map_err(err)?.map(|a| a.enabled);
        self.store.upsert_account(&account).map_err(err)?;
        match (was_enabled, account.enabled) {
            (Some(false), true) => self.start_account(account),
            (Some(true), false) => self.stop_account(&account.id),
            _ => {}
        }
        self.recompute_tray();
        self.frontend.emit(UiEvent::Accounts);
        Ok(())
    }

    /// Restarts a signed-out account after its credentials were refreshed.
    pub fn restart_account(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let account = self.store.get_account(id).map_err(err)?.ok_or("unknown account")?;
        self.start_account(account);
        Ok(())
    }

    pub fn reorder_accounts(&self, ids: &[String]) -> Result<(), String> {
        self.store.set_account_positions(ids).map_err(err)?;
        self.push_tray();
        self.frontend.emit(UiEvent::Accounts);
        Ok(())
    }

    pub fn remove_account(&self, id: &str, purge: bool) -> Result<(), String> {
        self.stop_account(id);
        self.store.delete_account(id, purge).map_err(err)?;
        secrets::delete(&secrets::account_key(id))?;
        lock(&self.dots).retain(|(acc, _), _| acc != id);
        self.push_tray();
        self.frontend.emit(UiEvent::Accounts);
        Ok(())
    }
}
