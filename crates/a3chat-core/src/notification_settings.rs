//! Notification settings types shared between core and app layers.
//!
//! These types are defined in `a3chat-core` because they are referenced
//! by the event system in `a3chat_core::event`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Global Do Not Disturb mode with optional time window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DndSettings {
    pub enabled: bool,
    pub quiet_from: Option<DateTime<Utc>>,
    pub quiet_until: Option<DateTime<Utc>>,
    pub allow_calls: bool,
    pub allow_pinned: bool,
}

impl Default for DndSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            quiet_from: None,
            quiet_until: None,
            allow_calls: true,
            allow_pinned: true,
        }
    }
}

impl DndSettings {
    pub fn is_active(&self) -> bool {
        if !self.enabled {
            return false;
        }
        let now = Utc::now();
        if let (Some(from), Some(until)) = (self.quiet_from, self.quiet_until) {
            now >= from && now <= until
        } else {
            true
        }
    }
}
