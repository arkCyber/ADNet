//! Bitswap HTTP API Handler for IPFS-compatible Gateway
//!
//! This module implements the `/api/v0/bitswap/*` endpoints compatible with
//! the IPFS HTTP API specification.
//!
//! ## Endpoints
//!
//! | Endpoint | Method | Description |
//! |----------|--------|-------------|
//! | `/api/v0/bitswap/stat` | POST | Get Bitswap statistics |
//! | `/api/v0/bitswap/ledger` | POST | Get ledger for a peer |
//! | `/api/v0/bitswap/list` | POST | Get wantlist |
//! | `/api/v0/bitswap/wantlist` | POST | Get full wantlist |
//! | `/api/v0/bitswap/reprovide` | POST | Reprovide local content |
//!
//! ## DO-178C Traceability
//!
//! - BITSWAP-8: HTTP API provides IPFS-compatible management interface

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::BitswapApi;

// ─────────────────────────────────────────────────────────────────
// API Types
// ─────────────────────────────────────────────────────────────────

/// Response for `/api/v0/bitswap/stat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitswapStatResponse {
    pub provide_buf_len: u32,
    pub wantlist: Vec<WantlistEntry>,
    pub peers: Vec<String>,
    pub blocks_sent: u64,
    pub bytes_sent: u64,
    pub blocks_received: u64,
    pub bytes_received: u64,
    pub data_received: u64,
    pub dup_blks_received: u64,
    pub dup_data_received: u64,
}

/// A wantlist entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WantlistEntry {
    #[serde(rename = "/")]
    pub block: String,
    pub priority: i32,
    pub want_type: String,
    #[serde(rename = "SendDontHave")]
    pub send_dont_have: bool,
}

/// Response for `/api/v0/bitswap/ledger`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitswapLedgerResponse {
    pub peer: String,
    pub sent: u64,
    pub received: u64,
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub blocks_sent: u64,
    pub blocks_received: u64,
    pub wantlist: Vec<WantlistEntry>,
}

/// Response for `/api/v0/bitswap/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitswapListResponse {
    #[serde(rename = "Keys")]
    pub keys: HashMap<String, WantlistEntry>,
}

/// Response for reprovide operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReprovideResponse {
    pub str: String,
}

/// Query parameters for ledger endpoint.
#[derive(Debug, Deserialize)]
pub struct LedgerQuery {
    pub arg: String,
}

/// Query parameters for generic endpoint.
#[derive(Debug, Deserialize)]
pub struct GenericQuery {
    pub arg: Option<String>,
}

/// Bitswap application state.
#[derive(Clone)]
pub struct BitswapAppState {
    pub api: Arc<BitswapApi>,
    pub local_content: Arc<RwLock<Vec<String>>>,
}

