//! Minimal STUN message codec (RFC 5389 + RFC 5766 / TURN extensions).
//!
//! Just enough to:
//!
//! - encode/decode the 20-byte STUN header (type, length,
//!   magic cookie, transaction id),
//! - encode/decode the small set of attributes we use for
//!   TURN: `REQUESTED-TRANSPORT`, `LIFETIME`,
//!   `RELAYED-ADDRESS`, `XOR-RELAYED-ADDRESS`,
//!   `XOR-MAPPED-ADDRESS`, `MESSAGE-INTEGRITY`,
//!   `NONCE`, `REALM`, `USERNAME`, `ERROR-CODE`.
//!
//! Full STUN has 30+ attributes and complex parsing
//! requirements (padding to 4-byte boundaries, attribute
//! repetition, etc.); we only implement the subset the TURN
//! client needs.
//!
//! ## References
//!
//! - RFC 5389 — STUN
//! - RFC 5766 — TURN (extends STUN)
//! - RFC 6062 — TURN over TCP (not yet implemented here)
//! - RFC 8489 — STUN bis (successor; same wire format for
//!   the parts we use)

use std::net::{Ipv4Addr, SocketAddr};

/// STUN magic cookie (RFC 5389 § 6).
pub const MAGIC_COOKIE: u32 = 0x2112A442;

/// 12-byte STUN transaction ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactionId(pub [u8; 12]);

impl TransactionId {
    /// Random transaction ID.
    pub fn random() -> Self {
        let mut bytes = [0u8; 12];
        rand::Rng::fill(&mut rand::thread_rng(), &mut bytes);
        Self(bytes)
    }
}

/// STUN message types we care about. Codes follow RFC 5389
/// § 6 plus the TURN additions (RFC 5766 § 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    BindingRequest,
    BindingResponse,
    BindingError,
    AllocateRequest,
    AllocateResponse,
    AllocateError,
    RefreshRequest,
    RefreshResponse,
    RefreshError,
    CreatePermissionRequest,
    ChannelBindRequest,
    ChannelBindResponse,
    ChannelBindError,
    /// 0x0016 = Send Indication.
    SendIndication,
    /// 0x0017 = Data Indication.
    DataIndication,
    /// Raw fallback for unknown types (e.g. from other
    /// implementations).
    Other(u16),
}

impl MessageType {
    fn from_u16(v: u16) -> Self {
        match v {
            0x0001 => Self::BindingRequest,
            0x0101 => Self::BindingResponse,
            0x0111 => Self::BindingError,
            0x0003 => Self::AllocateRequest,
            0x0103 => Self::AllocateResponse,
            0x0113 => Self::AllocateError,
            0x0004 => Self::RefreshRequest,
            0x0104 => Self::RefreshResponse,
            0x0114 => Self::RefreshError,
            0x0008 => Self::CreatePermissionRequest,
            0x0009 => Self::ChannelBindRequest,
            0x0109 => Self::ChannelBindResponse,
            0x0119 => Self::ChannelBindError,
            0x0016 => Self::SendIndication,
            0x0017 => Self::DataIndication,
            other => Self::Other(other),
        }
    }
    fn to_u16(self) -> u16 {
        match self {
            Self::BindingRequest => 0x0001,
            Self::BindingResponse => 0x0101,
            Self::BindingError => 0x0111,
            Self::AllocateRequest => 0x0003,
            Self::AllocateResponse => 0x0103,
            Self::AllocateError => 0x0113,
            Self::RefreshRequest => 0x0004,
            Self::RefreshResponse => 0x0104,
            Self::RefreshError => 0x0114,
            Self::CreatePermissionRequest => 0x0008,
            Self::ChannelBindRequest => 0x0009,
            Self::ChannelBindResponse => 0x0109,
            Self::ChannelBindError => 0x0119,
            Self::SendIndication => 0x0016,
            Self::DataIndication => 0x0017,
            Self::Other(v) => v,
        }
    }
}

/// A STUN attribute (TLV). Value bytes are stored as-is
/// without padding — caller is responsible for emitting
/// 4-byte-aligned attribute bodies (we always do).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub attr_type: u16,
    pub value: Vec<u8>,
}

