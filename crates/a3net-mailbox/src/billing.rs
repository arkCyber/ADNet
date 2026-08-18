//! Billing and metering for the mailbox server.
//!
//! ## Overview
//!
//! Operators who want to monetise the mailbox can enable the `billing`
//! feature. This activates a **pledge verification layer** in the enqueue
//! path:
//!
//! 1. The sender attaches a signed [`a3net_token::Pledge`] to the request
//!    as an `X-A3Net-Pledge` header (URL-encoded form).
//! 2. The server verifies the pledge is unexpired and the recovered
//!    signer matches the `sender_id`.
//! 3. If valid, the pledged amount is converted to bonus quota bytes
//!    (default: 1 atomic unit → 1 byte of extra storage quota).
//! 4. Free-tier senders (no pledge header) receive the default
//!    [`QuotaPolicy`] quota.
//!
//! ## Pledge requirements
//!
//! For a pledge to be accepted:
//! - `recipient` must equal the mailbox server's own address.
//! - `expiry_unix` must be in the future.
//! - The signature must recover to `pledgor == sender_id`.
//! - The `chain_id` must match the configured chain.
//!
//! ## Quota bonus
//!
//! `pledged_amount_atomic * bonus_bytes_per_unit` bytes are added to the
//! recipient's quota budget. The multiplier is configurable.

/// Bonus bytes awarded per atomic unit of pledged token amount.
/// Default: 1 atomic unit → 1 byte of extra quota.
/// At 6 decimal places (USDC-like), 1_000_000 units ≈ 1 MiB.
const BONUS_BYTES_PER_UNIT: u128 = 1;

/// Billing policy for the mailbox.
///
/// When the `billing` feature is disabled, all methods return no bonus.
/// When enabled, verified pledges grant bonus quota bytes.
#[derive(Debug, Clone)]
pub struct BillingPolicy {
    /// Server's EVM address (used as `Pledge::recipient` check).
    pub server_address: String,
    /// Expected chain ID (e.g. 1 for Ethereum mainnet, 137 for Polygon).
    pub chain_id: u64,
    /// Extra bytes per atomic unit of pledged amount.
    pub bonus_bytes_per_unit: u128,
    /// Whether billing is mandatory (reject enqueue without a valid pledge).
    /// Default: false (free tier with default quota).
    pub mandatory: bool,
}

impl Default for BillingPolicy {
    fn default() -> Self {
        Self {
            server_address: "0x0000000000000000000000000000000000000000".to_string(),
            chain_id: 1,
            bonus_bytes_per_unit: BONUS_BYTES_PER_UNIT,
            mandatory: false,
        }
    }
}

impl BillingPolicy {
    /// Build a policy that accepts pledges on the given chain.
    pub fn new(server_address: &str, chain_id: u64) -> Self {
        Self {
            server_address: server_address.to_string(),
            chain_id,
            bonus_bytes_per_unit: BONUS_BYTES_PER_UNIT,
            mandatory: false,
        }
    }

    /// Verify a pledge URL string and return the bonus quota bytes.
    ///
    /// Returns `Ok(Some(bonus_bytes))` if the pledge is valid and grants quota.
    /// Returns `Ok(None)` if no pledge was provided (free tier).
    /// Returns `Err` if the pledge is malformed or invalid.
    ///
    /// The pledge is verified against:
    /// - `now_unix`: current wall-clock time (UTC seconds).
    /// - `chain_id`: must match the configured chain.
    /// - `recipient`: must be the server's own address.
    /// - Signature: must recover to `pledgor`.
    pub fn verify_pledge(
        &self,
        pledge_url: &str,
        sender_id: &str,
        now_unix: i64,
    ) -> Result<Option<u64>, BillingError> {
        // Parse the pledge from URL form.
        let pledge = a3net_token::Pledge::from_url(pledge_url)
            .map_err(|e| BillingError::Parse(e.to_string()))?;

        // `from_url` leaves pledgor as zero-address; recover the actual signer.
        let pledgor = pledge
            .verify_with_recovered(now_unix)
            .map_err(|e| BillingError::Verification(e.to_string()))?;

        // Chain-id check (separate from verify_with_recovered).
        if pledge.chain_id != self.chain_id {
            return Err(BillingError::Verification(format!(
                "chain_id mismatch: expected {}, got {}",
                self.chain_id, pledge.chain_id
            )));
        }

        // Verify the pledgor matches the sender_id.
        let pledgor_checksum = pledgor.to_checksum();
        if pledgor_checksum != sender_id {
            return Err(BillingError::SignerMismatch {
                sender: sender_id.to_string(),
                pledgor: pledgor_checksum,
            });
        }

        // Verify recipient matches our server address.
        if pledge.recipient.to_checksum() != self.server_address {
            return Err(BillingError::WrongRecipient {
                expected: self.server_address.clone(),
                got: pledge.recipient.to_checksum(),
            });
        }

        // Compute bonus quota.
        let bonus = pledge
            .amount_atomic
            .saturating_mul(self.bonus_bytes_per_unit)
            .min(u64::MAX as u128) as u64;

        Ok(Some(bonus))
    }

