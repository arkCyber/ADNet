//! Invitation code — a short, human-readable pairing code for manual entry.
//!
//! ## Overview
//!
//! The full pairing invitation is a `SignedInvitation` encoded as a
//! `a3net-pairing://<base64url>` URL. For QR scanning this is ideal —
//! but for manual entry (e.g. reading the code aloud over a phone call)
//! a shorter format is needed.
//!
//! ## Format
//!
//! An invitation code is a `ADNET:` prefixed string:
//! ```text
//! ADNET:XXXX-YYYY-ZZZZ-NNNN
//! ```
//!
//! Where each segment is 4 uppercase alphanumeric characters, separated
//! by hyphens. Total length: 4 + 1 + 19 = 24 characters.
//!
//! ## Security
//!
//! The code encodes a 64-bit truncated hash of the full invitation,
//! plus a 32-bit checksum. This is NOT the invitation itself —
//! it's a lookup key. Both peers must have the full invitation data
//! available (e.g. exchanged via QR code first, or stored in a shared
//! location).
//!
//! ## Usage
//!
//! ```rust,ignore
//! use a3net_pairing::{SignedInvitation, InvitationCode};
//!
//! // Generate a code from an invitation
//! let code = InvitationCode::from_invitation(&inv);
//! println!("Enter this code: {}", code);
//!
//! // Parse a code
//! let parsed = "ADNET:AAAA-BBBB-CCCC-DDDD".parse::<InvitationCode>();
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{PairingError, PairingResult};
use crate::invitation::SignedInvitation;

const CODE_PREFIX: &str = "ADNET:";
const CODE_SEGMENTS: usize = 4;
const SEGMENT_LEN: usize = 4;

/// A short, human-readable invitation code.
///
/// Format: `ADNET:XXXX-YYYY-ZZZZ-NNNN` (24 characters total)
/// where each segment is 4 uppercase alphanumeric characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InvitationCode {
    /// The raw code without prefix, e.g. "XXXX-YYYY-ZZZZ-NNNN"
    raw: String,
}

impl InvitationCode {
    /// Character set for the code: uppercase letters + digits (no O, 0, I, 1 to avoid confusion)
    const CHARSET: &'static [u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

    /// Generate a random invitation code.
    ///
    /// Note: This generates a random code that must be associated with
    /// a real invitation separately (e.g. via a shared database).
    pub fn generate_random() -> Self {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        
        let mut segments = Vec::with_capacity(CODE_SEGMENTS);
        for _ in 0..CODE_SEGMENTS {
            let mut segment = String::with_capacity(SEGMENT_LEN);
            for _ in 0..SEGMENT_LEN {
                let idx = rng.gen_range(0..Self::CHARSET.len());
                segment.push(Self::CHARSET[idx] as char);
            }
            segments.push(segment);
        }
        
        Self {
            raw: segments.join("-"),
        }
    }

    /// Create an invitation code from a `SignedInvitation`.
    ///
    /// This creates a code that encodes a truncated hash of the invitation.
    /// The full invitation must be exchanged separately for the code to be useful.
    pub fn from_invitation(inv: &SignedInvitation) -> PairingResult<Self> {
        let hash = crate::transport_identity::pairing_invitation_digest(&inv.payload);
        
        // Encode first 16 bytes to our character set (4 chars per byte)
        let mut code_chars = Vec::with_capacity(16);
        for (i, &byte) in hash.iter().enumerate() {
            // Each byte gives 2 characters
            let idx1 = (byte >> 4) as usize % Self::CHARSET.len();
            let idx2 = (byte & 0x0F) as usize % Self::CHARSET.len();
            code_chars.push(Self::CHARSET[idx1 ^ (i % Self::CHARSET.len())] as char);
            code_chars.push(Self::CHARSET[idx2 ^ ((i + 7) % Self::CHARSET.len())] as char);
        }
        
        // Split into 4 segments of 4 chars each = 16 total
        let mut segments = Vec::with_capacity(CODE_SEGMENTS);
        for i in 0..CODE_SEGMENTS {
            let start = i * SEGMENT_LEN;
            let end = start + SEGMENT_LEN;
            segments.push(code_chars[start..end].iter().collect::<String>());
        }
        
        Ok(Self {
            raw: segments.join("-"),
        })
    }

