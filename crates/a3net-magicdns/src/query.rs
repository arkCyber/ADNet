//! Mesh DNS name parsing.
//!
//! The mesh DNS name space is:
//!
//! ```text
//! <hostname>.<network>.ray     (full)
//! <hostname>.<network>         (short, no TLD)
//! <hostname>.ray               (flat, walks all networks)
//! ```
//!
//! Labels are lowercased ASCII. Hyphens are allowed mid-label
//! but not at the start or end. Each label is at most 63
//! bytes; the full name at most [`MAX_NAME_LEN`] bytes.

use serde::{Deserialize, Serialize};

use crate::error::{MagicError, MagicResult};

/// Maximum total length of a mesh DNS name.
pub const MAX_NAME_LEN: usize = 253;

/// Maximum length of a single label (RFC 1035 § 2.3.4).
pub const MAX_LABEL_LEN: usize = 63;

/// Canonical TLD suffix that marks a mesh network name.
pub const TLD_SUFFIX: &str = "ray";

/// Parsed mesh DNS name.
///
/// `network` is `Some(...)` for full / short forms, `None`
/// for the flat `.ray` form. The hostname is always
/// lower-cased.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MagicName {
    pub hostname: String,
    pub network: Option<String>,
    /// Whether the original input included the `.ray` TLD.
    pub has_tld: bool,
}

impl MagicName {
    /// Parse a name string. Accepts both `alice.gaming.ray`
    /// and `alice.gaming` and `alice.ray`. The TLD presence
    /// is preserved in `has_tld` for diagnostics.
    pub fn parse(raw: &str) -> MagicResult<Self> {
        if raw.is_empty() {
            return Err(MagicError::Empty);
        }
        if raw.len() > MAX_NAME_LEN {
            return Err(MagicError::NameTooLong { actual: raw.len() });
        }
        let parts: Vec<&str> = raw.split('.').collect();
        // Reject trailing dot.
        if parts.last().is_some_and(|p| p.is_empty()) {
            return Err(MagicError::MalformedName("trailing dot".into()));
        }
        if parts.iter().any(|p| p.is_empty()) {
            return Err(MagicError::MalformedName("empty label".into()));
        }
        // We only accept ASCII labels.
        for label in &parts {
            if !label.is_ascii() {
                return Err(MagicError::MalformedName(format!(
                    "non-ASCII label {label:?}"
                )));
            }
            // RFC 1035 § 2.3.4: each label is at most 63
            // bytes. The total-length check above covers
            // the full name (≤ 253) but not individual
            // labels, so reject here.
            if label.len() > MAX_LABEL_LEN {
                return Err(MagicError::MalformedName(format!(
                    "label {label:?} exceeds {} bytes",
                    MAX_LABEL_LEN
                )));
            }
            // Hostnames cannot start or end with a hyphen.
            if label.starts_with('-') || label.ends_with('-') {
                return Err(MagicError::MalformedName(format!(
                    "label {label:?} has leading/trailing hyphen"
                )));
            }
            // Hostnames cannot contain anything outside
            // [a-z0-9-] once lowercased.
            let lc = label.to_ascii_lowercase();
            if !lc.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Err(MagicError::MalformedName(format!(
                    "label {label:?} has invalid characters"
                )));
            }
        }

        let has_tld = parts.last().copied() == Some(TLD_SUFFIX);
        let (hostname, network) = match (parts.len(), has_tld) {
            // `alice.ray`
            (2, true) => (parts[0].to_ascii_lowercase(), None),
            // `alice.gaming`
            (2, false) => (parts[0].to_ascii_lowercase(), Some(parts[1].to_ascii_lowercase())),
            // `alice.gaming.ray`
            (3, true) => (
                parts[0].to_ascii_lowercase(),
                Some(parts[1].to_ascii_lowercase()),
            ),
            _ => {
                return Err(MagicError::MalformedName(format!(
                    "expected <host>.<net>[.ray] or <host>.ray, got {raw:?}"
                )));
            }
        };
        Ok(Self {
            hostname,
            network,
            has_tld,
        })
    }

    /// Render the canonical full form (`<host>.<net>.ray`).
    /// If the input had no `network` (flat `.ray`), only
    /// the hostname is rendered.
    pub fn full_name(&self) -> String {
        match &self.network {
            Some(n) => format!("{}.{}.{}", self.hostname, n, TLD_SUFFIX),
            None => format!("{}.{}", self.hostname, TLD_SUFFIX),
        }
    }
}

