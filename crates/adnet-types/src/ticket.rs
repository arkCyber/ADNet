//! Tickets — iroh-blobs `BlobTicket`-compatible peer addressing.
//!
//! Three flavors live here:
//! - [`PeerTicket`][]:  `adnet-peer://<node_id>@<host>:<port>`
//! - [`NodeAddrTicket`][]: iroh-style `NodeAddr` printable form
//! - [`BlobTicket`][]:  `adnet-blob://<node_id>@<host>:<port>/<hash>[/range]`
//!
//! All three are valid hex-decoded via `parse` / `encode` helpers so they
//! can be embedded in gossip payloads, QR codes, or deep links.

use serde::{Deserialize, Serialize};

use crate::content::ContentHash;
use crate::error::{AdnetError, Result};
use crate::node::{NodeAddr, NodeId};
use crate::range::RangeSpec;
use crate::wallet_address::WalletAddress;

/// Peer reachability ticket — used for "node is online at <host:port>" gossip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerTicket {
    pub node_id: NodeId,
    pub endpoint: NodeAddr,
}

impl PeerTicket {
    pub fn encode(node_id: &NodeId, endpoint: &NodeAddr) -> String {
        format!("adnet-peer://{}", endpoint.display_with_id(node_id))
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let rest = raw
            .strip_prefix("adnet-peer://")
            .ok_or_else(|| AdnetError::InvalidTicket(raw.to_string()))?;
        let addr = NodeAddr::parse(rest)?;
        Ok(Self {
            node_id: addr.node_id.clone(),
            endpoint: addr,
        })
    }
}

/// NodeAddr as a self-contained ticket — useful for handing out "dial me"
/// info without committing to a specific transport (iroh can take either
/// direct or relay).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAddrTicket(pub NodeAddr);

impl NodeAddrTicket {
    pub fn encode(addr: &NodeAddr) -> String {
        format!("adnet-addr://{}", addr.display())
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let rest = raw
            .strip_prefix("adnet-addr://")
            .ok_or_else(|| AdnetError::InvalidTicket(raw.to_string()))?;
        Ok(Self(NodeAddr::parse(rest)?))
    }
}

/// Blob ticket — used for "node has blob `<hash>` at `<host:port>`, optionally
/// restricted to a byte range". Mirrors iroh `BlobTicket`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobTicket {
    pub node_id: NodeId,
    pub endpoint: NodeAddr,
    pub content_hash: ContentHash,
    /// Optional sub-range. `RangeSpec::All` (the default) means "fetch the
    /// whole blob".
    #[serde(default)]
    pub range: RangeSpec,
}

impl BlobTicket {
    /// Build a "whole blob" ticket.
    pub fn whole(node_id: &NodeId, endpoint: &NodeAddr, hash: &ContentHash) -> Self {
        Self {
            node_id: node_id.clone(),
            endpoint: endpoint.clone(),
            content_hash: hash.clone(),
            range: RangeSpec::All,
        }
    }

    /// Build a ticket that requests only a sub-range.
    pub fn with_range(mut self, range: RangeSpec) -> Self {
        self.range = range;
        self
    }

