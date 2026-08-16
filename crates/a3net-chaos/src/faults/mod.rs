//! Fault injection types and traits for chaos engineering.
//!
//! This module defines the core fault types that can be injected into the A3Net
//! system to test resilience and fault tolerance.

use std::time::Duration;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{ChaosError, Severity};

/// Represents the type of fault to inject
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultType {
    /// Network-level faults
    NetworkFault(NetworkFaultType),
    /// Node-level faults
    NodeFault(NodeFaultType),
    /// Data-level faults
    DataFault(DataFaultType),
    /// System-level faults
    SystemFault(SystemFaultType),
}

/// Network fault types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkFaultType {
    /// Packet loss (percentage 0-100)
    PacketLoss,
    /// Network latency (milliseconds)
    Latency,
    /// Network partition (isolates nodes)
    Partition,
    /// Bandwidth throttling (percentage)
    BandwidthThrottle,
    /// DNS resolution failure
    DnsFailure,
    /// Connection timeout
    ConnectionTimeout,
    /// Corrupt packet data
    PacketCorruption,
    /// Reorder packets
    PacketReorder,
    /// Duplicate packets
    PacketDuplication,
}

/// Node fault types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeFaultType {
    /// Node crash (instant termination)
    Crash,
    /// Node suspend (pause for duration)
    Suspend,
    /// CPU stress (percentage 0-100)
    CpuStress,
    /// Memory stress (percentage 0-100)
    MemoryStress,
    /// Disk I/O stress
    DiskStress,
    /// Process kill (SIGKILL)
    ProcessKill,
    /// Process pause (SIGSTOP)
    ProcessPause,
    /// Restart with delay
    Restart,
}

/// Data fault types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataFaultType {
    /// Data corruption
    Corruption,
    /// Data loss
    Loss,
    /// Data duplication
    Duplication,
    /// Replay old data
    Replay,
    /// Inject inconsistent data
    Inconsistency,
    /// Partition data
    Partition,
    /// Delay data delivery
    Delay,
}

/// System fault types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemFaultType {
    /// Clock skew (milliseconds)
    ClockSkew,
    /// System resource exhaustion
    ResourceExhaustion,
    /// Service dependency failure
    DependencyFailure,
    /// Rate limiting
    RateLimit,
}

impl FaultType {
    /// Get the default severity for this fault type
    pub fn default_severity(&self) -> Severity {
        match self {
            FaultType::NetworkFault(f) => match f {
                NetworkFaultType::PacketLoss => Severity::Medium,
                NetworkFaultType::Latency => Severity::Low,
                NetworkFaultType::Partition => Severity::Critical,
                NetworkFaultType::BandwidthThrottle => Severity::Low,
                NetworkFaultType::DnsFailure => Severity::High,
                NetworkFaultType::ConnectionTimeout => Severity::Medium,
                NetworkFaultType::PacketCorruption => Severity::High,
                NetworkFaultType::PacketReorder => Severity::Medium,
                NetworkFaultType::PacketDuplication => Severity::Low,
            },
            FaultType::NodeFault(f) => match f {
                NodeFaultType::Crash => Severity::Critical,
                NodeFaultType::Suspend => Severity::High,
                NodeFaultType::CpuStress => Severity::Medium,
                NodeFaultType::MemoryStress => Severity::High,
                NodeFaultType::DiskStress => Severity::Medium,
                NodeFaultType::ProcessKill => Severity::Critical,
                NodeFaultType::ProcessPause => Severity::High,
                NodeFaultType::Restart => Severity::Medium,
            },
            FaultType::DataFault(f) => match f {
                DataFaultType::Corruption => Severity::High,
                DataFaultType::Loss => Severity::High,
                DataFaultType::Duplication => Severity::Medium,
                DataFaultType::Replay => Severity::Medium,
                DataFaultType::Inconsistency => Severity::Critical,
                DataFaultType::Partition => Severity::High,
                DataFaultType::Delay => Severity::Low,
            },
            FaultType::SystemFault(f) => match f {
                SystemFaultType::ClockSkew => Severity::Medium,
                SystemFaultType::ResourceExhaustion => Severity::Critical,
                SystemFaultType::DependencyFailure => Severity::High,
                SystemFaultType::RateLimit => Severity::Low,
            },
        }
    }
}