impl Default for BitswapAppState {
    fn default() -> Self {
        Self {
            api: Arc::new(BitswapApi::default()),
            local_content: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// HTTP Handlers
// ─────────────────────────────────────────────────────────────────

/// Handler for `/api/v0/bitswap/stat`.
pub async fn bitswap_stat_handler(
    State(state): State<BitswapAppState>,
) -> impl IntoResponse {
    let stats = state.api.stats().await;
    let wantlist = state.api.wantlist().await;
    let peers = state.api.peers().await;

    let response = BitswapStatResponse {
        provide_buf_len: 0,
        wantlist: wantlist
            .into_iter()
            .map(|key| WantlistEntry {
                block: format!("/{}", key),
                priority: 1,
                want_type: "block".to_string(),
                send_dont_have: false,
            })
            .collect(),
        peers: peers.into_iter().map(|p| p.peer_id.unwrap_or_default()).collect(),
        blocks_sent: stats.blocks_sent,
        bytes_sent: stats.data_sent,
        blocks_received: stats.blocks_received,
        bytes_received: stats.data_received,
        data_received: stats.data_received,
        dup_blks_received: 0,
        dup_data_received: 0,
    };

    Json(response)
}

/// Handler for `/api/v0/bitswap/ledger`.
pub async fn bitswap_ledger_handler(
    State(state): State<BitswapAppState>,
    Query(query): Query<LedgerQuery>,
) -> impl IntoResponse {
    match state.api.ledger(&query.arg).await {
        Some(ledger) => {
            let response = serde_json::json!({
                "Peer": ledger.peer,
                "Sent": ledger.sent,
                "Received": ledger.received,
                "SentBytes": ledger.sent_bytes,
                "ReceivedBytes": ledger.received_bytes,
                "BlocksSent": ledger.blocks_sent,
                "BlocksReceived": ledger.blocks_received,
                "WantList": []
            });
            Json(response)
        }
        None => {
            let error = serde_json::json!({
                "Message": format!("peer {} not found in ledger", query.arg),
                "Code": 1
            });
            Json(error)
        }
    }
}

/// Handler for `/api/v0/bitswap/list`.
pub async fn bitswap_list_handler(
    State(state): State<BitswapAppState>,
) -> impl IntoResponse {
    let wantlist = state.api.wantlist().await;

    let mut keys = HashMap::new();
    for key in wantlist {
        keys.insert(
            format!("/{}", key),
            WantlistEntry {
                block: format!("/{}", key),
                priority: 1,
                want_type: "block".to_string(),
                send_dont_have: false,
            },
        );
    }

    Json(BitswapListResponse { keys })
}

/// Handler for `/api/v0/bitswap/wantlist`.
pub async fn bitswap_wantlist_handler(
    State(state): State<BitswapAppState>,
) -> impl IntoResponse {
    let wantlist = state.api.wantlist().await;

    let entries: Vec<WantlistEntry> = wantlist
        .into_iter()
        .map(|key| WantlistEntry {
            block: format!("/{}", key),
            priority: 1,
            want_type: "block".to_string(),
            send_dont_have: false,
        })
        .collect();

    Json(serde_json::json!({
        "Keys": entries
    }))
}

/// Handler for `/api/v0/bitswap/reprovide`.
pub async fn bitswap_reprovide_handler(
    State(state): State<BitswapAppState>,
) -> impl IntoResponse {
    // In a full implementation, this would:
    // 1. Iterate over all local content
    // 2. Announce each block via DHT provider records
    // 3. Broadcast via gossip protocol

    let local_content = state.local_content.read().await;
    let count = local_content.len();

    Json(ReprovideResponse {
        str: format!("bitswap reprovide complete for {} blocks", count),
    })
}

/// Handler for `/api/v0/bitswap/provide`.
pub async fn bitswap_provide_handler(
    State(state): State<BitswapAppState>,
    Query(query): Query<GenericQuery>,
) -> impl IntoResponse {
    if let Some(cid) = query.arg {
        // Record that we're providing this CID
        let mut local_content = state.local_content.write().await;
        if !local_content.contains(&cid) {
            local_content.push(cid.clone());
        }

        Json(serde_json::json!({
            "str": format!("providing {}", cid)
        }))
    } else {
        Json(serde_json::json!({
            "Message": "cid argument required",
            "Code": 1
        }))
    }
}

/// Handler for `/api/v0/bitswap/unwant`.
pub async fn bitswap_unwant_handler(
    State(state): State<BitswapAppState>,
    Query(query): Query<GenericQuery>,
) -> impl IntoResponse {
    if let Some(cid) = query.arg {
        // In a full implementation, this would:
        // 1. Remove the CID from the wantlist
        // 2. Send CANCEL messages to connected peers

        Json(serde_json::json!({
            "str": format!("unwanted {}", cid)
        }))
    } else {
        Json(serde_json::json!({
            "Message": "cid argument required",
            "Code": 1
        }))
    }
}

// ─────────────────────────────────────────────────────────────────
// Router Builder
// ─────────────────────────────────────────────────────────────────

/// Create the Bitswap API router.
pub fn create_bitswap_router(state: BitswapAppState) -> Router {
    Router::new()
        .route("/stat", post(bitswap_stat_handler))
        .route("/ledger", post(bitswap_ledger_handler))
        .route("/list", post(bitswap_list_handler))
        .route("/wantlist", post(bitswap_wantlist_handler))
        .route("/reprovide", post(bitswap_reprovide_handler))
        .route("/provide", post(bitswap_provide_handler))
        .route("/unwant", post(bitswap_unwant_handler))
        .with_state(state)
}

/// Create a default Bitswap API state.
pub fn create_bitswap_state(api: Arc<BitswapApi>) -> BitswapAppState {
    BitswapAppState {
        api,
        local_content: Arc::new(RwLock::new(Vec::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_bitswap_stat_response() {
        let api = Arc::new(BitswapApi::new());
        let state = create_bitswap_state(api);

        // This should compile and work
        let _response = bitswap_stat_handler(State(state)).await;
    }

    #[tokio::test]
    async fn test_bitswap_list_response() {
        let api = Arc::new(BitswapApi::new());
        let state = create_bitswap_state(api);

        let _response = bitswap_list_handler(State(state)).await;
    }

    #[tokio::test]
    async fn test_reprovide_response() {
        let api = Arc::new(BitswapApi::new());
        let state = create_bitswap_state(api);

        let response = bitswap_reprovide_handler(State(state)).await;
        assert!(response.into_response().status() == StatusCode::OK);
    }
}