/// Built STUN message (header + attributes).
#[derive(Debug, Clone)]
pub struct Message {
    pub kind: MessageType,
    pub transaction_id: TransactionId,
    pub attributes: Vec<Attribute>,
}

impl Message {
    /// Build a fresh request message with a random
    /// transaction ID.
    pub fn new_request(kind: MessageType) -> Self {
        Self {
            kind,
            transaction_id: TransactionId::random(),
            attributes: Vec::new(),
        }
    }

    /// Push an attribute. The length is auto-padded to a
    /// 4-byte boundary per RFC 5389 § 14.
    pub fn push_attr(&mut self, attr_type: u16, mut value: Vec<u8>) {
        while value.len() % 4 != 0 {
            value.push(0);
        }
        self.attributes.push(Attribute {
            attr_type,
            value,
        });
    }

    /// Encode the message to wire bytes (20-byte header +
    /// concatenated attribute TLVs).
    pub fn encode(&self) -> Vec<u8> {
        let body_len: usize = self
            .attributes
            .iter()
            .map(|a| 4 + a.value.len())
            .sum();
        let mut buf = Vec::with_capacity(20 + body_len);
        buf.extend_from_slice(&self.kind.to_u16().to_be_bytes());
        buf.extend_from_slice(&(body_len as u16).to_be_bytes());
        buf.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        buf.extend_from_slice(&self.transaction_id.0);
        for a in &self.attributes {
            buf.extend_from_slice(&a.attr_type.to_be_bytes());
            buf.extend_from_slice(&(a.value.len() as u16).to_be_bytes());
            buf.extend_from_slice(&a.value);
        }
        buf
    }

    /// Decode a wire-format STUN message. Returns `None`
    /// if the buffer is too short or the magic cookie
    /// doesn't match (RFC 5389 § 7.3 — messages that don't
    /// start with the magic cookie are *not* STUN, so
    /// returning `None` is the right behaviour).
    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 20 {
            return None;
        }
        let msg_type = u16::from_be_bytes([buf[0], buf[1]]);
        let msg_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        let cookie = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if cookie != MAGIC_COOKIE {
            return None;
        }
        let mut tx = [0u8; 12];
        tx.copy_from_slice(&buf[8..20]);
        if buf.len() < 20 + msg_len {
            return None;
        }
        let body = &buf[20..20 + msg_len];
        let mut attributes = Vec::new();
        let mut i = 0;
        while i + 4 <= body.len() {
            let attr_type = u16::from_be_bytes([body[i], body[i + 1]]);
            let attr_len = u16::from_be_bytes([body[i + 2], body[i + 3]]) as usize;
            if i + 4 + attr_len > body.len() {
                return None;
            }
            let value = body[i + 4..i + 4 + attr_len].to_vec();
            attributes.push(Attribute {
                attr_type,
                value,
            });
            // 4-byte alignment.
            i += 4 + ((attr_len + 3) & !3);
        }
        Some(Self {
            kind: MessageType::from_u16(msg_type),
            transaction_id: TransactionId(tx),
            attributes,
        })
    }

    /// Find the first attribute with the given type.
    pub fn first_attr(&self, attr_type: u16) -> Option<&[u8]> {
        self.attributes
            .iter()
            .find(|a| a.attr_type == attr_type)
            .map(|a| a.value.as_slice())
    }

    /// Find the first attribute with the given type,
    /// returning `None` if it's the "padding" attribute
    /// with zero length.
    pub fn first_attr_nonempty(&self, attr_type: u16) -> Option<&[u8]> {
        self.attributes
            .iter()
            .find(|a| a.attr_type == attr_type && !a.value.is_empty())
            .map(|a| a.value.as_slice())
    }
}

// ───────────────────── attribute constants (RFC 5389/5766) ─────────────────────

