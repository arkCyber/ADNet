//! Intrusion Detection System (IDS) for A3Net.
//!
//! Monitors for suspicious activity and potential security threats.
//! Provides real-time threat detection with configurable patterns and thresholds.

use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::error::{SecurityError, SecurityResult};

/// Threat level classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreatLevel {
    Info = 0,
    Low = 1,
    Medium = 2,
    High = 3,
    Critical = 4,
}

impl ThreatLevel {
    /// Check if this level requires immediate action.
    pub fn requires_action(&self) -> bool {
        matches!(self, ThreatLevel::High | ThreatLevel::Critical)
    }

    /// Get a human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            ThreatLevel::Info => "Informational event",
            ThreatLevel::Low => "Minor anomaly detected",
            ThreatLevel::Medium => "Suspicious activity",
            ThreatLevel::High => "Potential attack detected",
            ThreatLevel::Critical => "Active threat confirmed",
        }
    }
}

/// Types of security threats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreatType {
    BruteForce,
    CredentialStuffing,
    TrafficAnomaly,
    PortScan,
    DosAttack,
    IntrusionAttempt,
    DataExfiltration,
    MalwareActivity,
    SocialEngineering,
    Unknown,
}

impl ThreatType {
    /// Get the default threat level for this type.
    pub fn default_level(&self) -> ThreatLevel {
        match self {
            ThreatType::BruteForce => ThreatLevel::High,
            ThreatType::CredentialStuffing => ThreatLevel::Critical,
            ThreatType::TrafficAnomaly => ThreatLevel::Medium,
            ThreatType::PortScan => ThreatLevel::Low,
            ThreatType::DosAttack => ThreatLevel::High,
            ThreatType::IntrusionAttempt => ThreatLevel::Critical,
            ThreatType::DataExfiltration => ThreatLevel::Critical,
            ThreatType::MalwareActivity => ThreatLevel::Critical,
            ThreatType::SocialEngineering => ThreatLevel::Medium,
            ThreatType::Unknown => ThreatLevel::Info,
        }
    }
}

/// A security event detected by the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEvent {
    pub id: String,
    pub event_type: SecurityEventType,
    pub threat_level: ThreatLevel,
    pub threat_type: Option<ThreatType>,
    pub source_ip: Option<String>,
    pub target_id: Option<String>,
    pub description: String,
    pub details: HashMap<String, serde_json::Value>,
    pub timestamp: DateTime<Utc>,
    pub resolved: bool,
    pub resolved_at: Option<DateTime<Utc>>,
}

impl SecurityEvent {
    /// Create a new security event.
    pub fn new(
        event_type: SecurityEventType,
        threat_level: ThreatLevel,
        description: String,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            event_type,
            threat_level,
            threat_type: None,
            source_ip: None,
            target_id: None,
            description,
            details: HashMap::new(),
            timestamp: Utc::now(),
            resolved: false,
            resolved_at: None,
        }
    }

    /// Mark this event as resolved.
    pub fn resolve(&mut self) {
        self.resolved = true;
        self.resolved_at = Some(Utc::now());
    }
}

/// Types of security events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityEventType {
    LoginAttempt,
    LoginSuccess,
    LoginFailure,
    Logout,
    PermissionDenied,
    ResourceAccess,
    DataModification,
    ConfigurationChange,
    KeyGeneration,
    KeyRotation,
    SessionCreation,
    SessionTermination,
    NetworkConnection,
    NetworkDisconnection,
    FileAccess,
    FileModification,
    FileDeletion,
    Error,
    Warning,
    AnomalyDetected,
    ThreatDetected,
}

/// Anomaly detection score (0.0 to 1.0).
pub struct AnomalyScore {
    pub value: f64,
    pub components: HashMap<String, f64>,
    pub threshold: f64,
    pub is_anomaly: bool,
}

impl AnomalyScore {
    /// Create a new anomaly score.
    pub fn new(value: f64, threshold: f64) -> Self {
        Self {
            is_anomaly: value > threshold,
            value,
            components: HashMap::new(),
            threshold,
        }
    }

    /// Add a component to the score.
    pub fn with_component(mut self, name: &str, value: f64) -> Self {
        self.components.insert(name.to_string(), value);
        self
    }
}

/// A threat pattern for detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreatPattern {
    pub id: String,
    pub name: String,
    pub description: String,
    pub pattern_type: ThreatPatternType,
    pub regex: Option<String>,
    pub conditions: Vec<ThreatCondition>,
    pub threat_type: ThreatType,
    pub base_score: f64,
    pub enabled: bool,
}