    pub fn encode(&self) -> String {
        let range_part = match &self.range {
            RangeSpec::All => String::new(),
            RangeSpec::Single(r) => format!("/{}..{}", r.start, r.end),
            RangeSpec::Multi(rs) => {
                let parts: Vec<String> = rs
                    .iter()
                    .map(|r| format!("{}..{}", r.start, r.end))
                    .collect();
                format!("/{{{}}}", parts.join(","))
            }
        };
        format!(
            "adnet-blob://{}/{}{}",
            self.endpoint.display_with_id(&self.node_id),
            self.content_hash.as_hex(),
            range_part
        )
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let rest = raw
            .strip_prefix("adnet-blob://")
            .ok_or_else(|| AdnetError::InvalidTicket(raw.to_string()))?;
        // The blob ticket is `adnet-blob://<addr-display>/<hash>[/<range>]`.
        // The `<addr-display>` is itself whitespace-separated (`<id>
        // direct=<host:port> relay=<url>`), but the relay URL may contain
        // `/` characters and the final `/<hash>` lives in its own segment.
        // We therefore:
        //   1. split off the trailing `/<hash>[/<range>]` using the LAST
        //      `/<64 hex chars>` we find in the string,
        //   2. hand the remainder to `NodeAddr::parse`.
        let (slash_idx, hash_hex, range_part) = split_hash_and_range(rest)?;
        let authority = rest[..slash_idx].trim();
        let addr = NodeAddr::parse(authority)?;
        let content_hash = ContentHash::from_hex(hash_hex)?;
        let range = match range_part {
            None => RangeSpec::All,
            Some(s) => parse_range_part(s)?,
        };
        Ok(Self {
            node_id: addr.node_id.clone(),
            endpoint: addr,
            content_hash,
            range,
        })
    }

    /// `http://host:port` base URL for HTTP-mesh fallback fetching.
    pub fn http_base(&self) -> Option<String> {
        self.endpoint
            .direct
            .as_ref()
            .map(|d| format!("http://{}", d.as_str()))
    }
}

/// Locate the trailing `/<hash>[/<range>]` of a blob ticket.
///
/// A blob ticket looks like `adnet-blob://<addr-display>/<hash>[/<range>]`.
/// We find the position of the **last** occurrence of `/<64 hex chars>` in
/// `rest`; anything after the hash is the (optional) `/<range>` suffix.
fn split_hash_and_range(rest: &str) -> Result<(usize, &str, Option<&str>)> {
    let bytes = rest.as_bytes();
    let hex_len = ContentHash::HEX_LEN;
    if bytes.len() < 1 + hex_len {
        return Err(AdnetError::InvalidTicket(rest.to_string()));
    }
    // The hash sits at the very end OR just before a `/<range>` suffix.
    // Identify the range suffix first: it starts with `/` followed by
    // either `}` (multi) or a digit (single).
    let cursor = bytes.len();
    // Try to peel a range suffix. The range is the substring after the LAST
    // `/`, ending at the end of the string. For it to be valid there must
    // be 64 hex chars immediately before that `/`.
    let last_is_digit_or_brace = matches!(bytes.last(), Some(b'}' | b'0'..=b'9'));
    if last_is_digit_or_brace {
        if let Some(range_slash_pos) = bytes[..cursor].iter().rposition(|&b| b == b'/') {
            if let Some(hash_start) = range_slash_pos.checked_sub(hex_len) {
                if hash_start > 0
                    && bytes[hash_start..range_slash_pos]
                        .iter()
                        .all(|b| b.is_ascii_hexdigit())
                    && bytes[hash_start - 1] == b'/'
                {
                    let hash_hex = &rest[hash_start..range_slash_pos];
                    let slash_idx = hash_start - 1;
                    let range_part = &rest[range_slash_pos + 1..];
                    return Ok((slash_idx, hash_hex, Some(range_part)));
                }
            }
        }
    }
    // No range suffix — the body must end with `/<64 hex chars>`.
    let total = bytes.len();
    if total < 1 + hex_len {
        return Err(AdnetError::InvalidTicket(rest.to_string()));
    }
    let hash_start = total - hex_len;
    if bytes[hash_start - 1] != b'/' {
        return Err(AdnetError::InvalidContentHash(rest.to_string()));
    }
    if !bytes[hash_start..total]
        .iter()
        .all(|b| b.is_ascii_hexdigit())
    {
        return Err(AdnetError::InvalidContentHash(rest.to_string()));
    }
    let slash_idx = hash_start - 1;
    let hash_hex = &rest[hash_start..total];
    Ok((slash_idx, hash_hex, None))
}

