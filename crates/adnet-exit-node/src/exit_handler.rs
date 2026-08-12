//! VPN exit traffic handler.
//!
//! This module provides the data-plane processing for exit node traffic.
//! It handles:
//! - Packet forwarding decisions based on routing table
//! - Traffic metering and bandwidth accounting
//! - Integration with billing systems

use std::net::IpAddr;
use std::sync::Arc;

use adnet_types::NodeId;
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::bandwidth::{BandwidthStats, ExitNodeMeter, RateLimitResult};
use crate::billing::{BillingEngine, BillingStatus};
use crate::client::Client;
use crate::gateway::{Gateway, GatewayState};
use crate::router::Router;

/// Maximum packet size (MTU).
pub const MAX_PACKET_SIZE: usize = 65535;

/// Exit traffic direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrafficKind {
    /// Traffic from mesh to Internet (exit traffic).
    ExitUpload,
    /// Traffic from Internet to mesh (return traffic).
    ExitDownload,
    /// Traffic between mesh nodes.
    MeshLocal,
}

/// A single packet record for logging/debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketRecord {
    pub source: NodeId,
    pub destination: IpAddr,
    pub kind: TrafficKind,
    pub size_bytes: usize,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl PacketRecord {
    fn new(source: NodeId, destination: IpAddr, kind: TrafficKind, size_bytes: usize) -> Self {
        Self {
            source,
            destination,
            kind,
            size_bytes,
            timestamp: chrono::Utc::now(),
        }
    }
}

/// Configuration for the exit handler.
#[derive(Debug, Clone)]
pub struct ExitHandlerConfig {
    /// Enable bandwidth metering.
    pub enable_metering: bool,
    /// Enable billing integration.
    pub enable_billing: bool,
    /// Default rate limit for clients (bytes per second).
    pub default_rate_limit: u64,
    /// Maximum burst size.
    pub max_burst_bytes: u64,
    /// Enable verbose packet logging.
    pub log_packets: bool,
}

impl Default for ExitHandlerConfig {
    fn default() -> Self {
        Self {
            enable_metering: true,
            enable_billing: true,
            default_rate_limit: 10 * 1024 * 1024, // 10 MB/s
            max_burst_bytes: 5 * 1024 * 1024,    // 5 MB
            log_packets: false,
        }
    }
}

/// Result of processing a packet.
#[derive(Debug, Clone)]
pub struct PacketResult {
    /// Whether the packet was allowed.
    pub allowed: bool,
    /// The action taken.
    pub action: PacketAction,
    /// Bytes processed.
    pub bytes: usize,
    /// Drop reason if not allowed.
    pub drop_reason: Option<String>,
}

/// Action taken on a packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PacketAction {
    /// Packet forwarded to mesh.
    ForwardToMesh,
    /// Packet forwarded via gateway.
    ForwardViaGateway { gateway: NodeId },
    /// Packet dropped.
    Dropped,
    /// Packet queued for later processing.
    Queued,
    /// Packet held for rate limiting.
    RateLimited { wait_ms: u64 },
}

impl PacketResult {
    fn allowed(action: PacketAction, bytes: usize) -> Self {
        Self {
            allowed: action != PacketAction::Dropped,
            action,
            bytes,
            drop_reason: None,
        }
    }

    fn rate_limited(wait_ms: u64, bytes: usize) -> Self {
        Self {
            allowed: false,
            action: PacketAction::RateLimited { wait_ms },
            bytes,
            drop_reason: Some(format!("rate limited, retry in {}ms", wait_ms)),
        }
    }

    fn dropped(reason: String, bytes: usize) -> Self {
        Self {
            allowed: false,
            action: PacketAction::Dropped,
            bytes,
            drop_reason: Some(reason),
        }
    }
}

struct ExitHandlerInner {
    config: ExitHandlerConfig,
    router: Router,
    bandwidth_meter: ExitNodeMeter,
    billing_engine: Option<BillingEngine>,
    packet_log: RwLock<Vec<PacketRecord>>,
    event_sender: RwLock<Option<mpsc::UnboundedSender<ExitEvent>>>,
}

