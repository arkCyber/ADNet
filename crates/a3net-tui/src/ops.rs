//! Operations dashboard widgets for A3Net node management.
//!
//! Provides high-level TUI components for:
//! - Node status overview
//! - Peer/connection management
//! - Replication monitoring
//! - System metrics display
//! - Log viewer with filtering

use crate::color::{Color, StyledStr};
use crate::box_drawing::{Box as Panel};
use crate::progress::human_bytes;
use chrono::{DateTime, Utc};

/// Node connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connected,
    Connecting,
    Disconnected,
    Error,
}

impl ConnectionStatus {
    /// Get the display color for this status.
    pub fn color(&self) -> Color {
        match self {
            ConnectionStatus::Connected => Color::Green,
            ConnectionStatus::Connecting => Color::Yellow,
            ConnectionStatus::Disconnected => Color::Dim,
            ConnectionStatus::Error => Color::Red,
        }
    }

    /// Get the display text for this status.
    pub fn text(&self) -> &'static str {
        match self {
            ConnectionStatus::Connected => "Connected",
            ConnectionStatus::Connecting => "Connecting",
            ConnectionStatus::Disconnected => "Disconnected",
            ConnectionStatus::Error => "Error",
        }
    }
}

/// Peer information for display.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub id: String,
    pub short_id: String,
    pub role: String,
    pub latency_ms: Option<u32>,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub connected_at: Option<DateTime<Utc>>,
    pub status: ConnectionStatus,
}

impl PeerInfo {
    /// Create a new peer info with minimal data.
    pub fn new(id: &str) -> Self {
        let short_id = if id.len() > 12 {
            format!("{}…", &id[..8])
        } else {
            id.to_string()
        };
        Self {
            id: id.to_string(),
            short_id,
            role: "peer".to_string(),
            latency_ms: None,
            bytes_sent: 0,
            bytes_recv: 0,
            connected_at: None,
            status: ConnectionStatus::Connected,
        }
    }

    /// Set the peer role.
    pub fn role(mut self, role: &str) -> Self {
        self.role = role.to_string();
        self
    }

    /// Set the latency.
    pub fn latency(mut self, ms: u32) -> Self {
        self.latency_ms = Some(ms);
        self
    }

    /// Set transfer statistics.
    pub fn transfer(mut self, sent: u64, recv: u64) -> Self {
        self.bytes_sent = sent;
        self.bytes_recv = recv;
        self
    }

    /// Set connection time.
    pub fn connected_since(mut self, time: DateTime<Utc>) -> Self {
        self.connected_at = Some(time);
        self
    }

    /// Set the connection status.
    pub fn status(mut self, status: ConnectionStatus) -> Self {
        self.status = status;
        self
    }

    /// Format latency for display.
    pub fn formatted_latency(&self) -> String {
        match self.latency_ms {
            Some(ms) => format!("{}ms", ms),
            None => "N/A".to_string(),
        }
    }
}

/// Replication status information.
#[derive(Debug, Clone, Default)]
pub struct ReplicationStatus {
    pub factor: u8,
    pub sweeps_total: u64,
    pub blocks_pushed_total: u64,
    pub push_errors_total: u64,
    pub under_replicated_blocks: u64,
    pub fully_replicated_blocks: u64,
}

impl ReplicationStatus {
    /// Check if replication is healthy.
    pub fn is_healthy(&self) -> bool {
        self.push_errors_total == 0 && self.under_replicated_blocks == 0
    }

    /// Get health status color.
    pub fn health_color(&self) -> Color {
        if self.push_errors_total > 0 || self.under_replicated_blocks > 10 {
            Color::Red
        } else if self.under_replicated_blocks > 0 {
            Color::Yellow
        } else {
            Color::Green
        }
    }
}

/// Storage usage information.
#[derive(Debug, Clone, Default)]
pub struct StorageUsage {
    pub private_used: u64,
    pub private_cap: u64,
    pub private_blobs: u64,
    pub shared_used: u64,
    pub shared_cap: u64,
    pub shared_blobs: u64,
}

impl StorageUsage {
    /// Calculate usage percentage.
    pub fn private_percent(&self) -> f64 {
        if self.private_cap == 0 {
            0.0
        } else {
            self.private_used as f64 / self.private_cap as f64
        }
    }

    /// Calculate shared usage percentage.
    pub fn shared_percent(&self) -> f64 {
        if self.shared_cap == 0 {
            0.0
        } else {
            self.shared_used as f64 / self.shared_cap as f64
        }
    }

    /// Get color based on usage level.
    pub fn usage_color(percent: f64) -> Color {
        if percent >= 0.9 {
            Color::Red
        } else if percent >= 0.7 {
            Color::Yellow
        } else {
            Color::Green
        }
    }
}

/// Alert/warning information.
#[derive(Debug, Clone)]
pub struct Alert {
    pub level: AlertLevel,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertLevel {
    Critical,
    Warning,
    Info,
}

impl Alert {
    /// Create a new alert.
    pub fn new(level: AlertLevel, code: &str, message: &str) -> Self {
        Self {
            level,
            code: code.to_string(),
            message: message.to_string(),
        }
    }

    /// Get icon for this alert level.
    pub fn icon(&self) -> &'static str {
        match self.level {
            AlertLevel::Critical => "⛔",
            AlertLevel::Warning => "⚠",
            AlertLevel::Info => "ℹ",
        }
    }

    /// Get color for this alert level.
    pub fn color(&self) -> Color {
        match self.level {
            AlertLevel::Critical => Color::Red,
            AlertLevel::Warning => Color::Yellow,
            AlertLevel::Info => Color::Cyan,
        }
    }
}

/// System metrics snapshot.
#[derive(Debug, Clone, Default)]
pub struct SystemMetrics {
    pub cpu_percent: f32,
    pub memory_used: u64,
    pub memory_total: u64,
    pub uptime_seconds: u64,
    pub network_sent: u64,
    pub network_recv: u64,
}

impl SystemMetrics {
    /// Calculate memory usage percentage.
    pub fn memory_percent(&self) -> f64 {
        if self.memory_total == 0 {
            0.0
        } else {
            self.memory_used as f64 / self.memory_total as f64
        }
    }

    /// Format uptime as human-readable string.
    pub fn formatted_uptime(&self) -> String {
        let secs = self.uptime_seconds;
        if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m {}s", secs / 60, secs % 60)
        } else if secs < 86400 {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        } else {
            format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
        }
    }
}

/// Render a node status panel.
pub fn node_status_panel(
    node_id: &str,
    status: ConnectionStatus,
    peer_count: usize,
    uptime_seconds: u64,
) -> String {
    let status_colored = status.color().paint(status.text());

    let panel = Panel::with_title("Node Status")
        .header_color(Color::Cyan)
        .add_field("Node ID", node_id)
        .add_field("Status", status_colored)
        .add_field("Peers", peer_count.to_string())
        .add_field("Uptime", format_uptime(uptime_seconds));

    panel.render()
}

/// Render a storage usage panel.
pub fn storage_panel(usage: &StorageUsage) -> String {
    let private_pct = usage.private_percent();
    let shared_pct = usage.shared_percent();
    
    let private_bar = usage_bar(private_pct);
    let shared_bar = usage_bar(shared_pct);
    
    let private_colored = StorageUsage::usage_color(private_pct).paint(&private_bar);
    let shared_colored = StorageUsage::usage_color(shared_pct).paint(&shared_bar);

    let panel = Panel::with_title("Storage")
        .header_color(Color::Cyan)
        .add_field(
            "Private",
            format!("{} / {} ({}%)", 
                human_bytes(usage.private_used),
                human_bytes(usage.private_cap),
                (private_pct * 100.0) as u32
            )
        )
        .add_field("Private Usage", private_colored)
        .add_field(
            "Shared",
            format!("{} / {} ({}%)",
                human_bytes(usage.shared_used),
                human_bytes(usage.shared_cap),
                (shared_pct * 100.0) as u32
            )
        )
        .add_field("Shared Usage", shared_colored);

    panel.render()
}

/// Render a replication status panel.
pub fn replication_panel(status: &ReplicationStatus) -> String {
    let health_indicator = if status.is_healthy() {
        Color::Green.paint("Healthy")
    } else {
        Color::Red.paint("Issues Detected")
    };

    let panel = Panel::with_title("Replication")
        .header_color(Color::Cyan)
        .add_field("Factor", status.factor.to_string())
        .add_field("Sweeps", status.sweeps_total.to_string())
        .add_field("Blocks Pushed", status.blocks_pushed_total.to_string())
        .add_field("Errors", status.push_errors_total.to_string())
        .add_field("Under-replicated", status.under_replicated_blocks.to_string())
        .add_field("Health", health_indicator);

    panel.render()
}

/// Render a peer list table.
pub fn peer_list(peers: &[PeerInfo]) -> String {
    if peers.is_empty() {
        return Panel::with_title("Peers")
            .add_field("Status", "No peers connected")
            .render();
    }

    let mut lines = Vec::new();
    
    // Header
    lines.push(format!("{} Peers", peers.len()));
    lines.push("─".repeat(60));
    
    for peer in peers {
        let status_icon = match peer.status {
            ConnectionStatus::Connected => Color::Green.paint("●"),
            ConnectionStatus::Connecting => Color::Yellow.paint("◐"),
            ConnectionStatus::Disconnected => Color::Dim.paint("○"),
            ConnectionStatus::Error => Color::Red.paint("✗"),
        };
        
        let line = format!(
            "{} {} {}  {}  ↑{} ↓{}",
            status_icon,
            peer.colored_short_id(),
            peer.role,
            peer.formatted_latency(),
            human_bytes(peer.bytes_sent),
            human_bytes(peer.bytes_recv)
        );
        lines.push(line);
    }

    lines.join("\n")
}

impl PeerInfo {
    /// Get a colored short ID.
    pub fn colored_short_id(&self) -> StyledStr {
        self.status.color().paint(&self.short_id)
    }
}

/// Render alerts list.
pub fn alerts_panel(alerts: &[Alert]) -> String {
    if alerts.is_empty() {
        return Color::Green.paint("✓ No alerts").to_string();
    }

    let mut lines = Vec::new();
    lines.push(Color::Cyan.paint("Alerts").to_string());
    lines.push("─".repeat(40));

    for alert in alerts {
        let icon = alert.color().paint(alert.icon());
        let level = alert.color().paint(format!("{:?}", alert.level));
        lines.push(format!("{} [{}] {}", icon, level, alert.message));
    }

    lines.join("\n")
}

/// Render system metrics panel.
pub fn metrics_panel(metrics: &SystemMetrics) -> String {
    let mem_pct = metrics.memory_percent();
    let mem_color = StorageUsage::usage_color(mem_pct);

    let panel = Panel::with_title("System")
        .header_color(Color::Cyan)
        .add_field("CPU", format!("{:.1}%", metrics.cpu_percent))
        .add_field(
            "Memory",
            format!("{} / {}", human_bytes(metrics.memory_used), human_bytes(metrics.memory_total))
        )
        .add_field("Memory Usage", mem_color.paint(format!("{:.0}%", mem_pct * 100.0)))
        .add_field("Uptime", metrics.formatted_uptime())
        .add_field("Network ↑", human_bytes(metrics.network_sent))
        .add_field("Network ↓", human_bytes(metrics.network_recv));

    panel.render()
}

/// Render a mini progress bar for inline use.
pub fn mini_bar(percent: f64, width: usize) -> StyledStr {
    let filled = (percent.clamp(0.0, 1.0) * width as f64) as usize;
    let empty = width - filled;
    
    let bar: StyledStr = StyledStr::plain(&format!(
        "[{}{}]",
        "█".repeat(filled),
        "░".repeat(empty)
    ));

    StorageUsage::usage_color(percent).paint(bar.ansi())
}

/// Render an inline storage indicator.
pub fn usage_bar(percent: f64) -> String {
    mini_bar(percent, 20).ansi()
}

