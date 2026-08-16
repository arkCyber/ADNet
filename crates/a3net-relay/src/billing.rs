//! Optional billing endpoints for relay operators.
//!
//! This module is only compiled when the `billing` cargo feature is enabled
//! at build time. Operators who do not enable billing keep a pure forward
//! proxy (no extra deps, no extra routes, no extra endpoints to audit).
//!
//! ## Wire protocol
//!
//! The billing layer adds three HTTP endpoints, all returning JSON:
//!
//! | Method | Path                    | Body                | Returns |
//! |--------|-------------------------|---------------------|---------|
//! | POST   | `/relay/billing/pledge` | `Pledge` (URL form) | `{ "ok": true }` |
//! | POST   | `/relay/billing/redeem` | `Receipt`           | `{ "ok": true }` |
//! | GET    | `/relay/billing/status` | —                   | `{ "open_amount_atomic": u128, "issued_receipts": u64 }` |
//!
//! The relay keeps *no* state about pending pledges or redeemed receipts
//! except for an in-memory counter; persistence is the settlement service's
//! job. If the relay restarts, open amounts are simply recomputed from the
//! outstanding receipts at the next `/status` call.

#[cfg(feature = "billing")]
mod billing_impl {
    use std::collections::HashMap;
    use std::sync::Arc;