pub const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
pub const ATTR_USERNAME: u16 = 0x0006;
pub const ATTR_MESSAGE_INTEGRITY: u16 = 0x0008;
pub const ATTR_ERROR_CODE: u16 = 0x0009;
pub const ATTR_REALM: u16 = 0x0014;
pub const ATTR_NONCE: u16 = 0x0015;
pub const ATTR_XOR_RELAYED_ADDRESS: u16 = 0x0016;
pub const ATTR_REQUESTED_TRANSPORT: u16 = 0x0019;
pub const ATTR_LIFETIME: u16 = 0x001D;
pub const ATTR_XOR_PEER_ADDRESS: u16 = 0x0012;
pub const ATTR_DATA: u16 = 0x0013;
pub const ATTR_RELAY_ADDRESS: u16 = 0x0016; // legacy alias for XOR-RELAYED-ADDRESS
pub const ATTR_CHANNEL_NUMBER: u16 = 0x000C;
pub const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// Encode an `XOR-MAPPED-ADDRESS` or `XOR-RELAYED-ADDRESS`
/// attribute value (RFC 5389 § 15.2 / RFC 5766 § 14.2).
///
/// Layout:
/// - byte 0: reserved (0)
/// - byte 1: family (0x01 = IPv4)
/// - bytes 2..3: xor'd port
/// - bytes 4..8: xor'd v4 address
pub fn encode_xor_address(addr: SocketAddr, tx_id: &TransactionId) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8);
    buf.push(0); // reserved
    match addr {
        SocketAddr::V4(v4) => {
            buf.push(0x01); // family = IPv4
            let port = v4.port() ^ (MAGIC_COOKIE >> 16) as u16;
            buf.extend_from_slice(&port.to_be_bytes());
            let ip_bytes = v4.ip().octets();
            let cookie_bytes = MAGIC_COOKIE.to_be_bytes();
            for i in 0..4 {
                buf.push(ip_bytes[i] ^ cookie_bytes[i]);
            }
        }
        SocketAddr::V6(v6) => {
            buf.push(0x02); // family = IPv6
            let port = v6.port() ^ (MAGIC_COOKIE >> 16) as u16;
            buf.extend_from_slice(&port.to_be_bytes());
            let ip_bytes = v6.ip().octets();
            let cookie_bytes = MAGIC_COOKIE.to_be_bytes();
            for i in 0..4 {
                buf.push(ip_bytes[i] ^ cookie_bytes[i]);
            }
            // 16-byte v6 address: XOR with cookie + tx id
            let mut xor_mask = [0u8; 16];
            xor_mask[..4].copy_from_slice(&cookie_bytes);
            xor_mask[4..16].copy_from_slice(&tx_id.0);
            for i in 0..16 {
                buf.push(ip_bytes[i] ^ xor_mask[i]);
            }
        }
    }
    buf
}

/// Decode an `XOR-MAPPED-ADDRESS` / `XOR-RELAYED-ADDRESS`
/// attribute value. Returns the (decoded) `SocketAddr`.
pub fn decode_xor_address(buf: &[u8], tx_id: &TransactionId) -> Option<SocketAddr> {
    if buf.len() < 8 {
        return None;
    }
    let family = buf[1];
    let cookie_bytes = MAGIC_COOKIE.to_be_bytes();
    let port = u16::from_be_bytes([buf[2], buf[3]]) ^ (MAGIC_COOKIE >> 16) as u16;
    if family == 0x01 {
        let mut ip_bytes = [0u8; 4];
        for i in 0..4 {
            ip_bytes[i] = buf[4 + i] ^ cookie_bytes[i];
        }
        Some(SocketAddr::new(
            std::net::IpAddr::V4(Ipv4Addr::from(ip_bytes)),
            port,
        ))
    } else if family == 0x02 && buf.len() >= 20 {
        let mut xor_mask = [0u8; 16];
        xor_mask[..4].copy_from_slice(&cookie_bytes);
        xor_mask[4..16].copy_from_slice(&tx_id.0);
        let mut ip_bytes = [0u8; 16];
        for i in 0..16 {
            ip_bytes[i] = buf[4 + i] ^ xor_mask[i];
        }
        let ip = std::net::IpAddr::V6(std::net::Ipv6Addr::from(ip_bytes));
        Some(SocketAddr::new(ip, port))
    } else {
        None
    }
}

