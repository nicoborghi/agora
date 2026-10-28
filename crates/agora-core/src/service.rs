//! The contract every service connector implements.

use crate::model::*;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// Token expired or revoked. The engine stops retrying and greys the account out.
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("rate limited, retry in {0:?}")]
    RateLimited(Duration),
    #[error("network: {0}")]
    Network(String),
    #[error("not supported by this service")]
    Unsupported,
    #[error("{0}")]
    Other(String),
}

pub type SResult<T> = Result<T, ServiceError>;

/// Where a connector pushes realtime events. Bounded, so a flood of events
/// applies back-pressure to the socket instead of growing memory.
#[derive(Clone)]
pub struct EventSink {
    account_id: AccountId,
    tx: mpsc::Sender<(AccountId, ServiceEvent)>,
}

impl EventSink {
    pub fn new(account_id: AccountId, tx: mpsc::Sender<(AccountId, ServiceEvent)>) -> Self {
        Self { account_id, tx }
    }

    /// Returns `false` once the engine is gone; the connector should then return.
    pub async fn send(&self, event: ServiceEvent) -> bool {
        self.tx.send((self.account_id.clone(), event)).await.is_ok()
    }
}

/// History window. Results are always returned oldest first.
#[derive(Debug, Clone, Default)]
pub struct HistoryQuery {
    /// Only messages strictly newer than this.
    pub after: Option<Millis>,
    /// Only messages strictly older than this.
    pub before: Option<Millis>,
    /// Id of the message at `before`, for services that page by id (Mattermost).
    pub before_id: Option<String>,
    pub limit: u32,
}

/// A file or image fetched with the account's credentials.
pub struct Media {
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[async_trait]
pub trait Service: Send + Sync {
    /// Self user, users and conversations (with unread counts when the service has them).
    async fn bootstrap(&self) -> SResult<Bootstrap>;

    /// Opens the realtime socket and pushes events until it drops. Must send
    /// `ServiceEvent::Connection(ConnState::Online)` once the handshake succeeds.
    async fn run(&self, sink: EventSink) -> SResult<()>;

    async fn history(&self, channel_id: &str, query: HistoryQuery) -> SResult<Vec<Message>>;

    /// Root message and all replies, oldest first.
    async fn thread(&self, channel_id: &str, root_id: &str) -> SResult<Vec<Message>>;

    /// `text` is standard Markdown; the connector converts it for its service.
    async fn send(&self, channel_id: &str, text: &str, thread_root: Option<&str>) -> SResult<Message>;

    async fn edit(&self, channel_id: &str, message_id: &str, text: &str) -> SResult<()>;

    async fn delete(&self, channel_id: &str, message_id: &str) -> SResult<()>;

    async fn react(&self, channel_id: &str, message_id: &str, emoji: &str, add: bool) -> SResult<()>;

    async fn mark_read(&self, channel_id: &str, latest: Option<&Message>) -> SResult<()>;

    async fn upload(
        &self,
        _channel_id: &str,
        _file_name: &str,
        _bytes: Vec<u8>,
        _thread_root: Option<&str>,
    ) -> SResult<()> {
        Err(ServiceError::Unsupported)
    }

    /// Adds or removes a favorite on the service. `Unsupported` keeps the star local.
    async fn set_starred(&self, _channel_id: &str, _starred: bool) -> SResult<()> {
        Err(ServiceError::Unsupported)
    }

    /// Server-side message search with the service's own syntax (`from:`, `in:`, `on:`...).
    /// Newest first.
    async fn search(&self, _query: &str, _limit: u32) -> SResult<Vec<Message>> {
        Err(ServiceError::Unsupported)
    }

    /// Sets the signed-in user's status.
    async fn set_presence(&self, _presence: Presence) -> SResult<()> {
        Err(ServiceError::Unsupported)
    }

    /// The workspace's own logo (team icon, server favicon), if it has one.
    async fn workspace_icon(&self) -> SResult<Option<Media>> {
        Ok(None)
    }

    /// Profiles for user ids found in history that the archive does not know yet.
    async fn users(&self, _ids: &[String]) -> SResult<Vec<User>> {
        Ok(vec![])
    }

    /// A person's full profile (title, contact, time zone, custom status...). `Unsupported`
    /// makes the engine show what the archive knows.
    async fn profile(&self, _user_id: &str) -> SResult<Profile> {
        Err(ServiceError::Unsupported)
    }

    /// Members of a conversation, a page at a time (`cursor` from the previous page's `next`).
    /// `Unsupported` makes the engine use the member list the conversation came with.
    async fn members(&self, _channel_id: &str, _cursor: Option<&str>) -> SResult<MemberPage> {
        Err(ServiceError::Unsupported)
    }

    /// A conversation's description, creator, bookmarks and pinned messages, for its Details
    /// panel. `Unsupported` shows only what the archive knows.
    async fn channel_details(&self, _channel_id: &str) -> SResult<ChannelDetails> {
        Err(ServiceError::Unsupported)
    }

    /// One canvas (`ChannelDetails::canvases`, or a canvas link in a message).
    async fn canvas(&self, _id: &str) -> SResult<Canvas> {
        Err(ServiceError::Unsupported)
    }

    /// Downloads a service path produced by [`crate::model::media_url`] (files, thumbnails,
    /// avatars), so the UI can show content that needs authentication.
    async fn fetch(&self, _path: &str) -> SResult<Media> {
        Err(ServiceError::Unsupported)
    }

    /// What the other side receives for `md` (used by "preview as others see it").
    fn to_remote_markdown(&self, md: &str) -> String {
        md.to_string()
    }
}

/// Builds connectors from stored accounts. Implemented by the app, which is the
/// only place that knows every connector crate.
pub trait ServiceFactory: Send + Sync {
    fn create(&self, account: &Account) -> Result<Arc<dyn Service>, String>;
}
