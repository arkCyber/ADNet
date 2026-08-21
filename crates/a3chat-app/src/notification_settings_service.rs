//! Notification settings / Do Not Disturb service.
//!
//! Handles per-conversation and global notification preferences.
//!
//! DO-178C §6.4.4: Notification settings are local preferences.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use a3chat_core::error::A3chatError;
use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::notification_settings::DndSettings;

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;

/// Per-conversation notification level.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    #[default]
    All,
    MentionsOnly,
    None,
}

impl NotificationLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            NotificationLevel::All => "all",
            NotificationLevel::MentionsOnly => "mentions_only",
            NotificationLevel::None => "none",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "all" => Some(NotificationLevel::All),
            "mentions_only" => Some(NotificationLevel::MentionsOnly),
            "none" => Some(NotificationLevel::None),
            _ => None,
        }
    }
}

/// Notification settings for a single conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConversationNotificationSettings {
    pub conversation_id: ConversationId,
    pub muted: bool,
    pub level: NotificationLevel,
    pub custom_sound: Option<String>,
    pub show_preview: bool,
}

impl ConversationNotificationSettings {
    /// Validate the per-conversation settings.
    pub fn validate(&self) -> Result<(), AppError> {
        if self.conversation_id.as_str().is_empty() {
            return Err(AppError::Domain(
                "conversation_id must be non-empty".into(),
            ));
        }
        a3chat_core::id::validate_id("conversation_id", self.conversation_id.as_str())
            .map_err(|e| AppError::Domain(e.to_string()))?;
        if let Some(sound) = &self.custom_sound {
            if sound.len() > 256 {
                return Err(AppError::Domain(
                    "custom_sound must be <= 256 chars".into(),
                ));
            }
        }
        Ok(())
    }
}

impl Default for ConversationNotificationSettings {
    fn default() -> Self {
        Self {
            conversation_id: ConversationId::from(""),
            muted: false,
            level: NotificationLevel::All,
            custom_sound: None,
            show_preview: true,
        }
    }
}

/// The notification settings service.
///
/// # Per-user scoping
/// All DND + per-conversation state is keyed by `UserId`. A
/// multi-user daemon (e.g. a shared server) won't have Alice's
/// mute list leak into Bob's view.
#[derive(Clone, Debug)]
pub struct NotificationSettingsService {
    bus: NotificationBus,
    per_user: Arc<RwLock<HashMap<UserId, UserState>>>,
}

#[derive(Debug, Default, Clone)]
struct UserState {
    dnd: DndSettings,
    per_conversation: HashMap<String, ConversationNotificationSettings>,
}

impl Default for NotificationSettingsService {
    fn default() -> Self {
        Self::new(NotificationBus::default())
    }
}

