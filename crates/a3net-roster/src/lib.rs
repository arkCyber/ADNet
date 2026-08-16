//! `a3net-roster` — local contact directory (people, agents, IoT devices),
//! contact groups, friend-request settings, and 12-digit Exodus ID ↔ NodeId
//! mappings.
//!
//! ## Provenance
//!
//! Originally implemented in
//! `Exodus@src-backup/src-tauri/src/microservice/contact_directory_service.rs`
//! as an in-process Tauri microservice with a JSON snapshot at
//! `{app_data}/contact_directory/directory.json`. This crate is the
//! A3Net-friendly port: it exposes a [`RosterStore`] trait with two
//! reference implementations:
//!
//! - [`mem::InMemoryRosterStore`] — `HashMap`-backed, useful for tests.
//! - [`sqlite::SqliteRosterStore`] — `rusqlite`-bundled SQLite, the
//!   cross-restart persistence backend.
//!
//! The 12-digit "Exodus ID" helper
//! [`digit::stable_digit_from_node`] is a direct port of the original
//! blake3-based fold.
//!
//! ## Data model
//!
//! - [`Contact`] — single entry. Same struct serves humans, AI agents and
//!   IoT devices (`contact_type` discriminates); IoT-specific fields are
//!   kept as `Option<String>` for forward compatibility.
//! - [`ContactGroup`] — named, color-tagged grouping.
//! - [`FriendRequestSetting`] — per-user accept mode (`auto_accept` /
//!   `require_confirmation`).
//! - [`DigitMapping`] — bidirectional `12_digit_id <-> node_id` record.
//!
//! ## What is NOT here
//!
//! Conversation / message tables live in `a3net-chatstore`. User accounts
//! (`username`, `display_name`, `last_seen`, 12-digit ID generation) live
//! in `a3net-userstore`. The roster deliberately keeps just the
//! address-book slice — search, favorites, blocking, and grouping.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod digit;
pub mod error;
pub mod group;
pub mod mapping;
pub mod mem;
pub mod model;
pub mod settings;
pub mod sqlite;
pub mod store;

pub use digit::{MAX_DIGIT_LEN, MIN_DIGIT_LEN, stable_digit_from_node, validate_digit_id};
pub use error::{RosterError, RosterResult};
pub use group::{ContactGroup, GroupColor, MAX_GROUP_COLOR_LEN};
pub use mapping::DigitMapping;
pub use mem::InMemoryRosterStore;
pub use model::{
    AgentDeploymentType, Contact, ContactType, IoTCapability, IoTDeviceType, IoTEvent,
    IoTProtocol, IoTStatus, MAX_CONTACT_NAME_LEN, MAX_GROUPS_PER_CONTACT,
    MAX_TAGS_PER_CONTACT,
};
pub use settings::{FriendRequestMode, FriendRequestSetting};
pub use sqlite::{SCHEMA_VERSION, SqliteRosterStore, SqliteRosterStoreConfig};
pub use store::{RosterStore, RosterStoreInfo};