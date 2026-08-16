//! Alert management system for A3Net observability.
//!
//! Provides comprehensive alerting with:
//! - Alert rules engine with configurable thresholds
//! - Multiple notification channels (webhook, email, Slack, PagerDuty)
//! - Alert aggregation and deduplication
//! - Alert history and tracking
//! - Escalation policies

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Alert severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    Debug,
    Info,
    Warning,
    Error,
    Critical,
}

impl AlertSeverity {
    /// Get the numeric value for comparison.
    pub fn level(&self) -> u8 {
        match self {
            AlertSeverity::Debug => 0,
            AlertSeverity::Info => 1,
            AlertSeverity::Warning => 2,
            AlertSeverity::Error => 3,
            AlertSeverity::Critical => 4,
        }
    }

    /// Get a human-readable name.
    pub fn name(&self) -> &'static str {
        match self {
            AlertSeverity::Debug => "debug",
            AlertSeverity::Info => "info",
            AlertSeverity::Warning => "warning",
            AlertSeverity::Error => "error",
            AlertSeverity::Critical => "critical",
        }
    }

    /// Check if this severity requires immediate action.
    pub fn requires_action(&self) -> bool {
        matches!(self, AlertSeverity::Error | AlertSeverity::Critical)
    }
}

impl std::fmt::Display for AlertSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Alert status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertStatus {
    /// Alert is firing
    Firing,
    /// Alert has been acknowledged
    Acknowledged,
    /// Alert has been resolved
    Resolved,
    /// Alert is muted/silenced
    Silenced,
}

impl Default for AlertStatus {
    fn default() -> Self {
        AlertStatus::Firing
    }
}

/// Alert rule types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertRuleType {
    /// Threshold-based alert
    Threshold,
    /// Change-based alert (rate of change)
    RateOfChange,
    /// Absence alert (metric not present)
    Absence,
    /// Anomaly-based alert
    Anomaly,
}

/// An alert rule definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    /// Unique identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Description
    pub description: String,
    /// Alert type
    pub rule_type: AlertRuleType,
    /// Metric to monitor
    pub metric_name: String,
    /// Optional labels to filter by
    pub labels: HashMap<String, String>,
    /// Severity when triggered
    pub severity: AlertSeverity,
    /// Threshold value (for Threshold type)
    pub threshold: Option<f64>,
    /// Comparison operator
    pub operator: AlertOperator,
    /// Time window for evaluation (seconds)
    pub window_seconds: u64,
    /// Minimum number of evaluations before firing
    pub min_evaluations: u32,
    /// Alert cooldown (seconds) before firing again
    pub cooldown_seconds: u64,
    /// Whether this rule is enabled
    pub enabled: bool,
    /// Annotations for display
    pub annotations: HashMap<String, String>,
}

impl AlertRule {
    /// Create a new alert rule.
    pub fn new(name: &str, metric_name: &str, severity: AlertSeverity) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            description: String::new(),
            rule_type: AlertRuleType::Threshold,
            metric_name: metric_name.to_string(),
            labels: HashMap::new(),
            severity,
            threshold: None,
            operator: AlertOperator::GreaterThan,
            window_seconds: 60,
            min_evaluations: 1,
            cooldown_seconds: 300,
            enabled: true,
            annotations: HashMap::new(),
        }
    }

    /// Set threshold value.
    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.threshold = Some(threshold);
        self
    }

    /// Set comparison operator.
    pub fn with_operator(mut self, op: AlertOperator) -> Self {
        self.operator = op;
        self
    }

    /// Set evaluation window.
    pub fn with_window(mut self, seconds: u64) -> Self {
        self.window_seconds = seconds;
        self
    }

    /// Add a label filter.
    pub fn with_label(mut self, key: &str, value: &str) -> Self {
        self.labels.insert(key.to_string(), value.to_string());
        self
    }

    /// Add an annotation.
    pub fn with_annotation(mut self, key: &str, value: &str) -> Self {
        self.annotations.insert(key.to_string(), value.to_string());
        self
    }

    /// Check if current value violates the rule.
    pub fn evaluate(&self, value: f64) -> bool {
        match self.operator {
            AlertOperator::GreaterThan => {
                if let Some(t) = self.threshold {
                    value > t
                } else {
                    false
                }
            }
            AlertOperator::GreaterThanOrEqual => {
                if let Some(t) = self.threshold {
                    value >= t
                } else {
                    false
                }
            }
            AlertOperator::LessThan => {
                if let Some(t) = self.threshold {
                    value < t
                } else {
                    false
                }
            }
            AlertOperator::LessThanOrEqual => {
                if let Some(t) = self.threshold {
                    value <= t
                } else {
                    false
                }
            }
            AlertOperator::Equal => {
                if let Some(t) = self.threshold {
                    (value - t).abs() < f64::EPSILON
                } else {
                    false
                }
            }
            AlertOperator::NotEqual => {
                if let Some(t) = self.threshold {
                    (value - t).abs() >= f64::EPSILON
                } else {
                    true
                }
            }
        }
    }
}

