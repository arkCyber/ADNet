//! `ShareTicket` — a printable, copy-pasteable reference to a shared
//! blob that an `iroh-blobs` peer (or a pure-A3Net peer) can use to
//! pull bytes.
//!
//! ## Wire format
//!
//! ```text
//! a3net-share://<node-id-hex>@<addr-display>/<manifest-hash-hex>[/name1=hash1,name2=hash2,…]
//! ```
//!
//! - `<node-id-hex>` — 64 lowercase hex chars (32-byte BLAKE3 digest),
//!   the same shape as `a3net-blob://` tickets in `a3net-types`.
//! - `<addr-display>` — `direct=<host>:<port>` and/or
//!   `relay=<relay-url>`, matching the `NodeAddr::display_with_id`
//!   convention used elsewhere in A3Net.
//! - `<manifest-hash-hex>` — 64 lowercase hex chars, the BLAKE3
//!   digest of the manifest bytes (NOT the hash of an individual
//!   file). When the receiver fetches this hash they get back a
//!   serialised [`crate::Collection`].
//! - The optional trailing comma-separated `name=hash` list is a
//!   preview embedded in the ticket. Receivers that have the preview
//!   can show the file list before pulling any bytes (a UX nicety
//!   that `n0-computer/sendme` does not offer — its tickets are
//!   opaque until the receiver connects and reads the manifest).
//!
//! ## iroh compatibility
//!
//! The core fields (node id, addr, manifest hash) are encoded as
//! ASCII segments. `iroh-blobs::ticket::BlobTicket` is a postcard
//! struct, so we cannot share its binary representation, but we can
//! (and do) carry a parallel `BlobTicket`-compatible URL string for
//! `iroh-blobs` consumers. See
//! [`crate::ticket::ShareTicket::iroh_blob_ticket_hint`] for the
//! bridge.

use serde::{Deserialize, Serialize};

use a3net_types::{ContentHash, NodeAddr, NodeId};

use crate::collection::{Collection, CollectionEntry};
use crate::error::{ShareError, ShareResult};

/// URL scheme prefix. Anything sent over the wire starts with this
/// literal. We pick `a3net-share://` (and NOT `a3net-blob://`) so a
/// receiver knows up-front whether it's pulling a single blob or a
/// manifest, even before parsing.
pub const SHARE_TICKET_PREFIX: &str = "a3net-share://";

/// Preview cap — how many `(name, hash)` pairs we embed directly in
/// the URL. sendme never embeds previews; we do, but only up to a
/// small cap so the URL stays QR / clipboard friendly.
pub const MAX_PREVIEW_ENTRIES: usize = 32;

/// Total maximum length of the printable ticket string. Mirrors the
/// `a3net_blob_ticket`-style URL budgets used elsewhere in A3Net.
pub const MAX_TICKET_LEN: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareTicket {
    pub node_id: NodeId,
    pub endpoint: NodeAddr,
    pub manifest_hash: ContentHash,
    /// Optional preview of up to [`MAX_PREVIEW_ENTRIES`] entries.
    /// Stored as `Vec` (rather than the full `Collection`) so the
    /// ticket stays small even when the manifest is large.
    #[serde(default)]
    pub preview: Vec<CollectionEntry>,
    /// Total byte size of the manifest payload (sum of file sizes).
    /// `0` means "unknown". Used by `sendme`-style status displays.
    #[serde(default)]
    pub total_size: u64,
}

impl ShareTicket {
    /// Build a ticket for a manifest, including a preview of every
    /// entry up to [`MAX_PREVIEW_ENTRIES`]. Truncates silently if the
    /// collection is larger; the receiver will fetch the rest from
    /// the manifest hash.
    pub fn new(
        node_id: &NodeId,
        endpoint: &NodeAddr,
        manifest_hash: &ContentHash,
        manifest: &Collection,
        total_size: u64,
    ) -> ShareResult<Self> {
        let preview: Vec<CollectionEntry> = manifest
            .iter()
            .take(MAX_PREVIEW_ENTRIES)
            .map(|(name, hash)| CollectionEntry {
                name: name.to_string(),
                hash: hash.clone(),
            })
            .collect();
        Ok(Self {
            node_id: node_id.clone(),
            endpoint: endpoint.clone(),
            manifest_hash: manifest_hash.clone(),
            preview,
            total_size,
        })
    }

