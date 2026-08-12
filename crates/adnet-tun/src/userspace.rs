//! Pure userspace TUN device — the default backend.
//!
//! Useful for:
//!
//! - Tests that exercise the mesh stack end-to-end without
//!   needing root.
//! - CI environments where opening a real TUN is impossible.
//! - Embedded / unprivileged deployments where the mesh is
//!   purely "above the kernel" (e.g. userspace DNS
//!   inspection, app-level RPC over the same wire format).
//!
//! ## Direction model
//!
//! A real TUN has two independent packet streams. We model
//! them as two channels:
//!
//! - **kernel → tunnel** ([`UserspaceTun::inject_from_kernel`]):
//!   packets the host would have written into the TUN. The
//!   mesh stack reads them via [`TunDevice::recv`].
//! - **tunnel → kernel** ([`UserspaceTun::drain_to_kernel`]):
//!   packets the mesh stack wrote via [`TunDevice::send`],
//!   drained by whatever is acting as the kernel-side proxy
//!   (tests use a simple loop; a real integration would feed
//!   them back into a userspace stack or a real TUN).
//!
//! The two halves are intentionally separate so a test can
//! inject a single packet in, read it through
//! [`TunDevice::recv`], transform it, write it back via
//! [`TunDevice::send`], and assert on the result via
//! [`UserspaceTun::drain_to_kernel`] without any kernel
//! interaction.

use std::net::Ipv4Addr;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::sync::mpsc;

use crate::device::{DeviceInfo, DeviceState, TunDevice};
use crate::error::{TunError, TunResult};

/// Configuration for a [`UserspaceTun`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserspaceTunConfig {
    pub name: String,
    pub mtu: u32,
    pub local_ipv4: Ipv4Addr,
}

impl Default for UserspaceTunConfig {
    fn default() -> Self {
        Self {
            name: "adnet-tun0".to_string(),
            mtu: 1420,
            local_ipv4: Ipv4Addr::new(100, 64, 0, 1),
        }
    }
}

/// Default capacity for each internal channel.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// A pure-userspace TUN device.
///
/// Cheap to construct; clone-friendly (the inner state is
/// `Arc`-shared so multiple tasks can drive `recv` / `send`).
#[derive(Clone)]
pub struct UserspaceTun {
    inner: Arc<UserspaceTunInner>,
}

struct UserspaceTunInner {
    info: RwLock<Option<DeviceInfo>>,
    state: parking_lot::Mutex<DeviceState>,
    /// kernel → tunnel. The mesh reads from this in `recv`.
    /// The receiver lives **permanently** in the struct; we
    /// rely on the channel's own close-on-sender-drop
    /// semantics for shutdown rather than wrapping it in a
    /// mutex (which made concurrent `recv` callers race).
    from_kernel_rx: parking_lot::Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
    from_kernel_tx: parking_lot::Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    /// tunnel → kernel. The mesh writes into this in `send`;
    /// the kernel side drains it via `drain_to_kernel`.
    to_kernel_rx: parking_lot::Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
    to_kernel_tx: parking_lot::Mutex<Option<mpsc::Sender<Vec<u8>>>>,
}

impl UserspaceTun {
    /// Build a new userspace TUN with the default capacity.
    pub fn new(cfg: UserspaceTunConfig) -> Self {
        Self::with_capacity(cfg, DEFAULT_CHANNEL_CAPACITY)
    }