/// Target for fault injection
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "id")]
pub enum FaultTarget {
    /// Target a specific node by ID
    Node(String),
    /// Target all nodes
    AllNodes,
    /// Target nodes matching a tag
    NodesByTag(String),
    /// Target network links
    Network,
    /// Target specific peer connection
    Peer(String),
    /// Target a specific service
    Service(String),
}

impl FaultTarget {
    /// Create a target for a single node
    pub fn node(id: impl Into<String>) -> Self {
        FaultTarget::Node(id.into())
    }

    /// Create a target for all nodes
    pub fn all_nodes() -> Self {
        FaultTarget::AllNodes
    }
}

/// Configuration for a fault
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaultConfig {
    /// The type of fault to inject
    pub fault_type: FaultType,
    
    /// Target of the fault
    pub target: FaultTarget,
    
    /// Severity of the fault
    pub severity: Severity,
    
    /// Duration of the fault (None = until manually recovered)
    pub duration: Option<Duration>,
    
    /// Fault-specific parameters
    pub parameters: FaultParameters,
    
    /// Whether to auto-recover after duration
    pub auto_recover: bool,
}

impl FaultConfig {
    /// Create a new fault configuration
    pub fn new(fault_type: FaultType, target: FaultTarget) -> Self {
        Self {
            severity: fault_type.default_severity(),
            parameters: FaultParameters::default(),
            auto_recover: true,
            duration: None,
            fault_type,
            target,
        }
    }

    /// Set the fault duration
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    /// Set auto-recovery
    pub fn with_auto_recover(mut self, auto_recover: bool) -> Self {
        self.auto_recover = auto_recover;
        self
    }

    /// Set custom parameters
    pub fn with_parameters(mut self, params: FaultParameters) -> Self {
        self.parameters = params;
        self
    }

    /// Set severity
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }
}

/// Parameters for configuring specific fault behaviors
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FaultParameters {
    /// For packet loss: percentage (0-100)
    pub loss_percentage: Option<f64>,
    
    /// For latency: delay in milliseconds
    pub latency_ms: Option<u64>,
    
    /// For bandwidth: throttle percentage (0-100)
    pub bandwidth_limit: Option<f64>,
    
    /// For corruption: corruption percentage (0-100)
    pub corruption_percentage: Option<f64>,
    
    /// For partition: list of nodes to isolate (empty = all)
    pub isolated_nodes: Vec<String>,
    
    /// For cpu/memory stress: percentage (0-100)
    pub stress_percentage: Option<f64>,
    
    /// For clock skew: offset in milliseconds
    pub clock_offset_ms: Option<i64>,
    
    /// Custom key-value parameters
    pub custom: std::collections::HashMap<String, String>,
}

impl FaultParameters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_loss(mut self, percentage: f64) -> Self {
        self.loss_percentage = Some(percentage);
        self
    }

    pub fn with_latency(mut self, ms: u64) -> Self {
        self.latency_ms = Some(ms);
        self
    }

    pub fn with_corruption(mut self, percentage: f64) -> Self {
        self.corruption_percentage = Some(percentage);
        self
    }
}

/// A fault that can be injected
#[derive(Debug, Clone)]
pub struct Fault {
    pub config: FaultConfig,
    pub id: String,
    pub injected_at: Option<std::time::Instant>,
}

impl Fault {
    pub fn new(config: FaultConfig) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            config,
            id,
            injected_at: None,
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }
}

/// Trait for fault injectors
#[async_trait]
pub trait FaultInjector: Send + Sync {
    /// Inject a fault
    async fn inject(&self, fault: &Fault) -> Result<(), ChaosError>;

    /// Recover from a fault
    async fn recover(&self, fault: &Fault) -> Result<(), ChaosError>;

    /// Check if a fault is currently active
    async fn is_active(&self, fault_id: &str) -> bool;

    /// List all active faults
    async fn active_faults(&self) -> Vec<String>;
}

/// Errors specific to fault injection
#[derive(Debug, Error)]
pub enum FaultError {
    #[error("Target not found: {0}")]
    TargetNotFound(String),

    #[error("Fault already active: {0}")]
    FaultAlreadyActive(String),

    #[error("Fault not active: {0}")]
    FaultNotActive(String),

    #[error("Injection failed: {0}")]
    InjectionFailed(String),

    #[error("Recovery failed: {0}")]
    RecoveryFailed(String),

    #[error("Invalid parameters: {0}")]
    InvalidParameters(String),
}