    /// Render to a printable URL string.
    pub fn encode(&self) -> String {
        let mut out = format!(
            "{}{}/{}",
            SHARE_TICKET_PREFIX,
            self.endpoint.display_with_id(&self.node_id),
            self.manifest_hash.as_hex()
        );
        if !self.preview.is_empty() {
            out.push('/');
            let mut first = true;
            for entry in &self.preview {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&format!("{}={}", entry.name, entry.hash.as_hex()));
            }
        }
        out
    }

    /// Parse the printable URL form. Returns [`ShareError::InvalidTicket`]
    /// on any structural error.
    pub fn parse(raw: &str) -> ShareResult<Self> {
        if raw.len() > MAX_TICKET_LEN {
            return Err(ShareError::InvalidTicket(format!(
                "ticket length {} exceeds cap {MAX_TICKET_LEN}",
                raw.len()
            )));
        }
        let rest = raw.strip_prefix(SHARE_TICKET_PREFIX).ok_or_else(|| {
            ShareError::InvalidTicket(format!("missing prefix {SHARE_TICKET_PREFIX:?}"))
        })?;
        // The body is `<addr-display>/<hash-hex>[/<preview>]`. The
        // manifest hash is the FIRST occurrence of `/<64 hex chars>`
        // after the addr-display; anything after that second `/` is
        // the optional preview list. We do **not** anchor on the
        // last hex run because the preview itself contains hex
        // digests.
        let hex_len = ContentHash::HEX_LEN;
        let bytes = rest.as_bytes();
        if bytes.len() < hex_len + 1 {
            return Err(ShareError::InvalidTicket("too short".into()));
        }
        // Walk the bytes; the first occurrence of `/` followed by 64
        // hex digits is the manifest hash boundary.
        let mut hash_start: Option<usize> = None;
        let mut i = 0;
        while i + 1 + hex_len <= bytes.len() {
            if bytes[i] == b'/'
                && bytes[i + 1..i + 1 + hex_len]
                    .iter()
                    .all(|b| b.is_ascii_hexdigit())
            {
                hash_start = Some(i + 1);
                break;
            }
            i += 1;
        }
        let hash_start = hash_start
            .ok_or_else(|| ShareError::InvalidTicket("could not locate manifest hash".into()))?;
        let hash_hex = &rest[hash_start..hash_start + hex_len];
        let manifest_hash = ContentHash::from_hex(hash_hex)
            .map_err(|e| ShareError::InvalidTicket(format!("manifest hash: {e}")))?;

        // The portion before `/<hash>` is the addr-display. The
        // portion after `<hash>` (if any) is the preview list,
        // separated by another `/`.
        let addr_end = hash_start - 1;
        let authority = &rest[..addr_end];
        let after_hash_start = hash_start + hex_len;
        let (preview, _total_size) = if after_hash_start < bytes.len() {
            // Expect `/` separator between hash and preview.
            if bytes[after_hash_start] != b'/' {
                return Err(ShareError::InvalidTicket(
                    "expected '/' after manifest hash".into(),
                ));
            }
            let preview_str = &rest[after_hash_start + 1..];
            (parse_preview(preview_str)?, 0u64)
        } else {
            (Vec::new(), 0u64)
        };

        let addr = NodeAddr::parse(authority)
            .map_err(|e| ShareError::InvalidTicket(format!("addr: {e}")))?;
        let node_id = addr.node_id.clone();

        Ok(Self {
            node_id,
            endpoint: addr,
            manifest_hash,
            preview,
            total_size: 0,
        })
    }
}