fn parse_range_part(s: &str) -> Result<RangeSpec> {
    if let Some(inner) = s.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
        let mut ranges = Vec::new();
        for part in inner.split(',') {
            let (a, b) = part
                .split_once("..")
                .ok_or_else(|| AdnetError::InvalidTicket(s.to_string()))?;
            let start: u64 = a
                .parse()
                .map_err(|_| AdnetError::InvalidTicket(s.to_string()))?;
            let end: u64 = b
                .parse()
                .map_err(|_| AdnetError::InvalidTicket(s.to_string()))?;
            ranges.push(crate::range::ByteRange::new(start, end)?);
        }
        return Ok(RangeSpec::Multi(ranges));
    }
    let (a, b) = s
        .split_once("..")
        .ok_or_else(|| AdnetError::InvalidTicket(s.to_string()))?;
    let start: u64 = a
        .parse()
        .map_err(|_| AdnetError::InvalidTicket(s.to_string()))?;
    let end: u64 = b
        .parse()
        .map_err(|_| AdnetError::InvalidTicket(s.to_string()))?;
    RangeSpec::single(start, end)
}

/// Validate a [`BlobTicket`] for use at the IPC boundary. Performs a
/// round-trip encode/decode to catch corruption and a sanity check on
/// any range the ticket carries.
pub fn validate_blob_ticket(t: &BlobTicket) -> Result<()> {
    // Round-trip: tickets that don't survive a re-parse are unusable.
    let raw = t.encode();
    let _ = BlobTicket::parse(&raw)?;
    // Range sanity: any byte range must have `start <= end` and be
    // bounded by the source size if the caller provided one (we don't
    // always have size here, so we only check the start/end ordering).
    match &t.range {
        RangeSpec::All => {}
        RangeSpec::Single(r) => {
            if r.start > r.end {
                return Err(AdnetError::Validation(format!(
                    "blob_ticket range: start {} > end {}",
                    r.start, r.end
                )));
            }
        }
        RangeSpec::Multi(rs) => {
            for r in rs {
                if r.start > r.end {
                    return Err(AdnetError::Validation(format!(
                        "blob_ticket range: start {} > end {}",
                        r.start, r.end
                    )));
                }
            }
        }
    }
    Ok(())
}

impl NodeAddr {
    /// Render with a specific node id (used when encoding tickets where the
    /// id might be supplied separately from a borrowed `NodeAddr`).
    pub fn display_with_id(&self, id: &NodeId) -> String {
        let mut s = id.to_string();
        if let Some(d) = &self.direct {
            s.push_str(&format!(" direct={d}"));
        }
        if let Some(r) = &self.relay {
            s.push_str(&format!(" relay={r}"));
        }
        s
    }
}

/// A [`PeerTicket`] with an attached signature and the wallet address that
/// produced it.
///
/// The signature is *opaque* from `adnet-types`' perspective — 64 raw bytes
/// that any verifier (typically the [`crate::wallet_address::WalletAddress`]
/// paired with an EIP-191 / Ed25519 / BLS implementation in a higher crate)
/// can interpret. We deliberately do not constrain the signature scheme
/// here so the protocol layer stays free of crypto dependencies.
///
/// Wire format: `adnet-signed-peer://<base64-url-payload>` where the
/// payload is `bincode(ticket, signer, signature)`.
///
/// [`crate::wallet_address::WalletAddress`]: crate::wallet_address::WalletAddress
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedPeerTicket {
    pub ticket: PeerTicket,
    pub signer: WalletAddress,
    /// Signature bytes. We store this as `Vec<u8>` (rather than `[u8; 64]`)
    /// so future schemes (compact BLS / PQ signatures / Schnorr over
    /// Ristretto) can flow through without touching the protocol layer.
    /// The first byte is the scheme tag (0 = 64-byte EIP-191 over secp256k1,
    /// 1 = 64-byte Ed25519, 2 = 96-byte BLS12-381 G2). Length constraints
    /// are enforced at the verifier in `adnet-identity`.
    #[serde(with = "signature_bytes")]
    pub signature: Vec<u8>,
}