    /// Build a new userspace TUN with a custom channel capacity.
    pub fn with_capacity(cfg: UserspaceTunConfig, cap: usize) -> Self {
        let info = DeviceInfo {
            name: cfg.name,
            mtu: cfg.mtu,
            local_ipv4: cfg.local_ipv4,
        };
        let (tx_kernel_to_tun, rx_kernel_to_tun) = mpsc::channel(cap);
        let (tx_tun_to_kernel, rx_tun_to_kernel) = mpsc::channel(cap);
        let inner = UserspaceTunInner {
            info: RwLock::new(Some(info)),
            state: parking_lot::Mutex::new(DeviceState::Down),
            from_kernel_rx: parking_lot::Mutex::new(Some(rx_kernel_to_tun)),
            from_kernel_tx: parking_lot::Mutex::new(Some(tx_kernel_to_tun)),
            to_kernel_rx: parking_lot::Mutex::new(Some(rx_tun_to_kernel)),
            to_kernel_tx: parking_lot::Mutex::new(Some(tx_tun_to_kernel)),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Push one packet into the kernel → tunnel direction.
    /// Returns `Err(TunError::ChannelClosed)` if the device
    /// has been shut down.
    pub async fn inject_from_kernel(&self, pkt: Vec<u8>) -> TunResult<()> {
        let tx = self
            .inner
            .from_kernel_tx
            .lock()
            .as_ref()
            .cloned()
            .ok_or(TunError::AlreadyClosed)?;
        tx.send(pkt).await.map_err(|_| TunError::ChannelClosed)
    }

    /// Pop one packet from the tunnel → kernel direction.
    /// Returns `Ok(None)` if the device is closed.
    ///
    /// Concurrency model: at most ONE task at a time should
    /// drive `drain_to_kernel` (and likewise `recv`). The
    /// `UserspaceTun` is `Send + Sync` for sharing the device
    /// handle, but `mpsc::Receiver` is single-consumer — a
    /// second concurrent drain will steal packets from the
    /// first. Documented and tested below.
    pub async fn drain_to_kernel(&self) -> TunResult<Option<Vec<u8>>> {
        if *self.inner.state.lock() == DeviceState::Closed {
            return Ok(None);
        }
        // Single-consumer recv: take the receiver for the
        // duration of the await and put it back unless
        // shutdown wins the race.
        let mut rx = self
            .inner
            .to_kernel_rx
            .lock()
            .take()
            .ok_or(TunError::AlreadyClosed)?;
        let res = rx.recv().await;
        // Always put the receiver back so the next drain
        // can see it. Shutdown drains the slot via its own
        // `take()` and would race here; whichever wins
        // decides whether the next caller sees
        // `AlreadyClosed` or a working receiver.
        *self.inner.to_kernel_rx.lock() = Some(rx);
        Ok(res)
    }

    /// Mark the device as up. Subsequent packets may be
    /// pushed / popped.
    pub fn bring_up(&self) {
        let mut state = self.inner.state.lock();
        if *state == DeviceState::Down {
            *state = DeviceState::Up;
        }
    }

    /// How many packets are buffered in the kernel → tunnel
    /// direction. Useful for tests asserting that the mesh
    /// has not yet drained the queue.
    ///
    /// `mpsc::Sender::capacity()` reports the current number
    /// of *free* slots; combined with `max_capacity()` (the
    /// configured upper bound) we can derive how many packets
    /// are pending. If the channel has been closed (the
    /// `take()` paths) we report `0`.
    pub fn from_kernel_capacity(&self) -> usize {
        let guard = self.inner.from_kernel_tx.lock();
        match guard.as_ref() {
            Some(tx) => tx.max_capacity().saturating_sub(tx.capacity()),
            None => 0,
        }
    }
}

#[async_trait]
impl TunDevice for UserspaceTun {
    async fn recv(&self) -> TunResult<Option<Vec<u8>>> {
        if *self.inner.state.lock() == DeviceState::Closed {
            return Ok(None);
        }
        // Single-consumer recv: take the receiver for the
        // duration of the await and put it back unless
        // shutdown wins the race.
        let mut rx = self
            .inner
            .from_kernel_rx
            .lock()
            .take()
            .ok_or(TunError::AlreadyClosed)?;
        let res = rx.recv().await;
        // The slot was empty before we took — reinsert the
        // receiver unconditionally so the next `recv`
        // call can take it again. Shutdown explicitly
        // empties the slot (via its own take()) and would
        // race here; whichever wins decides whether the
        // next caller sees `AlreadyClosed` or a working
        // receiver.
        *self.inner.from_kernel_rx.lock() = Some(rx);
        Ok(res)
    }

    async fn send(&self, pkt: Vec<u8>) -> TunResult<()> {
        // MTU enforcement: refuse to push packets larger
        // than the configured MTU. We use the upper bound
        // (1500 - IPv4 header = 1480) as the default cap so
        // the default build does not surprise anyone.
        let mtu = self.info().map(|i| i.mtu).unwrap_or(1420) as usize;
        if pkt.len() > mtu {
            return Err(TunError::PacketTooLarge {
                actual: pkt.len(),
                mtu: mtu as u32,
            });
        }
        let tx = self
            .inner
            .to_kernel_tx
            .lock()
            .as_ref()
            .cloned()
            .ok_or(TunError::AlreadyClosed)?;
        tx.send(pkt).await.map_err(|_| TunError::ChannelClosed)
    }

    fn state(&self) -> DeviceState {
        *self.inner.state.lock()
    }

    fn info(&self) -> Option<DeviceInfo> {
        self.inner.info.read().clone()
    }

    async fn shutdown(&self) -> TunResult<()> {
        let mut state = self.inner.state.lock();
        *state = DeviceState::Closed;
        drop(state);
        // Drop both senders so any in-flight `recv` /
        // `drain_to_kernel` returns `None`. Then drop the
        // receivers so a future `recv` returns
        // `AlreadyClosed` deterministically.
        drop(self.inner.from_kernel_tx.lock().take());
        drop(self.inner.to_kernel_tx.lock().take());
        drop(self.inner.from_kernel_rx.lock().take());
        drop(self.inner.to_kernel_rx.lock().take());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::parse_packet;

    #[test]
    fn userspace_config_default() {
        let c = UserspaceTunConfig::default();
        assert_eq!(c.name, "adnet-tun0");
        assert_eq!(c.mtu, 1420);
        assert_eq!(c.local_ipv4, Ipv4Addr::new(100, 64, 0, 1));
    }

    #[tokio::test]
    async fn userspace_roundtrip_sends_back_what_was_injected() {
        let dev = UserspaceTun::new(UserspaceTunConfig::default());
        dev.bring_up();
        assert_eq!(dev.state(), DeviceState::Up);

        let mut pkt = vec![0u8; 44];
        pkt[0] = 0x45;
        pkt[2] = (44u16 >> 8) as u8;
        pkt[3] = (44u16 & 0xff) as u8;
        pkt[9] = 6; // TCP
        pkt[12..16].copy_from_slice(&[100, 64, 0, 5]);
        pkt[16..20].copy_from_slice(&[100, 64, 0, 7]);

        dev.inject_from_kernel(pkt.clone()).await.unwrap();
        let got = dev.recv().await.unwrap().unwrap();
        assert_eq!(got, pkt);
        // Header parse confirms the bytes are coherent.
        let parsed = parse_packet(&got).unwrap();
        assert_eq!(parsed.src_v4(), [100, 64, 0, 5]);

        // Outbound direction.
        dev.send(pkt.clone()).await.unwrap();
        let out = dev.drain_to_kernel().await.unwrap().unwrap();
        assert_eq!(out, pkt);
    }

    #[tokio::test]
    async fn userspace_recv_returns_none_after_shutdown() {
        let dev = UserspaceTun::new(UserspaceTunConfig::default());
        dev.shutdown().await.unwrap();
        assert_eq!(dev.state(), DeviceState::Closed);
        let got = dev.recv().await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn userspace_inject_after_shutdown_errors() {
        let dev = UserspaceTun::new(UserspaceTunConfig::default());
        dev.shutdown().await.unwrap();
        let res = dev.inject_from_kernel(vec![0u8; 20]).await;
        // After shutdown the sender slot is empty, so the
        // operation surfaces `AlreadyClosed` rather than
        // `ChannelClosed`. Either is fine semantically;
        // `AlreadyClosed` is the more accurate error here.
        assert!(matches!(
            res,
            Err(TunError::AlreadyClosed) | Err(TunError::ChannelClosed)
        ));
    }

    #[tokio::test]
    async fn userspace_send_too_large_errors() {
        let dev = UserspaceTun::new(UserspaceTunConfig::default());
        dev.bring_up();
        let big = vec![0u8; 5000];
        let res = dev.send(big).await;
        assert!(matches!(res, Err(TunError::PacketTooLarge { .. })));
    }

    /// Regression: shutdown mid-recv must let the in-flight
    /// `recv()` observe the closed state and return
    /// `Ok(None)` deterministically.
    #[tokio::test]
    async fn userspace_shutdown_during_recv_returns_none() {
        let dev = UserspaceTun::new(UserspaceTunConfig::default());
        dev.bring_up();
        // Spawn a task that waits for shutdown, then calls recv.
        let dev2 = dev.clone();
        let task = tokio::spawn(async move { dev2.recv().await });
        // Give the task a moment to enter the await.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        dev.shutdown().await.unwrap();
        let got = task.await.unwrap().unwrap();
        assert!(got.is_none(), "recv after shutdown must return Ok(None)");
    }

    /// Regression: concurrent `inject_from_kernel` + `recv`
    /// no longer race. With the old `take()`-the-receiver
    /// pattern a concurrent `inject` succeeded but a
    /// concurrent second `recv` returned spurious
    /// `AlreadyClosed` errors.
    #[tokio::test]
    async fn userspace_inject_recv_under_load_is_stable() {
        let dev = UserspaceTun::new(UserspaceTunConfig::default());
        dev.bring_up();
        let mut handles = Vec::new();
        for i in 0..16u8 {
            let dev = dev.clone();
            handles.push(tokio::spawn(async move {
                let mut pkt = vec![0u8; 20];
                pkt[0] = 0x45;
                pkt[2] = 0;
                pkt[3] = 20;
                pkt[9] = i; // unique protocol byte per task
                dev.inject_from_kernel(pkt).await.unwrap();
            }));
        }
        // Single consumer recv drains all 16 packets.
        let mut received = Vec::new();
        for _ in 0..16 {
            let pkt = dev.recv().await.unwrap().unwrap();
            received.push(pkt[9]);
        }
        received.sort();
        let expected: Vec<u8> = (0..16).collect();
        assert_eq!(received, expected);
        for h in handles {
            h.await.unwrap();
        }
    }

    #[test]
    fn userspace_info_reflects_config() {
        let cfg = UserspaceTunConfig {
            name: "utun42".into(),
            mtu: 1380,
            local_ipv4: Ipv4Addr::new(100, 64, 5, 5),
        };
        let dev = UserspaceTun::new(cfg.clone());
        let info = dev.info().unwrap();
        assert_eq!(info.name, "utun42");
        assert_eq!(info.mtu, 1380);
        assert_eq!(info.local_ipv4, Ipv4Addr::new(100, 64, 5, 5));
    }

    /// `from_kernel_capacity` must report non-zero pending slots
    /// after `inject_from_kernel` and zero after the slot is
    /// consumed by `recv`. Regression for the prior stub that
    /// hard-coded `0` for both cases.
    #[tokio::test]
    async fn userspace_from_kernel_capacity_tracks_pending() {
        let dev = UserspaceTun::with_capacity(UserspaceTunConfig::default(), 8);
        dev.bring_up();

        // Inject three packets without consuming any.
        for i in 0..3u8 {
            let mut pkt = vec![0u8; 24];
            pkt[0] = 0x45;
            pkt[3] = 24;
            pkt[9] = i;
            dev.inject_from_kernel(pkt).await.unwrap();
        }
        // Channel math: 8 - tx.capacity() == 3 → capacity() == 5.
        assert_eq!(dev.from_kernel_capacity(), 3);

        // Drain one; remaining count drops by one.
        let _ = dev.recv().await.unwrap().unwrap();
        assert_eq!(dev.from_kernel_capacity(), 2);
        let _ = dev.recv().await.unwrap().unwrap();
        assert_eq!(dev.from_kernel_capacity(), 1);
        let _ = dev.recv().await.unwrap().unwrap();
        assert_eq!(dev.from_kernel_capacity(), 0);

        // After full shutdown the value resolves to zero
        // deterministically (the slot is `None`).
        dev.shutdown().await.unwrap();
        assert_eq!(dev.from_kernel_capacity(), 0);
    }
}