    /// Return the bonus quota from a pledge if valid, or `Ok(0)` for free tier.
    /// Logs errors so billing failures don't break the free tier.
    pub fn try_grant_quota(
        &self,
        pledge_url: Option<&str>,
        sender_id: &str,
        now_unix: i64,
    ) -> u64 {
        let Some(url) = pledge_url else { return 0 };
        match self.verify_pledge(url, sender_id, now_unix) {
            Ok(Some(bonus)) => bonus,
            Ok(None) => 0,
            Err(e) => {
                tracing::warn!(error = %e, sender = %sender_id, "pledge verification failed, granting 0 bonus quota");
                0
            }
        }
    }
}

/// Errors from pledge verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BillingError {
    /// The pledge URL could not be parsed.
    Parse(String),
    /// The pledge failed cryptographic / expiry verification.
    Verification(String),
    /// The pledge `recipient` does not match the server address.
    WrongRecipient { expected: String, got: String },
    /// The pledge `pledgor` does not match the `sender_id`.
    SignerMismatch { sender: String, pledgor: String },
}

impl std::fmt::Display for BillingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(s) => write!(f, "pledge parse error: {s}"),
            Self::Verification(s) => write!(f, "pledge verification failed: {s}"),
            Self::WrongRecipient { expected, got } => {
                write!(f, "pledge recipient mismatch: expected {expected}, got {got}")
            }
            Self::SignerMismatch { sender, pledgor } => {
                write!(f, "pledge signer mismatch: sender={sender}, pledgor={pledgor}")
            }
        }
    }
}

impl std::error::Error for BillingError {}