/// Format uptime from seconds.
pub fn format_uptime(seconds: u64) -> String {
    let secs = seconds;
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

/// Render a bandwidth meter with rate.
pub fn bandwidth_meter(sent: u64, recv: u64, sent_rate: u64, recv_rate: u64) -> String {
    let panel = Panel::with_title("Bandwidth")
        .header_color(Color::Cyan)
        .add_field("Total Sent", human_bytes(sent))
        .add_field("Send Rate", format!("{}/s", human_bytes(sent_rate)))
        .add_field("Total Received", human_bytes(recv))
        .add_field("Receive Rate", format!("{}/s", human_bytes(recv_rate)));

    panel.render()
}

/// Render a connection details panel.
pub fn connection_panel(
    local_addr: &str,
    remote_addr: Option<&str>,
    status: ConnectionStatus,
    connected_at: Option<DateTime<Utc>>,
) -> String {
    let duration = connected_at.map(|t| {
        let duration = Utc::now() - t;
        format_duration(duration.num_seconds())
    }).unwrap_or_else(|| "N/A".to_string());

    let panel = Panel::with_title("Connection")
        .header_color(Color::Cyan)
        .add_field("Local", local_addr)
        .add_field("Remote", remote_addr.unwrap_or("N/A"))
        .add_field("Status", status.color().paint(status.text()))
        .add_field("Duration", duration);

    panel.render()
}

/// Format duration in seconds to human readable.
fn format_duration(seconds: i64) -> String {
    if seconds < 60 {
        format!("{}s", seconds)
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else if seconds < 86400 {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    } else {
        format!("{}d {}h", seconds / 86400, (seconds % 86400) / 3600)
    }
}

/// Render a dashboard combining multiple panels.
pub fn ops_dashboard(
    node_id: &str,
    connection_status: ConnectionStatus,
    peer_count: usize,
    storage: &StorageUsage,
    replication: &ReplicationStatus,
    alerts: &[Alert],
) -> String {
    let mut lines = Vec::new();

    // Header
    lines.push("╔══════════════════════════════════════════════════════════════╗".to_string());
    lines.push(format!(
        "║{:^64}║",
        Color::Cyan.paint("A3Net Operations Dashboard").to_string()
    ));
    lines.push("╚══════════════════════════════════════════════════════════════╝".to_string());
    lines.push(String::new());

    // Node Status
    lines.push(node_status_panel(node_id, connection_status, peer_count, 0));
    lines.push(String::new());

    // Storage
    lines.push(storage_panel(storage));
    lines.push(String::new());

    // Replication
    lines.push(replication_panel(replication));
    lines.push(String::new());

    // Alerts
    lines.push(alerts_panel(alerts));

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_status_display() {
        assert_eq!(ConnectionStatus::Connected.text(), "Connected");
        assert_eq!(ConnectionStatus::Disconnected.text(), "Disconnected");
        assert_eq!(ConnectionStatus::Error.text(), "Error");
    }

    #[test]
    fn test_peer_info() {
        let peer = PeerInfo::new("12D3KooWabcdefghijklmnopqrstuvwxyz1234567890")
            .role("relay")
            .latency(42);
        
        assert!(peer.short_id.contains("…"));
        assert_eq!(peer.role, "relay");
        assert_eq!(peer.latency_ms, Some(42));
    }

    #[test]
    fn test_storage_usage_percent() {
        let mut usage = StorageUsage::default();
        usage.private_used = 500;
        usage.private_cap = 1000;
        
        assert!((usage.private_percent() - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_replication_health() {
        let healthy = ReplicationStatus {
            factor: 3,
            push_errors_total: 0,
            under_replicated_blocks: 0,
            ..Default::default()
        };
        assert!(healthy.is_healthy());

        let unhealthy = ReplicationStatus {
            factor: 3,
            push_errors_total: 5,
            under_replicated_blocks: 0,
            ..Default::default()
        };
        assert!(!unhealthy.is_healthy());
    }

    #[test]
    fn test_format_uptime() {
        assert_eq!(format_uptime(30), "30s");
        assert_eq!(format_uptime(90), "1m 30s");
        assert_eq!(format_uptime(3661), "1h 1m");
        assert_eq!(format_uptime(90061), "1d 1h");
    }

    #[test]
    fn test_mini_bar() {
        let bar = mini_bar(0.5, 10);
        let ansi = bar.ansi();
        assert!(ansi.contains('█'));
        assert!(ansi.contains('░'));
    }

    #[test]
    fn test_alert() {
        let alert = Alert::new(AlertLevel::Critical, "E001", "Disk full");
        assert_eq!(alert.icon(), "⛔");
        assert_eq!(alert.message, "Disk full");
    }

    #[test]
    fn test_system_metrics_uptime() {
        let metrics = SystemMetrics {
            uptime_seconds: 3661,
            ..Default::default()
        };
        assert_eq!(metrics.formatted_uptime(), "1h 1m");
    }

    // ────────────────────────────────────────────────────────────
    //  Log viewer, network topology, task progress tests
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_log_level_color_distinct() {
        assert_ne!(LogLevel::Error.color(), LogLevel::Info.color());
        assert_ne!(LogLevel::Warn.color(), LogLevel::Debug.color());
    }

    #[test]
    fn test_log_entry_short_message() {
        let entry = LogEntry::new(LogLevel::Info, "INFO", "hello");
        assert_eq!(entry.short(), "hello");
        let long_msg = "x".repeat(50);
        let entry = LogEntry::new(LogLevel::Info, "INFO", &long_msg);
        let s = entry.short();
        assert!(s.len() <= 30);
        assert!(s.ends_with("…"));
    }

    #[test]
    fn test_log_viewer_filter_levels() {
        let entries = vec![
            LogEntry::new(LogLevel::Error, "ERR", "e1"),
            LogEntry::new(LogLevel::Warn, "WARN", "w1"),
            LogEntry::new(LogLevel::Info, "INFO", "i1"),
            LogEntry::new(LogLevel::Debug, "DEBUG", "d1"),
        ];
        let mut viewer = LogViewer::new(entries);
        viewer.set_min_level(LogLevel::Warn);
        assert_eq!(viewer.filtered_len(), 2);

        let mut viewer = LogViewer::new(vec![]);
        viewer.add(LogEntry::new(LogLevel::Error, "ERR", "late"));
        assert_eq!(viewer.filtered_len(), 1);
    }

    #[test]
    fn test_log_viewer_keyword_filter() {
        let entries = vec![
            LogEntry::new(LogLevel::Info, "INFO", "replication sweep started"),
            LogEntry::new(LogLevel::Info, "INFO", "block imported"),
            LogEntry::new(LogLevel::Warn, "WARN", "push error timeout"),
        ];
        let mut viewer = LogViewer::new(entries);
        viewer.set_keyword(Some("push"));
        assert_eq!(viewer.filtered_len(), 1);
    }

    #[test]
    fn test_log_viewer_render_renders_filtered_entries() {
        let entries = vec![
            LogEntry::new(LogLevel::Error, "ERR", "something failed"),
            LogEntry::new(LogLevel::Info, "INFO", "all good"),
        ];
        let mut viewer = LogViewer::new(entries);
        viewer.set_min_level(LogLevel::Error);
        let out = viewer.render();
        assert!(out.contains("something failed"));
        assert!(!out.contains("all good"));
    }

    #[test]
    fn test_topology_node_link_create() {
        let n1 = TopologyNode::new("node1", "relay");
        let n2 = TopologyNode::new("node2", "peer");
        let mut topo = NetworkTopology::new("hub");
        topo.add_node(n1);
        topo.add_node(n2);
        let link = TopologyLink::new("node1", "node2", LinkQuality::Good);
        topo.add_link(link);
        assert_eq!(topo.nodes.len(), 2);
        assert_eq!(topo.links.len(), 1);
    }

    #[test]
    fn test_topology_node_counts_by_role() {
        let mut topo = NetworkTopology::new("hub");
        topo.add_node(TopologyNode::new("a", "relay"));
        topo.add_node(TopologyNode::new("b", "relay"));
        topo.add_node(TopologyNode::new("c", "peer"));
        assert_eq!(topo.node_count("relay"), 2);
        assert_eq!(topo.node_count("peer"), 1);
        assert_eq!(topo.node_count("exit"), 0);
    }

    #[test]
    fn test_topology_render_includes_roles() {
        // Topology with hub at center, two spokes attached via hub.
        let mut topo = NetworkTopology::new("hub");
        topo.add_node(TopologyNode::new("r1", "relay"));
        topo.add_node(TopologyNode::new("p1", "peer"));
        topo.add_link(TopologyLink::new("hub", "r1", LinkQuality::Good));
        topo.add_link(TopologyLink::new("hub", "p1", LinkQuality::Good));
        let out = topo.render();
        assert!(out.contains("r1"));
        assert!(out.contains("p1"));
        assert!(out.contains("relay"));
        assert!(out.contains("peer"));
    }

    #[test]
    fn test_link_quality_color() {
        assert_ne!(LinkQuality::Good.color(), LinkQuality::Poor.color());
        assert_ne!(LinkQuality::Poor.color(), LinkQuality::Unknown.color());
    }

    #[test]
    fn test_task_progress_set_and_complete() {
        let mut task = TaskProgress::new("import", 100);
        task.update(50);
        assert!(!task.is_complete());
        assert!((task.percent() - 0.5).abs() < 0.01);

        task.update(100);
        assert!(task.is_complete());
    }

    #[test]
    fn test_task_progress_zero_total() {
        let task = TaskProgress::new("noop", 0);
        assert_eq!(task.percent(), 0.0);
    }

    #[test]
    fn test_task_progress_eta_calculation() {
        let mut task = TaskProgress::new("import", 100);
        task.update(25);
        // No elapsed yet → no ETA.
        assert_eq!(task.eta_seconds(), None);
        task.elapsed_seconds = 100;
        let eta = task.eta_seconds().unwrap();
        // 75 remaining at 0.25 units/s = 300s
        assert!((eta - 300.0).abs() < 1.0);
    }

    #[test]
    fn test_task_dashboard_renders_all_tasks() {
        let mut dash = TaskDashboard::new();
        let mut t1 = TaskProgress::new("import", 100);
        t1.update(50);
        let mut t2 = TaskProgress::new("gc", 200);
        t2.update(200);
        dash.add(t1);
        dash.add(t2);
        let out = dash.render();
        assert!(out.contains("import"));
        assert!(out.contains("gc"));
    }

    #[test]
    fn test_task_progress_render_uses_mini_bar() {
        let mut task = TaskProgress::new("import", 100);
        task.update(75);
        let out = task.render();
        assert!(out.contains("import"));
        assert!(out.contains("75"));
    }

    #[test]
    fn test_log_viewer_default_min_level_info() {
        let viewer = LogViewer::new(vec![]);
        // Without Debug entries visible.
        let debug_entry = LogEntry::new(LogLevel::Debug, "DBG", "trace");
        let info_entry = LogEntry::new(LogLevel::Info, "INFO", "msg");
        let entries = vec![debug_entry.clone(), info_entry.clone()];
        let mut v = LogViewer::new(entries);
        // Default min level = Info → Debug hidden.
        assert_eq!(v.filtered_len(), 1);
        v.add(debug_entry);
        v.set_min_level(LogLevel::Debug);
        assert_eq!(v.filtered_len(), 3);
    }

    // ────────────────────────────────────────────────────────────
    //  Backup widget tests
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_backup_phase_color_distinct() {
        assert_ne!(BackupPhase::Idle.color(), BackupPhase::Failed.color());
        assert_ne!(BackupPhase::Complete.color(), BackupPhase::Encrypting.color());
    }

    #[test]
    fn test_backup_progress_percent() {
        let mut p = BackupProgress::default();
        p.bytes_total = 1000;
        p.bytes_processed = 250;
        assert!((p.percent() - 0.25).abs() < 0.01);
    }

    #[test]
    fn test_backup_progress_zero_total_safe() {
        let p = BackupProgress::default();
        assert_eq!(p.percent(), 0.0);
    }

    #[test]
    fn test_backup_progress_eta_calculation() {
        let mut p = BackupProgress::default();
        p.bytes_total = 1000;
        p.bytes_processed = 250;
        p.elapsed_seconds = 10;
        let eta = p.eta_seconds().unwrap();
        // 750 remaining at 25 bytes/s = 30s
        assert!((eta - 30.0).abs() < 0.1);
    }

    #[test]
    fn test_backup_progress_render_includes_phase() {
        let mut p = BackupProgress::default();
        p.phase = BackupPhase::Compressing;
        p.bytes_total = 100;
        p.bytes_processed = 50;
        let out = p.render();
        assert!(out.contains("compressing"));
        assert!(out.contains("50%"));
    }

    // ────────────────────────────────────────────────────────────
    //  Reputation widget tests
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_peer_reputation_score_color() {
        let high = PeerReputation::new("a", 80.0);
        let low = PeerReputation::new("b", -80.0);
        let mid = PeerReputation::new("c", 20.0);
        assert_ne!(high.score_color(), low.score_color());
        assert_ne!(high.score_color(), mid.score_color());
    }

    #[test]
    fn test_peer_reputation_trust_label() {
        let p = PeerReputation::new("a", 0.0).trust_level(2);
        assert_eq!(p.trust_label(), "friend");
        let p = PeerReputation::new("b", 0.0).trust_level(-3);
        assert_eq!(p.trust_label(), "blocked");
    }

    #[test]
    fn test_peer_reputation_score_gauge_clamped() {
        let high = PeerReputation::new("a", 200.0);
        assert_eq!(high.score_gauge(), 1.0);
        let low = PeerReputation::new("b", -200.0);
        assert_eq!(low.score_gauge(), 0.0);
    }

    #[test]
    fn test_reputation_list_renders_all() {
        let peers = vec![
            PeerReputation::new("peer-a", 50.0).trust_level(2),
            PeerReputation::new("peer-b", -30.0).trust_level(-1),
        ];
        let out = reputation_list(&peers);
        assert!(out.contains("peer-a"));
        assert!(out.contains("peer-b"));
    }

    #[test]
    fn test_reputation_list_empty() {
        let out = reputation_list(&[]);
        assert!(out.contains("no reputation"));
    }

    #[test]
    fn test_reputation_detail_includes_score() {
        let peer = PeerReputation::new("peer-x", 75.5).trust_level(3);
        let out = reputation_detail(&peer);
        assert!(out.contains("+75.5"));
        assert!(out.contains("trusted"));
    }

    // ────────────────────────────────────────────────────────────
    //  Gossip topics widget tests
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_gossip_topic_counts() {
        let topic = GossipTopic::new("room:lobby")
            .peers(12)
            .counters(100, 250);
        assert_eq!(topic.peers, 12);
        assert_eq!(topic.messages_sent, 100);
        assert_eq!(topic.messages_received, 250);
        assert_eq!(topic.activity_per_min(), 350.0);
    }

    #[test]
    fn test_gossip_topics_empty() {
        let out = gossip_topics(&[]);
        assert!(out.contains("no subscribed"));
    }

    #[test]
    fn test_gossip_topics_renders_table() {
        let topics = vec![
            GossipTopic::new("room:lobby").peers(5).counters(10, 20),
            GossipTopic::new("room:files").peers(2).counters(0, 0),
        ];
        let out = gossip_topics(&topics);
        assert!(out.contains("room:lobby"));
        assert!(out.contains("room:files"));
    }

    // ────────────────────────────────────────────────────────────
    //  Bandwidth sparkline tests
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_bandwidth_sparkline_push_and_stats() {
        let mut sp = BandwidthSparkline::new("up");
        sp.push(100);
        sp.push(200);
        sp.push(300);
        assert_eq!(sp.peak(), 300);
        assert_eq!(sp.current(), 300);
        assert_eq!(sp.average() as u64, 200);
    }

    #[test]
    fn test_bandwidth_sparkline_empty() {
        let sp = BandwidthSparkline::new("down");
        let out = sp.render(20);
        assert!(out.contains("no samples"));
    }

    #[test]
    fn test_bandwidth_sparkline_render_shows_unicode_bars() {
        let mut sp = BandwidthSparkline::new("test");
        for v in [10, 50, 100, 500, 1000] {
            sp.push(v);
        }
        let out = sp.render(20);
        assert!(out.contains("test"));
        // Unicode block chars.
        assert!(out.chars().any(|c| "▁▂▃▄▅▆▇█".contains(c)));
    }

    #[test]
    fn test_bandwidth_dashboard_renders_both() {
        let mut up = BandwidthSparkline::new("up");
        up.push(1000);
        let mut down = BandwidthSparkline::new("down");
        down.push(2000);
        let out = bandwidth_dashboard(&up, &down, 10);
        assert!(out.contains("up"));
        assert!(out.contains("down"));
    }

    // ────────────────────────────────────────────────────────────
    //  Menu widget tests
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_menu_navigation() {
        let mut menu = Menu::new("Main")
            .item('s', "Status", "Show node status")
            .item('l', "Logs", "View logs")
            .item('q', "Quit", "Exit");
        assert_eq!(menu.selected, 0);
        menu.down();
        assert_eq!(menu.selected, 1);
        menu.down();
        assert_eq!(menu.selected, 2);
        menu.down();  // should clamp
        assert_eq!(menu.selected, 2);
        menu.up();
        menu.up();
        assert_eq!(menu.selected, 0);
        menu.up();  // should clamp
        assert_eq!(menu.selected, 0);
    }

    #[test]
    fn test_menu_current_item() {
        let menu = Menu::new("Main")
            .item('a', "A", "first")
            .item('b', "B", "second");
        assert_eq!(menu.current().unwrap().key, 'a');
    }

    #[test]
    fn test_menu_render_shows_keys_and_labels() {
        let menu = Menu::new("Main")
            .item('s', "Status", "Show status");
        let out = menu.render();
        assert!(out.contains("[s]"));
        assert!(out.contains("Status"));
        assert!(out.contains("Show status"));
    }

    // ────────────────────────────────────────────────────────────
    //  Command palette tests
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_palette_filter_by_name() {
        let mut palette = CommandPalette::new()
            .register(CommandEntry { name: "status", shortcut: "Ctrl+S", description: "Show status", category: "view" })
            .register(CommandEntry { name: "logs", shortcut: "Ctrl+L", description: "View logs", category: "view" });
        palette.set_filter("stat");
        let f = palette.filtered();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].name, "status");
    }

    #[test]
    fn test_palette_filter_by_category() {
        let mut palette = CommandPalette::new()
            .register(CommandEntry { name: "alpha", shortcut: "a", description: "d", category: "ops" })
            .register(CommandEntry { name: "beta", shortcut: "b", description: "d", category: "view" });
        palette.set_filter("ops");
        assert_eq!(palette.filtered().len(), 1);
    }

    #[test]
    fn test_palette_empty_filter_shows_all() {
        let palette = CommandPalette::new()
            .register(CommandEntry { name: "a", shortcut: "a", description: "a", category: "x" })
            .register(CommandEntry { name: "b", shortcut: "b", description: "b", category: "x" });
        assert_eq!(palette.filtered().len(), 2);
    }

    #[test]
    fn test_palette_no_match() {
        let mut palette = CommandPalette::new()
            .register(CommandEntry { name: "alpha", shortcut: "a", description: "d", category: "x" });
        palette.set_filter("zzz");
        assert!(palette.filtered().is_empty());
        assert!(palette.render().contains("no commands match"));
    }

    // ────────────────────────────────────────────────────────────
    //  Badge and health summary tests
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_badge_color_distinct() {
        assert_ne!(Badge::Ok.color(), Badge::Critical.color());
        assert_ne!(Badge::Encrypted.color(), Badge::Sealed.color());
    }

    #[test]
    fn test_badge_render_shows_text() {
        assert!(Badge::Ok.render().contains("OK"));
        assert!(Badge::Critical.render().contains("CRIT"));
    }

    #[test]
    fn test_badges_row_joins_with_spaces() {
        let row = badges_row(&[Badge::Ok, Badge::Encrypted, Badge::Sealed]);
        assert!(row.contains("[OK]"));
        assert!(row.contains("[ENC]"));
        assert!(row.contains("[SEAL]"));
    }

    #[test]
    fn test_health_summary_overall_ok() {
        let s = HealthSummary::new()
            .check("storage", Badge::Ok)
            .check("peers", Badge::Ok);
        assert_eq!(s.overall(), Badge::Ok);
    }

    #[test]
    fn test_health_summary_overall_warn() {
        let s = HealthSummary::new()
            .check("storage", Badge::Ok)
            .check("peers", Badge::Warn);
        assert_eq!(s.overall(), Badge::Warn);
    }

    #[test]
    fn test_health_summary_overall_critical() {
        let s = HealthSummary::new()
            .check("storage", Badge::Warn)
            .check("data-integrity", Badge::Critical);
        assert_eq!(s.overall(), Badge::Critical);
    }

    #[test]
    fn test_health_summary_render_includes_checks() {
        let s = HealthSummary::new()
            .check("storage", Badge::Ok)
            .check_with_detail("replication", Badge::Warn, "3 push errors");
        let out = s.render();
        assert!(out.contains("storage"));
        assert!(out.contains("replication"));
        assert!(out.contains("3 push errors"));
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("short", 10), "short");
        let long = "abcdefghijklmnopqrstuvwxyz";
        let t = truncate_str(long, 10);
        assert!(t.chars().count() <= 10);
        assert!(t.ends_with('…'));
    }

    // ────────────────────────────────────────────────────────────
    //  Quota / Feed / JSON / Diff / Crumbs / Jobs tests
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_quota_allocation_fraction_used() {
        let alloc = QuotaAllocation {
            scope: "private",
            fraction: 0.5,
            bytes_used: 750,
            bytes_budget: 1000,
            hard_cap_bytes: 1500,
        };
        assert!((alloc.fraction_used() - 0.75).abs() < 0.01);
        assert!(!alloc.over_budget());
        assert!(!alloc.over_hard_cap());
    }

    #[test]
    fn test_quota_allocation_over_budget() {
        let alloc = QuotaAllocation {
            scope: "shared",
            fraction: 0.5,
            bytes_used: 1100,
            bytes_budget: 1000,
            hard_cap_bytes: 1500,
        };
        assert!(alloc.over_budget());
        assert!(!alloc.over_hard_cap());
    }

    #[test]
    fn test_quota_policy_view_shows_scopes() {
        let allocs = vec![
            QuotaAllocation {
                scope: "private",
                fraction: 0.6,
                bytes_used: 50,
                bytes_budget: 600,
                hard_cap_bytes: 1000,
            },
            QuotaAllocation {
                scope: "shared",
                fraction: 0.4,
                bytes_used: 300,
                bytes_budget: 400,
                hard_cap_bytes: 800,
            },
        ];
        let out = quota_policy_view(1_000_000_000, &allocs);
        assert!(out.contains("private"));
        assert!(out.contains("shared"));
        assert!(out.contains("60%"));
        assert!(out.contains("40%"));
    }

    #[test]
    fn test_quota_policy_view_over_hard_cap() {
        let allocs = vec![QuotaAllocation {
            scope: "private",
            fraction: 1.0,
            bytes_used: 1_500_000_000_000,
            bytes_budget: 800_000_000_000,
            hard_cap_bytes: 1_000_000_000_000,
        }];
        let out = quota_policy_view(1_000_000_000_000, &allocs);
        assert!(out.contains("OVER HARD CAP"));
    }

    #[test]
    fn test_feed_entry_short_hash() {
        let entry = FeedEntry::new(
            "abcdef1234567890abcdef",
            "hello world",
            "video",
            1024,
            "peer-a",
            1691740000,
            3,
        );
        // 10 chars + "…" (3-byte UTF-8) = 13 bytes
        assert!(entry.short_hash.chars().count() == 11);
        assert!(entry.short_hash.ends_with('…'));
    }

    #[test]
    fn test_room_feed_view_empty() {
        let out = room_feed_view("lobby", &[]);
        assert!(out.contains("no entries"));
    }

    #[test]
    fn test_room_feed_view_renders_table() {
        let entries = vec![
            FeedEntry::new("hashA", "first", "video", 1024, "peer-a", 1691740001, 2),
            FeedEntry::new("hashB", "second", "image", 2048, "peer-b", 1691740000, 3),
        ];
        let out = room_feed_view("lobby", &entries);
        assert!(out.contains("lobby"));
        assert!(out.contains("first"));
        assert!(out.contains("second"));
        assert!(out.contains("hashA"));
    }

    #[test]
    fn test_json_tree_parse_null() {
        let t = JsonTree::parse("null");
        assert!(matches!(t, JsonTree::Null));
        assert_eq!(t.size(), 1);
    }

    #[test]
    fn test_json_tree_parse_bool() {
        let t = JsonTree::parse("true");
        assert!(matches!(t, JsonTree::Bool(true)));
    }

    #[test]
    fn test_json_tree_parse_number() {
        let t = JsonTree::parse("42.5");
        assert!(matches!(t, JsonTree::Number(n) if (n - 42.5).abs() < 0.01));
    }

    #[test]
    fn test_json_tree_parse_string() {
        let t = JsonTree::parse(r#""hello""#);
        assert!(matches!(t, JsonTree::String(s) if s == "hello"));
    }

    #[test]
    fn test_json_tree_parse_array() {
        let t = JsonTree::parse(r#"[1, 2, "three"]"#);
        if let JsonTree::Array(items) = t {
            assert_eq!(items.len(), 3);
        } else {
            panic!("expected array");
        }
    }

    #[test]
    fn test_json_tree_parse_object() {
        let t = JsonTree::parse(r#"{"a": 1, "b": "two"}"#);
        if let JsonTree::Object(pairs) = t {
            assert_eq!(pairs.len(), 2);
            assert_eq!(pairs[0].0, "a");
            assert_eq!(pairs[1].0, "b");
        } else {
            panic!("expected object");
        }
    }

    #[test]
    fn test_json_tree_size() {
        let t = JsonTree::parse(r#"{"a": [1, 2], "b": {"c": 3}}"#);
        // Implementation: leaves count 1, Array counts 1 + children,
        // Object counts just children (no +1 for the wrapper).
        assert_eq!(t.size(), 6);
    }

    #[test]
    fn test_json_tree_render_includes_braces() {
        let t = JsonTree::parse(r#"{"k": "v"}"#);
        let out = json_tree_view(&t);
        assert!(out.contains("v"));
        assert!(out.contains("k"));
    }

    #[test]
    fn test_diff_kind_symbols() {
        assert_eq!(DiffKind::Added.symbol(), "+");
        assert_eq!(DiffKind::Removed.symbol(), "-");
        assert_eq!(DiffKind::Modified.symbol(), "~");
        assert_eq!(DiffKind::Unchanged.symbol(), " ");
    }

    #[test]
    fn test_diff_string_maps_added() {
        let mut before = std::collections::BTreeMap::new();
        let mut after = std::collections::BTreeMap::new();
        after.insert("a".to_string(), "new".to_string());
        let diffs = diff_string_maps(&before, &after);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, DiffKind::Added);
    }

    #[test]
    fn test_diff_string_maps_removed() {
        let mut before = std::collections::BTreeMap::new();
        let mut after = std::collections::BTreeMap::new();
        before.insert("a".to_string(), "old".to_string());
        let diffs = diff_string_maps(&before, &after);
        assert_eq!(diffs[0].kind, DiffKind::Removed);
    }

    #[test]
    fn test_diff_string_maps_modified() {
        let mut before = std::collections::BTreeMap::new();
        let mut after = std::collections::BTreeMap::new();
        before.insert("a".to_string(), "old".to_string());
        after.insert("a".to_string(), "new".to_string());
        let diffs = diff_string_maps(&before, &after);
        assert_eq!(diffs[0].kind, DiffKind::Modified);
    }

    #[test]
    fn test_diff_string_maps_unchanged() {
        let mut before = std::collections::BTreeMap::new();
        let mut after = std::collections::BTreeMap::new();
        before.insert("a".to_string(), "same".to_string());
        after.insert("a".to_string(), "same".to_string());
        let diffs = diff_string_maps(&before, &after);
        assert_eq!(diffs[0].kind, DiffKind::Unchanged);
    }

    #[test]
    fn test_diff_view_renders_summary() {
        let diffs = vec![
            DiffLine {
                kind: DiffKind::Added,
                path: "x".to_string(),
                before: None,
                after: Some("new".to_string()),
            },
            DiffLine {
                kind: DiffKind::Removed,
                path: "y".to_string(),
                before: Some("old".to_string()),
                after: None,
            },
        ];
        let out = diff_view(&diffs);
        assert!(out.contains("Summary"));
        assert!(out.contains("1 added"));
        assert!(out.contains("1 removed"));
    }

    #[test]
    fn test_breadcrumbs_active_segment_colored() {
        let crumbs = vec![
            Crumb::new("home"),
            Crumb::new("ops"),
            Crumb::new("status").active(),
        ];
        let out = breadcrumbs_view(&crumbs);
        assert!(out.contains("home"));
        assert!(out.contains("ops"));
        assert!(out.contains("status"));
        assert!(out.contains("›"));
    }

    #[test]
    fn test_breadcrumbs_empty() {
        let out = breadcrumbs_view(&[]);
        assert!(out.contains("no path"));
    }

    #[test]
    fn test_scheduled_job_record_run() {
        let mut job = ScheduledJob::new("gc", 300);
        job.record_run(150, true, 1_000);
        assert_eq!(job.runs_total, 1);
        assert_eq!(job.runs_failed, 0);
        assert_eq!(job.last_duration_ms, 150);
        assert_eq!(job.next_run_unix, Some(1_300));
        assert!(!job.is_pending());
    }

    #[test]
    fn test_scheduled_job_failure_rate() {
        let mut job = ScheduledJob::new("replicate", 60);
        job.record_run(100, true, 1);
        job.record_run(100, false, 61);
        job.record_run(100, true, 121);
        job.record_run(100, false, 181);
        assert_eq!(job.runs_total, 4);
        assert_eq!(job.runs_failed, 2);
        assert!((job.failure_rate() - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_scheduled_job_pending_state() {
        let job = ScheduledJob::new("audit", 3600);
        assert!(job.is_pending());
        assert_eq!(job.failure_rate(), 0.0);
    }

    #[test]
    fn test_scheduled_jobs_view_empty() {
        let out = scheduled_jobs_view(&[]);
        assert!(out.contains("no scheduled"));
    }

    #[test]
    fn test_scheduled_jobs_view_renders_table() {
        let mut gc = ScheduledJob::new("gc", 300);
        gc.record_run(150, true, 1_000);
        let mut rep = ScheduledJob::new("replicate", 60);
        rep.record_run(80, false, 1_000);
        let audit = ScheduledJob::new("audit", 3600);
        let jobs = vec![gc, rep, audit];
        let out = scheduled_jobs_view(&jobs);
        assert!(out.contains("gc"));
        assert!(out.contains("replicate"));
        assert!(out.contains("audit"));
        assert!(out.contains("pending"));
    }

    #[test]
    fn test_diff_kind_colors_distinct() {
        assert_ne!(DiffKind::Added.color(), DiffKind::Removed.color());
        assert_ne!(DiffKind::Added.color(), DiffKind::Modified.color());
    }

    // ────────────────────────────────────────────────────────────
    //  Conversation / Message thread / Service / Identity / Timeline
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_conversation_summary_full() {
        let conv = ConversationSummary::new("conv-1", "Team Chat", "group")
            .messages(42, 100)
            .members(5)
            .last_activity(1_700_000_000)
            .unread(3);
        assert_eq!(conv.title, "Team Chat");
        assert_eq!(conv.message_count, 42);
        assert_eq!(conv.unread, 3);
    }

    #[test]
    fn test_conversation_list_empty() {
        let out = conversation_list(&[]);
        assert!(out.contains("no conversations"));
    }

    #[test]
    fn test_conversation_list_renders_table() {
        let convs = vec![
            ConversationSummary::new("a", "Alpha team", "group")
                .messages(10, 5)
                .last_activity(2_000),
            ConversationSummary::new("b", "Bob DM", "dm")
                .messages(50, 30)
                .last_activity(1_000),
        ];
        let out = conversation_list(&convs);
        assert!(out.contains("Alpha team"));
        assert!(out.contains("Bob DM"));
        // Sort: latest activity first.
        assert!(out.find("Alpha team").unwrap() < out.find("Bob DM").unwrap());
    }

    #[test]
    fn test_chat_message_preview() {
        let m = ChatMessage::new("1", "alice", "Hello world!", 1, 1);
        assert_eq!(m.preview(), "Hello world!");
        let long = "a".repeat(60);
        let m2 = ChatMessage::new("2", "bob", &long, 2, 2);
        let p = m2.preview();
        assert!(p.chars().count() <= 41);
        assert!(p.ends_with('…'));
    }

    #[test]
    fn test_message_thread_empty() {
        let out = message_thread(&[], 100);
        assert!(out.contains("no messages"));
    }

    #[test]
    fn test_message_thread_renders() {
        let messages = vec![
            ChatMessage::new("1", "alice", "Hi everyone!", 1_700_000_000, 1),
            ChatMessage::new("2", "bob", "Hello alice", 1_700_000_005, 2)
                .sender("Bob"),
        ];
        let out = message_thread(&messages, 100);
        assert!(out.contains("alice"));
        assert!(out.contains("Bob"));
        assert!(out.contains("Hi everyone!"));
        assert!(out.contains("Hello alice"));
    }

    #[test]
    fn test_wrap_text() {
        let s = "the quick brown fox jumps over the lazy dog";
        let wrapped = wrap_text(s, 10);
        assert!(wrapped.len() > 1);
        for line in &wrapped {
            assert!(line.len() <= 10);
        }
    }

    #[test]
    fn test_service_state_render() {
        assert!(ServiceState::Running.render().contains("running"));
        assert!(ServiceState::Crashed.render().contains("crashed"));
    }

    #[test]
    fn test_service_status_table_summary() {
        let services = vec![
            ServiceEntry::new("relay", ServiceState::Running).address("0.0.0.0:443"),
            ServiceEntry::new("mesh", ServiceState::Stopped),
        ];
        let out = service_status_table(&services);
        assert!(out.contains("relay"));
        assert!(out.contains("mesh"));
        assert!(out.contains("Summary"));
        // 1 of 2 running → yellow
        assert!(out.contains("1/2"));
    }

    #[test]
    fn test_service_status_table_all_running() {
        let services = vec![
            ServiceEntry::new("a", ServiceState::Running),
            ServiceEntry::new("b", ServiceState::Running),
        ];
        let out = service_status_table(&services);
        assert!(out.contains("2/2"));
    }

    #[test]
    fn test_service_status_table_empty() {
        let out = service_status_table(&[]);
        assert!(out.contains("no services"));
    }

    #[test]
    fn test_identity_full_record() {
        let id = IdentityInfo::new("node-1", "ed25519", "pubkey...", "abc:123")
            .created(1_000)
            .expires(2_000)
            .use_for("sign")
            .use_for("encrypt");
        assert_eq!(id.usage.len(), 2);
        assert_eq!(id.remaining_seconds(1_500), Some(500));
    }

    #[test]
    fn test_identity_detail_shows_fields() {
        let id = IdentityInfo::new("node-1", "ed25519", "abc123pubkey", "fingerprint");
        let out = identity_detail(&id);
        assert!(out.contains("node-1"));
        assert!(out.contains("ed25519"));
        assert!(out.contains("fingerprint"));
    }

    #[test]
    fn test_identity_list_renders_table() {
        let ids = vec![
            IdentityInfo::new("node-1", "ed25519", "...", "fp1").use_for("sign"),
            IdentityInfo::new("node-2", "x25519", "...", "fp2").use_for("encrypt"),
        ];
        let out = identity_list(&ids);
        assert!(out.contains("node-1"));
        assert!(out.contains("node-2"));
    }

    #[test]
    fn test_timeline_event_render() {
        let e = TimelineEvent::new(1_700_000_000, AlertLevel::Warning, "STOREFULL", "90% full");
        let out = e.render();
        assert!(out.contains("STOREFULL"));
        assert!(out.contains("90% full"));
    }

    #[test]
    fn test_alert_timeline_empty() {
        let out = alert_timeline(&[]);
        assert!(out.contains("no events"));
    }

    #[test]
    fn test_alert_timeline_sorts_newest_first() {
        let events = vec![
            TimelineEvent::new(1_000, AlertLevel::Info, "EVT1", "old"),
            TimelineEvent::new(3_000, AlertLevel::Critical, "EVT3", "newest"),
            TimelineEvent::new(2_000, AlertLevel::Warning, "EVT2", "middle"),
        ];
        let out = alert_timeline(&events);
        let pos_new = out.find("newest").unwrap();
        let pos_mid = out.find("middle").unwrap();
        let pos_old = out.find("old").unwrap();
        assert!(pos_new < pos_mid);
        assert!(pos_mid < pos_old);
    }

    #[test]
    fn test_alert_timeline_counts_by_level() {
        let events = vec![
            TimelineEvent::new(1, AlertLevel::Info, "i1", ""),
            TimelineEvent::new(2, AlertLevel::Info, "i2", ""),
            TimelineEvent::new(3, AlertLevel::Warning, "w1", ""),
            TimelineEvent::new(4, AlertLevel::Critical, "c1", ""),
        ];
        let out = alert_timeline(&events);
        assert!(out.contains("4 events"));
        assert!(out.contains("2 info"));
        assert!(out.contains("1 warn"));
        assert!(out.contains("1 crit"));
    }

    #[test]
    fn test_service_state_colors_distinct() {
        assert_ne!(ServiceState::Running.color(), ServiceState::Crashed.color());
        assert_ne!(ServiceState::Running.color(), ServiceState::Disabled.color());
    }
}

// ─────────────────────────────────────────────────────────────────────
//  Log viewer (audit: ops need a scrolling log surface)
// ─────────────────────────────────────────────────────────────────────

/// Log severity for the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    #[default]
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl LogLevel {
    /// Get the display text for this level.
    pub fn text(&self) -> &'static str {
        match self {
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }

    /// Get the color for this level.
    pub fn color(&self) -> Color {
        match self {
            LogLevel::Trace => Color::Dim,
            LogLevel::Debug => Color::Cyan,
            LogLevel::Info => Color::White,
            LogLevel::Warn => Color::Yellow,
            LogLevel::Error => Color::Red,
        }
    }

    /// Parse from a typical log level string.
    pub fn parse_label(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "TRACE" => LogLevel::Trace,
            "DEBUG" | "DBG" => LogLevel::Debug,
            "INFO" => LogLevel::Info,
            "WARN" | "WARNING" => LogLevel::Warn,
            "ERROR" | "ERR" | "FATAL" | "CRITICAL" => LogLevel::Error,
            _ => LogLevel::Info,
        }
    }
}

/// One log entry.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
}

impl LogEntry {
    /// Create a new log entry with current timestamp.
    pub fn new(level: LogLevel, target: &str, message: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            level,
            target: target.to_string(),
            message: message.to_string(),
        }
    }

    /// Create a log entry with a specific timestamp.
    pub fn with_time(timestamp: DateTime<Utc>, level: LogLevel, target: &str, message: &str) -> Self {
        Self {
            timestamp,
            level,
            target: target.to_string(),
            message: message.to_string(),
        }
    }

    /// Truncate message for compact display.
    pub fn short(&self) -> String {
        if self.message.len() <= 28 {
            self.message.clone()
        } else {
            format!("{}…", &self.message[..27])
        }
    }

    /// Render as a single line.
    pub fn render(&self) -> String {
        format!(
            "{} {} {} {}",
            self.timestamp.format("%H:%M:%S%.3f"),
            self.level.color().paint(self.level.text()),
            self.target,
            self.message,
        )
    }
}

/// Scrollable log viewer with filtering.
#[derive(Debug, Default)]
pub struct LogViewer {
    entries: Vec<LogEntry>,
    min_level: LogLevel,
    keyword: Option<String>,
    /// Max lines to render (tail).
    max_lines: usize,
}

impl LogViewer {
    /// Create a viewer with the given entries.
    pub fn new(entries: Vec<LogEntry>) -> Self {
        Self {
            entries,
            min_level: LogLevel::Info,
            keyword: None,
            max_lines: 100,
        }
    }

    /// Append a log entry.
    pub fn add(&mut self, entry: LogEntry) {
        self.entries.push(entry);
    }

    /// Set the minimum level to display.
    pub fn set_min_level(&mut self, level: LogLevel) {
        self.min_level = level;
    }

    /// Set a keyword filter (case-insensitive substring match).
    pub fn set_keyword(&mut self, keyword: Option<&str>) {
        self.keyword = keyword.map(|s| s.to_ascii_lowercase());
    }

    /// Set the maximum number of lines to render (tail).
    pub fn set_max_lines(&mut self, n: usize) {
        self.max_lines = n;
    }

    /// Number of entries that pass the current filter.
    pub fn filtered_len(&self) -> usize {
        self.filtered().count()
    }

    /// Iterator over filtered entries.
    pub fn filtered(&self) -> impl Iterator<Item = &LogEntry> {
        self.entries
            .iter()
            .filter(|e| e.level >= self.min_level)
            .filter(|e| match &self.keyword {
                None => true,
                Some(kw) => {
                    let m = e.message.to_ascii_lowercase();
                    let t = e.target.to_ascii_lowercase();
                    m.contains(kw) || t.contains(kw)
                }
            })
    }

    /// Render the viewer.
    pub fn render(&self) -> String {
        let filtered: Vec<&LogEntry> = self.filtered().collect();
        let start = filtered.len().saturating_sub(self.max_lines);

        let mut lines = Vec::new();
        let header = format!(
            "{} {}",
            Color::Cyan.paint("Log Viewer").bold(),
            Color::Dim.paint(format!(
                "(min={}, kw={}, showing {}/{})",
                self.min_level.text(),
                self.keyword.as_deref().unwrap_or("-"),
                filtered.len().saturating_sub(start),
                self.entries.len(),
            ))
        );
        lines.push(header);
        lines.push("─".repeat(80));

        if filtered.is_empty() {
            lines.push(Color::Dim.paint("(no entries match filter)").to_string());
            return lines.join("\n");
        }

        for entry in &filtered[start..] {
            lines.push(entry.render());
        }

        lines.join("\n")
    }
}

// ─────────────────────────────────────────────────────────────────────
//  Network topology visualization
// ─────────────────────────────────────────────────────────────────────

/// Quality of a network link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkQuality {
    Excellent,
    Good,
    Fair,
    Poor,
    Unknown,
}

impl LinkQuality {
    /// Get the color for this link quality.
    pub fn color(&self) -> Color {
        match self {
            LinkQuality::Excellent => Color::Green,
            LinkQuality::Good => Color::Cyan,
            LinkQuality::Fair => Color::Yellow,
            LinkQuality::Poor => Color::Red,
            LinkQuality::Unknown => Color::Dim,
        }
    }

    /// Get the symbol for this link quality.
    pub fn symbol(&self) -> &'static str {
        match self {
            LinkQuality::Excellent => "━",
            LinkQuality::Good => "─",
            LinkQuality::Fair => "·",
            LinkQuality::Poor => "┄",
            LinkQuality::Unknown => "?",
        }
    }
}

/// A node in the network topology graph.
#[derive(Debug, Clone)]
pub struct TopologyNode {
    pub id: String,
    pub role: String,
    pub label: Option<String>,
}

impl TopologyNode {
    /// Create a new topology node.
    pub fn new(id: &str, role: &str) -> Self {
        Self {
            id: id.to_string(),
            role: role.to_string(),
            label: None,
        }
    }

    /// Set the display label.
    pub fn label(mut self, label: &str) -> Self {
        self.label = Some(label.to_string());
        self
    }
}

/// A link between two topology nodes.
#[derive(Debug, Clone)]
pub struct TopologyLink {
    pub from: String,
    pub to: String,
    pub quality: LinkQuality,
    pub label: Option<String>,
}

impl TopologyLink {
    /// Create a new topology link.
    pub fn new(from: &str, to: &str, quality: LinkQuality) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
            quality,
            label: None,
        }
    }

    /// Set the link label (e.g. RTT).
    pub fn label(mut self, label: &str) -> Self {
        self.label = Some(label.to_string());
        self
    }
}

/// A network topology (nodes + links) suitable for TUI rendering.
#[derive(Debug, Default)]
pub struct NetworkTopology {
    pub center: Option<String>,
    pub nodes: Vec<TopologyNode>,
    pub links: Vec<TopologyLink>,
}

impl NetworkTopology {
    /// Create a topology with a center node id.
    pub fn new(center: &str) -> Self {
        Self {
            center: Some(center.to_string()),
            nodes: Vec::new(),
            links: Vec::new(),
        }
    }

    /// Add a node.
    pub fn add_node(&mut self, node: TopologyNode) {
        self.nodes.push(node);
    }

    /// Add a link.
    pub fn add_link(&mut self, link: TopologyLink) {
        self.links.push(link);
    }

    /// Count nodes by role.
    pub fn node_count(&self, role: &str) -> usize {
        self.nodes.iter().filter(|n| n.role == role).count()
    }

    /// Render as a hub-and-spoke ASCII diagram.
    pub fn render(&self) -> String {
        let mut lines = Vec::new();

        // Header.
        let center = self.center.as_deref().unwrap_or("?");
        lines.push(
            Color::Cyan
                .paint(format!("Network Topology (center: {center})"))
                .bold()
                .to_string(),
        );
        lines.push("─".repeat(60));

        // Center node.
        lines.push(format!(
            "        {}",
            Color::Yellow.paint(format!("[{}]", center)).bold()
        ));

        // Spokes (one per link from the center).
        let center_links: Vec<&TopologyLink> = self
            .links
            .iter()
            .filter(|l| l.from == center || l.to == center)
            .collect();

        if center_links.is_empty() {
            lines.push(Color::Dim.paint("        (no peers)").to_string());
        } else {
            // Top connector.
            lines.push("        │".to_string());
            for link in &center_links {
                let other = if link.from == center {
                    &link.to
                } else {
                    &link.from
                };
                let role = self
                    .nodes
                    .iter()
                    .find(|n| n.id == *other)
                    .map(|n| n.role.as_str())
                    .unwrap_or("peer");
                let connector = link.quality.symbol();
                let label = link
                    .label
                    .as_deref()
                    .map(|s| format!(" ({s})"))
                    .unwrap_or_default();
                let colored_connector = link.quality.color().paint(connector);
                lines.push(format!(
                    "   {colored_connector}── {role:8} {other}{label}",
                    colored_connector = colored_connector,
                ));
            }
        }

        // Per-role summary at the bottom.
        let mut by_role: std::collections::BTreeMap<String, usize> = Default::default();
        for n in &self.nodes {
            *by_role.entry(n.role.clone()).or_default() += 1;
        }
        if !by_role.is_empty() {
            lines.push(String::new());
            lines.push(Color::Cyan.paint("Roles:").to_string());
            for (role, count) in &by_role {
                lines.push(format!("  {role}: {count}"));
            }
        }

        lines.join("\n")
    }
}

// ─────────────────────────────────────────────────────────────────────
//  Task / job progress (audit: ops need to see long-running tasks)
// ─────────────────────────────────────────────────────────────────────

/// Progress of a long-running task.
#[derive(Debug, Clone)]
pub struct TaskProgress {
    pub name: String,
    pub total: u64,
    pub current: u64,
    pub elapsed_seconds: u64,
    pub rate_per_sec: f64,
}

impl TaskProgress {
    /// Create a new task with `total` units of work.
    pub fn new(name: &str, total: u64) -> Self {
        Self {
            name: name.to_string(),
            total,
            current: 0,
            elapsed_seconds: 0,
            rate_per_sec: 0.0,
        }
    }

    /// Update progress to `current` units.
    pub fn update(&mut self, current: u64) {
        self.current = current.min(self.total);
    }

    /// Mark the task complete.
    pub fn complete(&mut self) {
        self.current = self.total;
    }

    /// Compute the current progress fraction.
    pub fn percent(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.current as f64 / self.total as f64
        }
    }

    /// Whether the task is done.
    pub fn is_complete(&self) -> bool {
        self.total > 0 && self.current >= self.total
    }

    /// Compute ETA in seconds (best effort).
    pub fn eta_seconds(&self) -> Option<f64> {
        if self.is_complete() || self.elapsed_seconds == 0 {
            return None;
        }
        let rate = self.current as f64 / self.elapsed_seconds as f64;
        if rate <= 0.0 {
            return None;
        }
        let remaining = (self.total - self.current) as f64;
        Some(remaining / rate)
    }

    /// Render this task as a single line.
    pub fn render(&self) -> String {
        let bar = mini_bar(self.percent(), 20);
        let pct = (self.percent() * 100.0) as u32;
        let eta = match self.eta_seconds() {
            Some(s) if s >= 60.0 => format!("{}m", (s / 60.0) as u32),
            Some(s) => format!("{:.0}s", s),
            None => "—".to_string(),
        };
        let status = if self.is_complete() {
            Color::Green.paint("done").to_string()
        } else {
            Color::Yellow.paint("running").to_string()
        };
        format!(
            "{:<16} {} {:>3}% [{}/{}] eta={} {}",
            self.name,
            bar.ansi(),
            pct,
            self.current,
            self.total,
            eta,
            status,
        )
    }
}

/// Multiple tasks shown together.
#[derive(Debug, Default)]
pub struct TaskDashboard {
    tasks: Vec<TaskProgress>,
}

impl TaskDashboard {
    /// Create an empty dashboard.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a task.
    pub fn add(&mut self, task: TaskProgress) {
        self.tasks.push(task);
    }

    /// Number of tracked tasks.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Whether there are no tasks.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Render the dashboard.
    pub fn render(&self) -> String {
        if self.tasks.is_empty() {
            return Color::Dim.paint("(no tasks)").to_string();
        }
        let mut lines = Vec::new();
        lines.push(
            Color::Cyan
                .paint(format!("Tasks ({})", self.tasks.len()))
                .bold()
                .to_string(),
        );
        lines.push("─".repeat(80));
        for t in &self.tasks {
            lines.push(t.render());
        }
        lines.join("\n")
    }
}

// ─────────────────────────────────────────────────────────────────────
//  Backup/restore widget (audit: ops need to monitor incremental backups)
// ─────────────────────────────────────────────────────────────────────

/// Status of a backup or restore operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackupPhase {
    #[default]
    Idle,
    Scanning,
    Compressing,
    Encrypting,
    Uploading,
    Verifying,
    Complete,
    Failed,
}

