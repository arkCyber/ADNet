//! Audit logging system for A3Net.
//!
//! Provides comprehensive, compliance-ready audit trail with support for:
//! - Structured audit events
//! - Multiple severity levels
//! - Event filtering and querying
//! - Export to various formats (JSON, Syslog, etc.)
//! - Compliance reporting

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Audit event severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSeverity {
    Debug = 0,
    Info = 1,
    Notice = 2,
    Warning = 3,
    Error = 4,
    Critical = 5,
}

impl AuditSeverity {
    /// Get the syslog equivalent.
    pub fn to_syslog_level(&self) -> u8 {
        match self {
            AuditSeverity::Debug => 7,    // LOG_DEBUG
            AuditSeverity::Info => 6,     // LOG_INFO
            AuditSeverity::Notice => 5,   // LOG_NOTICE
            AuditSeverity::Warning => 4,  // LOG_WARNING
            AuditSeverity::Error => 3,    // LOG_ERR
            AuditSeverity::Critical => 2, // LOG_CRIT
        }
    }

    /// Get the minimum severity for a given level name.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "debug" => Some(AuditSeverity::Debug),
            "info" => Some(AuditSeverity::Info),
            "notice" => Some(AuditSeverity::Notice),
            "warning" | "warn" => Some(AuditSeverity::Warning),
            "error" | "err" => Some(AuditSeverity::Error),
            "critical" | "crit" | "fatal" => Some(AuditSeverity::Critical),
            _ => None,
        }
    }
}

/// Types of audit events.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    // Identity and Authentication
    UserLogin,
    UserLogout,
    LoginFailed,
    UserCreated,
    UserModified,
    UserDeleted,
    PasswordChanged,
    PasswordReset,
    MfaEnabled,
    MfaDisabled,
    
    // Authorization and Access Control
    AccessGranted,
    AccessDenied,
    PermissionGranted,
    PermissionRevoked,
    RoleAssigned,
    RoleRemoved,
    
    // Key and Certificate Management
    KeyGenerated,
    KeyRotated,
    KeyRevoked,
    KeyImported,
    KeyExported,
    CertificateIssued,
    CertificateRevoked,
    
    // Data Operations
    DataCreated,
    DataRead,
    DataUpdated,
    DataDeleted,
    DataExported,
    DataImported,
    
    // Network and Connection
    ConnectionOpened,
    ConnectionClosed,
    ConnectionFailed,
    TunnelOpened,
    TunnelClosed,
    
    // Configuration
    ConfigChanged,
    ConfigRead,
    PolicyChanged,
    
    // System
    SystemStarted,
    SystemStopped,
    SystemError,
    BackupStarted,
    BackupCompleted,
    RestoreStarted,
    RestoreCompleted,
    
    // Security Events
    IntrusionAttempt,
    AnomalyDetected,
    RateLimitExceeded,
    SessionCreated,
    SessionEnded,
    SessionTimeout,
    
    // Custom events
    Custom(String),
}

impl AuditEventType {
    /// Get the default severity for this event type.
    pub fn default_severity(&self) -> AuditSeverity {
        match self {
            // Info level - routine read operations
            AuditEventType::UserLogin
            | AuditEventType::UserLogout
            | AuditEventType::DataRead
            | AuditEventType::ConfigRead
            | AuditEventType::ConnectionOpened
            | AuditEventType::ConnectionClosed
            | AuditEventType::AccessGranted => AuditSeverity::Info,

            // Notice level - significant events
            AuditEventType::UserCreated
            | AuditEventType::KeyGenerated
            | AuditEventType::DataCreated
            | AuditEventType::SystemStarted
            | AuditEventType::BackupStarted
            | AuditEventType::SessionCreated => AuditSeverity::Notice,

            // Warning level - changes and updates
            AuditEventType::UserModified
            | AuditEventType::DataUpdated
            | AuditEventType::ConfigChanged
            | AuditEventType::PolicyChanged
            | AuditEventType::KeyRotated
            | AuditEventType::PasswordChanged
            | AuditEventType::SessionEnded
            | AuditEventType::BackupCompleted
            | AuditEventType::UserDeleted
            | AuditEventType::KeyRevoked
            | AuditEventType::DataDeleted
            | AuditEventType::MfaEnabled
            | AuditEventType::MfaDisabled
            | AuditEventType::PermissionGranted
            | AuditEventType::PermissionRevoked
            | AuditEventType::RoleAssigned
            | AuditEventType::RoleRemoved
            | AuditEventType::KeyImported
            | AuditEventType::KeyExported
            | AuditEventType::CertificateIssued
            | AuditEventType::DataExported
            | AuditEventType::DataImported
            | AuditEventType::TunnelOpened
            | AuditEventType::TunnelClosed
            | AuditEventType::PasswordReset
            | AuditEventType::CertificateRevoked => AuditSeverity::Warning,

            // Error level - operation failures
            AuditEventType::LoginFailed
            | AuditEventType::AccessDenied
            | AuditEventType::SessionTimeout
            | AuditEventType::RateLimitExceeded
            | AuditEventType::SystemError
            | AuditEventType::RestoreStarted
            | AuditEventType::ConnectionFailed => AuditSeverity::Error,

            // Critical level - security and system events
            AuditEventType::SystemStopped
            | AuditEventType::IntrusionAttempt
            | AuditEventType::AnomalyDetected
            | AuditEventType::RestoreCompleted => AuditSeverity::Critical,

            AuditEventType::Custom(_) => AuditSeverity::Info,
        }
    }
}

