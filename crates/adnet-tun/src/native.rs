//! Native TUN backend — backed by the `tun` crate.
//!
//! Available behind the `native` cargo feature. The `tun`
//! crate wraps the platform-specific ioctl / device:
//!
//! - **macOS / iOS** — opens a `utun` socket (no kernel
//!   module needed). IPv4 / IPv6 frames come through as
//!   4-byte-pseudo-header + IP packet.
//! - **Linux** — opens `/dev/net/tun` and registers a
//!   `tun0`-style interface.
//! - **Windows** — talks to a wintun.dll downloaded by the
//!   `tun` crate if missing.
//!
//! On all three, the wire format the mesh stack sees is
//! the same: raw IP packets (no platform shim). The
//! `tun::Device` returns a `AsyncDevice` whose `recv` /
//! `send` methods already implement that contract.

use std::net::Ipv4Addr;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::device::{DeviceInfo, DeviceState, TunDevice};
use crate::error::{TunError, TunResult};

/// Configuration for the native backend.
#[derive(Debug, Clone)]
pub struct NativeTunConfig {
    pub name: String,
    pub mtu: u32,
    pub local_ipv4: Ipv4Addr,
}

impl Default for NativeTunConfig {
    fn default() -> Self {
        Self {
            name: "adnet-tun0".to_string(),
            mtu: 1420,
            local_ipv4: Ipv4Addr::new(100, 64, 0, 1),
        }
    }
}

/// A real, kernel-backed TUN device.
///
/// Construction requires the platform-specific privilege to
/// open the underlying device (root on Linux, `com.apple.networking.tunnel` entitlement
/// on macOS / iOS, Admin on Windows). On failure we return
/// a `TunError::Platform` with the ioctl message verbatim.
pub struct NativeTun {
    inner: Arc<NativeTunInner>,
}

struct NativeTunInner {
    info: Mutex<Option<DeviceInfo>>,
    state: Mutex<DeviceState>,
    /// Underlying async device. We hold an Option so
    /// `shutdown` can drop it, which causes any in-flight
    /// `recv` to return Err.
    dev: Mutex<Option<tun::AsyncDevice>>,
    closed: Notify,
}

impl NativeTun {
    /// Open a native TUN device with the platform default
    /// settings (utun on macOS, `/dev/net/tun` on Linux,
    /// wintun on Windows).
    ///
    /// On Linux the call also brings the interface up with
    /// the supplied IPv4 address and MTU; on macOS the
    /// address is configured via a follow-up `ifconfig`
    /// invocation that the operator is expected to run
    /// (or that the daemon will run on their behalf).
    pub async fn open(cfg: NativeTunConfig) -> TunResult<Self> {
        let mut dev = tun::Configuration::default();
        dev.name(&cfg.name)
            .mtu(cfg.mtu)
            .address(cfg.local_ipv4)
            .netmask(Ipv4Addr::new(255, 192, 0, 0))
            .up()
            .push();
        let dev = tun::create_as_async(&dev).map_err(|e| {
            TunError::Platform(format!("tun::create_as_async failed: {e}"))
        })?;
        let info = DeviceInfo {
            name: cfg.name,
            mtu: cfg.mtu,
            local_ipv4: cfg.local_ipv4,
        };
        Ok(Self {
            inner: Arc::new(NativeTunInner {
                info: Mutex::new(Some(info)),
                state: Mutex::new(DeviceState::Up),
                dev: Mutex::new(Some(dev)),
                closed: Notify::new(),
            }),
        })
    }
}

#[async_trait]
impl TunDevice for NativeTun {
    async fn recv(&self) -> TunResult<Option<Vec<u8>>> {
        // Take the device out of the slot for the duration of
        // the await. parking_lot's Mutex doesn't poison, so
        // this is safe even after a panic during recv.
        let mut dev_guard = self.inner.dev.lock();
        let dev = match dev_guard.as_mut() {
            Some(d) => d,
            None => return Ok(None),
        };
        // tun::AsyncDevice::recv yields a Result<Vec<u8>>. We
        // forward any I/O error directly.
        let pkt = dev.recv().await.map_err(TunError::Io)?;
        Ok(Some(pkt))
    }

    async fn send(&self, pkt: Vec<u8>) -> TunResult<()> {
        let mtu = self
            .info()
            .map(|i| i.mtu)
            .unwrap_or(1420) as usize;
        if pkt.len() > mtu {
            return Err(TunError::PacketTooLarge {
                actual: pkt.len(),
                mtu: mtu as u32,
            });
        }
        let mut dev_guard = self.inner.dev.lock();
        let dev = match dev_guard.as_mut() {
            Some(d) => d,
            None => return Err(TunError::AlreadyClosed),
        };
        dev.send(&pkt).await.map_err(TunError::Io)?;
        Ok(())
    }

    fn state(&self) -> DeviceState {
        *self.inner.state.lock()
    }

    fn info(&self) -> Option<DeviceInfo> {
        self.inner.info.lock().clone()
    }

    async fn shutdown(&self) -> TunResult<()> {
        let mut state = self.inner.state.lock();
        *state = DeviceState::Closed;
        drop(state);
        // Drop the device. Any outstanding `recv` will then
        // see `None` on the next iteration. (The `tun` crate's
        // async wrapper does not currently expose cancellation
        // so the in-flight recv may complete once; subsequent
        // recvs will see `None`.)
        let mut dev_guard = self.inner.dev.lock();
        *dev_guard = None;
        self.inner.closed.notify_waiters();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_config_default() {
        let c = NativeTunConfig::default();
        assert_eq!(c.name, "adnet-tun0");
        assert_eq!(c.mtu, 1420);
        assert_eq!(c.local_ipv4, Ipv4Addr::new(100, 64, 0, 1));
    }
}