impl BackupPhase {
    /// Get the display text for this phase.
    pub fn text(&self) -> &'static str {
        match self {
            BackupPhase::Idle => "idle",
            BackupPhase::Scanning => "scanning",
            BackupPhase::Compressing => "compressing",
            BackupPhase::Encrypting => "encrypting",
            BackupPhase::Uploading => "uploading",
            BackupPhase::Verifying => "verifying",
            BackupPhase::Complete => "complete",
            BackupPhase::Failed => "failed",
        }
    }

    /// Get the color for this phase.
    pub fn color(&self) -> Color {
        match self {
            BackupPhase::Idle => Color::Dim,
            BackupPhase::Scanning => Color::Cyan,
            BackupPhase::Compressing => Color::Cyan,
            BackupPhase::Encrypting => Color::Yellow,
            BackupPhase::Uploading => Color::Yellow,
            BackupPhase::Verifying => Color::Cyan,
            BackupPhase::Complete => Color::Green,
            BackupPhase::Failed => Color::Red,
        }
    }
}

/// Backup progress information.
#[derive(Debug, Clone, Default)]
pub struct BackupProgress {
    pub phase: BackupPhase,
    pub bytes_processed: u64,
    pub bytes_total: u64,
    pub files_processed: u64,
    pub files_total: u64,
    pub current_file: Option<String>,
    pub elapsed_seconds: u64,
    /// Chain length for incremental backups (1 = full, >1 = incremental).
    pub chain_length: u64,
    /// Cumulative bytes across the full chain.
    pub cumulative_bytes: u64,
    /// Whether the backup is encrypted.
    pub encrypted: bool,
}

