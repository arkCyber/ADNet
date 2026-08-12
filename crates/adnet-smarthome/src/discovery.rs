//! Device discovery service

use crate::device::{Device, DeviceType};
use crate::error::{Result, SmartHomeError};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info};

/// Discovery protocols supported
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryProtocol {
    Mdns,
    Upnp,
}

/// An event emitted when discovery finds or loses a device
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    Found(Device),
    Lost(String),
}

/// Manages multiple discovery backends
pub struct DiscoveryManager {
    tx: mpsc::Sender<DiscoveryEvent>,
    rx: tokio::sync::Mutex<mpsc::Receiver<DiscoveryEvent>>,
}

impl DiscoveryManager {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(256);
        Self {
            tx,
            rx: tokio::sync::Mutex::new(rx),
        }
    }

    /// Receive one discovery event (used by the hub background task)
    pub async fn recv(&self) -> Option<DiscoveryEvent> {
        self.rx.lock().await.recv().await
    }

    /// Kick off mDNS scanning for known service types
    pub async fn discover(&self, protocol: DiscoveryProtocol) -> Result<Vec<String>> {
        match protocol {
            DiscoveryProtocol::Mdns => self.run_mdns_scan().await,
            DiscoveryProtocol::Upnp => {
                debug!("UPnP discovery not yet implemented");
                Ok(Vec::new())
            }
        }
    }

    async fn run_mdns_scan(&self) -> Result<Vec<String>> {
        let tx = self.tx.clone();
        let service_types = vec!["_miio._udp.local.", "_hap._tcp.local.", "_matter._tcp.local."];

        let daemon = mdns_sd::ServiceDaemon::new()
            .map_err(|e| SmartHomeError::Discovery(e.to_string()))?;

        let mut handles = Vec::new();

        for svc_type in service_types {
            let receiver = daemon.browse(svc_type)
                .map_err(|e| SmartHomeError::Discovery(format!("{}: {}", svc_type, e)))?;
            let tx2 = tx.clone();
            let st = svc_type.to_string();

            handles.push(tokio::spawn(async move {
                // scan for 5 seconds then stop
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep_until(deadline) => break,
                        Ok(event) = receiver.recv_async() => {
                            match event {
                                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                                    info!("mDNS found {} via {}", info.get_fullname(), st);

                                    let ip = info.get_addresses()
                                        .iter()
                                        .next()
                                        .map(|a| a.to_string());

                                    let mut device = Device::new(
                                        format!("mdns_{}", info.get_hostname()),
                                        info.get_fullname(),
                                        DeviceType::WiFi,
                                    );
                                    device.ip_address = ip;
                                    device.online = true;

                                    let _ = tx2.send(DiscoveryEvent::Found(device)).await;
                                }
                                mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                                    let _ = tx2.send(
                                        DiscoveryEvent::Lost(format!("mdns_{}", fullname))
                                    ).await;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }));
        }

        for h in handles {
            let _ = h.await;
        }

        Ok(Vec::new())
    }
}

impl Default for DiscoveryManager {
    fn default() -> Self {
        Self::new()
    }
}