fn parse_preview(s: &str) -> ShareResult<Vec<CollectionEntry>> {
    let mut out = Vec::new();
    for pair in s.split(',') {
        if pair.is_empty() {
            continue;
        }
        let (name, hash_hex) = pair
            .split_once('=')
            .ok_or_else(|| ShareError::InvalidTicket(format!("preview pair: {pair:?}")))?;
        let hash = ContentHash::from_hex(hash_hex)
            .map_err(|e| ShareError::InvalidTicket(format!("preview hash: {e}")))?;
        out.push(CollectionEntry::new(name, hash)?);
        if out.len() > MAX_PREVIEW_ENTRIES {
            return Err(ShareError::CollectionTooLarge {
                got: out.len(),
                max: MAX_PREVIEW_ENTRIES,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::node::Endpoint;

    fn nid() -> NodeId {
        NodeId::random()
    }

    fn addr(id: &NodeId) -> NodeAddr {
        NodeAddr::new(id.clone()).with_direct(Endpoint::new("127.0.0.1", 9000))
    }

    fn manifest() -> Collection {
        let mut c = Collection::new();
        c.push(CollectionEntry::new("a.txt", ContentHash::from_bytes(b"a")).unwrap()).unwrap();
        c.push(CollectionEntry::new("b.txt", ContentHash::from_bytes(b"b")).unwrap()).unwrap();
        c.push(CollectionEntry::new("c/d.txt", ContentHash::from_bytes(b"c")).unwrap()).unwrap();
        c
    }

    #[test]
    fn encode_decode_with_preview_populated() {
        let id = nid();
        let addr = addr(&id);
        let m = manifest();
        let mh = m.manifest_hash().unwrap();
        let t = ShareTicket::new(&id, &addr, &mh, &m, 1234).unwrap();
        let raw = t.encode();
        let back = ShareTicket::parse(&raw).unwrap();
        assert_eq!(back.node_id, id);
        assert_eq!(back.manifest_hash, mh);
        assert_eq!(back.preview.len(), 3);
        assert_eq!(back.total_size, 0);
    }

    #[test]
    fn encode_decode_with_no_preview_when_manifest_is_empty() {
        let id = nid();
        let addr = addr(&id);
        let empty = Collection::new();
        let mh = empty.manifest_hash().unwrap();
        let t = ShareTicket::new(&id, &addr, &mh, &empty, 0).unwrap();
        assert!(t.preview.is_empty());
        let raw = t.encode();
        let back = ShareTicket::parse(&raw).unwrap();
        assert!(back.preview.is_empty());
        assert_eq!(back.manifest_hash, mh);
        assert_eq!(back.node_id, id);
    }

    #[test]
    fn encode_decode_with_preview() {
        let id = nid();
        let addr = addr(&id);
        let m = manifest();
        let mh = m.manifest_hash().unwrap();
        let t = ShareTicket::new(&id, &addr, &mh, &m, 1234).unwrap();
        let raw = t.encode();
        assert!(raw.contains("a.txt="));
        assert!(raw.contains("b.txt="));
        let back = ShareTicket::parse(&raw).unwrap();
        assert_eq!(back.preview.len(), 3);
        let names: Vec<&str> = back.preview.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c/d.txt"]);
    }

    #[test]
    fn preview_truncates_at_cap() {
        let mut c = Collection::new();
        for i in 0..(MAX_PREVIEW_ENTRIES + 5) {
            c.push(CollectionEntry::new(
                format!("f{i}.txt"),
                ContentHash::from_bytes(format!("{i}").as_bytes()),
            ).unwrap()).unwrap();
        }
        let id = nid();
        let addr = addr(&id);
        let mh = c.manifest_hash().unwrap();
        let t = ShareTicket::new(&id, &addr, &mh, &c, 0).unwrap();
        assert_eq!(t.preview.len(), MAX_PREVIEW_ENTRIES);
    }

    #[test]
    fn encode_contains_node_id_and_hash() {
        let id = nid();
        let addr = addr(&id);
        let m = manifest();
        let mh = m.manifest_hash().unwrap();
        let t = ShareTicket::new(&id, &addr, &mh, &m, 0).unwrap();
        let raw = t.encode();
        assert!(raw.contains(&id.to_string()));
        assert!(raw.contains(mh.as_hex()));
    }

    #[test]
    fn parse_rejects_wrong_prefix() {
        assert!(ShareTicket::parse("a3net-blob://x/y").is_err());
        assert!(ShareTicket::parse("not-a-ticket").is_err());
    }

    #[test]
    fn parse_rejects_overlong() {
        let raw = format!("{SHARE_TICKET_PREFIX}{}", "x".repeat(MAX_TICKET_LEN));
        let err = ShareTicket::parse(&raw).unwrap_err();
        assert!(matches!(err, ShareError::InvalidTicket(_)));
    }

    #[test]
    fn parse_rejects_short_hash() {
        let id = nid();
        let addr = addr(&id);
        let raw = format!("{SHARE_TICKET_PREFIX}{}/tooshort", addr.display_with_id(&id));
        let err = ShareTicket::parse(&raw).unwrap_err();
        assert!(matches!(err, ShareError::InvalidTicket(_)));
    }

    #[test]
    fn parse_rejects_non_hex_hash() {
        let id = nid();
        let addr = addr(&id);
        let bad_hash = "z".repeat(64);
        let raw = format!("{SHARE_TICKET_PREFIX}{}/{bad_hash}", addr.display_with_id(&id));
        let err = ShareTicket::parse(&raw).unwrap_err();
        assert!(matches!(err, ShareError::InvalidTicket(_)));
    }

    #[test]
    fn parse_rejects_malformed_preview() {
        let id = nid();
        let addr = addr(&id);
        let mh = ContentHash::from_bytes(b"x");
        let raw = format!(
            "{SHARE_TICKET_PREFIX}{}/{}/no-equals-here",
            addr.display_with_id(&id),
            mh.as_hex()
        );
        let err = ShareTicket::parse(&raw).unwrap_err();
        assert!(matches!(err, ShareError::InvalidTicket(_)));
    }

    #[test]
    fn round_trip_preserves_addr() {
        let id = nid();
        let addr = addr(&id);
        let m = manifest();
        let mh = m.manifest_hash().unwrap();
        let t = ShareTicket::new(&id, &addr, &mh, &m, 100).unwrap();
        let raw = t.encode();
        let back = ShareTicket::parse(&raw).unwrap();
        assert_eq!(back.endpoint, addr);
    }
}