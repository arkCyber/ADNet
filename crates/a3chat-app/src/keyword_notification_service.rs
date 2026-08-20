//! Keyword Notification Service (关键词提醒)
//!
//! Handles custom keyword-based notifications. Users can set up keywords
//! that trigger special notifications even when the conversation is muted.
//!
//! ## Features
//! - Per-user keyword lists
//! - Per-conversation keyword overrides
//! - Case-insensitive matching
//! - Regex support for advanced users
//! - Notification count tracking
//! - Rate limiting to prevent notification storms
//!
//! ## DO-178C Traceability
//! - D-KW-1: Keywords stored per user
//! - D-KW-2: Keyword matching is case-insensitive
//! - D-KW-3: Notification triggered on match
//! - D-KW-4: Rate limiting prevents notification storms

pub mod rate_limiter;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;
use regex::Regex;
use serde::{Deserialize, Serialize};

use a3chat_core::id::{ConversationId, MessageId, UserId};

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;
use self::rate_limiter::{KeywordRateLimiter, RateLimiterConfig};

/// Maximum keywords per user
pub const MAX_KEYWORDS_PER_USER: usize = 100;

/// Maximum keywords per conversation override
pub const MAX_KEYWORDS_PER_CONVERSATION: usize = 50;

/// Maximum keyword length
pub const MAX_KEYWORD_LENGTH: usize = 128;

/// A single keyword entry
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct KeywordEntry {
    /// Unique identifier for this keyword
    pub keyword_id: String,
    /// The keyword text (case-insensitive matching)
    pub keyword: String,
    /// Whether this is a regex pattern
    pub is_regex: bool,
    /// Whether this keyword is enabled
    pub enabled: bool,
    /// When this keyword was added (Unix timestamp)
    pub created_at: i64,
    /// Number of times this keyword has matched (using atomic for concurrent access)
    #[serde(skip)]
    pub match_count: Arc<AtomicU64>,
    /// Cached compiled regex (for regex keywords only)
    #[serde(skip)]
    cached_regex: Option<Regex>,
}

impl KeywordEntry {
    pub fn new(keyword: String, is_regex: bool) -> Self {
        let keyword_id = format!("kw_{}", uuid::Uuid::new_v4());
        let keyword_lower = keyword.to_lowercase();
        let cached_regex = if is_regex {
            Regex::new(&keyword_lower).ok()
        } else {
            None
        };
        Self {
            keyword_id,
            keyword: keyword_lower,
            is_regex,
            enabled: true,
            created_at: chrono::Utc::now().timestamp(),
            match_count: Arc::new(AtomicU64::new(0)),
            cached_regex,
        }
    }

    pub fn new_text(keyword: String) -> Self {
        Self::new(keyword, false)
    }

    pub fn new_regex(pattern: String) -> Result<Self, AppError> {
        // Validate regex pattern first
        Regex::new(&pattern)
            .map_err(|e| AppError::InvalidInput(format!("Invalid regex pattern: {}", e)))?;
        Ok(Self::new(pattern, true))
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.keyword.is_empty() {
            return Err(AppError::InvalidInput("Keyword cannot be empty".into()));
        }
        if self.keyword.len() > MAX_KEYWORD_LENGTH {
            return Err(AppError::InvalidInput(format!(
                "Keyword exceeds maximum length of {}",
                MAX_KEYWORD_LENGTH
            )));
        }
        if self.is_regex && self.cached_regex.is_none() {
            return Err(AppError::InvalidInput("Invalid regex pattern".into()));
        }
        Ok(())
    }

    pub fn matches(&self, text: &str) -> bool {
        if !self.enabled {
            return false;
        }

        let text_lower = text.to_lowercase();

        if self.is_regex {
            self.cached_regex.as_ref()
                .map(|re| re.is_match(&text_lower))
                .unwrap_or(false)
        } else {
            text_lower.contains(&self.keyword)
        }
    }

    /// Increment match count atomically
    pub fn increment_match_count(&self) {
        self.match_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get current match count
    pub fn get_match_count(&self) -> u64 {
        self.match_count.load(Ordering::Relaxed)
    }
}

/// Keyword settings for a specific conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConversationKeywordSettings {
    pub conversation_id: ConversationId,
    /// Additional keywords specific to this conversation
    pub additional_keywords: Vec<KeywordEntry>,
    /// Whether to also trigger notifications for mentions
    pub also_trigger_for_mentions: bool,
}