impl SignedPeerTicket {
    pub fn new(ticket: PeerTicket, signer: WalletAddress, signature: Vec<u8>) -> Self {
        Self {
            ticket,
            signer,
            signature,
        }
    }

    /// Encode as a printable URL. Uses URL-safe base64 (no padding) so the
    /// result is QR / deep-link friendly.
    pub fn encode(&self) -> String {
        let bytes = bincode::serialize(&(&self.ticket, &self.signer, &self.signature))
            .expect("SignedPeerTicket is always bincode-serializable");
        format!("adnet-signed-peer://{}", base64_url_encode(&bytes))
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let rest = raw
            .strip_prefix("adnet-signed-peer://")
            .ok_or_else(|| AdnetError::InvalidTicket(raw.to_string()))?;
        let bytes = base64_url_decode(rest)
            .map_err(|e| AdnetError::InvalidTicket(format!("base64: {e}")))?;
        let (ticket, signer, signature): (PeerTicket, WalletAddress, Vec<u8>) =
            bincode::deserialize(&bytes)
                .map_err(|e| AdnetError::InvalidTicket(format!("bincode: {e}")))?;
        Ok(Self {
            ticket,
            signer,
            signature,
        })
    }
}

mod signature_bytes {
    //! Serde adapter that turns `Vec<u8>` into a serde sequence. Default
    //! `Vec<u8>` serializes as `[u8; N]` in some formats; this module makes
    //! the encoding explicit and stable across formats.
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        bytes.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        Vec::<u8>::deserialize(d)
    }
}

// -- base64 url helpers (no extra dep — small & self-contained) ---------