/// HTTP status code for each billing error.
impl BillingError {
    pub fn http_status(&self) -> axum::http::StatusCode {
        match self {
            Self::Parse(_) | Self::Verification(_) => axum::http::StatusCode::BAD_REQUEST,
            Self::WrongRecipient { .. } => axum::http::StatusCode::BAD_REQUEST,
            Self::SignerMismatch { .. } => axum::http::StatusCode::UNAUTHORIZED,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(feature = "billing")]
#[cfg(test)]
mod tests {
    use super::*;
    use a3net_identity::{Address, Wallet};
    use a3net_token::{Pledge, PledgeBody};

    const SERVER: &str = "0x0000000000000000000000000000000000000001";
    const CHAIN: u64 = 137;

    /// Build a pledge URL signed by `wallet` for testing.
    fn pledge_url(
        chain_id: u64,
        contract: &str,
        token: &str,
        amount: u128,
        server: &str,
        expiry: i64,
        wallet: &Wallet,
    ) -> String {
        // Use Pledge::body which returns a PledgeBody, then sign.
        let server_addr = Address::from_hex(server).expect("valid address");
        let body = Pledge::body(
            chain_id,
            contract.to_string(),
            token.to_string(),
            amount,
            server_addr,
            hex::encode(rand::random::<[u8; 32]>()),
            expiry,
        )
        .expect("valid body");
        let pledge = Pledge::sign(body, wallet).expect("sign succeeded");
        pledge.to_url()
    }

    #[test]
    fn default_policy_has_sane_values() {
        let p = BillingPolicy::default();
        assert_eq!(p.bonus_bytes_per_unit, BONUS_BYTES_PER_UNIT);
        assert!(!p.mandatory);
    }

    #[test]
    fn try_grant_quota_returns_zero_when_no_pledge() {
        let p = BillingPolicy::new(SERVER, CHAIN);
        let bonus = p.try_grant_quota(None, SERVER, chrono::Utc::now().timestamp());
        assert_eq!(bonus, 0);
    }

    #[test]
    fn try_grant_quota_returns_zero_on_invalid_pledge() {
        let p = BillingPolicy::new(SERVER, CHAIN);
        let bonus =
            p.try_grant_quota(Some("not-a-valid-url"), SERVER, chrono::Utc::now().timestamp());
        assert_eq!(bonus, 0, "invalid pledge should not panic, returns 0");
    }

    #[test]
    fn verify_pledge_rejects_wrong_chain() {
        let p = BillingPolicy::new(SERVER, 999); // wrong chain
        let wallet = Wallet::generate();
        let url = pledge_url(
            1, // correct chain
            "0x0000000000000000000000000000000000000001",
            "0x0000000000000000000000000000000000000001",
            1_000_000u128,
            SERVER,
            chrono::Utc::now().timestamp() + 3600,
            &wallet,
        );
        let result = p.verify_pledge(&url, wallet.public().address().to_checksum().as_str(), chrono::Utc::now().timestamp());
        assert!(
            matches!(result, Err(BillingError::Verification(_))),
            "wrong chain should fail verification"
        );
    }

    #[test]
    fn verify_pledge_accepts_valid_pledge() {
        let wallet = Wallet::generate();
        let sender = wallet.public().address().to_checksum();
        let p = BillingPolicy::new(SERVER, CHAIN);
        let url = pledge_url(
            CHAIN,
            "0x0000000000000000000000000000000000000001",
            "0x0000000000000000000000000000000000000001",
            100_000u128, // 100K atomic units → 100K bonus bytes
            SERVER,
            chrono::Utc::now().timestamp() + 3600,
            &wallet,
        );
        let bonus = p.verify_pledge(&url, &sender, chrono::Utc::now().timestamp());
        assert!(bonus.is_ok(), "valid pledge should verify: {bonus:?}");
        assert_eq!(bonus.unwrap(), Some(100_000), "bonus should equal pledged amount");
    }

    #[test]
    fn verify_pledge_rejects_signer_mismatch() {
        let alice = Wallet::generate();
        let bob = Wallet::generate(); // different wallet
        let sender = alice.public().address().to_checksum();
        let p = BillingPolicy::new(SERVER, CHAIN);
        let url = pledge_url(
            CHAIN,
            "0x0000000000000000000000000000000000000001",
            "0x0000000000000000000000000000000000000001",
            1_000_000u128,
            SERVER,
            chrono::Utc::now().timestamp() + 3600,
            &bob, // signed by bob
        );
        let result = p.verify_pledge(&url, &sender, chrono::Utc::now().timestamp());
        assert!(
            matches!(result, Err(BillingError::SignerMismatch { .. })),
            "signer mismatch should be rejected"
        );
    }

    #[test]
    fn verify_pledge_rejects_expired_pledge() {
        let wallet = Wallet::generate();
        let sender = wallet.public().address().to_checksum();
        let p = BillingPolicy::new(SERVER, CHAIN);
        let url = pledge_url(
            CHAIN,
            "0x0000000000000000000000000000000000000001",
            "0x0000000000000000000000000000000000000001",
            1_000_000u128,
            SERVER,
            chrono::Utc::now().timestamp() - 1, // expired
            &wallet,
        );
        let result = p.verify_pledge(&url, &sender, chrono::Utc::now().timestamp());
        assert!(
            matches!(result, Err(BillingError::Verification(_))),
            "expired pledge should fail"
        );
    }
}