impl ThreatPattern {
    /// Create a new threat pattern.
    pub fn new(name: String, threat_type: ThreatType) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            description: String::new(),
            pattern_type: ThreatPatternType::Simple,
            regex: None,
            conditions: Vec::new(),
            threat_type,
            base_score: 0.5,
            enabled: true,
        }
    }

    /// Add a condition to this pattern.
    pub fn with_condition(mut self, condition: ThreatCondition) -> Self {
        self.conditions.push(condition);
        self
    }

    /// Set the base score for this pattern.
    pub fn with_score(mut self, score: f64) -> Self {
        self.base_score = score.clamp(0.0, 1.0);
        self
    }
}

/// Type of threat pattern.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreatPatternType {
    Simple,
    RateBased,
    ThresholdBased,
    Behavioral,
    MachineLearning,
}

/// Conditions for threat pattern matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreatCondition {
    /// Check if a metric exceeds a threshold
    Threshold {
        metric: String,
        operator: String,
        value: f64,
    },
    /// Check if an event occurs too frequently
    RateLimit {
        window: Duration,
        max_count: u64,
    },
    /// Check if a sequence of events occurred
    Sequence {
        events: Vec<SecurityEventType>,
        window: Duration,
    },
    /// Check geographic location
    GeoRestriction {
        blocked_countries: Vec<String>,
    },
}

/// Configuration for the intrusion detector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntrusionConfig {
    /// Anomaly detection threshold (0.0 to 1.0)
    pub anomaly_threshold: f64,
    /// Time window for rate limiting
    pub rate_window: Duration,
    /// Maximum failed login attempts
    pub max_failed_logins: u64,
    /// Lockout duration after failed attempts
    pub lockout_duration: Duration,
    /// Enable automatic blocking
    pub auto_block: bool,
    /// Block duration for threats
    pub block_duration: Duration,
    /// Enable threat pattern matching
    pub pattern_matching: bool,
    /// Cleanup interval for old events
    pub cleanup_interval: Duration,
    /// Event retention period
    pub retention_period: Duration,
}

impl Default for IntrusionConfig {
    fn default() -> Self {
        Self {
            anomaly_threshold: 0.7,
            rate_window: Duration::minutes(15),
            max_failed_logins: 5,
            lockout_duration: Duration::minutes(30),
            auto_block: true,
            block_duration: Duration::hours(1),
            pattern_matching: true,
            cleanup_interval: Duration::hours(1),
            retention_period: Duration::days(90),
        }
    }
}

/// Main intrusion detection system.
#[derive(Debug)]
pub struct IntrusionDetector {
    config: IntrusionConfig,
    patterns: Arc<RwLock<Vec<ThreatPattern>>>,
    events: Arc<RwLock<Vec<SecurityEvent>>>,
    metrics: Arc<RwLock<HashMap<String, Vec<MetricSample>>>>,
    blocked_ips: Arc<RwLock<HashMap<String, BlockInfo>>>,
    failed_attempts: Arc<RwLock<HashMap<String, FailedAttemptTracker>>>,
}