impl BackupProgress {
    /// Compute progress fraction.
    pub fn percent(&self) -> f64 {
        if self.bytes_total == 0 {
            0.0
        } else {
            (self.bytes_processed as f64 / self.bytes_total as f64).clamp(0.0, 1.0)
        }
    }

    /// Compute ETA in seconds (best effort).
    pub fn eta_seconds(&self) -> Option<f64> {
        if self.elapsed_seconds == 0 || self.bytes_processed == 0 {
            return None;
        }
        let rate = self.bytes_processed as f64 / self.elapsed_seconds as f64;
        if rate <= 0.0 {
            return None;
        }
        let remaining = self.bytes_total.saturating_sub(self.bytes_processed) as f64;
        Some(remaining / rate)
    }

    /// Compute transfer rate.
    pub fn rate_bytes_per_sec(&self) -> f64 {
        if self.elapsed_seconds == 0 {
            0.0
        } else {
            self.bytes_processed as f64 / self.elapsed_seconds as f64
        }
    }

    /// Render as a single panel.
    pub fn render(&self) -> String {
        let phase_str = self.phase.color().paint(self.phase.text()).bold();
        let bar = mini_bar(self.percent(), 30);
        let pct = (self.percent() * 100.0) as u32;
        let rate_str = format!("{}/s", human_bytes(self.rate_bytes_per_sec() as u64));
        let eta_str = match self.eta_seconds() {
            Some(s) if s >= 60.0 => format!("{}m {}s", (s / 60.0) as u32, (s % 60.0) as u32),
            Some(s) => format!("{:.0}s", s),
            None => "—".to_string(),
        };
        let encrypted_badge = if self.encrypted {
            Color::Yellow.paint("[encrypted]").to_string()
        } else {
            Color::Dim.paint("[plain]").to_string()
        };

        let lines = vec![
            format!("Phase:    {}", phase_str),
            format!(
                "Progress: {} {}% ({}/{})",
                bar.ansi(),
                pct,
                human_bytes(self.bytes_processed),
                human_bytes(self.bytes_total),
            ),
            format!(
                "Files:    {} / {}",
                self.files_processed, self.files_total
            ),
            format!("Rate:     {}  ETA: {}", rate_str, eta_str),
            format!(
                "Elapsed:  {}  Chain: {}  Cumulative: {}",
                format_uptime(self.elapsed_seconds),
                self.chain_length,
                human_bytes(self.cumulative_bytes),
            ),
            format!("Crypto:   {}", encrypted_badge),
        ];

        let mut panel = Panel::with_title("Backup Progress");
        for line in &lines {
            panel = panel.add_field("", line.clone());
        }
        panel.render()
    }
}