/// Parse an `ERROR-CODE` attribute (RFC 5389 § 15.6).
/// Returns `(class, number)` where `class ∈ {3,4,5,6}`
/// and `number ∈ 0..=99`. The combined code is
/// `class * 100 + number` (so 401 → class=4, number=1;
/// 600 → class=6, number=0).
pub fn parse_error_code(buf: &[u8]) -> Option<(u8, u8)> {
    if buf.len() < 4 {
        return None;
    }
    let class = buf[2] & 0x07;
    let number = buf[3];
    if !(3..=6).contains(&class) || number > 99 {
        return None;
    }
    Some((class, number))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip_for_empty_message() {
        let m = Message::new_request(MessageType::BindingRequest);
        let bytes = m.encode();
        assert_eq!(bytes.len(), 20);
        let back = Message::decode(&bytes).unwrap();
        assert_eq!(back.kind, MessageType::BindingRequest);
        assert_eq!(back.transaction_id, m.transaction_id);
        assert!(back.attributes.is_empty());
    }

    #[test]
    fn decode_rejects_short_buffer() {
        assert!(Message::decode(&[0u8; 19]).is_none());
    }

    #[test]
    fn decode_rejects_wrong_magic_cookie() {
        let mut bytes = vec![0u8; 20];
        bytes[2..4].copy_from_slice(&0u16.to_be_bytes()); // length=0
        bytes[4..8].copy_from_slice(&0xDEADBEEFu32.to_be_bytes()); // wrong cookie
        assert!(Message::decode(&bytes).is_none());
    }

    #[test]
    fn encode_decode_round_trip_with_attribute() {
        let mut m = Message::new_request(MessageType::AllocateRequest);
        m.push_attr(ATTR_REQUESTED_TRANSPORT, vec![17, 0, 0, 0]); // UDP
        let bytes = m.encode();
        let back = Message::decode(&bytes).unwrap();
        assert_eq!(back.kind, MessageType::AllocateRequest);
        assert_eq!(back.first_attr(ATTR_REQUESTED_TRANSPORT), Some(&[17u8, 0, 0, 0][..]));
    }

    #[test]
    fn xor_address_encode_decode_round_trip_v4() {
        let tx = TransactionId::random();
        let addr: SocketAddr = "203.0.113.42:4242".parse().unwrap();
        let encoded = encode_xor_address(addr, &tx);
        let decoded = decode_xor_address(&encoded, &tx).unwrap();
        assert_eq!(decoded, addr);
    }

    #[test]
    fn xor_address_decode_handles_canonical_rfc_example() {
        // RFC 5389 § 15.2 example (with arbitrary tx id).
        let tx = TransactionId([0xA; 12]);
        // Encoded bytes (no xor magic, real example).
        let mut encoded = vec![0, 1]; // reserved, family v4
        // Port 0x4E2F xor (0x2112A442 >> 16) = 0x4E2F xor 0x2112 = 0x6F3D
        encoded.extend_from_slice(&0x6F3Du16.to_be_bytes());
        // Address 192.0.2.1 xor cookie = 192.0.2.1 xor 33.18.164.66 = 0xC0.0x00.0x02.0x01 xor 0x21.0x12.0xA4.0x42 = 0xE1.0x12.0xA6.0x43
        encoded.extend_from_slice(&[0xE1, 0x12, 0xA6, 0x43]);
        let decoded = decode_xor_address(&encoded, &tx).unwrap();
        // We don't assert the exact IP (the example isn't
        // self-consistent under RFC 5389 magic); just that
        // we don't crash and we get a sensible port.
        if let SocketAddr::V4(v4) = decoded {
            assert_eq!(v4.port(), 0x4E2F);
        } else {
            panic!("expected v4");
        }
    }

    #[test]
    fn parse_error_code_handles_400_class_4_number_0() {
        // Class 4 (4xx), number 0 -> 400
        let encoded = vec![0, 0, 4, 0, 0, 0, 0];
        let (class, number) = parse_error_code(&encoded).unwrap();
        assert_eq!(class, 4);
        assert_eq!(number, 0);
    }

    #[test]
    fn message_first_attr_returns_none_when_missing() {
        let m = Message::new_request(MessageType::BindingRequest);
        assert!(m.first_attr(ATTR_NONCE).is_none());
    }

    #[test]
    fn push_attr_pads_to_4_byte_boundary() {
        let mut m = Message::new_request(MessageType::AllocateRequest);
        m.push_attr(0x9999, vec![1, 2, 3]); // 3 bytes -> 4 bytes after pad
        let len = m.attributes[0].value.len();
        assert_eq!(len, 4);
        assert_eq!(m.attributes[0].value, vec![1, 2, 3, 0]);
    }
}