    use a3net_identity::{IdentityError, Treasury, Wallet};
    use a3net_token::{Pledge, Receipt};
    use axum::{
        Json, Router,
        extract::State,
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use serde::Serialize;
    use serde_json::json;
    use tokio::sync::Mutex;
    use tracing::{info, warn};

    /// Shared in-memory state for the billing layer.
    #[derive(Default, Debug)]
    pub struct BillingState {
        /// Open pledges keyed by pledge nonce (the nonce is unique per
        /// pledgor's namespace and is the only stable identifier that
        /// survives the receipt ↔ pledge binding). Each entry holds
        /// the total amount pledged minus the amount already redeemed.
        ///
        /// We key by nonce (not by pledgor address) so receipts can be
        /// matched back to a specific pledge — without that, a relay
        /// would have no way to know whether a receipt was minted
        /// against an actually-pledged nonce or a forged one.
        pub open: Mutex<HashMap<String, u128>>,
        /// How many receipts this relay has issued (lifetime counter).
        pub issued: Mutex<u64>,
    }

    /// Insert or add to an existing open-balance entry.
    fn add_open(open: &mut HashMap<String, u128>, key: String, amount: u128) {
        let entry = open.entry(key).or_insert(0);
        *entry = entry.saturating_add(amount);
    }

    /// Mode parameter for [`crate::server::RelayServer::start`].
    ///
    /// `Enabled` carries the relay's signing wallet and shared state;
    /// `Disabled` causes the billing routes to be entirely absent (no
    /// `/relay/billing/*` paths exposed, no extra deps pulled in).
    #[derive(Default, Clone, Debug)]
    pub enum BillingMode {
        #[default]
        Disabled,
        Enabled {
            wallet: Arc<Wallet>,
            state: Arc<BillingState>,
        },
    }

    impl BillingMode {
        /// The default constructor — billing off.
        pub fn disabled() -> Self {
            Self::Disabled
        }

        /// Construct the billing-enabled variant from a wallet and shared
        /// state. Most callers will want [`BillingMode::from_treasury`]
        /// instead, which derives the same wallet from a `Treasury` so the
        /// relay shares an identity with the rest of the node.
        pub fn enabled(wallet: Arc<Wallet>, state: Arc<BillingState>) -> Self {
            Self::Enabled { wallet, state }
        }

        /// Construct the billing-enabled variant from a [`Treasury`].
        ///
        /// The relay will sign receipts with the treasury's *root* wallet,
        /// which is the same long-lived identity the rest of the node uses
        /// for pledges, peer tickets, and announcements. Per-session
        /// receipt wallets (the ones the treasury issues on demand) are
        /// **not** used here — receipts must be signed by the root so
        /// peers can verify them against a stable on-chain identity.
        ///
        /// If the treasury has no root loaded yet (it was just deserialized
        /// from disk), this returns `Err`. Callers should chain
        /// [`Treasury::with_root`] first.
        pub fn from_treasury(treasury: Arc<Treasury>) -> Result<Self, IdentityError> {
            let wallet = Arc::new(
                treasury
                    .root()
                    .map_err(|e| match e {
                        IdentityError::InvalidSecretKey(msg) => IdentityError::InvalidSecretKey(
                            format!("treasury root unavailable: {msg}"),
                        ),
                        other => other,
                    })?
                    .clone(),
            );
            Ok(Self::enabled(wallet, Arc::new(BillingState::default())))
        }

        /// Build the routes that should be `merge`'d into the relay's main
        /// axum `Router`. Returns an empty router when billing is off so
        /// the merge is always a no-op.
        pub fn routes(&self) -> Router {
            match self {
                BillingMode::Disabled => Router::new(),
                BillingMode::Enabled { wallet, state } => Router::new()
                    .route("/v1/pledge", post(handle_pledge))
                    .route("/v1/redeem", post(handle_redeem))
                    .route("/v1/status", get(handle_status))
                    .with_state(EnabledState {
                        wallet: wallet.clone(),
                        state: state.clone(),
                    }),
            }
        }
    }

    #[derive(Clone)]
    struct EnabledState {
        wallet: Arc<Wallet>,
        state: Arc<BillingState>,
    }

    // -- handlers -------------------------------------------------------------

    async fn handle_pledge(State(handle): State<EnabledState>, body: String) -> Response {
        let mut pledge = match Pledge::from_url(body.trim()) {
            Ok(p) => p,
            Err(e) => return err_response(StatusCode::BAD_REQUEST, format!("parse: {e}")),
        };
        let now = chrono::Utc::now().timestamp();
        let expected_chain_id = 1; // mainnet by default; multi-chain TBD
        let pledgor = match pledge.verify_with_recovered(now) {
            Ok(addr) => addr,
            Err(e) => return err_response(StatusCode::UNAUTHORIZED, format!("verify: {e}")),
        };
        if pledge.chain_id != expected_chain_id {
            return err_response(
                StatusCode::BAD_REQUEST,
                format!(
                    "chain_id mismatch: expected {expected_chain_id}, got {}",
                    pledge.chain_id
                ),
            );
        }
        if pledge.recipient != handle.wallet.public().address() {
            return err_response(
                StatusCode::BAD_REQUEST,
                "pledge.recipient != this relay".into(),
            );
        }
        // Pin the recovered pledgor so subsequent bookkeeping sees the
        // right address.
        pledge.pledgor = pledgor;
        // Key the open balance by **nonce**, not pledgor address: we
        // need the receipt ↔ pledge binding to survive across requests.
        // Pledgor address alone would let any receipt redeem against
        // any open balance from that pledgor — including a balance
        // minted for a completely different pledge nonce. Nonce is
        // unique within the pledgor's namespace and is what's in the
        // receipt body, so it's the right join key.
        let key = pledge.nonce.clone();
        let mut open = handle.state.open.lock().await;
        add_open(&mut open, key, pledge.amount_atomic);
        info!(pledgor = %pledge.pledgor, nonce = %pledge.nonce, amount = pledge.amount_atomic, "pledge accepted");
        (StatusCode::OK, Json(json!({"ok": true}))).into_response()
    }

    async fn handle_redeem(
        State(handle): State<EnabledState>,
        Json(receipt): Json<Receipt>,
    ) -> Response {
        if let Err(e) = receipt.verify() {
            return err_response(StatusCode::UNAUTHORIZED, format!("verify: {e}"));
        }
        if receipt.relay != handle.wallet.public().address() {
            return err_response(
                StatusCode::BAD_REQUEST,
                "receipt.relay != this relay".into(),
            );
        }
        // Bind the receipt to an actual open pledge. Without this
        // check a forged receipt could be redeemed against an open
        // balance minted for an *unrelated* pledge nonce (or against
        // no pledge at all).
        let mut open = handle.state.open.lock().await;
        let key = receipt.pledge_nonce.clone();
        match open.get_mut(&key) {
            None => {
                return err_response(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "no open pledge with nonce {}; refusing to redeem an orphan receipt",
                        receipt.pledge_nonce
                    ),
                );
            }
            Some(balance) if *balance < receipt.charged_atomic => {
                return err_response(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "open balance {} for nonce {} is below charged_atomic {}",
                        balance, receipt.pledge_nonce, receipt.charged_atomic
                    ),
                );
            }
            Some(balance) => {
                *balance = balance.saturating_sub(receipt.charged_atomic);
            }
        }
        *handle.state.issued.lock().await += 1;
        info!(
            pledgor = %receipt.pledgor,
            nonce = %receipt.pledge_nonce,
            charged = receipt.charged_atomic,
            "receipt redeemed"
        );
        (StatusCode::OK, Json(json!({"ok": true}))).into_response()
    }

    async fn handle_status(State(handle): State<EnabledState>) -> Response {
        let open = handle.state.open.lock().await;
        let open_total: u128 = open.values().sum();
        #[derive(Serialize)]
        struct Status {
            open_amount_atomic: u128,
            issued_receipts: u64,
        }
        let issued = *handle.state.issued.lock().await;
        Json(Status {
            open_amount_atomic: open_total,
            issued_receipts: issued,
        })
        .into_response()
    }

    fn err_response(status: StatusCode, msg: String) -> Response {
        if status.is_server_error() {
            warn!(error = %msg, "billing endpoint failed");
        }
        (status, Json(json!({"ok": false, "error": msg}))).into_response()
    }
}

#[cfg(feature = "billing")]
pub use billing_impl::{BillingMode, BillingState};

/// No-op marker when the `billing` feature is off. `BillingMode` is still
/// exported so call sites can always write `BillingMode::Disabled`.
#[cfg(not(feature = "billing"))]
#[derive(Default, Clone)]
pub enum BillingMode {
    #[default]
    Disabled,
}

#[cfg(not(feature = "billing"))]
#[derive(Default, Clone)]
pub struct BillingState;