// ─────────────────────────────────────────────────────────────────────
//  Reputation widget (audit: PeerScore -100..+100 and TrustLevel -3..+3)
// ─────────────────────────────────────────────────────────────────────

/// One peer's reputation snapshot.
#[derive(Debug, Clone)]
pub struct PeerReputation {
    pub peer_id: String,
    pub short_id: String,
    pub score: f64,
    pub trust_level: i8,
    pub positive_count: u64,
    pub negative_count: u64,
    pub last_updated_unix: i64,
}

impl PeerReputation {
    /// Create a new reputation record.
    pub fn new(peer_id: &str, score: f64) -> Self {
        let short_id = if peer_id.len() > 12 {
            format!("{}…", &peer_id[..8])
        } else {
            peer_id.to_string()
        };
        Self {
            peer_id: peer_id.to_string(),
            short_id,
            score,
            trust_level: 0,
            positive_count: 0,
            negative_count: 0,
            last_updated_unix: 0,
        }
    }

    /// Set trust level.
    pub fn trust_level(mut self, level: i8) -> Self {
        self.trust_level = level;
        self
    }

    /// Set event counts.
    pub fn counts(mut self, positive: u64, negative: u64) -> Self {
        self.positive_count = positive;
        self.negative_count = negative;
        self
    }

    /// Get color for the current score.
    pub fn score_color(&self) -> Color {
        if self.score >= 50.0 {
            Color::Green
        } else if self.score >= 0.0 {
            Color::Cyan
        } else if self.score >= -50.0 {
            Color::Yellow
        } else {
            Color::Red
        }
    }

    /// Map a score to a textual level.
    pub fn trust_label(&self) -> &'static str {
        match self.trust_level {
            3 => "trusted",
            2 => "friend",
            1 => "known",
            0 => "neutral",
            -1 => "caution",
            -2 => "untrusted",
            -3 => "blocked",
            _ if self.trust_level > 3 => "trusted",
            _ => "blocked",
        }
    }

    /// Compute a mini gauge (0..1) for the score.
    pub fn score_gauge(&self) -> f64 {
        ((self.score + 100.0) / 200.0).clamp(0.0, 1.0)
    }
}

/// Render a list of peer reputations.
pub fn reputation_list(peers: &[PeerReputation]) -> String {
    if peers.is_empty() {
        return Color::Dim.paint("(no reputation data)").to_string();
    }

    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!("Peer Reputation ({} peers)", peers.len()))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(80));

    // Sort by score descending.
    let mut sorted: Vec<&PeerReputation> = peers.iter().collect();
    sorted.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    for peer in sorted {
        let score_colored = peer.score_color().paint(format!("{:+.1}", peer.score));
        let trust_colored = match peer.trust_level {
            t if t >= 2 => Color::Green.paint(peer.trust_label()),
            1 => Color::Cyan.paint(peer.trust_label()),
            0 => Color::White.paint(peer.trust_label()),
            -1 => Color::Yellow.paint(peer.trust_label()),
            _ => Color::Red.paint(peer.trust_label()),
        };
        let gauge = mini_bar(peer.score_gauge(), 10);

        lines.push(format!(
            "{} {:>7} {:9} +{:<3} -{} {}",
            peer.short_id,
            score_colored,
            trust_colored,
            peer.positive_count,
            peer.negative_count,
            gauge.ansi(),
        ));
    }

    lines.join("\n")
}

/// Render a single peer reputation detail.
pub fn reputation_detail(peer: &PeerReputation) -> String {
    let score_colored = peer.score_color().paint(format!("{:+.1}", peer.score));
    let trust_colored = match peer.trust_level {
        t if t >= 2 => Color::Green.paint(peer.trust_label()),
        1 => Color::Cyan.paint(peer.trust_label()),
        0 => Color::White.paint(peer.trust_label()),
        -1 => Color::Yellow.paint(peer.trust_label()),
        _ => Color::Red.paint(peer.trust_label()),
    };

    let mut panel = Panel::with_title(format!("Peer {}", peer.short_id));
    panel = panel
        .add_field("Peer ID", peer.peer_id.clone())
        .add_field("Score", score_colored)
        .add_field("Trust Level", trust_colored)
        .add_field(
            "Events",
            format!("+{} positive / -{} negative", peer.positive_count, peer.negative_count),
        );
    panel.render()
}

// ─────────────────────────────────────────────────────────────────────
//  Gossip/topic widget (audit: ops need to see room subscriptions)
// ─────────────────────────────────────────────────────────────────────

/// A subscribed gossip topic.
#[derive(Debug, Clone)]
pub struct GossipTopic {
    pub name: String,
    pub topic_id: String,
    pub peers: usize,
    pub messages_sent: u64,
    pub messages_received: u64,
    pub last_message_unix: Option<i64>,
}

impl GossipTopic {
    /// Create a new gossip topic.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            topic_id: format!("blake3:{}", &name[..name.len().min(8)]),
            peers: 0,
            messages_sent: 0,
            messages_received: 0,
            last_message_unix: None,
        }
    }

    /// Set peer count.
    pub fn peers(mut self, n: usize) -> Self {
        self.peers = n;
        self
    }

    /// Set message counters.
    pub fn counters(mut self, sent: u64, recv: u64) -> Self {
        self.messages_sent = sent;
        self.messages_received = recv;
        self
    }

    /// Compute activity rate (msg/min) — simple proxy.
    pub fn activity_per_min(&self) -> f64 {
        let total = self.messages_sent + self.messages_received;
        if total == 0 {
            0.0
        } else {
            total as f64
        }
    }
}

/// Render gossip topic subscriptions.
pub fn gossip_topics(topics: &[GossipTopic]) -> String {
    if topics.is_empty() {
        return Color::Dim.paint("(no subscribed topics)").to_string();
    }

    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!("Gossip Topics ({})", topics.len()))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(80));

    lines.push(format!(
        "{:<20} {:<14} {:>5} {:>8} {:>8} {:>10}",
        Color::Cyan.paint("NAME").bold(),
        Color::Cyan.paint("TOPIC").bold(),
        Color::Cyan.paint("PEERS").bold(),
        Color::Cyan.paint("SENT").bold(),
        Color::Cyan.paint("RECV").bold(),
        Color::Cyan.paint("ACTIVITY").bold(),
    ));

    for topic in topics {
        let activity = topic.activity_per_min();
        let activity_str = if activity > 100.0 {
            Color::Red.paint(format!("{:.0}/m", activity)).to_string()
        } else if activity > 10.0 {
            Color::Yellow.paint(format!("{:.1}/m", activity)).to_string()
        } else if activity > 0.0 {
            Color::Green.paint(format!("{:.1}/m", activity)).to_string()
        } else {
            Color::Dim.paint("idle").to_string()
        };

        lines.push(format!(
            "{:<20} {:<14} {:>5} {:>8} {:>8} {:>10}",
            truncate_str(&topic.name, 20),
            truncate_str(&topic.topic_id, 14),
            topic.peers,
            topic.messages_sent,
            topic.messages_received,
            activity_str,
        ));
    }

    lines.join("\n")
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

// ─────────────────────────────────────────────────────────────────────
//  Transfer sparkline widget (audit: ops need to see bandwidth over time)
// ─────────────────────────────────────────────────────────────────────

/// A series of bandwidth samples.
#[derive(Debug, Clone, Default)]
pub struct BandwidthSparkline {
    pub label: String,
    /// Bytes per second over the last N seconds.
    pub samples: Vec<u64>,
    /// Sample interval in seconds.
    pub sample_interval_secs: u64,
}