const B64_URL_CHARS: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64_url_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity((input.len() * 4).div_ceil(3));
    let mut i = 0;
    while i + 3 <= input.len() {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(B64_URL_CHARS[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_URL_CHARS[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64_URL_CHARS[((n >> 6) & 0x3f) as usize] as char);
        out.push(B64_URL_CHARS[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(B64_URL_CHARS[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_URL_CHARS[((n >> 12) & 0x3f) as usize] as char);
    } else if rem == 2 {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(B64_URL_CHARS[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_URL_CHARS[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64_URL_CHARS[((n >> 6) & 0x3f) as usize] as char);
    }
    out
}

fn base64_url_decode(input: &str) -> std::result::Result<Vec<u8>, String> {
    let mut lookup = [0xffu8; 256];
    for (i, &c) in B64_URL_CHARS.iter().enumerate() {
        lookup[c as usize] = i as u8;
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        let v = lookup[b as usize];
        if v == 0xff {
            return Err(format!("invalid base64-url char: 0x{b:02x}"));
        }
        buf = (buf << 6) | (v as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Endpoint, RelayUrl};
    use crate::range::ByteRange;

    fn nid() -> NodeId {
        NodeId::random()
    }

    #[test]
    fn peer_ticket_roundtrip() {
        let id = nid();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("192.168.1.10", 7878));
        let raw = PeerTicket::encode(&id, &addr);
        let parsed = PeerTicket::parse(&raw).unwrap();
        assert_eq!(parsed.node_id, id);
        assert_eq!(parsed.endpoint, addr);
    }

    #[test]
    fn blob_ticket_roundtrip_whole() {
        let id = nid();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("127.0.0.1", 9000));
        let h = ContentHash::from_bytes(b"hello");
        let t = BlobTicket::whole(&id, &addr, &h);
        let raw = t.encode();
        let parsed = BlobTicket::parse(&raw).unwrap();
        assert_eq!(parsed.node_id, id);
        assert_eq!(parsed.content_hash, h);
        assert_eq!(parsed.range, RangeSpec::All);
        assert_eq!(parsed.http_base(), Some("http://127.0.0.1:9000".into()));
    }

    #[test]
    fn blob_ticket_roundtrip_range() {
        let id = nid();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("127.0.0.1", 9000));
        let h = ContentHash::from_bytes(b"world");
        let t = BlobTicket::whole(&id, &addr, &h)
            .with_range(RangeSpec::Single(ByteRange::new(10, 20).unwrap()));
        let raw = t.encode();
        let parsed = BlobTicket::parse(&raw).unwrap();
        assert_eq!(
            parsed.range,
            RangeSpec::Single(ByteRange::new(10, 20).unwrap())
        );
    }

    #[test]
    fn blob_ticket_roundtrip_multi() {
        let id = nid();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("127.0.0.1", 9000));
        let h = ContentHash::from_bytes(b"multi");
        let t = BlobTicket::whole(&id, &addr, &h).with_range(RangeSpec::Multi(vec![
            ByteRange::new(0, 5).unwrap(),
            ByteRange::new(100, 200).unwrap(),
        ]));
        let raw = t.encode();
        let parsed = BlobTicket::parse(&raw).unwrap();
        match parsed.range {
            RangeSpec::Multi(rs) => {
                assert_eq!(rs.len(), 2);
                assert_eq!(rs[0], ByteRange::new(0, 5).unwrap());
                assert_eq!(rs[1], ByteRange::new(100, 200).unwrap());
            }
            _ => panic!("expected multi"),
        }
    }

    #[test]
    fn node_addr_ticket_roundtrip() {
        let id = nid();
        let addr = NodeAddr::new(id.clone())
            .with_direct(Endpoint::new("10.0.0.1", 5000))
            .with_relay(RelayUrl::new("https://relay.example.com"));
        let raw = NodeAddrTicket::encode(&addr);
        let parsed = NodeAddrTicket::parse(&raw).unwrap();
        assert_eq!(parsed.0, addr);
    }

    #[test]
    fn invalid_ticket_rejected() {
        assert!(PeerTicket::parse("not-a-ticket").is_err());
        assert!(BlobTicket::parse("adnet-blob://nope@1.2.3.4:80/short").is_err());
    }

    #[test]
    fn validate_blob_ticket_accepts_whole() {
        let id = nid();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("127.0.0.1", 9000));
        let h = ContentHash::from_bytes(b"zip");
        let t = BlobTicket::whole(&id, &addr, &h);
        assert!(validate_blob_ticket(&t).is_ok());
    }

    #[test]
    fn signed_peer_ticket_round_trip() {
        let id = nid();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("10.0.0.1", 5000));
        let ticket = PeerTicket {
            node_id: id.clone(),
            endpoint: addr,
        };
        let signer = WalletAddress::from_bytes([0xab; 20]);
        // First byte = scheme tag (0 = EIP-191 / secp256k1), then 64 bytes.
        let mut sig = vec![0u8];
        sig.extend_from_slice(&[0x42u8; 64]);
        let spt = SignedPeerTicket::new(ticket, signer, sig.clone());
        let encoded = spt.encode();
        let parsed = SignedPeerTicket::parse(&encoded).unwrap();
        assert_eq!(parsed.ticket.node_id, id);
        assert_eq!(parsed.signer, signer);
        assert_eq!(parsed.signature, sig);
    }

    #[test]
    fn signed_peer_ticket_rejects_bad_prefix() {
        let raw = "adnet-peer://xxx";
        assert!(SignedPeerTicket::parse(raw).is_err());
    }

    #[test]
    fn wallet_address_serde_round_trip() {
        let a = WalletAddress::from_bytes([0x11; 20]);
        let json = serde_json::to_string(&a).unwrap();
        let b: WalletAddress = serde_json::from_str(&json).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn validate_blob_ticket_rejects_inverted_range() {
        let id = nid();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("127.0.0.1", 9000));
        let h = ContentHash::from_bytes(b"x");
        // Bypass the `ByteRange::new` constructor's own check.
        let bad_range = ByteRange {
            start: 100,
            end: 50,
        };
        let t = BlobTicket::whole(&id, &addr, &h).with_range(RangeSpec::Single(bad_range));
        let err = validate_blob_ticket(&t).unwrap_err();
        assert!(err.to_string().contains("start"), "got {err}");
    }
}
