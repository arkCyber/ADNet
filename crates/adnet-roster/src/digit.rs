//! 12-digit Exodus ID — stable, content-derived identifier for a P2P node.
//!
//! This is a direct port of the helper from
//! `Exodus@src-backup/src-tauri/src/microservice/contact_directory_service.rs`
//! (lines 2981–2997). It is exposed here so other crates (notably
//! `adnet-userstore`) can use the same derivation without taking a
//! dependency on the full roster.

use crate::error::{RosterError, RosterResult};

/// Minimum valid 12-digit id length. Exposed for callers that need to
/// bound-check user input.
pub const MIN_DIGIT_LEN: usize = 12;

/// Maximum valid 12-digit id length. Padded ids are always exactly 12
/// digits, but we accept longer inputs (e.g. with country/region prefix)
/// for forward compatibility.
pub const MAX_DIGIT_LEN: usize = 32;

/// Compute the deterministic 12-digit Exodus id for `node_id`. The
/// derivation is intentionally **not** cryptographic — it is a stable
/// fold of `blake3(node_id)` so the same node id always maps to the same
/// 12-digit string across processes, devices, and restarts.
pub fn stable_digit_from_node(node_id: &str) -> String {
    let hash = blake3::hash(node_id.as_bytes());
    let mut n: u64 = 0;
    for b in hash.as_bytes().iter().take(8) {
        n = n.wrapping_mul(31).wrapping_add(*b as u64);
    }
    format!("{:012}", n % 1_000_000_000_000)
}

/// Validate a 12-digit string supplied by the user / network.
pub fn validate_digit_id(digit_id: &str) -> RosterResult<()> {
    if digit_id.len() < MIN_DIGIT_LEN || digit_id.len() > MAX_DIGIT_LEN {
        return Err(RosterError::InvalidParameter {
            parameter: "digit_id".to_string(),
            reason: format!(
                "length {} outside [{}-{}]",
                digit_id.len(),
                MIN_DIGIT_LEN,
                MAX_DIGIT_LEN
            ),
        });
    }
    if !digit_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(RosterError::InvalidParameter {
            parameter: "digit_id".to_string(),
            reason: "must be ASCII digits".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_for_same_input() {
        let a = stable_digit_from_node("node-123");
        let b = stable_digit_from_node("node-123");
        assert_eq!(a, b);
        assert_eq!(a.len(), 12);
        assert!(a.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn different_inputs_diverge() {
        let a = stable_digit_from_node("node-1");
        let b = stable_digit_from_node("node-2");
        assert_ne!(a, b);
    }

    #[test]
    fn validate_accepts_padded() {
        validate_digit_id("123456789012").unwrap();
        validate_digit_id("000000000000").unwrap();
    }

    #[test]
    fn validate_rejects_short_or_non_digit() {
        assert!(validate_digit_id("12345").is_err());
        assert!(validate_digit_id("12-456789012").is_err());
        assert!(validate_digit_id(&"a".repeat(50)).is_err());
    }
}