/// A single resolution request.
///
/// `name` is the parsed form; `network_hint` is the
/// network to search first when the name has no `network`
/// (the flat form). The resolver walks the hint network,
/// then any other networks the local node belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagicQuery {
    pub name: MagicName,
    pub network_hint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_form() {
        let n = MagicName::parse("alice.gaming.ray").unwrap();
        assert_eq!(n.hostname, "alice");
        assert_eq!(n.network.as_deref(), Some("gaming"));
        assert!(n.has_tld);
    }

    #[test]
    fn parse_short_form() {
        let n = MagicName::parse("alice.gaming").unwrap();
        assert_eq!(n.hostname, "alice");
        assert_eq!(n.network.as_deref(), Some("gaming"));
        assert!(!n.has_tld);
    }

    #[test]
    fn parse_flat_form() {
        let n = MagicName::parse("alice.ray").unwrap();
        assert_eq!(n.hostname, "alice");
        assert!(n.network.is_none());
        assert!(n.has_tld);
    }

    #[test]
    fn parse_lowercases() {
        let n = MagicName::parse("Alice.GAMING.ray").unwrap();
        assert_eq!(n.hostname, "alice");
        assert_eq!(n.network.as_deref(), Some("gaming"));
    }

    #[test]
    fn parse_rejects_empty() {
        assert!(MagicName::parse("").is_err());
    }

    #[test]
    fn parse_rejects_trailing_dot() {
        assert!(MagicName::parse("alice.").is_err());
        assert!(MagicName::parse("alice.gaming.ray.").is_err());
    }

    #[test]
    fn parse_rejects_internal_empty_label() {
        assert!(MagicName::parse("alice..gaming").is_err());
    }

    #[test]
    fn parse_rejects_non_ascii() {
        assert!(MagicName::parse("alicé.gaming.ray").is_err());
    }

    #[test]
    fn parse_rejects_invalid_chars() {
        assert!(MagicName::parse("ali ce.gaming.ray").is_err());
        assert!(MagicName::parse("alice.gam_ing.ray").is_err());
    }

    #[test]
    fn parse_rejects_hyphen_edges() {
        assert!(MagicName::parse("-alice.gaming.ray").is_err());
        assert!(MagicName::parse("alice-.gaming.ray").is_err());
    }

    #[test]
    fn parse_rejects_too_long() {
        let big = format!("{}.ray", "a".repeat(MAX_NAME_LEN));
        assert!(MagicName::parse(&big).is_err());
    }

    #[test]
    fn parse_rejects_too_many_labels() {
        assert!(MagicName::parse("a.b.c.d.ray").is_err());
    }

    #[test]
    fn parse_accepts_two_label_short_form() {
        let n = MagicName::parse("alice.gaming").unwrap();
        assert_eq!(n.full_name(), "alice.gaming.ray");
    }

    #[test]
    fn parse_accepts_hyphen_in_middle() {
        let n = MagicName::parse("alice-2.gaming.ray").unwrap();
        assert_eq!(n.hostname, "alice-2");
    }

    /// RFC 1035 § 2.3.4: each label must be ≤ 63 bytes.
    #[test]
    fn parse_rejects_label_too_long() {
        let long_host = "a".repeat(MAX_LABEL_LEN + 1);
        let bad = format!("{long_host}.gaming.ray");
        let err = MagicName::parse(&bad).unwrap_err();
        assert!(err.to_string().contains("exceeds"));
    }

    #[test]
    fn parse_accepts_label_at_boundary() {
        // A 63-byte label is the maximum allowed.
        let long_host = "a".repeat(MAX_LABEL_LEN);
        let ok = format!("{long_host}.ray");
        // Total length is fine (63 + 1 + 3 = 67), label is
        // at the boundary.
        let n = MagicName::parse(&ok).unwrap();
        assert_eq!(n.hostname.len(), MAX_LABEL_LEN);
    }

    #[test]
    fn full_name_roundtrip() {
        let n = MagicName::parse("alice.gaming.ray").unwrap();
        assert_eq!(n.full_name(), "alice.gaming.ray");
        let back = MagicName::parse(&n.full_name()).unwrap();
        assert_eq!(back, n);
    }
}
