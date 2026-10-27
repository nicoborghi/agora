//! The common data model every service is converted into.

use serde::{Deserialize, Serialize};

pub type AccountId = String;

/// Milliseconds since the Unix epoch (UTC). Every service timestamp is normalised to this.
pub type Millis = i64;

/// Marker placed in [`Message::mentions`] for @channel / @here / @all.
pub const MENTION_ALL: &str = "!all";

/// Origin of the app's `agora` URI scheme (WebView2 serves custom schemes as http://<scheme>.localhost).
pub const MEDIA_ORIGIN: &str = "http://agora.localhost";

/// URL the UI can load for a service path that needs the account's credentials.
/// The app serves it through [`crate::service::Service::fetch`].
pub fn media_url(account_id: &str, path: &str) -> String {
    format!("{MEDIA_ORIGIN}/{account_id}{path}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceKind {
    Slack,
    Mattermost,
    RocketChat,
    Demo,
}

impl ServiceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Mattermost => "mattermost",
            Self::RocketChat => "rocketchat",
            Self::Demo => "demo",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "slack" => Self::Slack,
            "mattermost" => Self::Mattermost,
            "rocketchat" => Self::RocketChat,
            "demo" => Self::Demo,
            _ => return None,
        })
    }

    pub fn default_color(self) -> &'static str {
        match self {
            Self::Slack => "#7a4fc4",
            Self::Mattermost => "#2f6fd6",
            Self::RocketChat => "#d23c41",
            Self::Demo => "#3b8f5a",
        }
    }
}

/// One connected workspace / server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub kind: ServiceKind,
    /// Workspace or server label shown in the UI.
    pub name: String,
    pub server_url: String,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub self_user_id: Option<String>,
    /// One or two characters drawn in the rail; `None` = initials of `name`.
    #[serde(default)]
    pub icon: Option<String>,
    /// Logo as a data URL. `None` = fetch the workspace logo from the service;
    /// `Some("")` = the user chose initials.
    #[serde(default)]
    pub icon_image: Option<String>,
    pub color: String,
    #[serde(default)]
    pub position: i64,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

impl Account {
    pub fn new(kind: ServiceKind, name: &str, server_url: &str, position: i64) -> Self {
        Self {
            id: format!("{}-{:016x}", kind.as_str(), fastrand::u64(..)),
            kind,
            name: name.to_string(),
            server_url: server_url.to_string(),
            team_id: None,
            self_user_id: None,
            icon: None,
            icon_image: None,
            color: kind.default_color().to_string(),
            position,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    Public,
    Private,
    /// One-to-one direct message.
    Dm,
    /// Group direct message (2+ other people).
    Group,
}

impl ChannelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Dm => "dm",
            Self::Group => "group",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "private" => Self::Private,
            "dm" => Self::Dm,
            "group" => Self::Group,
            _ => Self::Public,
        }
    }

    pub fn is_direct(self) -> bool {
        matches!(self, Self::Dm | Self::Group)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub account_id: AccountId,
    pub id: String,
    pub kind: ChannelKind,
    pub name: String,
    #[serde(default)]
    pub topic: String,
    /// The other participants (DMs and groups only).
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub last_activity: Millis,
    #[serde(default)]
    pub last_read: Millis,
    #[serde(default)]
    pub unread: u32,
    #[serde(default)]
    pub mentions: u32,
    /// Favorite. Synced both ways when the service has favorites (see `starred_synced`).
    #[serde(default)]
    pub starred: bool,
    /// The service reports favorites, so its `starred` replaces the archived one.
    /// When false (demo, services without favorites) the star stays local.
    #[serde(default, skip_serializing)]
    pub starred_synced: bool,
    /// Service-side section, e.g. the Mattermost team when the account spans several. Empty = none.
    #[serde(default)]
    pub group: String,
    /// The user's own sidebar category on the service (Mattermost custom category). Empty = none.
    #[serde(default)]
    pub category: String,
    /// Agora-only group id (see `Settings::groups`); never sent to the service.
    #[serde(default)]
    pub local_group: Option<String>,
}

impl Channel {
    pub fn new(account_id: &str, id: &str, kind: ChannelKind, name: &str) -> Self {
        Self {
            account_id: account_id.to_string(),
            id: id.to_string(),
            kind,
            name: name.to_string(),
            topic: String::new(),
            members: vec![],
            last_activity: 0,
            last_read: 0,
            unread: 0,
            mentions: 0,
            starred: false,
            starred_synced: false,
            group: String::new(),
            category: String::new(),
            local_group: None,
        }
    }
}

/// Favorite flag and service category of one conversation, as reported by the service.
/// `None` leaves that part unchanged.
#[derive(Debug, Clone)]
pub struct ChannelSection {
    pub channel_id: String,
    pub starred: Option<bool>,
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub account_id: AccountId,
    pub id: String,
    /// Handle (`@name`).
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub is_bot: bool,
}