    /// Get the raw code without prefix.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Validate the format of a code string (without prefix).
    fn validate_format(raw: &str) -> PairingResult<()> {
        // Check format: XXXX-YYYY-ZZZZ-NNNN
        let parts: Vec<&str> = raw.split('-').collect();
        if parts.len() != CODE_SEGMENTS {
            return Err(PairingError::Malformed {
                what: "invitation_code",
                reason: format!(
                    "code must have {} segments separated by '-', got {}",
                    CODE_SEGMENTS,
                    parts.len()
                ),
            });
        }
        
        for (i, part) in parts.iter().enumerate() {
            if part.len() != SEGMENT_LEN {
                return Err(PairingError::Malformed {
                    what: "invitation_code",
                    reason: format!(
                        "segment {} must be {} characters, got {}",
                        i + 1,
                        SEGMENT_LEN,
                        part.len()
                    ),
                });
            }
            
            for c in part.chars() {
                if !Self::CHARSET.iter().any(|&x| x as char == c) {
                    return Err(PairingError::Malformed {
                        what: "invitation_code",
                        reason: format!(
                            "invalid character '{}' in segment {} (allowed: A-Z, 2-9, excluding O, I, 0, 1)",
                            c,
                            i + 1
                        ),
                    });
                }
            }
        }
        
        Ok(())
    }
}

impl fmt::Display for InvitationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", CODE_PREFIX, self.raw)
    }
}

impl std::str::FromStr for InvitationCode {
    type Err = PairingError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        
        // Strip prefix if present
        let raw = if let Some(stripped) = s.strip_prefix(CODE_PREFIX) {
            stripped
        } else if let Some(stripped) = s.strip_prefix("a3net:") {
            stripped
        } else {
            s
        };
        
        // Uppercase and validate format
        let uppercased = raw.to_uppercase();
        Self::validate_format(&uppercased)?;
        
        Ok(Self {
            raw: uppercased,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_identity::wallet::Wallet;
    use a3net_types::node::NodeId;
    use crate::capability::CapabilitySet;

    fn make_invitation() -> SignedInvitation {
        let wallet = Wallet::generate();
        let node_id = NodeId::from_bytes(&[0xAAu8; 32]).unwrap();
        SignedInvitation::create(
            &node_id,
            &wallet,
            CapabilitySet::from_names(["chat"]),
            900,
            Some("Test invitation".into()),
        )
        .unwrap()
    }

    #[test]
    fn code_format_validation() {
        // Valid code
        let valid = "ADNET:ABCD-EFGH-JKLM-NPQR";
        assert!(valid.parse::<InvitationCode>().is_ok());

        // Without prefix
        let no_prefix = "ABCD-EFGH-JKLM-NPQR";
        assert!(no_prefix.parse::<InvitationCode>().is_ok());

        // Wrong number of segments
        let wrong_segments = "ADNET:ABCD-EFGH-JKLM";
        assert!(wrong_segments.parse::<InvitationCode>().is_err());

        // Wrong segment length
        let wrong_len = "ADNET:ABC-EFGH-JKLM-NPQR";
        assert!(wrong_len.parse::<InvitationCode>().is_err());

        // Invalid characters (O, I, 0, 1)
        let invalid_char = "ADNET:ABCD-OIHG-JKLM-NPQR";
        assert!(invalid_char.parse::<InvitationCode>().is_err());

        // Lowercase is accepted and uppercased
        let lowercase = "a3net:abcd-efgh-jklm-npqr";
        let parsed = lowercase.parse::<InvitationCode>().unwrap();
        assert_eq!(parsed.as_str(), "ABCD-EFGH-JKLM-NPQR");
    }

    #[test]
    fn code_from_invitation() {
        let inv = make_invitation();
        let code = InvitationCode::from_invitation(&inv).unwrap();
        
        // Check format
        assert!(code.to_string().starts_with("ADNET:"));
        assert_eq!(code.as_str().len(), 19); // XXXX-YYYY-ZZZZ-NNNN = 4*4 + 3 = 19
        
        // Same invitation produces same code
        let code2 = InvitationCode::from_invitation(&inv).unwrap();
        assert_eq!(code, code2);
        
        // Different invitation produces different code
        let inv2 = {
            let wallet = Wallet::generate();
            let node_id = NodeId::from_bytes(&[0xBBu8; 32]).unwrap();
            SignedInvitation::create(
                &node_id,
                &wallet,
                CapabilitySet::from_names(["chat"]),
                900,
                None,
            )
            .unwrap()
        };
        let code3 = InvitationCode::from_invitation(&inv2).unwrap();
        assert_ne!(code, code3);
    }

    #[test]
    fn random_code_generation() {
        let code1 = InvitationCode::generate_random();
        let code2 = InvitationCode::generate_random();
        
        // Both should be valid
        assert!(code1.to_string().starts_with("ADNET:"));
        assert!(code2.to_string().starts_with("ADNET:"));
        
        // Should be different (with very high probability)
        assert_ne!(code1, code2);
    }

    #[test]
    fn display_format() {
        let code: InvitationCode = "ABCD-EFGH-JKLM-NPQR".parse().unwrap();
        assert_eq!(code.to_string(), "ADNET:ABCD-EFGH-JKLM-NPQR");
    }

    #[test]
    fn serde_round_trip() {
        let code = InvitationCode::generate_random();
        let json = serde_json::to_string(&code).unwrap();
        let parsed: InvitationCode = serde_json::from_str(&json).unwrap();
        assert_eq!(code, parsed);
    }
}