/// Exit handler - processes VPN exit traffic.
#[derive(Clone)]
pub struct ExitHandler {
    inner: Arc<ExitHandlerInner>,
}

/// Exit handler events for async notifications.
#[derive(Debug, Clone)]
pub enum ExitEvent {
    /// Traffic recorded for a client.
    TrafficRecorded {
        client: NodeId,
        bytes_sent: u64,
        bytes_received: u64,
    },
    /// Rate limit hit.
    RateLimitHit {
        client: NodeId,
        wait_ms: u64,
    },
    /// Client connected.
    ClientConnected(NodeId),
    /// Client disconnected.
    ClientDisconnected(NodeId),
    /// Gateway state changed.
    GatewayStateChanged {
        old_state: GatewayState,
        new_state: GatewayState,
    },
}

impl ExitHandler {
    /// Create a new exit handler.
    pub fn new(
        config: ExitHandlerConfig,
        client: Client,
        gateway: Gateway,
    ) -> Self {
        let router = Router::with_default(client, gateway.clone());
        let bandwidth_meter = ExitNodeMeter::new();
        let billing_engine = if config.enable_billing {
            Some(BillingEngine::new())
        } else {
            None
        };

        Self {
            inner: Arc::new(ExitHandlerInner {
                config,
                router,
                bandwidth_meter,
                billing_engine,
                packet_log: RwLock::new(Vec::new()),
                event_sender: RwLock::new(None),
            }),
        }
    }

    /// Create with explicit router and meters.
    pub fn with_meters(
        config: ExitHandlerConfig,
        router: Router,
        bandwidth_meter: ExitNodeMeter,
        billing_engine: Option<BillingEngine>,
    ) -> Self {
        Self {
            inner: Arc::new(ExitHandlerInner {
                config,
                router,
                bandwidth_meter,
                billing_engine,
                packet_log: RwLock::new(Vec::new()),
                event_sender: RwLock::new(None),
            }),
        }
    }

    /// Process a packet from the mesh.
    pub fn process_packet(
        &self,
        source: &NodeId,
        destination: IpAddr,
        payload_size: usize,
    ) -> PacketResult {
        let action = self.inner.router.route(destination);

        match &action {
            crate::router::RouteAction::ForwardToMesh => {
                if self.inner.config.enable_metering {
                    self.inner.bandwidth_meter.record_traffic(
                        payload_size as u64,
                        0,
                        1,
                    );
                }
                self.log_packet(source, destination, TrafficKind::MeshLocal, payload_size);
                PacketResult::allowed(PacketAction::ForwardToMesh, payload_size)
            }
            crate::router::RouteAction::ForwardViaGateway { gateway } => {
                let meter = self.inner.bandwidth_meter.get_or_create_meter(source.clone());
                let rate_limit = meter.check_rate_limit(payload_size as u64);

                match rate_limit {
                    RateLimitResult::Allowed => {
                        if self.inner.config.enable_metering {
                            self.inner.bandwidth_meter.record_client_traffic(
                                source,
                                payload_size as u64,
                                0,
                                1,
                            );
                            self.send_event(ExitEvent::TrafficRecorded {
                                client: source.clone(),
                                bytes_sent: payload_size as u64,
                                bytes_received: 0,
                            });
                        }

                        if let Some(billing) = &self.inner.billing_engine {
                            let _ = billing.record_traffic(source, payload_size as u64, 0);
                        }

                        self.log_packet(source, destination, TrafficKind::ExitUpload, payload_size);
                        PacketResult::allowed(
                            PacketAction::ForwardViaGateway { gateway: gateway.clone() },
                            payload_size,
                        )
                    }
                    RateLimitResult::Exceeded { wait_seconds } => {
                        let wait_ms = (wait_seconds * 1000.0) as u64;
                        self.send_event(ExitEvent::RateLimitHit {
                            client: source.clone(),
                            wait_ms,
                        });
                        PacketResult::rate_limited(wait_ms, payload_size)
                    }
                }
            }
            crate::router::RouteAction::Drop { reason } => {
                self.log_packet(source, destination, TrafficKind::ExitUpload, payload_size);
                PacketResult::dropped(reason.clone(), payload_size)
            }
        }
    }