impl BandwidthSparkline {
    /// Create a new sparkline.
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            samples: Vec::new(),
            sample_interval_secs: 1,
        }
    }

    /// Add a new sample.
    pub fn push(&mut self, bytes_per_sec: u64) {
        self.samples.push(bytes_per_sec);
    }

    /// Compute the peak sample.
    pub fn peak(&self) -> u64 {
        self.samples.iter().copied().max().unwrap_or(0)
    }

    /// Compute the average sample.
    pub fn average(&self) -> f64 {
        if self.samples.is_empty() {
            0.0
        } else {
            self.samples.iter().sum::<u64>() as f64 / self.samples.len() as f64
        }
    }

    /// Compute the most recent sample.
    pub fn current(&self) -> u64 {
        self.samples.last().copied().unwrap_or(0)
    }

    /// Render a sparkline as a Unicode bar chart.
    pub fn render(&self, width: usize) -> String {
        if self.samples.is_empty() {
            return Color::Dim.paint("(no samples)").to_string();
        }

        let window: Vec<u64> = if self.samples.len() > width {
            self.samples[self.samples.len() - width..].to_vec()
        } else {
            self.samples.clone()
        };

        let peak = window.iter().copied().max().unwrap_or(1).max(1);
        let blocks = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

        let sparkline: String = window
            .iter()
            .map(|&s| {
                let idx = ((s as f64 / peak as f64) * (blocks.len() - 1) as f64) as usize;
                blocks[idx.min(blocks.len() - 1)]
            })
            .collect();

        format!(
            "{}: {} (peak: {}/s, avg: {}/s, current: {}/s)",
            Color::Cyan.paint(&self.label).bold(),
            sparkline,
            human_bytes(self.peak()),
            human_bytes(self.average() as u64),
            human_bytes(self.current()),
        )
    }
}

/// Render multiple sparklines (up + down) together.
pub fn bandwidth_dashboard(up: &BandwidthSparkline, down: &BandwidthSparkline, width: usize) -> String {
    let lines = [
        Color::Cyan.paint("Bandwidth").bold().to_string(),
        up.render(width),
        down.render(width),
    ];
    lines.join("\n")
}

// ─────────────────────────────────────────────────────────────────────
//  Interactive menu widget (audit: a3net run REPL needs a menu)
// ─────────────────────────────────────────────────────────────────────

/// A menu item.
#[derive(Debug, Clone)]
pub struct MenuItem {
    pub key: char,
    pub label: &'static str,
    pub description: &'static str,
}

/// An interactive menu.
#[derive(Debug, Clone)]
pub struct Menu {
    pub title: &'static str,
    pub items: Vec<MenuItem>,
    pub selected: usize,
}

impl Menu {
    /// Create a new menu.
    pub fn new(title: &'static str) -> Self {
        Self {
            title,
            items: Vec::new(),
            selected: 0,
        }
    }

    /// Add an item.
    pub fn item(mut self, key: char, label: &'static str, description: &'static str) -> Self {
        self.items.push(MenuItem {
            key,
            label,
            description,
        });
        self
    }

    /// Move selection up.
    pub fn up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Move selection down.
    pub fn down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
    }

    /// Get currently selected item.
    pub fn current(&self) -> Option<&MenuItem> {
        self.items.get(self.selected)
    }

    /// Render the menu.
    pub fn render(&self) -> String {
        let mut lines = Vec::new();
        lines.push(Color::Cyan.paint(self.title).bold().to_string());
        lines.push("─".repeat(60));

        for (i, item) in self.items.iter().enumerate() {
            let cursor = if i == self.selected { "▶" } else { " " };
            let cursor_colored = if i == self.selected {
                Color::Yellow.paint(cursor).bold()
            } else {
                Color::Dim.paint(cursor)
            };
            let key_colored = Color::Green.paint(format!("[{}]", item.key)).bold();
            let label_colored = if i == self.selected {
                Color::White.paint(item.label).bold()
            } else {
                Color::White.paint(item.label)
            };
            lines.push(format!(
                "{} {} {:<20} {}",
                cursor_colored,
                key_colored,
                label_colored,
                Color::Dim.paint(item.description),
            ));
        }

        lines.push(String::new());
        lines.push(
            Color::Dim
                .paint("Use ↑/↓ to navigate, Enter to select, q to quit")
                .to_string(),
        );

        lines.join("\n")
    }
}

// ─────────────────────────────────────────────────────────────────────
//  Command palette widget (audit: ops need quick command lookup)
// ─────────────────────────────────────────────────────────────────────

/// A command entry for the palette.
#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub name: &'static str,
    pub shortcut: &'static str,
    pub description: &'static str,
    pub category: &'static str,
}

/// A fuzzy-searchable command palette.
#[derive(Debug, Clone)]
pub struct CommandPalette {
    pub commands: Vec<CommandEntry>,
    pub filter: String,
}

impl CommandPalette {
    /// Create an empty palette.
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            filter: String::new(),
        }
    }

    /// Register a command.
    pub fn register(mut self, entry: CommandEntry) -> Self {
        self.commands.push(entry);
        self
    }

    /// Set the current filter.
    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_ascii_lowercase();
    }

    /// Get filtered commands.
    pub fn filtered(&self) -> Vec<&CommandEntry> {
        if self.filter.is_empty() {
            return self.commands.iter().collect();
        }
        self.commands
            .iter()
            .filter(|c| {
                let name = c.name.to_ascii_lowercase();
                let desc = c.description.to_ascii_lowercase();
                let cat = c.category.to_ascii_lowercase();
                let sc = c.shortcut.to_ascii_lowercase();
                name.contains(&self.filter)
                    || desc.contains(&self.filter)
                    || cat.contains(&self.filter)
                    || sc.contains(&self.filter)
            })
            .collect()
    }

    /// Render the palette.
    pub fn render(&self) -> String {
        let filtered = self.filtered();
        let mut lines = Vec::new();
        lines.push(
            Color::Cyan
                .paint(format!("Commands ({})", filtered.len()))
                .bold()
                .to_string(),
        );
        lines.push(
            Color::Dim
                .paint(format!("Filter: \"{}\"", self.filter))
                .to_string(),
        );
        lines.push("─".repeat(80));

        if filtered.is_empty() {
            lines.push(Color::Dim.paint("(no commands match)").to_string());
            return lines.join("\n");
        }

        let mut by_cat: std::collections::BTreeMap<&str, Vec<&CommandEntry>> = Default::default();
        for cmd in &filtered {
            by_cat.entry(cmd.category).or_default().push(cmd);
        }

        for (cat, cmds) in &by_cat {
            lines.push(Color::Yellow.paint(format!("[{}]", cat)).to_string());
            for cmd in cmds {
                lines.push(format!(
                    "  {:<12} {:<30} {}",
                    Color::Green.paint(cmd.shortcut),
                    cmd.name,
                    Color::Dim.paint(cmd.description),
                ));
            }
        }

        lines.join("\n")
    }
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────
//  Status badges widget (audit: badges for encrypted/sealed/trusted/etc)
// ─────────────────────────────────────────────────────────────────────

/// A status badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    Ok,
    Warn,
    Critical,
    Encrypted,
    Sealed,
    Trusted,
    Blocked,
    Syncing,
    Disabled,
}

impl Badge {
    /// Get the text for this badge.
    pub fn text(&self) -> &'static str {
        match self {
            Badge::Ok => "OK",
            Badge::Warn => "WARN",
            Badge::Critical => "CRIT",
            Badge::Encrypted => "ENC",
            Badge::Sealed => "SEAL",
            Badge::Trusted => "TRUST",
            Badge::Blocked => "BLOCK",
            Badge::Syncing => "SYNC",
            Badge::Disabled => "OFF",
        }
    }

    /// Get the color for this badge.
    pub fn color(&self) -> Color {
        match self {
            Badge::Ok => Color::Green,
            Badge::Warn => Color::Yellow,
            Badge::Critical => Color::Red,
            Badge::Encrypted => Color::Yellow,
            Badge::Sealed => Color::Cyan,
            Badge::Trusted => Color::Green,
            Badge::Blocked => Color::Red,
            Badge::Syncing => Color::Cyan,
            Badge::Disabled => Color::Dim,
        }
    }

    /// Render as a styled badge like `[OK]`.
    pub fn render(&self) -> String {
        self.color().paint(format!("[{}]", self.text())).bold().to_string()
    }
}

/// Render a row of badges.
pub fn badges_row(badges: &[Badge]) -> String {
    badges
        .iter()
        .map(|b| b.render())
        .collect::<Vec<_>>()
        .join(" ")
}

// ─────────────────────────────────────────────────────────────────────
//  Health summary widget (audit: high-level "is the node OK" view)
// ─────────────────────────────────────────────────────────────────────

/// A health check.
#[derive(Debug, Clone)]
pub struct HealthCheck {
    pub name: String,
    pub status: Badge,
    pub detail: Option<String>,
}

/// A health summary.
#[derive(Debug, Clone, Default)]
pub struct HealthSummary {
    pub checks: Vec<HealthCheck>,
}

impl HealthSummary {
    /// Create an empty summary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a check.
    pub fn check(mut self, name: &str, status: Badge) -> Self {
        self.checks.push(HealthCheck {
            name: name.to_string(),
            status,
            detail: None,
        });
        self
    }

    /// Add a check with a detail message.
    pub fn check_with_detail(mut self, name: &str, status: Badge, detail: &str) -> Self {
        self.checks.push(HealthCheck {
            name: name.to_string(),
            status,
            detail: Some(detail.to_string()),
        });
        self
    }

    /// Compute overall health.
    pub fn overall(&self) -> Badge {
        if self.checks.iter().any(|c| c.status == Badge::Critical) {
            Badge::Critical
        } else if self.checks.iter().any(|c| c.status == Badge::Warn) {
            Badge::Warn
        } else {
            Badge::Ok
        }
    }

    /// Render the summary.
    pub fn render(&self) -> String {
        let overall = self.overall();
        let overall_text = match overall {
            Badge::Ok => "All systems operational",
            Badge::Warn => "Some warnings present",
            Badge::Critical => "Critical issues detected",
            _ => "Unknown",
        };

        let mut lines = Vec::new();
        lines.push(
            Color::Cyan
                .paint(format!("Health: {} {}", overall.render(), overall_text))
                .bold()
                .to_string(),
        );
        lines.push("─".repeat(60));

        for check in &self.checks {
            let badge = check.status.render();
            let line = if let Some(d) = &check.detail {
                format!("{} {:<20} {}", badge, check.name, Color::Dim.paint(d))
            } else {
                format!("{} {}", badge, check.name)
            };
            lines.push(line);
        }

        lines.join("\n")
    }
}

// ─────────────────────────────────────────────────────────────────────
//  Quota policy widget (audit: QuotaPolicy has no UI)
// ─────────────────────────────────────────────────────────────────────

/// A quota allocation for one scope (private/shared).
#[derive(Debug, Clone, Copy)]
pub struct QuotaAllocation {
    pub scope: &'static str,
    /// Fraction of total budget (0..1).
    pub fraction: f64,
    pub bytes_used: u64,
    pub bytes_budget: u64,
    pub hard_cap_bytes: u64,
}

impl QuotaAllocation {
    /// Compute usage fraction.
    pub fn fraction_used(&self) -> f64 {
        if self.bytes_budget == 0 {
            0.0
        } else {
            (self.bytes_used as f64 / self.bytes_budget as f64).clamp(0.0, 1.0)
        }
    }

    /// Whether scope is over budget.
    pub fn over_budget(&self) -> bool {
        self.bytes_used > self.bytes_budget
    }

    /// Whether scope is over hard cap.
    pub fn over_hard_cap(&self) -> bool {
        self.bytes_used > self.hard_cap_bytes
    }
}

/// Render a quota policy breakdown.
pub fn quota_policy_view(
    total_budget_bytes: u64,
    allocations: &[QuotaAllocation],
) -> String {
    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!(
                "Quota Policy (total: {})",
                human_bytes(total_budget_bytes)
            ))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(70));

    // Aggregate stats.
    let total_used: u64 = allocations.iter().map(|a| a.bytes_used).sum();
    let frac = if total_budget_bytes > 0 {
        total_used as f64 / total_budget_bytes as f64
    } else {
        0.0
    };
    let agg_color = if frac >= 0.9 {
        Color::Red
    } else if frac >= 0.7 {
        Color::Yellow
    } else {
        Color::Green
    };
    lines.push(format!(
        "Aggregate: {} / {} ({:.1}%)",
        agg_color.paint(human_bytes(total_used)),
        human_bytes(total_budget_bytes),
        frac * 100.0,
    ));
    lines.push(String::new());

    for alloc in allocations {
        let frac_used = alloc.fraction_used();
        let status_color = if alloc.over_hard_cap() {
            Color::Red
        } else if alloc.over_budget() || frac_used >= 0.9 {
            Color::Yellow
        } else {
            Color::Green
        };
        let bar = usage_bar(frac_used);
        let warn = if alloc.over_hard_cap() {
            Color::Red.paint(" OVER HARD CAP")
        } else if alloc.over_budget() {
            Color::Yellow.paint(" OVER BUDGET")
        } else {
            Color::Dim.paint("")
        };
        lines.push(format!(
            "{} ({:>3}%): {} {} {} / {} (hard cap: {}){}",
            alloc.scope,
            (alloc.fraction * 100.0) as u32,
            bar,
            status_color.paint(human_bytes(alloc.bytes_used)),
            "/",
            human_bytes(alloc.bytes_budget),
            human_bytes(alloc.hard_cap_bytes),
            warn,
        ));
    }

    lines.join("\n")
}

// ─────────────────────────────────────────────────────────────────────
//  Room feed viewer (audit: RoomFeed has no UI)
// ─────────────────────────────────────────────────────────────────────

/// One row in the room feed.
#[derive(Debug, Clone)]
pub struct FeedEntry {
    pub content_hash: String,
    pub short_hash: String,
    pub title: String,
    pub kind: String,
    pub size_bytes: u64,
    pub announcer: String,
    pub announced_at_unix: i64,
    pub peer_count: usize,
}

impl FeedEntry {
    /// Construct from fields.
    pub fn new(
        content_hash: &str,
        title: &str,
        kind: &str,
        size_bytes: u64,
        announcer: &str,
        announced_at_unix: i64,
        peer_count: usize,
    ) -> Self {
        let short_hash = if content_hash.len() > 10 {
            format!("{}…", &content_hash[..10])
        } else {
            content_hash.to_string()
        };
        Self {
            content_hash: content_hash.to_string(),
            short_hash,
            title: title.to_string(),
            kind: kind.to_string(),
            size_bytes,
            announcer: announcer.to_string(),
            announced_at_unix,
            peer_count,
        }
    }
}

/// Render a room feed table.
pub fn room_feed_view(room_id: &str, entries: &[FeedEntry]) -> String {
    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!("Room Feed: {} ({} entries)", room_id, entries.len()))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(100));

    if entries.is_empty() {
        lines.push(Color::Dim.paint("(no entries)").to_string());
        return lines.join("\n");
    }

    lines.push(format!(
        "{:<12} {:<30} {:<10} {:>10} {:>6}",
        Color::Cyan.paint("HASH").bold(),
        Color::Cyan.paint("TITLE").bold(),
        Color::Cyan.paint("KIND").bold(),
        Color::Cyan.paint("SIZE").bold(),
        Color::Cyan.paint("PEERS").bold(),
    ));

    // Sort newest first.
    let mut sorted: Vec<&FeedEntry> = entries.iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.announced_at_unix));

    for e in sorted {
        let title_trunc = if e.title.len() > 28 {
            format!("{}…", &e.title[..27])
        } else {
            e.title.clone()
        };
        lines.push(format!(
            "{:<12} {:<30} {:<10} {:>10} {:>6}",
            e.short_hash,
            title_trunc,
            truncate_str(&e.kind, 10),
            human_bytes(e.size_bytes),
            e.peer_count,
        ));
    }

    lines.join("\n")
}