#[derive(Debug, Clone)]
struct MetricSample {
    value: f64,
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct BlockInfo {
    blocked_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    reason: String,
    threat_level: ThreatLevel,
}

#[derive(Debug, Clone)]
struct FailedAttemptTracker {
    attempts: Vec<DateTime<Utc>>,
    locked_until: Option<DateTime<Utc>>,
}

impl IntrusionDetector {
    /// Create a new intrusion detector.
    pub fn new(config: IntrusionConfig) -> Self {
        Self {
            config,
            patterns: Arc::new(RwLock::new(Vec::new())),
            events: Arc::new(RwLock::new(Vec::new())),
            metrics: Arc::new(RwLock::new(HashMap::new())),
            blocked_ips: Arc::new(RwLock::new(HashMap::new())),
            failed_attempts: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create with default configuration.
    pub fn default_config() -> Self {
        Self::new(IntrusionConfig::default())
    }

    /// Add a threat pattern.
    pub async fn add_pattern(&self, pattern: ThreatPattern) {
        let mut patterns = self.patterns.write().await;
        patterns.push(pattern);
    }

    /// Record a security event.
    pub async fn record_event(&self, event: SecurityEvent) {
        let mut events = self.events.write().await;
        events.push(event.clone());

        // Record metrics
        self.record_metric(&format!("{:?}", event.event_type), 1.0).await;

        // Check for threats
        if event.threat_level >= ThreatLevel::Medium {
            self.handle_threat(&event).await;
        }
    }

    /// Record a metric sample.
    pub async fn record_metric(&self, name: &str, value: f64) {
        let mut metrics = self.metrics.write().await;
        let samples = metrics.entry(name.to_string()).or_insert_with(Vec::new);
        samples.push(MetricSample {
            value,
            timestamp: Utc::now(),
        });

        // Cleanup old samples
        let cutoff = Utc::now() - self.config.rate_window;
        samples.retain(|s| s.timestamp > cutoff);
    }

    /// Record a login attempt.
    pub async fn record_login_attempt(
        &self,
        ip: &str,
        user_id: &str,
        success: bool,
    ) -> SecurityResult<()> {
        let key = format!("{}:{}", ip, user_id);

        if success {
            // Clear failed attempts on successful login
            let mut attempts = self.failed_attempts.write().await;
            attempts.remove(&key);
        } else {
            // Record failed attempt
            let mut attempts = self.failed_attempts.write().await;
            let tracker = attempts.entry(key.clone()).or_insert_with(|| FailedAttemptTracker {
                attempts: Vec::new(),
                locked_until: None,
            });

            let now = Utc::now();
            let window_start = now - self.config.rate_window;

            // Remove old attempts
            tracker.attempts.retain(|t| *t > window_start);
            tracker.attempts.push(now);

            // Check if lockout should be applied
            if tracker.attempts.len() >= self.config.max_failed_logins as usize {
                if tracker.locked_until.is_none() {
                    tracker.locked_until = Some(now + self.config.lockout_duration);

                    let mut event = SecurityEvent::new(
                        SecurityEventType::Warning,
                        ThreatLevel::Medium,
                        format!("Account locked due to {} failed login attempts", self.config.max_failed_logins),
                    );
                    event.source_ip = Some(ip.to_string());
                    event.target_id = Some(user_id.to_string());
                    event.threat_type = Some(ThreatType::BruteForce);
                    self.record_event(event).await;
                }
            }
        }

        Ok(())
    }

    /// Check if an IP is blocked.
    pub async fn is_ip_blocked(&self, ip: &str) -> bool {
        let blocked = self.blocked_ips.read().await;
        if let Some(info) = blocked.get(ip) {
            return info.expires_at > Utc::now();
        }
        false
    }

    /// Block an IP address.
    pub async fn block_ip(&self, ip: &str, reason: &str, level: ThreatLevel) -> SecurityResult<()> {
        if !self.config.auto_block && level < ThreatLevel::High {
            return Ok(());
        }

        let now = Utc::now();
        let mut blocked = self.blocked_ips.write().await;
        blocked.insert(
            ip.to_string(),
            BlockInfo {
                blocked_at: now,
                expires_at: now + self.config.block_duration,
                reason: reason.to_string(),
                threat_level: level,
            },
        );

        Ok(())
    }

    /// Unblock an IP address.
    pub async fn unblock_ip(&self, ip: &str) -> SecurityResult<()> {
        let mut blocked = self.blocked_ips.write().await;
        blocked.remove(ip);
        Ok(())
    }

    /// Analyze a score for anomalies.
    pub async fn analyze_anomaly(&self, score: AnomalyScore) -> SecurityResult<AnomalyScore> {
        if score.is_anomaly {
            let event = SecurityEvent::new(
                SecurityEventType::AnomalyDetected,
                ThreatLevel::Medium,
                format!("Anomaly detected with score {}", score.value),
            );
            self.record_event(event).await;
        }
        Ok(score)
    }

    /// Handle a detected threat.
    async fn handle_threat(&self, event: &SecurityEvent) {
        if self.config.auto_block {
            if let Some(ref ip) = event.source_ip {
                if event.threat_level >= ThreatLevel::Medium {
                    self.block_ip(ip, &event.description, event.threat_level)
                        .await
                        .ok();
                }
            }
        }
    }

    /// Check for rate-based threats.
    pub async fn check_rate_limit(
        &self,
        identifier: &str,
        window: Duration,
        max_count: u64,
    ) -> SecurityResult<bool> {
        let mut metrics = self.metrics.write().await;
        let samples = metrics.entry(identifier.to_string()).or_insert_with(Vec::new);

        let now = Utc::now();
        let cutoff = now - window;
        samples.retain(|s| s.timestamp > cutoff);

        if samples.len() as u64 >= max_count {
            let mut event = SecurityEvent::new(
                SecurityEventType::AnomalyDetected,
                ThreatLevel::Medium,
                format!("Rate limit exceeded for {}", identifier),
            );
            event.details.insert("identifier".to_string(), serde_json::json!(identifier));
            event.details.insert("count".to_string(), serde_json::json!(samples.len()));
            self.record_event(event).await;
            return Ok(true);
        }

        Ok(false)
    }

    /// Get all security events.
    pub async fn get_events(&self, unresolved_only: bool) -> Vec<SecurityEvent> {
        let events = self.events.read().await;
        if unresolved_only {
            events.iter().filter(|e| !e.resolved).cloned().collect()
        } else {
            events.clone()
        }
    }

    /// Resolve a security event.
    pub async fn resolve_event(&self, event_id: &str) -> SecurityResult<()> {
        let mut events = self.events.write().await;
        if let Some(event) = events.iter_mut().find(|e| e.id == event_id) {
            event.resolve();
            Ok(())
        } else {
            Err(SecurityError::Internal {
                reason: format!("Event {} not found", event_id),
            })
        }
    }

    /// Get blocked IPs.
    pub async fn get_blocked_ips(&self) -> Vec<(String, BlockInfo)> {
        let blocked = self.blocked_ips.read().await;
        let now = Utc::now();
        blocked
            .iter()
            .filter(|(_, info)| info.expires_at > now)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Get security statistics.
    pub async fn get_stats(&self) -> IntrusionStats {
        let events = self.events.read().await;
        let blocked = self.blocked_ips.read().await;
        let attempts = self.failed_attempts.read().await;

        let unresolved = events.iter().filter(|e| !e.resolved).count();
        let critical = events.iter().filter(|e| e.threat_level == ThreatLevel::Critical).count();
        let now = Utc::now();

        IntrusionStats {
            total_events: events.len(),
            unresolved_events: unresolved,
            critical_threats: critical,
            blocked_ips: blocked.len(),
            locked_accounts: attempts.len(),
            last_cleanup: now,
        }
    }

    /// Cleanup old data.
    pub async fn cleanup(&self) {
        let cutoff = Utc::now() - self.config.retention_period;

        // Cleanup events
        let mut events = self.events.write().await;
        events.retain(|e| e.timestamp > cutoff || !e.resolved);

        // Cleanup metrics
        let mut metrics = self.metrics.write().await;
        for samples in metrics.values_mut() {
            samples.retain(|s| s.timestamp > cutoff);
        }

        // Cleanup expired blocks
        let mut blocked = self.blocked_ips.write().await;
        blocked.retain(|_, info| info.expires_at > Utc::now());

        // Cleanup failed attempts
        let mut attempts = self.failed_attempts.write().await;
        for tracker in attempts.values_mut() {
            tracker.attempts.retain(|t| *t > cutoff);
        }
        attempts.retain(|_, t| !t.attempts.is_empty());
    }
}

/// Intrusion detection statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntrusionStats {
    pub total_events: usize,
    pub unresolved_events: usize,
    pub critical_threats: usize,
    pub blocked_ips: usize,
    pub locked_accounts: usize,
    pub last_cleanup: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_login_attempt_tracking() {
        let detector = IntrusionDetector::default_config();

        // Record failed attempts
        for _ in 0..4 {
            detector
                .record_login_attempt("192.168.1.1", "user1", false)
                .await
                .unwrap();
        }

        // Should not be locked yet
        assert!(!detector.is_ip_blocked("192.168.1.1").await);

        // Record one more failed attempt
        detector
            .record_login_attempt("192.168.1.1", "user1", false)
            .await
            .unwrap();

        // Should be locked
        let events = detector.get_events(true).await;
        assert!(!events.is_empty());
    }

    #[tokio::test]
    async fn test_successful_login_clears_attempts() {
        let detector = IntrusionDetector::default_config();

        // Record some failed attempts
        for _ in 0..3 {
            detector
                .record_login_attempt("192.168.1.2", "user2", false)
                .await
                .unwrap();
        }

        // Successful login
        detector
            .record_login_attempt("192.168.1.2", "user2", true)
            .await
            .unwrap();

        // Check that failed attempts are cleared
        let stats = detector.get_stats().await;
        assert_eq!(stats.locked_accounts, 0);
    }

    #[tokio::test]
    async fn test_ip_blocking() {
        let detector = IntrusionDetector::default_config();

        detector
            .block_ip("10.0.0.1", "Test block", ThreatLevel::Medium)
            .await
            .unwrap();

        assert!(detector.is_ip_blocked("10.0.0.1").await);

        detector.unblock_ip("10.0.0.1").await.unwrap();

        assert!(!detector.is_ip_blocked("10.0.0.1").await);
    }

    #[tokio::test]
    async fn test_anomaly_detection() {
        let detector = IntrusionDetector::default_config();

        let normal_score = AnomalyScore::new(0.3, 0.7);
        let result = detector.analyze_anomaly(normal_score).await.unwrap();
        assert!(!result.is_anomaly);

        let high_score = AnomalyScore::new(0.9, 0.7);
        let result = detector.analyze_anomaly(high_score).await.unwrap();
        assert!(result.is_anomaly);
    }
}