/// Alert comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertOperator {
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
    Equal,
    NotEqual,
}

/// An active or historical alert instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    /// Unique identifier
    pub id: String,
    /// Reference to the rule that created this alert
    pub rule_id: String,
    /// Rule name at time of creation
    pub rule_name: String,
    /// Alert severity
    pub severity: AlertSeverity,
    /// Current status
    pub status: AlertStatus,
    /// Human-readable message
    pub message: String,
    /// Metric value that triggered the alert
    pub metric_value: f64,
    /// Threshold that was violated
    pub threshold: Option<f64>,
    /// Labels associated with this alert
    pub labels: HashMap<String, String>,
    /// When the alert was first fired
    pub fired_at: DateTime<Utc>,
    /// When the alert was last updated
    pub updated_at: DateTime<Utc>,
    /// When the alert was acknowledged (if applicable)
    pub acknowledged_at: Option<DateTime<Utc>>,
    /// Who acknowledged it
    pub acknowledged_by: Option<String>,
    /// When the alert was resolved
    pub resolved_at: Option<DateTime<Utc>>,
    /// Number of times this alert has fired
    pub fire_count: u32,
    /// Annotations
    pub annotations: HashMap<String, String>,
}

impl Alert {
    /// Create a new alert from a rule.
    pub fn from_rule(rule: &AlertRule, metric_value: f64) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            rule_id: rule.id.clone(),
            rule_name: rule.name.clone(),
            severity: rule.severity,
            status: AlertStatus::Firing,
            message: rule.annotations.get("message")
                .cloned()
                .unwrap_or_else(|| format!("Alert: {} threshold exceeded", rule.name)),
            metric_value,
            threshold: rule.threshold,
            labels: rule.labels.clone(),
            fired_at: now,
            updated_at: now,
            acknowledged_at: None,
            acknowledged_by: None,
            resolved_at: None,
            fire_count: 1,
            annotations: rule.annotations.clone(),
        }
    }

    /// Acknowledge this alert.
    pub fn acknowledge(&mut self, by: &str) {
        self.status = AlertStatus::Acknowledged;
        self.acknowledged_at = Some(Utc::now());
        self.acknowledged_by = Some(by.to_string());
        self.updated_at = Utc::now();
    }

    /// Resolve this alert.
    pub fn resolve(&mut self) {
        self.status = AlertStatus::Resolved;
        self.resolved_at = Some(Utc::now());
        self.updated_at = Utc::now();
    }

    /// Silence this alert.
    pub fn silence(&mut self) {
        self.status = AlertStatus::Silenced;
        self.updated_at = Utc::now();
    }

    /// Re-fire a resolved alert.
    pub fn refire(&mut self, new_value: f64) {
        self.status = AlertStatus::Firing;
        self.metric_value = new_value;
        self.fire_count += 1;
        self.updated_at = Utc::now();
    }

    /// Get the duration this alert has been active.
    pub fn duration(&self) -> chrono::Duration {
        Utc::now() - self.fired_at
    }

    /// Get the duration since last update.
    pub fn idle_duration(&self) -> chrono::Duration {
        Utc::now() - self.updated_at
    }
}