// ─────────────────────────────────────────────────────────────────────
//  JSON tree viewer (audit: ops need to inspect config files)
// ─────────────────────────────────────────────────────────────────────

/// JSON value tree node.
#[derive(Debug, Clone)]
pub enum JsonTree {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonTree>),
    Object(Vec<(String, JsonTree)>),
}

impl JsonTree {
    /// Construct from a `serde_json::Value`-like string.
    pub fn parse(s: &str) -> Self {
        // Minimal parser: walks char-by-char.
        let mut parser = JsonParser::new(s);
        parser.skip_ws();
        parser.parse_value()
    }

    /// Number of nodes in the tree.
    pub fn size(&self) -> usize {
        match self {
            JsonTree::Null | JsonTree::Bool(_) | JsonTree::Number(_) | JsonTree::String(_) => 1,
            JsonTree::Array(items) => 1 + items.iter().map(|i| i.size()).sum::<usize>(),
            JsonTree::Object(pairs) => {
                1 + pairs.iter().map(|(_, v)| v.size()).sum::<usize>()
            }
        }
    }
}

struct JsonParser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            input: s.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.input.len() && self.input[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn consume(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_value(&mut self) -> JsonTree {
        self.skip_ws();
        match self.peek() {
            Some(b'"') => JsonTree::String(self.parse_string()),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b't') | Some(b'f') => JsonTree::Bool(self.parse_bool()),
            Some(b'n') => {
                self.pos += 4;
                JsonTree::Null
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => JsonTree::Number(self.parse_number()),
            _ => JsonTree::Null,
        }
    }

    fn parse_string(&mut self) -> String {
        debug_assert_eq!(self.peek(), Some(b'"'));
        self.pos += 1;
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == b'"' {
                self.pos += 1;
                return out;
            }
            if c == b'\\' {
                self.pos += 1;
                match self.peek() {
                    Some(b'"') => {
                        out.push('"');
                        self.pos += 1;
                    }
                    Some(b'\\') => {
                        out.push('\\');
                        self.pos += 1;
                    }
                    Some(b'/') => {
                        out.push('/');
                        self.pos += 1;
                    }
                    Some(b'n') => {
                        out.push('\n');
                        self.pos += 1;
                    }
                    Some(b't') => {
                        out.push('\t');
                        self.pos += 1;
                    }
                    Some(b'r') => {
                        out.push('\r');
                        self.pos += 1;
                    }
                    _ => {
                        out.push('\\');
                    }
                }
            } else {
                out.push(c as char);
                self.pos += 1;
            }
        }
        out
    }

    fn parse_bool(&mut self) -> bool {
        if self.input[self.pos..].starts_with(b"true") {
            self.pos += 4;
            true
        } else if self.input[self.pos..].starts_with(b"false") {
            self.pos += 5;
            false
        } else {
            false
        }
    }

    fn parse_number(&mut self) -> f64 {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == b'.' || c == b'e' || c == b'E' || c == b'+' || c == b'-' {
                self.pos += 1;
            } else {
                break;
            }
        }
        std::str::from_utf8(&self.input[start..self.pos])
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0)
    }

    fn parse_array(&mut self) -> JsonTree {
        debug_assert_eq!(self.peek(), Some(b'['));
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return JsonTree::Array(items);
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value());
            self.skip_ws();
            if !self.consume(b',') {
                break;
            }
        }
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
        }
        JsonTree::Array(items)
    }

    fn parse_object(&mut self) -> JsonTree {
        debug_assert_eq!(self.peek(), Some(b'{'));
        self.pos += 1;
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return JsonTree::Object(pairs);
        }
        loop {
            self.skip_ws();
            let key = self.parse_string();
            self.skip_ws();
            if self.peek() == Some(b':') {
                self.pos += 1;
            }
            self.skip_ws();
            let value = self.parse_value();
            pairs.push((key, value));
            self.skip_ws();
            if !self.consume(b',') {
                break;
            }
        }
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
        }
        JsonTree::Object(pairs)
    }
}

/// Render a JSON tree.
pub fn json_tree_view(tree: &JsonTree) -> String {
    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!("JSON ({} nodes)", tree.size()))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(60));
    render_tree(tree, &mut lines, "", true);
    lines.join("\n")
}

fn render_tree(node: &JsonTree, lines: &mut Vec<String>, prefix: &str, is_last: bool) {
    let connector = if prefix.is_empty() {
        ""
    } else if is_last {
        "└─ "
    } else {
        "├─ "
    };
    let next_prefix = if prefix.is_empty() {
        "".to_string()
    } else if is_last {
        format!("{prefix}   ")
    } else {
        format!("{prefix}│  ")
    };
    match node {
        JsonTree::Null => lines.push(format!("{prefix}{connector}{}", Color::Dim.paint("null"))),
        JsonTree::Bool(b) => lines.push(format!(
            "{prefix}{connector}{}",
            if *b {
                Color::Green.paint("true")
            } else {
                Color::Red.paint("false")
            }
        )),
        JsonTree::Number(n) => lines.push(format!(
            "{prefix}{connector}{}",
            Color::Yellow.paint(format!("{n}"))
        )),
        JsonTree::String(s) => lines.push(format!(
            "{prefix}{connector}{}",
            Color::Cyan.paint(format!("\"{s}\""))
        )),
        JsonTree::Array(items) => {
            lines.push(format!("{prefix}{connector}[ ({}) ]", items.len()));
            for (i, item) in items.iter().enumerate() {
                let last = i + 1 == items.len();
                render_tree(item, lines, &next_prefix, last);
            }
        }
        JsonTree::Object(pairs) => {
            lines.push(format!("{prefix}{connector}{{ {} keys }}", pairs.len()));
            for (i, (k, v)) in pairs.iter().enumerate() {
                let last = i + 1 == pairs.len();
                let old_prefix = next_prefix.clone();
                let k_colored = Color::Green.paint(format!("\"{k}\""));
                lines.push(format!("{old_prefix}{}", if last { "└─ " } else { "├─ " }));
                let value_prefix = format!("{old_prefix}{}", if last { "   " } else { "│  " });
                match v {
                    JsonTree::Null | JsonTree::Bool(_) | JsonTree::Number(_) | JsonTree::String(_) => {
                        let rendered = match v {
                            JsonTree::Null => Color::Dim.paint("null").to_string(),
                            JsonTree::Bool(b) => if *b {
                                Color::Green.paint("true").to_string()
                            } else {
                                Color::Red.paint("false").to_string()
                            },
                            JsonTree::Number(n) => Color::Yellow.paint(format!("{n}")).to_string(),
                            JsonTree::String(s) => {
                                Color::Cyan.paint(format!("\"{s}\"")).to_string()
                            }
                            _ => unreachable!(),
                        };
                        lines.push(format!("{value_prefix}{k_colored}: {rendered}"));
                    }
                    _ => {
                        lines.push(format!("{value_prefix}{k_colored}:"));
                        render_tree(v, lines, &value_prefix, true);
                    }
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
//  Watch / diff widget (audit: ops need to see what changed)
// ─────────────────────────────────────────────────────────────────────

/// A diff between two values.
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Unchanged,
    Added,
    Removed,
    Modified,
}

impl DiffKind {
    /// Get the symbol for this diff kind.
    pub fn symbol(&self) -> &'static str {
        match self {
            DiffKind::Unchanged => " ",
            DiffKind::Added => "+",
            DiffKind::Removed => "-",
            DiffKind::Modified => "~",
        }
    }

    /// Get the color for this diff kind.
    pub fn color(&self) -> Color {
        match self {
            DiffKind::Unchanged => Color::Dim,
            DiffKind::Added => Color::Green,
            DiffKind::Removed => Color::Red,
            DiffKind::Modified => Color::Yellow,
        }
    }
}

/// Render a diff between two key-value lists.
pub fn diff_view(diffs: &[DiffLine]) -> String {
    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!("Diff ({} entries)", diffs.len()))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(80));

    let mut added = 0;
    let mut removed = 0;
    let mut modified = 0;

    for d in diffs {
        let sym = d.kind.color().paint(d.kind.symbol()).bold();
        let path = if d.path.is_empty() { "(root)".to_string() } else { d.path.clone() };
        match d.kind {
            DiffKind::Unchanged => {
                lines.push(format!(
                    "{} {} {}",
                    sym,
                    path,
                    Color::Dim.paint(d.before.clone().unwrap_or_default())
                ));
            }
            DiffKind::Added => {
                added += 1;
                lines.push(format!(
                    "{} {} → {}",
                    sym,
                    path,
                    Color::Green.paint(d.after.clone().unwrap_or_default()),
                ));
            }
            DiffKind::Removed => {
                removed += 1;
                lines.push(format!(
                    "{} {} → {}",
                    sym,
                    path,
                    Color::Red.paint(d.before.clone().unwrap_or_default()),
                ));
            }
            DiffKind::Modified => {
                modified += 1;
                lines.push(format!(
                    "{} {}  {} → {}",
                    sym,
                    path,
                    Color::Red.paint(d.before.clone().unwrap_or_default()),
                    Color::Green.paint(d.after.clone().unwrap_or_default()),
                ));
            }
        }
    }

    lines.push(String::new());
    lines.push(format!(
        "Summary: {} added, {} removed, {} modified",
        Color::Green.paint(added.to_string()),
        Color::Red.paint(removed.to_string()),
        Color::Yellow.paint(modified.to_string()),
    ));

    lines.join("\n")
}

/// Compute a simple key-by-key diff between two `BTreeMap`s of strings.
pub fn diff_string_maps<K: Ord + std::fmt::Display + std::hash::Hash + Eq + Clone>(
    before: &std::collections::BTreeMap<K, String>,
    after: &std::collections::BTreeMap<K, String>,
) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let all_keys: std::collections::BTreeSet<&K> =
        before.keys().chain(after.keys()).collect();
    for k in all_keys {
        let b = before.get(k);
        let a = after.get(k);
        let path = format!("{k}");
        match (b, a) {
            (None, Some(v)) => out.push(DiffLine {
                kind: DiffKind::Added,
                path,
                before: None,
                after: Some(v.clone()),
            }),
            (Some(v), None) => out.push(DiffLine {
                kind: DiffKind::Removed,
                path,
                before: Some(v.clone()),
                after: None,
            }),
            (Some(bv), Some(av)) if bv != av => out.push(DiffLine {
                kind: DiffKind::Modified,
                path,
                before: Some(bv.clone()),
                after: Some(av.clone()),
            }),
            (Some(bv), Some(_)) => out.push(DiffLine {
                kind: DiffKind::Unchanged,
                path,
                before: Some(bv.clone()),
                after: None,
            }),
            _ => {}
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────
//  Breadcrumbs / navigation widget (audit: long REPL sessions need nav)
// ─────────────────────────────────────────────────────────────────────

/// One breadcrumb segment.
#[derive(Debug, Clone)]
pub struct Crumb {
    pub label: String,
    pub active: bool,
}

impl Crumb {
    /// Create a new crumb.
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            active: false,
        }
    }

    /// Mark this crumb as active.
    pub fn active(mut self) -> Self {
        self.active = true;
        self
    }
}

/// Render a breadcrumbs path.
pub fn breadcrumbs_view(crumbs: &[Crumb]) -> String {
    if crumbs.is_empty() {
        return Color::Dim.paint("(no path)").to_string();
    }
    let mut parts = Vec::new();
    for (i, c) in crumbs.iter().enumerate() {
        let rendered = if c.active {
            Color::Yellow.paint(&c.label).bold().to_string()
        } else {
            Color::Cyan.paint(&c.label).to_string()
        };
        parts.push(rendered);
        if i + 1 < crumbs.len() {
            parts.push(Color::Dim.paint("›").to_string());
        }
    }
    parts.join(" ")
}

// ─────────────────────────────────────────────────────────────────────
//  Scheduled jobs widget (audit: GcScheduler/Replicator run silently)
// ─────────────────────────────────────────────────────────────────────

/// A scheduled / recurring job.
#[derive(Debug, Clone)]
pub struct ScheduledJob {
    pub name: String,
    pub interval_seconds: u64,
    pub last_run_unix: Option<i64>,
    pub next_run_unix: Option<i64>,
    pub last_duration_ms: u64,
    pub runs_total: u64,
    pub runs_failed: u64,
    pub enabled: bool,
}

impl ScheduledJob {
    /// Create a new scheduled job.
    pub fn new(name: &str, interval_seconds: u64) -> Self {
        Self {
            name: name.to_string(),
            interval_seconds,
            last_run_unix: None,
            next_run_unix: None,
            last_duration_ms: 0,
            runs_total: 0,
            runs_failed: 0,
            enabled: true,
        }
    }

    /// Record a run.
    pub fn record_run(&mut self, duration_ms: u64, success: bool, now_unix: i64) {
        self.last_run_unix = Some(now_unix);
        self.last_duration_ms = duration_ms;
        self.runs_total += 1;
        if !success {
            self.runs_failed += 1;
        }
        self.next_run_unix = Some(now_unix + self.interval_seconds as i64);
    }

    /// Compute failure rate (0..1).
    pub fn failure_rate(&self) -> f64 {
        if self.runs_total == 0 {
            0.0
        } else {
            self.runs_failed as f64 / self.runs_total as f64
        }
    }

    /// Whether the job has never run.
    pub fn is_pending(&self) -> bool {
        self.last_run_unix.is_none()
    }
}

/// Render the scheduled jobs view.
pub fn scheduled_jobs_view(jobs: &[ScheduledJob]) -> String {
    if jobs.is_empty() {
        return Color::Dim.paint("(no scheduled jobs)").to_string();
    }
    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!("Scheduled Jobs ({})", jobs.len()))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(80));

    lines.push(format!(
        "{:<20} {:<10} {:>10} {:>8} {:>6}",
        Color::Cyan.paint("NAME").bold(),
        Color::Cyan.paint("INTERVAL").bold(),
        Color::Cyan.paint("RUNS").bold(),
        Color::Cyan.paint("LAST").bold(),
        Color::Cyan.paint("FAIL%").bold(),
    ));

    for job in jobs {
        let status_color = if !job.enabled {
            Color::Dim
        } else if job.failure_rate() > 0.5 {
            Color::Red
        } else if job.failure_rate() > 0.1 {
            Color::Yellow
        } else {
            Color::Green
        };
        let status = if !job.enabled {
            Color::Dim.paint("[off]")
        } else if job.is_pending() {
            Color::Yellow.paint("[pending]")
        } else {
            Color::Green.paint("[active]")
        };
        let fail_pct = (job.failure_rate() * 100.0) as u32;
        let interval = format_uptime(job.interval_seconds);
        lines.push(format!(
            "{} {:<20} {:<10} {:>4}/{:>4} {:>6}ms {:>3}%",
            status,
            truncate_str(&job.name, 20),
            interval,
            job.runs_total - job.runs_failed,
            job.runs_total,
            job.last_duration_ms,
            status_color.paint(format!("{fail_pct}")),
        ));
    }

    lines.join("\n")
}