impl NotificationSettingsService {
    #[must_use = "constructing a notification settings service without using it is a bug"]
    pub fn new(bus: NotificationBus) -> Self {
        Self {
            bus,
            per_user: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Set global DND settings. Returns `Err` if the quiet window
    /// is inverted (`quiet_from > quiet_until`).
    pub fn set_dnd(&self, owner: &UserId, settings: DndSettings) -> AppResult<()> {
        if let (Some(from), Some(until)) = (settings.quiet_from, settings.quiet_until) {
            if until < from {
                return Err(AppError::Domain(
                    "DND quiet_until must be >= quiet_from".into(),
                ));
            }
        }
        let mut guard = self.per_user.write();
        let state = guard.entry(owner.clone()).or_default();
        state.dnd = settings.clone();
        drop(guard);

        self.bus
            .publish(a3chat_core::event::A3chatEvent::NotificationSettingsChanged {
                user_id: owner.clone(),
                conversation_id: None,
                global_dnd: Some(settings),
            });
        Ok(())
    }

    /// Get global DND settings for a user (defaults to off).
    pub fn get_dnd(&self, owner: &UserId) -> DndSettings {
        let guard = self.per_user.read();
        guard.get(owner).map(|s| s.dnd.clone()).unwrap_or_default()
    }

    /// Set per-conversation notification settings.
    pub fn set_conversation(
        &self,
        owner: &UserId,
        settings: ConversationNotificationSettings,
    ) -> AppResult<()> {
        settings.validate()?;
        let conv_id = settings.conversation_id.clone();
        let key = conv_id.as_str().to_string();
        let mut guard = self.per_user.write();
        let state = guard.entry(owner.clone()).or_default();
        state.per_conversation.insert(key, settings);
        drop(guard);
        self.bus
            .publish(a3chat_core::event::A3chatEvent::NotificationSettingsChanged {
                user_id: owner.clone(),
                conversation_id: Some(conv_id),
                global_dnd: None,
            });
        Ok(())
    }

    /// Get per-conversation notification settings, defaulting to
    /// caller-provided fallback if none have been stored.
    pub fn get_conversation(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
    ) -> ConversationNotificationSettings {
        let guard = self.per_user.read();
        guard
            .get(owner)
            .and_then(|s| s.per_conversation.get(conversation_id.as_str()).cloned())
            .unwrap_or_else(|| ConversationNotificationSettings {
                conversation_id: conversation_id.clone(),
                ..ConversationNotificationSettings::default()
            })
    }

    /// Mute a conversation.
    pub fn mute(&self, owner: &UserId, conversation_id: &ConversationId) -> AppResult<()> {
        if conversation_id.as_str().is_empty() {
            return Err(AppError::Domain("conversation_id must be non-empty".into()));
        }
        let mut guard = self.per_user.write();
        let state = guard.entry(owner.clone()).or_default();
        let entry = state
            .per_conversation
            .entry(conversation_id.as_str().to_string())
            .or_insert_with(|| ConversationNotificationSettings {
                conversation_id: conversation_id.clone(),
                ..ConversationNotificationSettings::default()
            });
        entry.muted = true;
        drop(guard);
        self.bus
            .publish(a3chat_core::event::A3chatEvent::NotificationSettingsChanged {
                user_id: owner.clone(),
                conversation_id: Some(conversation_id.clone()),
                global_dnd: None,
            });
        Ok(())
    }

    /// Unmute a conversation.
    pub fn unmute(&self, owner: &UserId, conversation_id: &ConversationId) -> AppResult<()> {
        if conversation_id.as_str().is_empty() {
            return Err(AppError::Domain("conversation_id must be non-empty".into()));
        }
        let mut guard = self.per_user.write();
        if let Some(state) = guard.get_mut(owner) {
            if let Some(entry) = state
                .per_conversation
                .get_mut(conversation_id.as_str())
            {
                entry.muted = false;
            }
        }
        drop(guard);
        self.bus
            .publish(a3chat_core::event::A3chatEvent::NotificationSettingsChanged {
                user_id: owner.clone(),
                conversation_id: Some(conversation_id.clone()),
                global_dnd: None,
            });
        Ok(())
    }

    /// List every conversation that is currently muted for a user.
    pub fn list_muted(&self, owner: &UserId) -> Vec<ConversationId> {
        let guard = self.per_user.read();
        guard
            .get(owner)
            .map(|s| {
                s.per_conversation
                    .values()
                    .filter(|c| c.muted)
                    .map(|c| c.conversation_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Check if a notification should be suppressed.
    ///
    /// Decision pipeline (whichever short-circuits first):
    /// 1. Per-conversation `level = None`  -> false
    /// 2. Per-conversation `muted`        -> false
    /// 3. Per-conversation `level = MentionsOnly` AND `is_mentioned = false` -> false
    /// 4. DND enabled + not (pinned && allow_pinned) -> false
    /// 5. Otherwise -> true.
    pub fn should_notify(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        is_mentioned: bool,
        is_pinned: bool,
    ) -> bool {
        let conv = self.get_conversation(owner, conversation_id);
        if conv.muted || matches!(conv.level, NotificationLevel::None) {
            return false;
        }
        if matches!(conv.level, NotificationLevel::MentionsOnly) && !is_mentioned {
            return false;
        }
        let dnd = self.get_dnd(owner);
        if dnd.is_active() {
            if dnd.allow_pinned && is_pinned {
                return true;
            }
            return false;
        }
        true
    }
}

/// Dispatcher entry point used by `a3chat-app::app::A3chatApp::dispatch`.
pub async fn dispatch(
    svc: Arc<NotificationSettingsService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        "a3chat.chat.notification.set_dnd" => {
            let settings: DndSettings = serde_json::from_value(params).map_err(|e| {
                A3chatError::InvalidInput(format!("invalid DndSettings: {e}"))
            })?;
            svc.set_dnd(owner, settings).map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "a3chat.chat.notification.get_dnd" => {
            let dnd = svc.get_dnd(owner);
            serde_json::to_value(dnd).map_err(A3chatError::from)
        }
        "a3chat.chat.notification.set_conversation" => {
            let settings: ConversationNotificationSettings = serde_json::from_value(params)
                .map_err(|e| {
                    A3chatError::InvalidInput(format!(
                        "invalid ConversationNotificationSettings: {e}"
                    ))
                })?;
            svc.set_conversation(owner, settings).map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "a3chat.chat.notification.get_conversation" => {
            let conv_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let settings = svc.get_conversation(owner, &conv_id);
            serde_json::to_value(settings).map_err(A3chatError::from)
        }
        "a3chat.chat.notification.mute" => {
            let conv_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.mute(owner, &conv_id).map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "a3chat.chat.notification.unmute" => {
            let conv_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.unmute(owner, &conv_id).map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "a3chat.chat.notification.list_muted" => {
            let list = svc.list_muted(owner);
            serde_json::to_value(list).map_err(A3chatError::from)
        }
        m => Err(A3chatError::Internal(format!(
            "NotificationSettingsService does not handle {m}"
        ))),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::event::A3chatEvent;
    use a3chat_core::id::{ConversationId, UserId};
    use crate::error::AppError;
    use crate::notification_bus::NotificationBus;

    fn alice() -> UserId {
        UserId::from("alice")
    }

    #[test]
    fn dnd_disabled_is_not_active() {
        let dnd = DndSettings {
            enabled: false,
            quiet_from: None,
            quiet_until: None,
            allow_calls: true,
            allow_pinned: true,
        };
        assert!(!dnd.is_active());
    }

    #[test]
    fn dnd_enabled_no_window_is_active() {
        let dnd = DndSettings {
            enabled: true,
            quiet_from: None,
            quiet_until: None,
            allow_calls: true,
            allow_pinned: true,
        };
        assert!(dnd.is_active());
    }

    #[test]
    fn notification_level_round_trip() {
        for level in [
            NotificationLevel::All,
            NotificationLevel::MentionsOnly,
            NotificationLevel::None,
        ] {
            let s = level.as_str();
            assert_eq!(NotificationLevel::from_str(s), Some(level));
        }
        assert_eq!(NotificationLevel::from_str("unknown"), None);
    }

    #[test]
    fn should_notify_respects_dnd_disabled() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        assert!(svc.should_notify(&alice(), &ConversationId::from("c1"), false, false));
        assert!(svc.should_notify(&alice(), &ConversationId::from("c1"), true, false));
    }

    #[test]
    fn should_notify_respects_dnd_enabled() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        svc.set_dnd(
            &alice(),
            DndSettings {
                enabled: true,
                quiet_from: None,
                quiet_until: None,
                allow_calls: false,
                allow_pinned: true,
            },
        ).unwrap();
        let conv = ConversationId::from("c1");
        assert!(!svc.should_notify(&alice(), &conv, false, false));
        assert!(svc.should_notify(&alice(), &conv, false, true));
    }

    #[test]
    fn should_notify_suppressed_for_muted_conversation() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        let conv = ConversationId::from("c1");
        svc.mute(&alice(), &conv).unwrap();
        // DND is off, but the conversation is muted — no notification.
        assert!(!svc.should_notify(&alice(), &conv, false, false));
    }

    #[test]
    fn should_notify_mentions_only_silences_unmentioned() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        let conv = ConversationId::from("c1");
        svc.set_conversation(
            &alice(),
            ConversationNotificationSettings {
                conversation_id: conv.clone(),
                muted: false,
                level: NotificationLevel::MentionsOnly,
                custom_sound: None,
                show_preview: true,
            },
        ).unwrap();
        assert!(svc.should_notify(&alice(), &conv, true, false));
        assert!(!svc.should_notify(&alice(), &conv, false, false));
    }

    #[test]
    fn should_notify_level_none_always_silent() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        let conv = ConversationId::from("c1");
        svc.set_conversation(
            &alice(),
            ConversationNotificationSettings {
                conversation_id: conv.clone(),
                muted: false,
                level: NotificationLevel::None,
                custom_sound: None,
                show_preview: true,
            },
        ).unwrap();
        assert!(!svc.should_notify(&alice(), &conv, true, true));
    }

    #[test]
    fn should_notify_other_user_mute_does_not_leak() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        let bob = UserId::from("bob");
        let conv = ConversationId::from("c1");
        svc.mute(&bob, &conv).unwrap();
        // Alice's view of the same conversation is unaffected.
        assert!(svc.should_notify(&alice(), &conv, false, false));
    }

    #[test]
    fn set_conversation_rejects_empty_id() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        let r = svc.set_conversation(
            &alice(),
            ConversationNotificationSettings::default(),
        );
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[test]
    fn set_dnd_rejects_inverted_window() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        let later = chrono::Utc::now();
        let earlier = later - chrono::Duration::hours(1);
        let r = svc.set_dnd(
            &alice(),
            DndSettings {
                enabled: true,
                quiet_from: Some(later),
                quiet_until: Some(earlier),
                allow_calls: true,
                allow_pinned: true,
            },
        );
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[test]
    fn muted_redundant_hashset_dropped() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        let conv = ConversationId::from("c1");
        svc.mute(&alice(), &conv).unwrap();
        // list_muted is computed from per_conversation, not the
        // redundant HashSet.
        let list = svc.list_muted(&alice());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], conv);
    }

    #[test]
    fn set_conversation_emits_effective_payload() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        let mut rx = svc.bus.subscribe_for(alice());
        let conv = ConversationId::from("c1");
        svc.set_conversation(
            &alice(),
            ConversationNotificationSettings {
                conversation_id: conv.clone(),
                muted: true,
                level: NotificationLevel::MentionsOnly,
                custom_sound: None,
                show_preview: false,
            },
        )
        .unwrap();
        let evt = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await });
        let evt = evt.expect("event").expect("some");
        match evt {
            A3chatEvent::NotificationSettingsChanged {
                conversation_id,
                global_dnd,
                ..
            } => {
                assert_eq!(conversation_id, Some(conv));
                assert!(global_dnd.is_none(), "set_conversation should not include global_dnd");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn conversation_notification_settings_default() {
        let settings = ConversationNotificationSettings::default();
        assert!(!settings.muted);
        assert_eq!(settings.level, NotificationLevel::All);
        assert!(settings.show_preview);
        assert!(settings.custom_sound.is_none());
    }

    #[test]
    fn get_dnd_returns_default_when_not_set() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        let dnd = svc.get_dnd(&alice());
        assert!(!dnd.enabled);
    }

    #[test]
    fn get_dnd_returns_updated_value() {
        let svc = NotificationSettingsService::new(NotificationBus::new(8));
        let settings = DndSettings {
            enabled: true,
            quiet_from: None,
            quiet_until: None,
            allow_calls: true,
            allow_pinned: false,
        };
        svc.set_dnd(&alice(), settings.clone()).unwrap();
        assert_eq!(svc.get_dnd(&alice()), settings);
    }

    #[test]
    fn notification_level_from_str_unknown() {
        assert_eq!(NotificationLevel::from_str("invalid"), None);
    }

    #[test]
    fn notification_settings_non_default() {
        let settings = ConversationNotificationSettings {
            conversation_id: ConversationId::from("dm:alice:bob"),
            muted: true,
            level: NotificationLevel::MentionsOnly,
            custom_sound: Some("chime".into()),
            show_preview: false,
        };
        assert!(settings.muted);
        assert_eq!(settings.level, NotificationLevel::MentionsOnly);
        assert!(!settings.show_preview);
    }
}