/// Alert notification channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertChannel {
    /// Unique identifier
    pub id: String,
    /// Channel name
    pub name: String,
    /// Channel type
    pub channel_type: AlertChannelType,
    /// Configuration (varies by type)
    pub config: HashMap<String, serde_json::Value>,
    /// Severity filter (only send alerts of this level or higher)
    pub min_severity: AlertSeverity,
    /// Whether this channel is enabled
    pub enabled: bool,
}

impl AlertChannel {
    /// Create a webhook channel.
    pub fn webhook(name: &str, url: &str) -> Self {
        let mut config = HashMap::new();
        config.insert("url".to_string(), serde_json::json!(url));

        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            channel_type: AlertChannelType::Webhook,
            config,
            min_severity: AlertSeverity::Warning,
            enabled: true,
        }
    }

    /// Create an email channel.
    pub fn email(name: &str, recipients: Vec<String>) -> Self {
        let mut config = HashMap::new();
        config.insert("recipients".to_string(), serde_json::json!(recipients));

        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            channel_type: AlertChannelType::Email,
            config,
            min_severity: AlertSeverity::Error,
            enabled: true,
        }
    }

    /// Create a Slack channel.
    pub fn slack(name: &str, webhook_url: &str, channel: &str) -> Self {
        let mut config = HashMap::new();
        config.insert("webhook_url".to_string(), serde_json::json!(webhook_url));
        config.insert("channel".to_string(), serde_json::json!(channel));

        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            channel_type: AlertChannelType::Slack,
            config,
            min_severity: AlertSeverity::Warning,
            enabled: true,
        }
    }

    /// Create a PagerDuty channel.
    pub fn pagerduty(name: &str, routing_key: &str) -> Self {
        let mut config = HashMap::new();
        config.insert("routing_key".to_string(), serde_json::json!(routing_key));

        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            channel_type: AlertChannelType::PagerDuty,
            config,
            min_severity: AlertSeverity::Error,
            enabled: true,
        }
    }
}

/// Alert channel types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertChannelType {
    Webhook,
    Email,
    Slack,
    PagerDuty,
}

/// Alert manager for handling alert evaluation and notifications.
#[derive(Debug)]
pub struct AlertManager {
    rules: Arc<RwLock<Vec<AlertRule>>>,
    channels: Arc<RwLock<Vec<AlertChannel>>>,
    active_alerts: Arc<RwLock<HashMap<String, Alert>>>,
    alert_history: Arc<RwLock<Vec<Alert>>>,
    cooldown_tracker: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
}

