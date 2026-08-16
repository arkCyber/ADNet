//! TUN device trait + lifecycle types.
//!
//! A TUN device is a single-producer, single-consumer packet
//! pipe: the kernel writes outbound (host → tunnel) packets
//! into the device, the userspace mesh stack reads them via
//! [`TunDevice::recv`], routes / transforms them, and writes
//! the resulting inbound (tunnel → host) packets back via
//! [`TunDevice::send`].

use std::net::Ipv4Addr;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::error::TunResult;

/// Information about an opened TUN device.
///
/// `name` is the platform-specific interface name (e.g.
/// `utun3` on macOS, `tun0` on Linux). `mtu` is the link MTU
/// the kernel reports; the mesh stack uses it to size
/// outbound packets so they fit in a single datagram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Platform-specific interface name.
    pub name: String,
    /// Maximum transmission unit (bytes).
    pub mtu: u32,
    /// The IPv4 address the kernel will route mesh-bound
    /// packets to. Derived from the operator-supplied mesh
    /// identity.
    pub local_ipv4: Ipv4Addr,
}

/// Lifecycle state of a TUN device.
///
/// `TunDevice::state` returns this so callers can react to
/// `Up -> Down` transitions (e.g. on `ray down` in
/// rayfish's model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceState {
    /// Constructed but not yet opened.
    Down,
    /// Opened and visible to the kernel.
    Up,
    /// The device was closed (administrative or error).
    Closed,
}

/// Asynchronous TUN device handle.
///
/// Implementations are split into two halves:
///
/// - `send` / `recv` move packets through the device.
/// - `state` / `info` expose the current lifecycle and
///   metadata. These are cheap and synchronous, so they
///   are not part of the `async_trait` async surface.
///
/// Implementations must be `Send + Sync` so a single device
/// can be shared across the firewall task, exit-node task,
/// and a debug inspector.
#[async_trait]
pub trait TunDevice: Send + Sync {
    /// Read one packet from the device (host → tunnel).
    ///
    /// Returns `Ok(None)` if the device is closed; callers
    /// typically translate that into "stop the read loop".
    /// Blocks until a packet arrives.
    async fn recv(&self) -> TunResult<Option<Vec<u8>>>;

    /// Write one packet to the device (tunnel → host).
    ///
    /// The caller must size the packet to ≤ `info().mtu` or
    /// the kernel will silently drop it. Returns `Err` if the
    /// device has been closed.
    async fn send(&self, pkt: Vec<u8>) -> TunResult<()>;

    /// Current device lifecycle state.
    fn state(&self) -> DeviceState;

    /// Device metadata. `None` if the device is `Down` or
    /// `Closed`.
    fn info(&self) -> Option<DeviceInfo>;

    /// Bring the device down and release resources. After
    /// `shutdown` returns, subsequent `send` / `recv` calls
    /// must fail with a "closed" error.
    async fn shutdown(&self) -> TunResult<()>;
}

/// Convenience wrapper that converts a [`TunDevice`] into
/// two separate channels (`tx`, `rx`) for callers that
/// prefer working with `mpsc` rather than the trait.
///
/// `tx` is a sink for `tunnel → host` packets (i.e. the
/// `TunDevice::send` direction).
/// `rx` is a stream of `host → tunnel` packets (i.e. the
/// `TunDevice::recv` direction).
///
/// The returned channels are unbounded. Callers that need
/// back-pressure should layer one in front of them.
pub fn split(
    dev: std::sync::Arc<dyn TunDevice>,
    buf: usize,
) -> (mpsc::Sender<Vec<u8>>, mpsc::Receiver<Vec<u8>>) {
    let (tx_out, mut rx_out) = mpsc::channel::<Vec<u8>>(buf);
    let (tx_in, rx_in) = mpsc::channel::<Vec<u8>>(buf);
    // Outbound task: drain `tx_out` into `dev.send`.
    let dev_out = dev.clone();
    tokio::spawn(async move {
        while let Some(pkt) = rx_out.recv().await {
            if let Err(e) = dev_out.send(pkt).await {
                tracing::warn!(error = %e, "tun send failed; outbound task exits");
                break;
            }
        }
    });
    // Inbound task: drain `dev.recv` into `tx_in`.
    tokio::spawn(async move {
        loop {
            match dev.recv().await {
                Ok(Some(pkt)) => {
                    if tx_in.send(pkt).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "tun recv failed; inbound task exits");
                    break;
                }
            }
        }
    });
    (tx_out, rx_in)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_state_equality() {
        assert_eq!(DeviceState::Down, DeviceState::Down);
        assert_ne!(DeviceState::Down, DeviceState::Up);
        assert_ne!(DeviceState::Up, DeviceState::Closed);
    }

    #[test]
    fn device_state_serde() {
        let s = serde_json::to_string(&DeviceState::Up).unwrap();
        let back: DeviceState = serde_json::from_str(&s).unwrap();
        assert_eq!(back, DeviceState::Up);
    }

    #[test]
    fn device_info_serde() {
        let info = DeviceInfo {
            name: "utun7".to_string(),
            mtu: 1420,
            local_ipv4: Ipv4Addr::new(100, 64, 23, 142),
        };
        let s = serde_json::to_string(&info).unwrap();
        let back: DeviceInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(info, back);
    }
}