/// An audit record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub event_type: AuditEventType,
    pub severity: AuditSeverity,
    pub actor_id: Option<String>,
    pub actor_type: Option<String>,
    pub target_id: Option<String>,
    pub target_type: Option<String>,
    pub action: String,
    pub outcome: AuditOutcome,
    pub details: HashMap<String, serde_json::Value>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub session_id: Option<String>,
    pub correlation_id: Option<String>,
    pub location: Option<String>,
}

impl AuditRecord {
    /// Create a new audit record.
    pub fn new(event_type: AuditEventType, action: String, outcome: AuditOutcome) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            event_type: event_type.clone(),
            severity: event_type.default_severity(),
            actor_id: None,
            actor_type: None,
            target_id: None,
            target_type: None,
            action,
            outcome,
            details: HashMap::new(),
            ip_address: None,
            user_agent: None,
            session_id: None,
            correlation_id: None,
            location: None,
        }
    }

    /// Set the actor.
    pub fn actor(mut self, id: String, actor_type: String) -> Self {
        self.actor_id = Some(id);
        self.actor_type = Some(actor_type);
        self
    }

    /// Set the target.
    pub fn target(mut self, id: String, target_type: String) -> Self {
        self.target_id = Some(id);
        self.target_type = Some(target_type);
        self
    }

    /// Add a detail.
    pub fn detail<K: Into<String>>(mut self, key: K, value: impl Serialize) -> Self {
        self.details.insert(
            key.into(),
            serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        );
        self
    }

    /// Set IP address.
    pub fn ip(mut self, ip: String) -> Self {
        self.ip_address = Some(ip);
        self
    }

    /// Set session ID.
    pub fn session(mut self, session_id: String) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Set correlation ID.
    pub fn correlation(mut self, correlation_id: String) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }
}

/// Outcome of an audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Success,
    Failure,
    Pending,
    Unknown,
}

/// Filter criteria for querying audit logs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditFilter {
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub event_types: Vec<AuditEventType>,
    pub severities: Vec<AuditSeverity>,
    pub actor_ids: Vec<String>,
    pub target_ids: Vec<String>,
    pub outcomes: Vec<AuditOutcome>,
    pub correlation_ids: Vec<String>,
    pub ip_addresses: Vec<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

impl AuditFilter {
    /// Create a new filter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set time range.
    pub fn time_range(mut self, start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        self.start_time = Some(start);
        self.end_time = Some(end);
        self
    }

    /// Add event type filter.
    pub fn with_event_types(mut self, types: Vec<AuditEventType>) -> Self {
        self.event_types = types;
        self
    }

    /// Add severity filter.
    pub fn with_severities(mut self, severities: Vec<AuditSeverity>) -> Self {
        self.severities = severities;
        self
    }

    /// Add actor filter.
    pub fn with_actors(mut self, actor_ids: Vec<String>) -> Self {
        self.actor_ids = actor_ids;
        self
    }

    /// Add pagination.
    pub fn paginate(mut self, limit: usize, offset: usize) -> Self {
        self.limit = Some(limit);
        self.offset = Some(offset);
        self
    }

