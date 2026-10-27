//! Agora core: the common data model, the local archive, secrets, notification
//! rules and the engine that supervises one isolated connector per account.
//!
//! Service connectors (Slack, Mattermost, Rocket.Chat) live in their own crates
//! and only implement [`service::Service`]; they convert their payloads into
//! [`model`] types and never touch storage or UI directly.

pub mod backoff;
pub mod emphasis;
pub mod engine;
pub mod model;
pub mod notify;
pub mod search;
pub mod secrets;
pub mod service;
pub mod settings;
pub mod store;
pub mod sync;
pub mod text;

#[cfg(feature = "demo")]
pub mod demo;