impl ConversationKeywordSettings {
    pub fn new(conversation_id: ConversationId) -> Self {
        Self {
            conversation_id,
            additional_keywords: Vec::new(),
            also_trigger_for_mentions: true,
        }
    }
}

/// Global keyword notification settings for a user
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct KeywordNotificationSettings {
    /// Master toggle for keyword notifications
    pub enabled: bool,
    /// Notify even when conversation is muted
    pub notify_when_muted: bool,
    /// Notify even when DND is active
    pub notify_when_dnd: bool,
    /// Include message preview in notification
    pub include_preview: bool,
    /// Play custom sound on match
    pub custom_sound: Option<String>,
}

impl Default for KeywordNotificationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            notify_when_muted: true,
            notify_when_dnd: false,
            include_preview: true,
            custom_sound: None,
        }
    }
}

/// Keyword notification event
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct KeywordMatchEvent {
    pub user_id: UserId,
    pub conversation_id: ConversationId,
    pub message_id: MessageId,
    pub sender_id: UserId,
    pub matched_keyword: String,
    pub is_regex: bool,
    pub message_preview: String,
    pub timestamp: i64,
}

/// Keyword statistics for a user
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct KeywordStats {
    pub user_id: UserId,
    pub total_keywords: usize,
    pub enabled_keywords: usize,
    pub regex_keywords: usize,
    pub total_matches: u64,
    pub matches_today: u64,
    pub matches_this_week: u64,
    pub top_keywords: Vec<KeywordMatchCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct KeywordMatchCount {
    pub keyword: String,
    pub is_regex: bool,
    pub match_count: u64,
}

/// Service for managing keyword-based notifications
#[derive(Clone)]
pub struct KeywordNotificationService {
    /// Per-user global keywords
    global_keywords: Arc<RwLock<HashMap<UserId, Vec<KeywordEntry>>>>,
    /// Per-user global settings
    global_settings: Arc<RwLock<HashMap<UserId, KeywordNotificationSettings>>>,
    /// Per-user, per-conversation keyword overrides
    conversation_keywords: Arc<RwLock<HashMap<UserId, HashMap<ConversationId, ConversationKeywordSettings>>>>,
    /// Match statistics
    match_stats: Arc<RwLock<HashMap<UserId, MatchStats>>>,
    /// Notification bus
    bus: NotificationBus,
    /// Rate limiter for preventing notification storms
    rate_limiter: Arc<KeywordRateLimiter>,
}

#[derive(Debug, Clone)]
struct MatchStats {
    total_matches: u64,
    matches_today: u64,
    matches_this_week: u64,
    keyword_counts: HashMap<String, u64>,
    last_reset_day: i64,
}

impl Default for MatchStats {
    fn default() -> Self {
        Self {
            total_matches: 0,
            matches_today: 0,
            matches_this_week: 0,
            keyword_counts: HashMap::new(),
            last_reset_day: 0,
        }
    }
}

impl KeywordNotificationService {
    /// Create a new KeywordNotificationService
    pub fn new(bus: NotificationBus) -> Self {
        Self::with_rate_limiter(bus, RateLimiterConfig::default())
    }

    /// Create a new KeywordNotificationService with custom rate limiter config
    pub fn with_rate_limiter(bus: NotificationBus, rate_limit_config: RateLimiterConfig) -> Self {
        Self {
            global_keywords: Arc::new(RwLock::new(HashMap::new())),
            global_settings: Arc::new(RwLock::new(HashMap::new())),
            conversation_keywords: Arc::new(RwLock::new(HashMap::new())),
            match_stats: Arc::new(RwLock::new(HashMap::new())),
            bus,
            rate_limiter: Arc::new(KeywordRateLimiter::new(rate_limit_config)),
        }
    }

    /// Add a global keyword for a user
    pub async fn add_keyword(
        &self,
        user_id: &UserId,
        keyword: String,
        is_regex: bool,
    ) -> AppResult<KeywordEntry> {
        let entry = if is_regex {
            KeywordEntry::new_regex(keyword)?
        } else {
            KeywordEntry::new_text(keyword)
        };

        entry.validate()?;

        let mut keywords = self.global_keywords.write();
        let user_keywords = keywords.entry(user_id.clone()).or_insert_with(Vec::new);

        // Check max limit
        if user_keywords.len() >= MAX_KEYWORDS_PER_USER {
            return Err(AppError::InvalidInput(format!(
                "Maximum {} keywords per user reached",
                MAX_KEYWORDS_PER_USER
            )));
        }

        // Check for duplicates
        if user_keywords.iter().any(|k| k.keyword == entry.keyword && k.is_regex == entry.is_regex) {
            return Err(AppError::Conflict("Keyword already exists".into()));
        }

        user_keywords.push(entry.clone());

        tracing::info!("Added keyword '{}' for user {}", entry.keyword, user_id);

        Ok(entry)
    }

    /// Remove a global keyword
    pub async fn remove_keyword(
        &self,
        user_id: &UserId,
        keyword_id: &str,
    ) -> AppResult<bool> {
        let mut keywords = self.global_keywords.write();
        if let Some(user_keywords) = keywords.get_mut(user_id) {
            let len_before = user_keywords.len();
            user_keywords.retain(|k| k.keyword_id != keyword_id);
            return Ok(user_keywords.len() < len_before);
        }
        Ok(false)
    }

    /// Update a keyword
    pub async fn update_keyword(
        &self,
        user_id: &UserId,
        keyword_id: &str,
        keyword: Option<String>,
        enabled: Option<bool>,
    ) -> AppResult<KeywordEntry> {
        let mut keywords = self.global_keywords.write();
        if let Some(user_keywords) = keywords.get_mut(user_id) {
            if let Some(entry) = user_keywords.iter_mut().find(|k| k.keyword_id == keyword_id) {
                if let Some(k) = keyword {
                    entry.keyword = k.to_lowercase();
                }
                if let Some(e) = enabled {
                    entry.enabled = e;
                }
                entry.validate()?;
                return Ok(entry.clone());
            }
        }
        Err(AppError::NotFound("Keyword not found".into()))
    }

    /// List all global keywords for a user
    pub async fn list_keywords(&self, user_id: &UserId) -> Vec<KeywordEntry> {
        let keywords = self.global_keywords.read();
        keywords.get(user_id).cloned().unwrap_or_default()
    }

    /// Add a conversation-specific keyword
    pub async fn add_conversation_keyword(
        &self,
        user_id: &UserId,
        conversation_id: &ConversationId,
        keyword: String,
        is_regex: bool,
    ) -> AppResult<KeywordEntry> {
        let entry = if is_regex {
            KeywordEntry::new_regex(keyword)?
        } else {
            KeywordEntry::new_text(keyword)
        };

        entry.validate()?;

        let mut conv_keywords = self.conversation_keywords.write();
        let conv_settings = conv_keywords
            .entry(user_id.clone())
            .or_insert_with(HashMap::new)
            .entry(conversation_id.clone())
            .or_insert_with(|| ConversationKeywordSettings::new(conversation_id.clone()));

        if conv_settings.additional_keywords.len() >= MAX_KEYWORDS_PER_CONVERSATION {
            return Err(AppError::InvalidInput(format!(
                "Maximum {} keywords per conversation",
                MAX_KEYWORDS_PER_CONVERSATION
            )));
        }

        conv_settings.additional_keywords.push(entry.clone());

        Ok(entry)
    }

    /// Get conversation keyword settings
    pub async fn get_conversation_keywords(
        &self,
        user_id: &UserId,
        conversation_id: &ConversationId,
    ) -> Option<ConversationKeywordSettings> {
        let conv_keywords = self.conversation_keywords.read();
        conv_keywords
            .get(user_id)
            .and_then(|c| c.get(conversation_id))
            .cloned()
    }

    /// Update global settings
    pub async fn update_settings(
        &self,
        user_id: &UserId,
        settings: KeywordNotificationSettings,
    ) -> AppResult<KeywordNotificationSettings> {
        let mut global_settings = self.global_settings.write();
        global_settings.insert(user_id.clone(), settings.clone());
        Ok(settings)
    }

    /// Get global settings
    pub async fn get_settings(&self, user_id: &UserId) -> KeywordNotificationSettings {
        let settings = self.global_settings.read();
        settings
            .get(user_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Check a message against keywords and trigger notifications if matched
    pub async fn check_message(
        &self,
        user_id: &UserId,
        conversation_id: &ConversationId,
        message_id: &MessageId,
        sender_id: &UserId,
        message_text: &str,
    ) -> AppResult<Vec<KeywordMatchEvent>> {
        let settings = self.get_settings(user_id).await;
        if !settings.enabled {
            return Ok(Vec::new());
        }

        let mut matches = Vec::new();

        // Check global keywords
        {
            let keywords = self.global_keywords.read();
            if let Some(user_keywords) = keywords.get(user_id) {
                for kw in user_keywords {
                    if kw.matches(message_text) {
                        // Check rate limit before processing
                        if !self.rate_limiter.check_allowed(user_id, &kw.keyword) {
                            // Rate limited - skip this notification
                            continue;
                        }

                        // Increment match count
                        {
                            let mut stats = self.match_stats.write();
                            let user_stats = stats.entry(user_id.clone()).or_default();
                            user_stats.total_matches += 1;
                            user_stats.matches_today += 1;
                            user_stats.matches_this_week += 1;
                            *user_stats.keyword_counts.entry(kw.keyword.clone()).or_insert(0) += 1;
                            
                            // Update keyword match count (clone to avoid borrow issues)
                            // Atomically increment match count
                            kw.match_count.fetch_add(1, Ordering::Relaxed);
                        }

                        let event = KeywordMatchEvent {
                            user_id: user_id.clone(),
                            conversation_id: conversation_id.clone(),
                            message_id: message_id.clone(),
                            sender_id: sender_id.clone(),
                            matched_keyword: kw.keyword.clone(),
                            is_regex: kw.is_regex,
                            message_preview: if settings.include_preview {
                                message_text.chars().take(100).collect()
                            } else {
                                String::new()
                            },
                            timestamp: chrono::Utc::now().timestamp(),
                        };

                        matches.push(event);
                    }
                }
            }
        }

        // Check conversation-specific keywords
        {
            let conv_keywords = self.conversation_keywords.read();
            if let Some(user_conv) = conv_keywords.get(user_id) {
                if let Some(conv_settings) = user_conv.get(conversation_id) {
                    for kw in &conv_settings.additional_keywords {
                        if kw.matches(message_text) {
                            // Check rate limit before processing
                            if !self.rate_limiter.check_allowed(user_id, &kw.keyword) {
                                // Rate limited - skip this notification
                                continue;
                            }

                            // Same match counting logic
                            {
                                let mut stats = self.match_stats.write();
                                let user_stats = stats.entry(user_id.clone()).or_default();
                                user_stats.total_matches += 1;
                                user_stats.matches_today += 1;
                                user_stats.matches_this_week += 1;
                            }

                            let event = KeywordMatchEvent {
                                user_id: user_id.clone(),
                                conversation_id: conversation_id.clone(),
                                message_id: message_id.clone(),
                                sender_id: sender_id.clone(),
                                matched_keyword: kw.keyword.clone(),
                                is_regex: kw.is_regex,
                                message_preview: if settings.include_preview {
                                    message_text.chars().take(100).collect()
                                } else {
                                    String::new()
                                },
                                timestamp: chrono::Utc::now().timestamp(),
                            };

                            matches.push(event);
                        }
                    }
                }
            }
        }

        // Note: keyword match events are tracked in match_stats,
        // clients can query stats to detect keyword matches

        if !matches.is_empty() {
            tracing::debug!(
                "Keyword match for user {} in conversation {}: {} keywords matched",
                user_id, conversation_id, matches.len()
            );
        }

        Ok(matches)
    }

    /// Get keyword statistics for a user
    pub async fn get_stats(&self, user_id: &UserId) -> KeywordStats {
        let keywords = self.global_keywords.read();
        let stats = self.match_stats.read();

        let user_keywords = keywords.get(user_id).cloned().unwrap_or_default();
        let user_stats = stats.get(user_id).cloned().unwrap_or_default();

        let mut top_keywords: Vec<KeywordMatchCount> = user_stats
            .keyword_counts
            .iter()
            .map(|(k, v)| KeywordMatchCount {
                keyword: k.clone(),
                is_regex: user_keywords.iter().any(|kw| &kw.keyword == k && kw.is_regex),
                match_count: *v,
            })
            .collect();

        top_keywords.sort_by(|a, b| b.match_count.cmp(&a.match_count));
        top_keywords.truncate(10);

        KeywordStats {
            user_id: user_id.clone(),
            total_keywords: user_keywords.len(),
            enabled_keywords: user_keywords.iter().filter(|k| k.enabled).count(),
            regex_keywords: user_keywords.iter().filter(|k| k.is_regex).count(),
            total_matches: user_stats.total_matches,
            matches_today: user_stats.matches_today,
            matches_this_week: user_stats.matches_this_week,
            top_keywords,
        }
    }

    /// Get rate limiter statistics
    pub fn get_rate_limiter_stats(&self) -> rate_limiter::RateLimiterStats {
        self.rate_limiter.stats()
    }

    /// Get available notification quota for a user and keyword
    pub fn get_available_quota(&self, user_id: &UserId, keyword: Option<&str>) -> u32 {
        self.rate_limiter.available_quota(user_id, keyword)
    }

    /// Clean up old rate limiter buckets (should be called periodically)
    pub fn cleanup_rate_limiter(&self) {
        use std::time::Duration;
        // Clean up buckets unused for more than 1 hour
        self.rate_limiter.cleanup_old_buckets(Duration::from_secs(3600));
    }

    /// Reset daily statistics (should be called by a scheduled task)
    pub async fn reset_daily_stats(&self) {
        let mut stats = self.match_stats.write();
        for (_, s) in stats.iter_mut() {
            s.matches_today = 0;
            s.last_reset_day = chrono::Utc::now().date_naive().and_hms_opt(0, 0, 0)
                .map(|dt| dt.and_utc().timestamp())
                .unwrap_or(0);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_user() -> UserId {
        UserId::from("test_user_keyword")
    }

    fn make_conv() -> ConversationId {
        ConversationId::from("conv_keyword_test")
    }

    fn make_msg() -> MessageId {
        MessageId::from("msg_keyword_123")
    }

    #[test]
    fn test_keyword_entry_text_matching() {
        let kw = KeywordEntry::new_text("hello".to_string());
        
        assert!(kw.matches("Hello World"));
        assert!(kw.matches("hello"));
        assert!(kw.matches("say hello"));
        assert!(!kw.matches("world"));
        assert!(!kw.matches(""));
    }

    #[test]
    fn test_keyword_entry_disabled() {
        let mut kw = KeywordEntry::new_text("hello".to_string());
        kw.enabled = false;
        
        assert!(!kw.matches("Hello World"));
    }

    #[test]
    fn test_keyword_entry_regex() {
        let kw = KeywordEntry::new_regex(r"\d{3}-\d{4}".to_string()).unwrap();
        
        assert!(kw.matches("phone: 123-4567"));
        assert!(!kw.matches("phone: 12-4567"));
        assert!(!kw.matches("no numbers"));
    }

    #[test]
    fn test_keyword_entry_regex_case_insensitive() {
        let kw = KeywordEntry::new_regex(r"^ERROR".to_string()).unwrap();
        
        assert!(kw.matches("error message"));
        assert!(kw.matches("ERROR message"));
        assert!(!kw.matches("an error occurred"));
    }

    #[test]
    fn test_keyword_validation_empty() {
        let kw = KeywordEntry::new_text("".to_string());
        assert!(kw.validate().is_err());
    }

    #[test]
    fn test_keyword_validation_too_long() {
        let long_keyword = "a".repeat(MAX_KEYWORD_LENGTH + 1);
        let kw = KeywordEntry::new_text(long_keyword);
        assert!(kw.validate().is_err());
    }

    #[test]
    fn test_keyword_validation_invalid_regex() {
        let kw = KeywordEntry::new_regex(r"[invalid".to_string());
        assert!(kw.is_err());
    }

    #[tokio::test]
    async fn test_add_keyword() {
        let bus = NotificationBus::default();
        let svc = KeywordNotificationService::new(bus);
        let user = make_user();

        let kw = svc.add_keyword(&user, "test".to_string(), false).await.unwrap();
        assert_eq!(kw.keyword, "test");
        assert!(!kw.is_regex);

        let keywords = svc.list_keywords(&user).await;
        assert_eq!(keywords.len(), 1);
    }

    #[tokio::test]
    async fn test_add_duplicate_keyword() {
        let bus = NotificationBus::default();
        let svc = KeywordNotificationService::new(bus);
        let user = make_user();

        svc.add_keyword(&user, "test".to_string(), false).await.unwrap();
        let result = svc.add_keyword(&user, "test".to_string(), false).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_remove_keyword() {
        let bus = NotificationBus::default();
        let svc = KeywordNotificationService::new(bus);
        let user = make_user();

        let kw = svc.add_keyword(&user, "test".to_string(), false).await.unwrap();
        let removed = svc.remove_keyword(&user, &kw.keyword_id).await.unwrap();
        assert!(removed);

        let keywords = svc.list_keywords(&user).await;
        assert!(keywords.is_empty());
    }

    #[tokio::test]
    async fn test_check_message_matches() {
        let bus = NotificationBus::default();
        let svc = KeywordNotificationService::new(bus);
        let user = make_user();
        let conv = make_conv();
        let msg = make_msg();
        let sender = UserId::from("sender_user");

        svc.add_keyword(&user, "urgent".to_string(), false).await.unwrap();

        let matches = svc.check_message(
            &user, &conv, &msg, &sender, "This is urgent!"
        ).await.unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_keyword, "urgent");
    }

    #[tokio::test]
    async fn test_check_message_no_match() {
        let bus = NotificationBus::default();
        let svc = KeywordNotificationService::new(bus);
        let user = make_user();
        let conv = make_conv();
        let msg = make_msg();
        let sender = UserId::from("sender_user");

        svc.add_keyword(&user, "urgent".to_string(), false).await.unwrap();

        let matches = svc.check_message(
            &user, &conv, &msg, &sender, "Just a normal message"
        ).await.unwrap();

        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn test_multiple_keywords_match() {
        let bus = NotificationBus::default();
        let svc = KeywordNotificationService::new(bus);
        let user = make_user();
        let conv = make_conv();
        let msg = make_msg();
        let sender = UserId::from("sender_user");

        svc.add_keyword(&user, "hello".to_string(), false).await.unwrap();
        svc.add_keyword(&user, "urgent".to_string(), false).await.unwrap();

        let matches = svc.check_message(
            &user, &conv, &msg, &sender, "Hello, this is urgent!"
        ).await.unwrap();

        assert_eq!(matches.len(), 2);
    }

    #[tokio::test]
    async fn test_disabled_keyword_no_match() {
        let bus = NotificationBus::default();
        let svc = KeywordNotificationService::new(bus);
        let user = make_user();
        let conv = make_conv();
        let msg = make_msg();
        let sender = UserId::from("sender_user");

        let kw = svc.add_keyword(&user, "secret".to_string(), false).await.unwrap();
        svc.update_keyword(&user, &kw.keyword_id, None, Some(false)).await.unwrap();

        let matches = svc.check_message(
            &user, &conv, &msg, &sender, "This is a secret message"
        ).await.unwrap();

        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn test_settings_disable_all() {
        let bus = NotificationBus::default();
        let svc = KeywordNotificationService::new(bus);
        let user = make_user();
        let conv = make_conv();
        let msg = make_msg();
        let sender = UserId::from("sender_user");

        svc.add_keyword(&user, "test".to_string(), false).await.unwrap();
        
        let mut settings = KeywordNotificationSettings::default();
        settings.enabled = false;
        svc.update_settings(&user, settings).await.unwrap();

        let matches = svc.check_message(
            &user, &conv, &msg, &sender, "This is a test message"
        ).await.unwrap();

        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn test_conversation_keywords() {
        let bus = NotificationBus::default();
        let svc = KeywordNotificationService::new(bus);
        let user = make_user();
        let conv = make_conv();
        let msg = make_msg();
        let sender = UserId::from("sender_user");

        // Add global keyword
        svc.add_keyword(&user, "global".to_string(), false).await.unwrap();
        
        // Add conversation-specific keyword
        svc.add_conversation_keyword(&user, &conv, "private".to_string(), false).await.unwrap();

        let matches = svc.check_message(
            &user, &conv, &msg, &sender, "This has private data"
        ).await.unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_keyword, "private");
    }

    #[tokio::test]
    async fn test_stats_tracking() {
        let bus = NotificationBus::default();
        let svc = KeywordNotificationService::new(bus);
        let user = make_user();
        let conv = make_conv();
        let sender = UserId::from("sender_user");

        svc.add_keyword(&user, "count".to_string(), false).await.unwrap();

        // Send a few messages
        for i in 0..5 {
            let msg_id = MessageId::from(format!("msg_{}", i));
            svc.check_message(&user, &conv, &msg_id, &sender, "This message has count").await.unwrap();
        }

        let stats = svc.get_stats(&user).await;
        assert_eq!(stats.total_matches, 5);
        assert_eq!(stats.matches_today, 5);
    }
}