// ─────────────────────────────────────────────────────────────────────
//  Conversation list (audit: ChatStore has no UI)
// ─────────────────────────────────────────────────────────────────────

/// One conversation summary for the list view.
#[derive(Debug, Clone)]
pub struct ConversationSummary {
    pub id: String,
    pub chat_type: &'static str,
    pub title: String,
    pub message_count: u32,
    pub member_count: u32,
    pub last_sequence: u32,
    pub last_activity_unix: i64,
    pub unread: u32,
}

impl ConversationSummary {
    /// Create a new conversation summary.
    pub fn new(id: &str, title: &str, chat_type: &'static str) -> Self {
        Self {
            id: id.to_string(),
            chat_type,
            title: title.to_string(),
            message_count: 0,
            member_count: 0,
            last_sequence: 0,
            last_activity_unix: 0,
            unread: 0,
        }
    }

    /// Set message count.
    pub fn messages(mut self, count: u32, last_seq: u32) -> Self {
        self.message_count = count;
        self.last_sequence = last_seq;
        self
    }

    /// Set member count.
    pub fn members(mut self, count: u32) -> Self {
        self.member_count = count;
        self
    }

    /// Set last activity timestamp.
    pub fn last_activity(mut self, unix: i64) -> Self {
        self.last_activity_unix = unix;
        self
    }

    /// Set unread count.
    pub fn unread(mut self, n: u32) -> Self {
        self.unread = n;
        self
    }
}

/// Render the conversation list.
pub fn conversation_list(conv: &[ConversationSummary]) -> String {
    if conv.is_empty() {
        return Color::Dim.paint("(no conversations)").to_string();
    }
    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!("Conversations ({})", conv.len()))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(80));

    lines.push(format!(
        "{:<10} {:<6} {:<30} {:>6} {:>6} {:>8} {:>6}",
        Color::Cyan.paint("TYPE").bold(),
        Color::Cyan.paint("KIND").bold(),
        Color::Cyan.paint("TITLE").bold(),
        Color::Cyan.paint("MSGS").bold(),
        Color::Cyan.paint("USERS").bold(),
        Color::Cyan.paint("LASTSEQ").bold(),
        Color::Cyan.paint("UNREAD").bold(),
    ));

    // Sort by last activity desc.
    let mut sorted: Vec<&ConversationSummary> = conv.iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.last_activity_unix));

    for c in sorted {
        let unread_str = if c.unread > 0 {
            Color::Yellow
                .paint(format!("{}", c.unread))
                .bold()
                .to_string()
        } else {
            Color::Dim.paint("-").to_string()
        };
        let kind_icon = match c.chat_type {
            "group" => "👥",
            "dm" | "1on1" => "💬",
            _ => "?",
        };
        lines.push(format!(
            "{:<10} {:<6} {:<30} {:>6} {:>6} {:>8} {:>6}",
            kind_icon,
            c.chat_type,
            truncate_str(&c.title, 30),
            c.message_count,
            c.member_count,
            c.last_sequence,
            unread_str,
        ));
    }

    lines.join("\n")
}

// ─────────────────────────────────────────────────────────────────────
//  Message thread viewer (audit: Message stream has no scroll view)
// ─────────────────────────────────────────────────────────────────────

/// A single message.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub body: String,
    pub sent_at_unix: i64,
    pub sequence: u32,
}

impl ChatMessage {
    /// Create a new chat message.
    pub fn new(id: &str, sender_id: &str, body: &str, sent_at_unix: i64, sequence: u32) -> Self {
        Self {
            id: id.to_string(),
            sender_id: sender_id.to_string(),
            sender_name: sender_id.to_string(),
            body: body.to_string(),
            sent_at_unix,
            sequence,
        }
    }

    /// Set the sender's display name.
    pub fn sender(mut self, name: &str) -> Self {
        self.sender_name = name.to_string();
        self
    }

    /// Truncate the message body for a one-line preview.
    pub fn preview(&self) -> String {
        let first_line = self.body.lines().next().unwrap_or("");
        if first_line.len() <= 40 {
            first_line.to_string()
        } else {
            format!("{}…", &first_line[..39])
        }
    }
}

/// Render a conversation thread (chronological scroll view).
pub fn message_thread(messages: &[ChatMessage], max_lines: usize) -> String {
    if messages.is_empty() {
        return Color::Dim.paint("(no messages)").to_string();
    }
    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!("Thread ({} messages)", messages.len()))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(80));

    // Take the last `max_lines` messages.
    let start = messages.len().saturating_sub(max_lines);

    let mut prev_sender: Option<&str> = None;
    for msg in &messages[start..] {
        let show_header = prev_sender != Some(&msg.sender_id);
        prev_sender = Some(&msg.sender_id);

        if show_header {
            let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(msg.sent_at_unix, 0)
                .map(|t| t.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| format!("@{}", msg.sent_at_unix));
            lines.push(format!(
                "{} {} {}",
                Color::Dim.paint(ts),
                Color::Green.paint(&msg.sender_name).bold(),
                Color::Dim.paint(format!("#{}", msg.sequence)),
            ));
        }

        // Word-wrap body at 70 chars.
        let wrapped = wrap_text(&msg.body, 70);
        for (i, wline) in wrapped.iter().enumerate() {
            let prefix = if i == 0 { "  " } else { "    " };
            lines.push(format!("{prefix}{wline}"));
        }
    }

    lines.join("\n")
}

fn wrap_text(s: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for word in s.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= max {
            current.push(' ');
            current.push_str(word);
        } else {
            out.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

// ─────────────────────────────────────────────────────────────────────
//  Service status table (audit: relay/mesh/derp running silently)
// ─────────────────────────────────────────────────────────────────────

/// State of one service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    Stopped,
    Starting,
    Crashed,
    Disabled,
}

impl ServiceState {
    /// Get the display text for this state.
    pub fn text(&self) -> &'static str {
        match self {
            ServiceState::Running => "running",
            ServiceState::Stopped => "stopped",
            ServiceState::Starting => "starting",
            ServiceState::Crashed => "crashed",
            ServiceState::Disabled => "disabled",
        }
    }

    /// Get the color for this state.
    pub fn color(&self) -> Color {
        match self {
            ServiceState::Running => Color::Green,
            ServiceState::Stopped => Color::Dim,
            ServiceState::Starting => Color::Yellow,
            ServiceState::Crashed => Color::Red,
            ServiceState::Disabled => Color::Dim,
        }
    }

    /// Render as a styled badge.
    pub fn render(&self) -> String {
        self.color().paint(format!("[{}]", self.text())).bold().to_string()
    }
}

/// One service entry.
#[derive(Debug, Clone)]
pub struct ServiceEntry {
    pub name: String,
    pub state: ServiceState,
    pub address: Option<String>,
    pub uptime_seconds: u64,
    pub version: Option<String>,
}

impl ServiceEntry {
    /// Create a new service entry.
    pub fn new(name: &str, state: ServiceState) -> Self {
        Self {
            name: name.to_string(),
            state,
            address: None,
            uptime_seconds: 0,
            version: None,
        }
    }

    /// Set bind address.
    pub fn address(mut self, addr: &str) -> Self {
        self.address = Some(addr.to_string());
        self
    }

    /// Set uptime.
    pub fn uptime(mut self, seconds: u64) -> Self {
        self.uptime_seconds = seconds;
        self
    }

    /// Set version.
    pub fn version(mut self, v: &str) -> Self {
        self.version = Some(v.to_string());
        self
    }
}

/// Render a service status table.
pub fn service_status_table(services: &[ServiceEntry]) -> String {
    if services.is_empty() {
        return Color::Dim.paint("(no services registered)").to_string();
    }
    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!("Services ({})", services.len()))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(80));

    // Count states.
    let running = services.iter().filter(|s| s.state == ServiceState::Running).count();
    let total = services.len();
    let summary_color = if running == total {
        Color::Green
    } else if running == 0 {
        Color::Red
    } else {
        Color::Yellow
    };
    lines.push(format!(
        "Summary: {}/{} running",
        summary_color.paint(format!("{running}")),
        total,
    ));
    lines.push(String::new());

    lines.push(format!(
        "{:<20} {:<10} {:<24} {:>8} {:>8}",
        Color::Cyan.paint("NAME").bold(),
        Color::Cyan.paint("STATE").bold(),
        Color::Cyan.paint("ADDRESS").bold(),
        Color::Cyan.paint("UPTIME").bold(),
        Color::Cyan.paint("VERSION").bold(),
    ));

    for s in services {
        let addr = s.address.as_deref().unwrap_or("-");
        let ver = s.version.as_deref().unwrap_or("-");
        lines.push(format!(
            "{:<20} {} {:<24} {:>8} {:>8}",
            truncate_str(&s.name, 20),
            s.state.render(),
            truncate_str(addr, 24),
            format_uptime(s.uptime_seconds),
            ver,
        ));
    }

    lines.join("\n")
}

// ─────────────────────────────────────────────────────────────────────
//  Identity inspector (audit: NodeId/keys have no UI)
// ─────────────────────────────────────────────────────────────────────

/// A cryptographic identity / key.
#[derive(Debug, Clone)]
pub struct IdentityInfo {
    pub label: String,
    pub key_type: String,
    pub public_key: String,
    pub fingerprint: String,
    pub created_at_unix: Option<i64>,
    pub expires_at_unix: Option<i64>,
    pub usage: Vec<String>,
}

impl IdentityInfo {
    /// Create a new identity record.
    pub fn new(label: &str, key_type: &str, public_key: &str, fingerprint: &str) -> Self {
        Self {
            label: label.to_string(),
            key_type: key_type.to_string(),
            public_key: public_key.to_string(),
            fingerprint: fingerprint.to_string(),
            created_at_unix: None,
            expires_at_unix: None,
            usage: Vec::new(),
        }
    }

    /// Mark an identity as expiring.
    pub fn expires(mut self, unix: i64) -> Self {
        self.expires_at_unix = Some(unix);
        self
    }

    /// Mark an identity as created at.
    pub fn created(mut self, unix: i64) -> Self {
        self.created_at_unix = Some(unix);
        self
    }

    /// Add a usage tag.
    pub fn use_for(mut self, usage: &str) -> Self {
        self.usage.push(usage.to_string());
        self
    }

    /// Compute remaining validity (None if no expiry).
    pub fn remaining_seconds(&self, now_unix: i64) -> Option<i64> {
        self.expires_at_unix.map(|e| e - now_unix)
    }
}

/// Render an identity inspector (single key with full details).
pub fn identity_detail(id: &IdentityInfo) -> String {
    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!("Identity: {}", id.label))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(60));

    lines.push(format!("Type:         {}", id.key_type));
    lines.push(format!("Fingerprint:  {}", id.fingerprint));

    // Truncate the public key for display.
    let pk_display = if id.public_key.len() > 60 {
        format!("{}…{}", &id.public_key[..30], &id.public_key[id.public_key.len()-30..])
    } else {
        id.public_key.clone()
    };
    lines.push(format!("Public key:   {}", pk_display));

    if let Some(c) = id.created_at_unix {
        lines.push(format!("Created:      @{}", c));
    }
    if let Some(e) = id.expires_at_unix {
        lines.push(format!("Expires:      @{}", e));
    }
    if !id.usage.is_empty() {
        lines.push(format!("Used for:     {}", id.usage.join(", ")));
    }

    lines.join("\n")
}

/// Render a list of identities.
pub fn identity_list(ids: &[IdentityInfo]) -> String {
    if ids.is_empty() {
        return Color::Dim.paint("(no identities)").to_string();
    }
    let mut lines = Vec::new();
    lines.push(
        Color::Cyan
            .paint(format!("Identities ({})", ids.len()))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(80));

    lines.push(format!(
        "{:<24} {:<8} {:<24} {:<10}",
        Color::Cyan.paint("LABEL").bold(),
        Color::Cyan.paint("TYPE").bold(),
        Color::Cyan.paint("FINGERPRINT").bold(),
        Color::Cyan.paint("USAGE").bold(),
    ));

    for id in ids {
        lines.push(format!(
            "{:<24} {:<8} {:<24} {:<10}",
            truncate_str(&id.label, 24),
            id.key_type,
            truncate_str(&id.fingerprint, 24),
            id.usage.join(","),
        ));
    }

    lines.join("\n")
}

// ─────────────────────────────────────────────────────────────────────
//  Alert timeline (audit: alerts are fire-and-forget)
// ─────────────────────────────────────────────────────────────────────

/// One alert event in a chronological log.
#[derive(Debug, Clone)]
pub struct TimelineEvent {
    pub timestamp_unix: i64,
    pub level: AlertLevel,
    pub code: String,
    pub message: String,
}

impl TimelineEvent {
    /// Create a new timeline event.
    pub fn new(timestamp_unix: i64, level: AlertLevel, code: &str, message: &str) -> Self {
        Self {
            timestamp_unix,
            level,
            code: code.to_string(),
            message: message.to_string(),
        }
    }

    /// Render this single event.
    pub fn render(&self) -> String {
        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(self.timestamp_unix, 0)
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| format!("@{}", self.timestamp_unix));
        let lvl = match self.level {
            AlertLevel::Info => Color::Cyan.paint("INFO "),
            AlertLevel::Warning => Color::Yellow.paint("WARN "),
            AlertLevel::Critical => Color::Red.paint("CRIT ").bold(),
        };
        format!("{} {} {} {}", Color::Dim.paint(ts), lvl, self.code, self.message)
    }
}

/// Render an alert timeline.
pub fn alert_timeline(events: &[TimelineEvent]) -> String {
    if events.is_empty() {
        return Color::Dim.paint("(no events)").to_string();
    }
    let mut lines = Vec::new();

    // Count by level.
    let info = events.iter().filter(|e| e.level == AlertLevel::Info).count();
    let warn = events.iter().filter(|e| e.level == AlertLevel::Warning).count();
    let crit = events.iter().filter(|e| e.level == AlertLevel::Critical).count();

    lines.push(
        Color::Cyan
            .paint(format!(
                "Alert Timeline ({} events: {} info, {} warn, {} crit)",
                events.len(),
                info,
                Color::Yellow.paint(format!("{warn}")),
                Color::Red.paint(format!("{crit}")),
            ))
            .bold()
            .to_string(),
    );
    lines.push("─".repeat(80));

    // Show newest first.
    let mut sorted: Vec<&TimelineEvent> = events.iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.timestamp_unix));

    for e in sorted {
        lines.push(e.render());
    }

    lines.join("\n")
}