/// Everything a service tells about a person, fetched when their profile is opened and not
/// kept: empty fields are ones the service does not have or does not show you.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    /// Handle (`@name`).
    pub name: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    /// Job title or position.
    pub title: String,
    pub email: String,
    pub phone: String,
    /// IANA time zone ("Europe/Rome"), when the service gives one.
    pub timezone: String,
    /// Minutes east of UTC, when the service gives an offset rather than (or with) a zone.
    pub utc_offset_min: Option<i32>,
    /// Custom status ("In a meeting") and its emoji shortcode.
    pub status_text: String,
    pub status_emoji: String,
    pub presence: Presence,
    pub is_bot: bool,
    /// Admin, owner, guest... as the service names them.
    pub roles: Vec<String>,
    /// Deactivated or deleted on the service.
    pub inactive: bool,
    /// Anything else the service exposes (bio, department, custom profile fields), as label/value.
    pub extra: Vec<(String, String)>,
}

impl Profile {
    /// What the archive already knows, for services that cannot say more.
    pub fn from_user(u: &User) -> Self {
        Self {
            id: u.id.clone(),
            name: u.name.clone(),
            display_name: u.display_name.clone(),
            avatar_url: u.avatar_url.clone(),
            is_bot: u.is_bot,
            ..Self::default()
        }
    }

    pub fn to_user(&self, account_id: &str) -> User {
        User {
            account_id: account_id.to_string(),
            id: self.id.clone(),
            name: self.name.clone(),
            display_name: self.display_name.clone(),
            avatar_url: self.avatar_url.clone(),
            is_bot: self.is_bot,
        }
    }
}

/// One page of a conversation's members. `next` continues the list; `total` is the member
/// count when the service reports it.
#[derive(Debug, Clone, Default)]
pub struct MemberPage {
    pub users: Vec<User>,
    /// Presence the service returned along with the list, by user id.
    pub presence: Vec<(String, Presence)>,
    pub next: Option<String>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    /// Shortcode without colons, e.g. `thumbsup` or a custom emoji name.
    pub emoji: String,
    pub users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub size: u64,
    pub url: String,
    #[serde(default)]
    pub thumb_url: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkPreview {
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub image_url: Option<String>,
}

/// The unified message. `text` is always standard Markdown, whatever the source service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub account_id: AccountId,
    pub channel_id: String,
    pub id: String,
    pub author_id: String,
    pub ts: Millis,
    pub text: String,
    #[serde(default)]
    pub thread_root: Option<String>,
    #[serde(default)]
    pub reply_count: u32,
    #[serde(default)]
    pub edited_at: Option<Millis>,
    #[serde(default)]
    pub deleted: bool,
    /// User ids mentioned in the message, or [`MENTION_ALL`].
    #[serde(default)]
    pub mentions: Vec<String>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub links: Vec<LinkPreview>,
}

impl Message {
    pub fn apply_reaction(&mut self, emoji: &str, user: &str, add: bool) {
        let pos = self.reactions.iter().position(|r| r.emoji == emoji);
        match (pos, add) {
            (Some(i), true) => {
                if !self.reactions[i].users.iter().any(|u| u == user) {
                    self.reactions[i].users.push(user.to_string());
                }
            }
            (None, true) => self.reactions.push(Reaction {
                emoji: emoji.to_string(),
                users: vec![user.to_string()],
            }),
            (Some(i), false) => {
                self.reactions[i].users.retain(|u| u != user);
                if self.reactions[i].users.is_empty() {
                    self.reactions.remove(i);
                }
            }
            (None, false) => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Presence {
    Online,
    Away,
    Dnd,
    Offline,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnState {
    Connecting,
    Online,
    Offline { retry_in: u64 },
    /// Token expired or revoked: the account stays grey until the user signs in again.
    AuthExpired { reason: String },
}

/// Everything a connector reports. Sent through [`crate::service::EventSink`].
#[derive(Debug, Clone)]
pub enum ServiceEvent {
    Connection(ConnState),
    /// New or edited message (the engine tells the two apart).
    MessageUpsert(Message),
    MessageDeleted { channel_id: String, id: String },
    Reaction { channel_id: String, message_id: String, emoji: String, user_id: String, added: bool },
    ChannelUpsert(Channel),
    /// The conversation was read elsewhere (phone, web): clear the local unread state.
    ChannelRead { channel_id: String, ts: Millis },
    /// Favorites or service categories changed (on another device, or confirming ours).
    Sections(Vec<ChannelSection>),
    UserUpsert(User),
    Typing { channel_id: String, user_id: String, thread_root: Option<String> },
    Presence { user_id: String, presence: Presence },
}

/// What a connector returns right after connecting.
#[derive(Debug, Clone)]
pub struct Bootstrap {
    pub self_user: User,
    pub users: Vec<User>,
    pub channels: Vec<Channel>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg() -> Message {
        Message {
            account_id: "a".into(),
            channel_id: "c".into(),
            id: "1".into(),
            author_id: "u".into(),
            ts: 0,
            text: String::new(),
            thread_root: None,
            reply_count: 0,
            edited_at: None,
            deleted: false,
            mentions: vec![],
            reactions: vec![],
            attachments: vec![],
            links: vec![],
        }
    }

    #[test]
    fn reactions_add_and_remove() {
        let mut m = msg();
        m.apply_reaction("tada", "u1", true);
        m.apply_reaction("tada", "u1", true);
        m.apply_reaction("tada", "u2", true);
        assert_eq!(m.reactions[0].users, vec!["u1", "u2"]);
        m.apply_reaction("tada", "u1", false);
        m.apply_reaction("tada", "u2", false);
        assert!(m.reactions.is_empty());
    }
}