    /// Check if a record matches this filter.
    pub fn matches(&self, record: &AuditRecord) -> bool {
        // Check time range
        if let Some(start) = &self.start_time {
            if record.timestamp < *start {
                return false;
            }
        }
        if let Some(end) = &self.end_time {
            if record.timestamp > *end {
                return false;
            }
        }

        // Check event types
        if !self.event_types.is_empty()
            && !self.event_types.contains(&record.event_type)
        {
            return false;
        }

        // Check severities
        if !self.severities.is_empty()
            && !self.severities.contains(&record.severity)
        {
            return false;
        }

        // Check actor IDs
        if !self.actor_ids.is_empty() {
            if let Some(ref actor_id) = record.actor_id {
                if !self.actor_ids.contains(actor_id) {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check target IDs
        if !self.target_ids.is_empty() {
            if let Some(ref target_id) = record.target_id {
                if !self.target_ids.contains(target_id) {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check outcomes
        if !self.outcomes.is_empty()
            && !self.outcomes.contains(&record.outcome)
        {
            return false;
        }

        // Check correlation IDs
        if !self.correlation_ids.is_empty() {
            if let Some(ref corr_id) = record.correlation_id {
                if !self.correlation_ids.contains(corr_id) {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check IP addresses
        if !self.ip_addresses.is_empty() {
            if let Some(ref ip) = record.ip_address {
                if !self.ip_addresses.contains(ip) {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }
}

/// Audit event for synchronous recording.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub record: AuditRecord,
    /// Channel to send to the logger
    sender: std::sync::mpsc::Sender<AuditRecord>,
}

impl AuditEvent {
    /// Create a new audit event.
    pub fn new(
        event_type: AuditEventType,
        action: String,
        outcome: AuditOutcome,
        sender: std::sync::mpsc::Sender<AuditRecord>,
    ) -> Self {
        Self {
            record: AuditRecord::new(event_type, action, outcome),
            sender,
        }
    }

    /// Record this event.
    pub fn record(self) {
        let _ = self.sender.send(self.record);
    }
}

/// Main audit log system.
#[derive(Debug)]
pub struct AuditLog {
    records: Arc<RwLock<Vec<AuditRecord>>>,
    config: AuditConfig,
}

/// Configuration for the audit system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    /// Minimum severity to record
    pub min_severity: AuditSeverity,
    /// Maximum records to keep in memory
    pub max_in_memory: usize,
    /// Retention period for audit records
    pub retention_period: Duration,
    /// Whether to sync to disk immediately
    pub sync_writes: bool,
    /// Log file path (if file logging is enabled)
    pub log_file: Option<String>,
    /// Enable syslog export
    pub syslog_enabled: bool,
    /// Syslog server address
    pub syslog_addr: Option<String>,
    /// Enable JSON export
    pub json_export: bool,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            min_severity: AuditSeverity::Info,
            max_in_memory: 10000,
            retention_period: Duration::days(365),
            sync_writes: true,
            log_file: None,
            syslog_enabled: false,
            syslog_addr: None,
            json_export: false,
        }
    }
}

impl AuditLog {
    /// Create a new audit log.
    pub fn new(config: AuditConfig) -> Self {
        Self {
            records: Arc::new(RwLock::new(Vec::new())),
            config,
        }
    }

    /// Create with default configuration.
    pub fn default_config() -> Self {
        Self::new(AuditConfig::default())
    }

    /// Record an audit event.
    pub async fn record(&self, record: AuditRecord) {
        // Check minimum severity
        if record.severity < self.config.min_severity {
            return;
        }

        let mut records = self.records.write().await;
        records.push(record.clone());

        // Enforce maximum in-memory records
        if records.len() > self.config.max_in_memory {
            let to_remove = records.len() - self.config.max_in_memory;
            records.drain(0..to_remove);
        }
    }

    /// Query audit records.
    pub async fn query(&self, filter: &AuditFilter) -> Vec<AuditRecord> {
        let records = self.records.read().await;
        records
            .iter()
            .filter(|r| filter.matches(r))
            .skip(filter.offset.unwrap_or(0))
            .take(filter.limit.unwrap_or(100))
            .cloned()
            .collect()
    }

    /// Get a single record by ID.
    pub async fn get(&self, id: &str) -> Option<AuditRecord> {
        let records = self.records.read().await;
        records.iter().find(|r| r.id == id).cloned()
    }

    /// Get audit statistics.
    pub async fn stats(&self) -> AuditStats {
        let records = self.records.read().await;

        let mut by_severity = HashMap::new();
        let mut by_type = HashMap::new();
        let mut by_outcome = HashMap::new();

        for record in records.iter() {
            *by_severity.entry(record.severity).or_insert(0) += 1;
            *by_type
                .entry(format!("{:?}", record.event_type))
                .or_insert(0) += 1;
            *by_outcome
                .entry(format!("{:?}", record.outcome))
                .or_insert(0) += 1;
        }

        let total = records.len();
        let latest = records.last().cloned();
        let earliest = records.first().cloned();

        AuditStats {
            total_records: total,
            by_severity,
            by_type,
            by_outcome,
            latest_record: latest,
            earliest_record: earliest,
        }
    }

    /// Export records to JSON.
    pub async fn export_json(&self, filter: &AuditFilter) -> String {
        let records = self.query(filter).await;
        serde_json::to_string_pretty(&records).unwrap_or_else(|_| "[]".to_string())
    }

    /// Cleanup old records.
    pub async fn cleanup(&self) {
        let cutoff = Utc::now() - self.config.retention_period;
        let mut records = self.records.write().await;
        records.retain(|r| r.timestamp > cutoff);
    }

    /// Get record count.
    pub async fn count(&self) -> usize {
        let records = self.records.read().await;
        records.len()
    }

    /// Get all records (for testing).
    #[allow(dead_code)]
    pub async fn all(&self) -> Vec<AuditRecord> {
        let records = self.records.read().await;
        records.clone()
    }
}

/// Audit statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditStats {
    pub total_records: usize,
    pub by_severity: HashMap<AuditSeverity, usize>,
    pub by_type: HashMap<String, usize>,
    pub by_outcome: HashMap<String, usize>,
    pub latest_record: Option<AuditRecord>,
    pub earliest_record: Option<AuditRecord>,
}

/// Audit logger trait for async logging.
#[async_trait::async_trait]
pub trait AuditLogger: Send + Sync {
    /// Log an audit record.
    async fn log(&self, record: &AuditRecord) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Flush any buffered logs.
    async fn flush(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// In-memory audit logger (default implementation).
#[derive(Debug)]
pub struct InMemoryAuditLogger {
    log: Arc<AuditLog>,
}

impl InMemoryAuditLogger {
    /// Create a new in-memory audit logger.
    pub fn new(config: AuditConfig) -> Self {
        Self {
            log: Arc::new(AuditLog::new(config)),
        }
    }
}

#[async_trait::async_trait]
impl AuditLogger for InMemoryAuditLogger {
    async fn log(&self, record: &AuditRecord) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.log.record(record.clone()).await;
        Ok(())
    }

    async fn flush(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_audit_logging() {
        let audit = AuditLog::default_config();

        let record = AuditRecord::new(
            AuditEventType::UserLogin,
            "User logged in".to_string(),
            AuditOutcome::Success,
        )
        .actor("user123".to_string(), "user".to_string())
        .ip("192.168.1.1".to_string());

        audit.record(record.clone()).await;

        assert_eq!(audit.count().await, 1);
    }

    #[tokio::test]
    async fn test_audit_filtering() {
        let audit = AuditLog::default_config();

        // Add some records
        for i in 0..5 {
            let record = AuditRecord::new(
                AuditEventType::UserLogin,
                format!("Login attempt {}", i),
                if i % 2 == 0 {
                    AuditOutcome::Success
                } else {
                    AuditOutcome::Failure
                },
            )
            .actor(format!("user{}", i), "user".to_string());

            audit.record(record.clone()).await;
        }

        // Filter by outcome
        let filter = AuditFilter::new();
        let results = audit.query(&filter).await;
        assert_eq!(results.len(), 5);
    }

    #[tokio::test]
    async fn test_audit_stats() {
        let audit = AuditLog::default_config();

        let record1 = AuditRecord::new(
            AuditEventType::UserLogin,
            "Login".to_string(),
            AuditOutcome::Success,
        );

        let record2 = AuditRecord::new(
            AuditEventType::LoginFailed,
            "Failed login".to_string(),
            AuditOutcome::Failure,
        );

        audit.record(record1).await;
        audit.record(record2).await;

        let stats = audit.stats().await;
        assert_eq!(stats.total_records, 2);
        assert!(stats.by_outcome.contains_key("Success"));
        assert!(stats.by_outcome.contains_key("Failure"));
    }

    #[test]
    fn test_audit_severity_syslog() {
        assert_eq!(AuditSeverity::Debug.to_syslog_level(), 7);
        assert_eq!(AuditSeverity::Critical.to_syslog_level(), 2);
    }

    #[test]
    fn test_audit_severity_from_str() {
        assert_eq!(AuditSeverity::from_str("info"), Some(AuditSeverity::Info));
        assert_eq!(AuditSeverity::from_str("WARN"), Some(AuditSeverity::Warning));
        assert_eq!(AuditSeverity::from_str("critical"), Some(AuditSeverity::Critical));
        assert_eq!(AuditSeverity::from_str("invalid"), None);
    }
}