    /// Process a return packet (from Internet to mesh).
    pub fn process_return_packet(
        &self,
        destination: &NodeId,
        source: IpAddr,
        payload_size: usize,
    ) -> PacketResult {
        if self.inner.config.enable_metering {
            self.inner.bandwidth_meter.record_client_traffic(
                destination,
                0,
                payload_size as u64,
                1,
            );
            self.send_event(ExitEvent::TrafficRecorded {
                client: destination.clone(),
                bytes_sent: 0,
                bytes_received: payload_size as u64,
            });
        }

        if let Some(billing) = &self.inner.billing_engine {
            let _ = billing.record_traffic(destination, 0, payload_size as u64);
        }

        self.log_packet(
            destination,
            source,
            TrafficKind::ExitDownload,
            payload_size,
        );
        PacketResult::allowed(PacketAction::ForwardToMesh, payload_size)
    }

    /// Get current bandwidth statistics for a client.
    pub fn get_client_bandwidth(&self, client_id: &NodeId) -> Option<BandwidthStats> {
        self.inner.bandwidth_meter.client_stats(client_id)
    }

    /// Get global bandwidth statistics.
    pub fn get_global_bandwidth(&self) -> BandwidthStats {
        self.inner.bandwidth_meter.global_stats()
    }

    /// Get billing status for a client.
    pub fn get_billing_status(&self, client_id: &NodeId) -> Option<BillingStatus> {
        self.inner.billing_engine.as_ref().map(|b| b.get_status(client_id))
    }

    /// Get the router.
    pub fn router(&self) -> &Router {
        &self.inner.router
    }

    /// Get bandwidth meter.
    pub fn bandwidth_meter(&self) -> &ExitNodeMeter {
        &self.inner.bandwidth_meter
    }

    /// Get billing engine.
    pub fn billing_engine(&self) -> Option<&BillingEngine> {
        self.inner.billing_engine.as_ref()
    }

    /// Get current gateway state.
    pub fn gateway_state(&self) -> GatewayState {
        self.inner.router.snapshot().gateway_state
    }

    /// Check if this node is offering gateway services.
    pub fn is_gateway(&self) -> bool {
        self.inner.router.is_gateway()
    }

    /// Set the event channel for async notifications.
    pub fn set_event_channel(&self, sender: mpsc::UnboundedSender<ExitEvent>) {
        *self.inner.event_sender.write() = Some(sender);
    }

    fn log_packet(&self, source: &NodeId, destination: IpAddr, kind: TrafficKind, size: usize) {
        if self.inner.config.log_packets {
            let record = PacketRecord::new(source.clone(), destination, kind, size);
            let mut log = self.inner.packet_log.write();
            log.push(record);
            if log.len() > 10000 {
                log.drain(0..5000);
            }
        }
    }

    fn send_event(&self, event: ExitEvent) {
        if let Some(sender) = self.inner.event_sender.read().as_ref() {
            let _ = sender.send(event);
        }
    }

    /// Get recent packet log.
    pub fn get_packet_log(&self, limit: usize) -> Vec<PacketRecord> {
        let log = self.inner.packet_log.read();
        log.iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Clear packet log.
    pub fn clear_packet_log(&self) {
        self.inner.packet_log.write().clear();
    }

    /// Take a full snapshot of the handler state.
    pub fn snapshot(&self) -> ExitHandlerSnapshot {
        ExitHandlerSnapshot {
            is_gateway: self.is_gateway(),
            gateway_state: self.gateway_state(),
            global_bandwidth: self.get_global_bandwidth(),
            tracked_clients: self.inner.bandwidth_meter.tracked_clients(),
            billing_enabled: self.inner.billing_engine.is_some(),
        }
    }
}

/// Snapshot of exit handler state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitHandlerSnapshot {
    pub is_gateway: bool,
    pub gateway_state: GatewayState,
    pub global_bandwidth: BandwidthStats,
    pub tracked_clients: Vec<NodeId>,
    pub billing_enabled: bool,
}

/// Trait for async packet processing (for integration with tokio-based systems).
#[async_trait]
pub trait AsyncExitHandler: Send + Sync {
    /// Process a packet asynchronously.
    async fn process_packet_async(
        &self,
        source: NodeId,
        destination: IpAddr,
        payload: Vec<u8>,
    ) -> PacketResult;