impl AlertManager {
    /// Create a new alert manager.
    pub fn new() -> Self {
        Self {
            rules: Arc::new(RwLock::new(Vec::new())),
            channels: Arc::new(RwLock::new(Vec::new())),
            active_alerts: Arc::new(RwLock::new(HashMap::new())),
            alert_history: Arc::new(RwLock::new(Vec::new())),
            cooldown_tracker: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add an alert rule.
    pub async fn add_rule(&self, rule: AlertRule) {
        let mut rules = self.rules.write().await;
        rules.push(rule);
    }

    /// Remove an alert rule.
    pub async fn remove_rule(&self, rule_id: &str) {
        let mut rules = self.rules.write().await;
        rules.retain(|r| r.id != rule_id);
    }

    /// Get all rules.
    pub async fn get_rules(&self) -> Vec<AlertRule> {
        let rules = self.rules.read().await;
        rules.clone()
    }

    /// Add an alert channel.
    pub async fn add_channel(&self, channel: AlertChannel) {
        let mut channels = self.channels.write().await;
        channels.push(channel);
    }

    /// Remove an alert channel.
    pub async fn remove_channel(&self, channel_id: &str) {
        let mut channels = self.channels.write().await;
        channels.retain(|c| c.id != channel_id);
    }

    /// Evaluate all rules against current metric values.
    pub async fn evaluate_rules(&self, metrics: &HashMap<String, f64>) -> Vec<Alert> {
        let rules = self.rules.read().await;
        let mut new_alerts = Vec::new();
        let mut cooldowns = self.cooldown_tracker.write().await;

        for rule in rules.iter().filter(|r| r.enabled) {
            if let Some(&value) = metrics.get(&rule.metric_name) {
                // Check label filters
                let matches_labels = rule.labels.is_empty() ||
                    rule.labels.iter().all(|(k, v)| {
                        metrics.get(&format!("{}.{}", rule.metric_name, k))
                            .map(|m| m.to_string() == *v)
                            .unwrap_or(false)
                    });

                if matches_labels && rule.evaluate(value) {
                    // Check cooldown
                    let now = Utc::now();
                    if let Some(last_fire) = cooldowns.get(&rule.id) {
                        let cooldown = chrono::Duration::seconds(rule.cooldown_seconds as i64);
                        if now - *last_fire < cooldown {
                            continue;
                        }
                    }

                    // Create alert
                    let alert = Alert::from_rule(rule, value);
                    new_alerts.push(alert.clone());

                    // Update cooldown
                    cooldowns.insert(rule.id.clone(), now);

                    // Track active alert
                    let mut active = self.active_alerts.write().await;
                    active.insert(alert.id.clone(), alert);
                }
            }
        }

        // Move to history
        for alert in &new_alerts {
            let mut history = self.alert_history.write().await;
            history.push(alert.clone());

            // Limit history size
            if history.len() > 10000 {
                history.drain(0..1000);
            }
        }

        new_alerts
    }

    /// Get all active alerts.
    pub async fn get_active_alerts(&self) -> Vec<Alert> {
        let active = self.active_alerts.read().await;
        active.values().cloned().collect()
    }

    /// Get alerts by severity.
    pub async fn get_alerts_by_severity(&self, severity: AlertSeverity) -> Vec<Alert> {
        let active = self.active_alerts.read().await;
        active.values()
            .filter(|a| a.severity >= severity)
            .cloned()
            .collect()
    }

    /// Acknowledge an alert.
    pub async fn acknowledge_alert(&self, alert_id: &str, by: &str) -> Option<Alert> {
        let mut active = self.active_alerts.write().await;
        if let Some(alert) = active.get_mut(alert_id) {
            alert.acknowledge(by);
            return Some(alert.clone());
        }
        None
    }

    /// Resolve an alert.
    pub async fn resolve_alert(&self, alert_id: &str) -> Option<Alert> {
        let mut active = self.active_alerts.write().await;
        if let Some(alert) = active.get_mut(alert_id) {
            alert.resolve();
            return Some(alert.clone());
        }
        None
    }

    /// Silence an alert.
    pub async fn silence_alert(&self, alert_id: &str) -> Option<Alert> {
        let mut active = self.active_alerts.write().await;
        if let Some(alert) = active.get_mut(alert_id) {
            alert.silence();
            return Some(alert.clone());
        }
        None
    }

    /// Get alert history.
    pub async fn get_history(&self, limit: usize) -> Vec<Alert> {
        let history = self.alert_history.read().await;
        history.iter().rev().take(limit).cloned().collect()
    }

    /// Get alert statistics.
    pub async fn get_stats(&self) -> AlertStats {
        let active = self.active_alerts.read().await;
        let mut by_severity = HashMap::new();

        for alert in active.values() {
            *by_severity.entry(alert.severity).or_insert(0) += 1;
        }

        AlertStats {
            total_active: active.len(),
            by_severity,
            total_history: self.alert_history.read().await.len(),
        }
    }

    /// Check if there are critical alerts.
    pub async fn has_critical_alerts(&self) -> bool {
        let active = self.active_alerts.read().await;
        active.values().any(|a| a.severity == AlertSeverity::Critical && a.status == AlertStatus::Firing)
    }
}

impl Default for AlertManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Alert statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertStats {
    pub total_active: usize,
    pub by_severity: HashMap<AlertSeverity, usize>,
    pub total_history: usize,
}

/// Predefined alert rules for A3Net.
pub mod presets {
    use super::*;

    /// Get default A3Net alert rules.
    pub fn default_rules() -> Vec<AlertRule> {
        vec![
            AlertRule::new("High Memory Usage", "a3net_memory_used_bytes", AlertSeverity::Warning)
                .with_threshold(0.9)
                .with_operator(AlertOperator::GreaterThan)
                .with_annotation("message", "Memory usage is above 90%")
                .with_annotation("runbook", "https://docs.example.com/runbooks/high-memory"),

            AlertRule::new("Disk Space Low", "a3net_disk_used_bytes", AlertSeverity::Error)
                .with_threshold(0.85)
                .with_operator(AlertOperator::GreaterThan)
                .with_annotation("message", "Disk space usage is above 85%"),

            AlertRule::new("High CPU Usage", "a3net_cpu_usage_percent", AlertSeverity::Warning)
                .with_threshold(80.0)
                .with_operator(AlertOperator::GreaterThan)
                .with_annotation("message", "CPU usage is above 80%"),

            AlertRule::new("Network Errors", "a3net_network_errors_total", AlertSeverity::Error)
                .with_threshold(10.0)
                .with_operator(AlertOperator::GreaterThan)
                .with_annotation("message", "Network error rate is elevated"),

            AlertRule::new("DHT Query Failures", "a3net_dht_query_failures_total", AlertSeverity::Warning)
                .with_threshold(5.0)
                .with_operator(AlertOperator::GreaterThan)
                .with_annotation("message", "DHT query failure rate is high"),

            AlertRule::new("Bitswap Block Missing", "a3net_bitswap_block_missing_total", AlertSeverity::Warning)
                .with_threshold(1.0)
                .with_operator(AlertOperator::GreaterThan)
                .with_annotation("message", "Bitswap is missing blocks"),

            AlertRule::new("Peer Count Low", "a3net_peer_count", AlertSeverity::Warning)
                .with_threshold(1.0)
                .with_operator(AlertOperator::LessThan)
                .with_annotation("message", "Connected peer count is very low"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alert_severity_ordering() {
        assert!(AlertSeverity::Critical > AlertSeverity::Warning);
        assert!(AlertSeverity::Warning > AlertSeverity::Info);
        assert!(AlertSeverity::Error.level() > AlertSeverity::Warning.level());
    }

    #[test]
    fn test_alert_rule_evaluation() {
        let rule = AlertRule::new("test", "test_metric", AlertSeverity::Warning)
            .with_threshold(100.0)
            .with_operator(AlertOperator::GreaterThan);

        assert!(rule.evaluate(150.0));
        assert!(!rule.evaluate(50.0));
        assert!(!rule.evaluate(100.0));
    }

    #[test]
    fn test_alert_lifecycle() {
        let rule = AlertRule::new("test", "test_metric", AlertSeverity::Warning)
            .with_threshold(100.0)
            .with_operator(AlertOperator::GreaterThan);

        let mut alert = Alert::from_rule(&rule, 150.0);

        assert_eq!(alert.status, AlertStatus::Firing);

        alert.acknowledge("admin");
        assert_eq!(alert.status, AlertStatus::Acknowledged);
        assert!(alert.acknowledged_by.is_some());

        alert.resolve();
        assert_eq!(alert.status, AlertStatus::Resolved);
        assert!(alert.resolved_at.is_some());
    }

    #[tokio::test]
    async fn test_alert_manager() {
        let manager = AlertManager::new();

        // Add a rule
        let rule = AlertRule::new("test", "test_metric", AlertSeverity::Warning)
            .with_threshold(100.0)
            .with_operator(AlertOperator::GreaterThan);
        manager.add_rule(rule).await;

        // Evaluate
        let mut metrics = HashMap::new();
        metrics.insert("test_metric".to_string(), 150.0);

        let alerts = manager.evaluate_rules(&metrics).await;
        assert_eq!(alerts.len(), 1);

        // Check active alerts
        let active = manager.get_active_alerts().await;
        assert_eq!(active.len(), 1);

        // Acknowledge
        manager.acknowledge_alert(&active[0].id, "admin").await;

        // Check stats
        let stats = manager.get_stats().await;
        assert_eq!(stats.total_active, 1);
    }

    #[tokio::test]
    async fn test_default_rules() {
        let rules = presets::default_rules();
        assert!(!rules.is_empty());
        assert!(rules.iter().all(|r| r.enabled));
    }
}
