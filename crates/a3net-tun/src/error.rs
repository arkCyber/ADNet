//! Error type for the TUN crate.
//!
//! Kept deliberately small — the mesh firewall / routing layers
//! are expected to map these errors into their own error types.

use thiserror::Error;

pub type TunResult<T> = std::result::Result<T, TunError>;

#[derive(Debug, Error)]
pub enum TunError {
    #[error("tun device is not open (state = {0:?})")]
    NotOpen(crate::device::DeviceState),

    #[error("tun device is already closed")]
    AlreadyClosed,

    #[error("invalid packet: {0}")]
    InvalidPacket(String),

    #[error("packet too large: {actual} bytes (mtu = {mtu})")]
    PacketTooLarge { actual: usize, mtu: u32 },

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("channel closed")]
    ChannelClosed,

    #[error("platform error: {0}")]
    Platform(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceState;

    #[test]
    fn not_open_includes_state() {
        let e = TunError::NotOpen(DeviceState::Closed);
        assert!(e.to_string().contains("Closed"));
    }

    #[test]
    fn packet_too_large_includes_sizes() {
        let e = TunError::PacketTooLarge {
            actual: 2000,
            mtu: 1500,
        };
        let s = e.to_string();
        assert!(s.contains("2000"));
        assert!(s.contains("1500"));
    }

    #[test]
    fn from_io_error() {
        let ioe = std::io::Error::other("boom");
        let e: TunError = ioe.into();
        assert!(matches!(e, TunError::Io(_)));
    }
}