    /// Get traffic statistics asynchronously.
    async fn get_stats(&self) -> ExitHandlerSnapshot;
}

#[async_trait]
impl AsyncExitHandler for ExitHandler {
    async fn process_packet_async(
        &self,
        source: NodeId,
        destination: IpAddr,
        payload: Vec<u8>,
    ) -> PacketResult {
        self.process_packet(&source, destination, payload.len())
    }

    async fn get_stats(&self) -> ExitHandlerSnapshot {
        self.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_handler() -> ExitHandler {
        let config = ExitHandlerConfig {
            enable_metering: true,
            enable_billing: false,
            ..Default::default()
        };
        let client = Client::default();
        let gateway = Gateway::new(NodeId::random());
        ExitHandler::new(config, client, gateway)
    }

    #[test]
    fn handler_processes_exit_packet() {
        let egress = NodeId::random();
        let client = Client::default();
        client.use_gateway(egress.clone()).unwrap();

        let config = ExitHandlerConfig {
            enable_metering: true,
            enable_billing: false,
            ..Default::default()
        };
        let gateway = Gateway::new(NodeId::random());
        let handler = ExitHandler::new(config, client, gateway);

        let source = NodeId::random();

        let result = handler.process_packet(
            &source,
            "8.8.8.8".parse().unwrap(),
            1024,
        );

        assert!(result.allowed);
    }

    #[test]
    fn handler_drops_without_gateway() {
        let handler = test_handler();
        let source = NodeId::random();

        let result = handler.process_packet(
            &source,
            "8.8.8.8".parse().unwrap(),
            1024,
        );

        assert!(!result.allowed);
        assert!(result.drop_reason.is_some());
    }

    #[test]
    fn handler_forwards_mesh_traffic() {
        let handler = test_handler();
        let source = NodeId::random();
        let target = NodeId::random();
        let vip = adnet_types::VirtualIp::from_node_id(&target);

        let result = handler.process_packet(
            &source,
            vip.ipv4.as_std().into(),
            1024,
        );

        assert!(result.allowed);
        assert_eq!(result.action, PacketAction::ForwardToMesh);
    }

    #[test]
    fn handler_tracks_bandwidth() {
        let egress = NodeId::random();
        let client = Client::default();
        client.use_gateway(egress.clone()).unwrap();

        let config = ExitHandlerConfig {
            enable_metering: true,
            enable_billing: false,
            ..Default::default()
        };
        let gateway = Gateway::new(NodeId::random());
        let handler = ExitHandler::new(config, client, gateway);

        let source = NodeId::random();

        handler.process_packet(&source, "8.8.8.8".parse().unwrap(), 1024);

        let stats = handler.get_client_bandwidth(&source).unwrap();
        assert_eq!(stats.bytes_sent, 1024);
    }

    #[test]
    fn handler_snapshot_contains_state() {
        let handler = test_handler();
        let snap = handler.snapshot();

        assert!(!snap.is_gateway);
        assert_eq!(snap.tracked_clients.len(), 0);
        assert!(!snap.billing_enabled);
    }

    #[test]
    fn packet_log_records_packets() {
        let mut config = ExitHandlerConfig::default();
        config.log_packets = true;

        let handler = ExitHandler::with_meters(
            config,
            Router::with_default(Client::default(), Gateway::new(NodeId::random())),
            ExitNodeMeter::new(),
            None,
        );

        let source = NodeId::random();
        let target = NodeId::random();
        let vip = adnet_types::VirtualIp::from_node_id(&target);

        handler.process_packet(&source, vip.ipv4.as_std().into(), 512);

        let log = handler.get_packet_log(10);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].size_bytes, 512);
    }

    #[test]
    fn process_return_packet_records_download() {
        let handler = test_handler();
        let dest = NodeId::random();

        handler.process_return_packet(&dest, "8.8.8.8".parse().unwrap(), 2048);

        let stats = handler.get_client_bandwidth(&dest).unwrap();
        assert_eq!(stats.bytes_received, 2048);
    }
}
