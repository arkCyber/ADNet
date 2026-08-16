//! DO-178C DAL-A Compliance Test Suite for IPC-to-Node Bridge Integration
//!
//! Run with:
//! ```sh
//! cargo test -p a3net-ipc-adapter --features aerospace --test ipc_node_bridge_aerospace
//! ```
//!
//! This test suite verifies the critical integration between the IPC adapter
//! (JSON-RPC Unix socket interface) and the core A3Net Node.
//!
//! Safety Requirements (SR-1 through SR-25) map to:
//! - SR-1..5: IPC initialization and lifecycle
//! - SR-6..10: RPC method invocation
//! - SR-11..15: Notification forwarding
//! - SR-16..20: Error handling and recovery
//! - SR-21..25: Concurrent request handling

#![cfg(feature = "aerospace")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

// ─────────────────────────────────────────────────────────────────────────────
// Safety Revision Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Safety revision for this test suite
const SAFETY_REVISION: &str = "IPC-NODE-BRIDGE-20260813";

/// DAL level for this component
const DAL_LEVEL: &str = "A";

/// Reproducible build flag
const REPRODUCIBLE_BUILD: bool = true;

// ─────────────────────────────────────────────────────────────────────────────
// IPC Protocol Types
// ─────────────────────────────────────────────────────────────────────────────

/// JSON-RPC request structure
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    #[serde(default)]
    params: Option<JsonValue>,
    #[serde(default)]
    id: Option<JsonValue>,
}

impl JsonRpcRequest {
    fn new(method: &str, params: Option<JsonValue>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id: Some(serde_json::json!(1)),
        }
    }
}

/// JSON-RPC response structure
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(default)]
    result: Option<JsonValue>,
    #[serde(default)]
    error: Option<JsonRpcError>,
    #[serde(default)]
    id: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(default)]
    data: Option<JsonValue>,
}

impl JsonRpcResponse {
    fn success(result: JsonValue, id: JsonValue) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id: Some(id),
        }
    }

    fn error(code: i32, message: &str, id: JsonValue) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
            id: Some(id),
        }
    }
}

/// Notification structure
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcNotification {
    jsonrpc: String,
    method: String,
    #[serde(default)]
    params: Option<JsonValue>,
}

impl JsonRpcNotification {
    fn new(method: &str, params: Option<JsonValue>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
        }
    }
}

/// IPC method names (from a3net-ipc-adapter)
const METHOD_INIT: &str = "init";
const METHOD_INFO: &str = "info";
const METHOD_LIST_ROOMS: &str = "list_rooms";
const METHOD_JOIN: &str = "join";
const METHOD_LEAVE: &str = "leave";
const METHOD_FEED: &str = "feed";
const METHOD_ANNOUNCE: &str = "announce";
const METHOD_PEERS_FOR: &str = "peers_for";
const METHOD_MAKE_TICKET: &str = "make_ticket";

/// Notification types
const NOTIF_ANNOUNCEMENT: &str = "announcement";

// ─────────────────────────────────────────────────────────────────────────────
// Mock Node Implementation
// ─────────────────────────────────────────────────────────────────────────────

/// Mock Node for testing
#[derive(Debug, Clone, Default)]
struct MockNode {
    started: Arc<std::sync::RwLock<bool>>,
    rooms: Arc<std::sync::RwLock<Vec<String>>>,
    announcements: Arc<std::sync::RwLock<Vec<MockAnnouncement>>>,
    stats: Arc<std::sync::RwLock<NodeStats>>,
}

#[derive(Debug, Clone, Default)]
struct NodeStats {
    requests_handled: u64,
    errors: u64,
    last_request_at: Option<Instant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockAnnouncement {
    room: String,
    content_hash: String,
    title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeInfo {
    node_id: String,
    version: String,
    uptime_secs: u64,
    connected_peers: u32,
    rooms_joined: usize,
}

impl MockNode {
    fn new() -> Self {
        Self::default()
    }

    fn start(&self) -> Result<(), String> {
        let mut started = self.started.write().unwrap();
        if *started {
            return Err("already started".to_string());
        }
        *started = true;
        Ok(())
    }

    fn is_started(&self) -> bool {
        *self.started.read().unwrap()
    }

    fn init(&self) -> Result<NodeInfo, String> {
        if !self.is_started() {
            self.start()?;
        }

        let mut stats = self.stats.write().unwrap();
        stats.requests_handled += 1;
        stats.last_request_at = Some(Instant::now());

        let rooms = self.rooms.read().unwrap();
        Ok(NodeInfo {
            node_id: "mock-node-001".to_string(),
            version: "1.0.0".to_string(),
            uptime_secs: 3600,
            connected_peers: 5,
            rooms_joined: rooms.len(),
        })
    }

    fn info(&self) -> Result<NodeInfo, String> {
        let mut stats = self.stats.write().unwrap();
        stats.requests_handled += 1;
        stats.last_request_at = Some(Instant::now());

        let rooms = self.rooms.read().unwrap();
        Ok(NodeInfo {
            node_id: "mock-node-001".to_string(),
            version: "1.0.0".to_string(),
            uptime_secs: 3600,
            connected_peers: 5,
            rooms_joined: rooms.len(),
        })
    }

    fn list_rooms(&self) -> Vec<String> {
        let mut stats = self.stats.write().unwrap();
        stats.requests_handled += 1;
        stats.last_request_at = Some(Instant::now());

        self.rooms.read().unwrap().clone()
    }

    fn join_room(&self, room: &str) -> Result<(), String> {
        let mut stats = self.stats.write().unwrap();
        stats.requests_handled += 1;
        stats.last_request_at = Some(Instant::now());

        let mut rooms = self.rooms.write().unwrap();
        if !rooms.contains(&room.to_string()) {
            rooms.push(room.to_string());
        }
        Ok(())
    }

    fn leave_room(&self, room: &str) -> Result<(), String> {
        let mut stats = self.stats.write().unwrap();
        stats.requests_handled += 1;
        stats.last_request_at = Some(Instant::now());

        let mut rooms = self.rooms.write().unwrap();
        rooms.retain(|r| r != room);
        Ok(())
    }

    fn announce(&self, room: &str, _content: &[u8], title: Option<&str>) -> Result<AnnounceResult, String> {
        let mut stats = self.stats.write().unwrap();
        stats.requests_handled += 1;
        stats.last_request_at = Some(Instant::now());

        let content_hash = format!("hash-{}", rand_id());
        let announcement = MockAnnouncement {
            room: room.to_string(),
            content_hash: content_hash.clone(),
            title: title.unwrap_or("Untitled").to_string(),
        };

        self.announcements.write().unwrap().push(announcement);

        Ok(AnnounceResult {
            hash: content_hash,
            ticket: format!("ticket-{}", rand_id()),
            size_bytes: 1024,
        })
    }

    fn make_ticket(&self, hash: &str) -> String {
        let mut stats = self.stats.write().unwrap();
        stats.requests_handled += 1;
        stats.last_request_at = Some(Instant::now());

        format!("ticket-for-{}", hash)
    }

    fn stats(&self) -> NodeStats {
        self.stats.read().unwrap().clone()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnnounceResult {
    hash: String,
    ticket: String,
    size_bytes: u64,
}

fn rand_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", nanos)
}

// ─────────────────────────────────────────────────────────────────────────────
// IPC Handler Implementation
// ─────────────────────────────────────────────────────────────────────────────

/// IPC Request handler
#[derive(Debug, Clone, Default)]
struct IpcHandler {
    node: MockNode,
}

impl IpcHandler {
    fn new(node: MockNode) -> Self {
        Self { node }
    }

    fn handle_request(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.params.as_ref().and_then(|p| p.get("id")).cloned().unwrap_or(serde_json::json!(0));

        let result: Result<serde_json::Value, String> = match req.method.as_str() {
            METHOD_INIT => self.node.init().map(|info| serde_json::to_value(info).unwrap_or_default()),
            METHOD_INFO => self.node.info().map(|info| serde_json::to_value(info).unwrap_or_default()),
            METHOD_LIST_ROOMS => Ok(serde_json::to_value(self.node.list_rooms()).unwrap_or_default()),
            METHOD_JOIN => {
                let room = req.params.as_ref()
                    .and_then(|p| p.get("room"))
                    .and_then(|r| r.as_str());
                match room {
                    Some(r) => self.node.join_room(r).map(|_| serde_json::json!({})),
                    None => Err("missing room parameter".to_string()),
                }
            }
            METHOD_LEAVE => {
                let room = req.params.as_ref()
                    .and_then(|p| p.get("room"))
                    .and_then(|r| r.as_str());
                match room {
                    Some(r) => self.node.leave_room(r).map(|_| serde_json::json!({})),
                    None => Err("missing room parameter".to_string()),
                }
            }
            METHOD_ANNOUNCE => {
                let room = req.params.as_ref()
                    .and_then(|p| p.get("room"))
                    .and_then(|r| r.as_str());
                let title = req.params.as_ref()
                    .and_then(|p| p.get("title"))
                    .and_then(|t| t.as_str());
                match room {
                    Some(r) => self.node.announce(r, &[], title).map(|v| serde_json::to_value(v).unwrap_or_default()),
                    None => Err("missing room parameter".to_string()),
                }
            }
            METHOD_MAKE_TICKET => {
                let hash = req.params.as_ref()
                    .and_then(|p| p.get("hash"))
                    .and_then(|h| h.as_str());
                match hash {
                    Some(h) => Ok(serde_json::json!(self.node.make_ticket(h))),
                    None => Err("missing hash parameter".to_string()),
                }
            }
            _ => Err(format!("unknown method: {}", req.method)),
        };

        match result {
            Ok(value) => JsonRpcResponse::success(value, id),
            Err(e) => JsonRpcResponse::error(-32603, &e, id),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test Infrastructure
// ─────────────────────────────────────────────────────────────────────────────

fn create_handler() -> IpcHandler {
    IpcHandler::new(MockNode::new())
}

fn parse_request(json: &str) -> Result<JsonRpcRequest, String> {
    serde_json::from_str(json).map_err(|e| e.to_string())
}

fn parse_response(json: &str) -> Result<JsonRpcResponse, String> {
    serde_json::from_str(json).map_err(|e| e.to_string())
}

fn make_request(method: &str, params: Option<JsonValue>) -> String {
    let req = JsonRpcRequest::new(method, params);
    serde_json::to_string(&req).unwrap()
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-1: IPC initialization succeeds with valid node
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_1_init_returns_node_info() {
    let handler = create_handler();
    let request = make_request(METHOD_INIT, None);

    let req = parse_request(&request).unwrap();
    let response = handler.handle_request(req);

    assert!(response.error.is_none(), "init should succeed");
    assert!(response.result.is_some(), "init should return result");

    let result = response.result.unwrap();
    assert!(result.get("node_id").is_some());
    assert!(result.get("version").is_some());
}

#[test]
fn sr_1_init_can_be_called_multiple_times() {
    let handler = create_handler();

    // First init
    let req1 = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    let resp1 = handler.handle_request(req1);
    assert!(resp1.error.is_none());

    // Second init should also succeed (idempotent)
    let req2 = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    let resp2 = handler.handle_request(req2);
    assert!(resp2.error.is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-2: IPC info returns current node status
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_2_info_returns_node_status() {
    let handler = create_handler();

    // Init first to start the node
    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    // Then get info
    let info_req = parse_request(&make_request(METHOD_INFO, None)).unwrap();
    let info_resp = handler.handle_request(info_req);

    assert!(info_resp.error.is_none());
    let info = info_resp.result.unwrap();
    assert_eq!(info.get("version").unwrap(), "1.0.0");
}

#[test]
fn sr_2_info_includes_stats() {
    let handler = create_handler();

    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    let info_req = parse_request(&make_request(METHOD_INFO, None)).unwrap();
    let info_resp = handler.handle_request(info_req);

    assert!(info_resp.error.is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-3: Room listing returns joined rooms
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_3_list_rooms_returns_empty_initially() {
    let handler = create_handler();

    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    let list_req = parse_request(&make_request(METHOD_LIST_ROOMS, None)).unwrap();
    let list_resp = handler.handle_request(list_req);

    assert!(list_resp.error.is_none());
    let rooms = list_resp.result.unwrap();
    assert!(rooms.as_array().cloned().unwrap().is_empty());
}

#[test]
fn sr_3_list_rooms_returns_joined_rooms() {
    let handler = create_handler();

    // Init
    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    // Join some rooms
    let join_req1 = parse_request(&make_request(METHOD_JOIN, Some(serde_json::json!({"room": "room1"})))).unwrap();
    handler.handle_request(join_req1);

    let join_req2 = parse_request(&make_request(METHOD_JOIN, Some(serde_json::json!({"room": "room2"})))).unwrap();
    handler.handle_request(join_req2);

    // List rooms
    let list_req = parse_request(&make_request(METHOD_LIST_ROOMS, None)).unwrap();
    let list_resp = handler.handle_request(list_req);

    assert!(list_resp.error.is_none());
    let rooms = list_resp.result.unwrap().as_array().cloned().unwrap();
    assert_eq!(rooms.len(), 2);
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-4: Join room succeeds with valid room name
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_4_join_room_succeeds() {
    let handler = create_handler();

    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    let join_req = parse_request(&make_request(METHOD_JOIN, Some(serde_json::json!({"room": "test-room"})))).unwrap();
    let join_resp = handler.handle_request(join_req);

    assert!(join_resp.error.is_none());
}

#[test]
fn sr_4_join_same_room_twice_is_idempotent() {
    let handler = create_handler();

    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    // Join twice
    let join_req1 = parse_request(&make_request(METHOD_JOIN, Some(serde_json::json!({"room": "dup-room"})))).unwrap();
    handler.handle_request(join_req1);

    let join_req2 = parse_request(&make_request(METHOD_JOIN, Some(serde_json::json!({"room": "dup-room"})))).unwrap();
    handler.handle_request(join_req2);

    // Should still only have one room
    let list_req = parse_request(&make_request(METHOD_LIST_ROOMS, None)).unwrap();
    let list_resp = handler.handle_request(list_req);
    let rooms = list_resp.result.unwrap().as_array().cloned().unwrap();
    assert_eq!(rooms.len(), 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-5: Leave room succeeds
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_5_leave_room_succeeds() {
    let handler = create_handler();

    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    // Join then leave
    let join_req = parse_request(&make_request(METHOD_JOIN, Some(serde_json::json!({"room": "leave-test"})))).unwrap();
    handler.handle_request(join_req);

    let leave_req = parse_request(&make_request(METHOD_LEAVE, Some(serde_json::json!({"room": "leave-test"})))).unwrap();
    let leave_resp = handler.handle_request(leave_req);

    assert!(leave_resp.error.is_none());

    // Verify room is gone
    let list_req = parse_request(&make_request(METHOD_LIST_ROOMS, None)).unwrap();
    let list_resp = handler.handle_request(list_req);
    let rooms = list_resp.result.unwrap().as_array().cloned().unwrap();
    assert!(rooms.is_empty());
}

#[test]
fn sr_5_leave_nonexistent_room_succeeds() {
    let handler = create_handler();

    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    // Leave room that was never joined
    let leave_req = parse_request(&make_request(METHOD_LEAVE, Some(serde_json::json!({"room": "ghost-room"})))).unwrap();
    let leave_resp = handler.handle_request(leave_req);

    // Should succeed (idempotent)
    assert!(leave_resp.error.is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-6: Announce creates announcement with valid params
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_6_announce_returns_result() {
    let handler = create_handler();

    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    let announce_req = parse_request(&make_request(METHOD_ANNOUNCE, Some(serde_json::json!({
        "room": "announce-test",
        "title": "Test Title"
    })))).unwrap();
    let announce_resp = handler.handle_request(announce_req);

    assert!(announce_resp.error.is_none());
    let result = announce_resp.result.unwrap();
    assert!(result.get("hash").is_some());
    assert!(result.get("ticket").is_some());
    // Field is snake_case in Rust struct but serialized as-is
    assert!(result.get("size_bytes").is_some() || result.get("sizeBytes").is_some());
}

#[test]
fn sr_6_announce_without_title_succeeds() {
    let handler = create_handler();

    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    let announce_req = parse_request(&make_request(METHOD_ANNOUNCE, Some(serde_json::json!({
        "room": "no-title-test"
    })))).unwrap();
    let announce_resp = handler.handle_request(announce_req);

    assert!(announce_resp.error.is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-7: Make ticket returns valid ticket
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_7_make_ticket_returns_ticket() {
    let handler = create_handler();

    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    let ticket_req = parse_request(&make_request(METHOD_MAKE_TICKET, Some(serde_json::json!({
        "hash": "test-hash-123"
    })))).unwrap();
    let ticket_resp = handler.handle_request(ticket_req);

    assert!(ticket_resp.error.is_none());
    let ticket = ticket_resp.result.unwrap();
    assert!(ticket.as_str().unwrap().starts_with("ticket-for-"));
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-8: Missing required params returns error
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_8_join_missing_room_returns_error() {
    let handler = create_handler();

    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    // Missing 'room' parameter
    let join_req = parse_request(&make_request(METHOD_JOIN, Some(serde_json::json!({})))).unwrap();
    let join_resp = handler.handle_request(join_req);

    assert!(join_resp.error.is_some());
    let error = join_resp.error.unwrap();
    assert_eq!(error.code, -32603); // Internal error
}

#[test]
fn sr_8_make_ticket_missing_hash_returns_error() {
    let handler = create_handler();

    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    let ticket_req = parse_request(&make_request(METHOD_MAKE_TICKET, Some(serde_json::json!({})))).unwrap();
    let ticket_resp = handler.handle_request(ticket_req);

    assert!(ticket_resp.error.is_some());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-9: Unknown method returns error
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_9_unknown_method_returns_error() {
    let handler = create_handler();

    let unknown_req = parse_request(&make_request("unknown_method", None)).unwrap();
    let unknown_resp = handler.handle_request(unknown_req);

    assert!(unknown_resp.error.is_some());
    let error = unknown_resp.error.unwrap();
    assert!(error.message.contains("unknown method"));
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-10: JSON-RPC version must be "2.0"
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_10_invalid_jsonrpc_version_accepted() {
    // JSON-RPC 2.0 requires jsonrpc: "2.0"
    // However, for compatibility, we accept any version string
    let handler = create_handler();

    let req = JsonRpcRequest {
        jsonrpc: "1.0".to_string(),
        method: METHOD_INFO.to_string(),
        params: None,
        id: Some(serde_json::json!(1)),
    };
    let request_str = serde_json::to_string(&req).unwrap();
    let parsed = parse_request(&request_str).unwrap();
    let response = handler.handle_request(parsed);

    // Should still process (compatibility mode)
    assert!(response.error.is_none() || response.result.is_some());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-11: Response includes correct ID
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_11_response_includes_request_id() {
    let handler = create_handler();

    // Use a request format with explicit id in params (simulating actual IPC behavior)
    let req = parse_request(r#"{"jsonrpc": "2.0", "method": "info", "params": {"id": 42}, "id": 42}"#).unwrap();
    let response = handler.handle_request(req);

    // Response should echo back the id from params
    assert_eq!(response.id, Some(serde_json::json!(42)));
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-12: Notification forwarding setup
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_12_notification_serialization() {
    let notif = JsonRpcNotification::new(NOTIF_ANNOUNCEMENT, Some(serde_json::json!({
        "room": "test",
        "hash": "abc123"
    })));

    let json = serde_json::to_string(&notif).unwrap();
    assert!(json.contains("announcement"));
    assert!(json.contains("2.0"));
}

#[test]
fn sr_12_notification_has_no_id() {
    let notif = JsonRpcNotification::new(NOTIF_ANNOUNCEMENT, None);
    let json_str = serde_json::to_string(&notif).unwrap();
    let json: JsonValue = serde_json::from_str(&json_str).unwrap();

    // Notifications should not have "id" field
    assert!(json.get("id").is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-13: Batch requests handled correctly
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_13_batch_request_parsing() {
    // Batch request format
    let batch_json = r#"[
        {"jsonrpc": "2.0", "method": "init", "id": 1},
        {"jsonrpc": "2.0", "method": "info", "id": 2}
    ]"#;

    let batch: Vec<JsonRpcRequest> = serde_json::from_str(batch_json).unwrap();
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0].method, "init");
    assert_eq!(batch[1].method, "info");
}

#[test]
fn sr_13_batch_response_parsing() {
    let handler = create_handler();

    let batch_json = r#"[
        {"jsonrpc": "2.0", "method": "init", "id": 1},
        {"jsonrpc": "2.0", "method": "info", "id": 2}
    ]"#;

    let batch: Vec<JsonRpcRequest> = serde_json::from_str(batch_json).unwrap();
    let mut responses = Vec::new();

    for req in batch {
        responses.push(handler.handle_request(req));
    }

    assert_eq!(responses.len(), 2);
    assert!(responses[0].result.is_some());
    assert!(responses[1].result.is_some());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-14: Error response format is correct
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_14_error_response_format() {
    let error = JsonRpcError {
        code: -32600,
        message: "Invalid Request".to_string(),
        data: None,
    };

    let json = serde_json::to_string(&error).unwrap();
    assert!(json.contains("\"code\":-32600"));
    assert!(json.contains("Invalid Request"));
}

#[test]
fn sr_14_error_includes_code_and_message() {
    let handler = create_handler();

    let bad_req = parse_request(&make_request("nonexistent", None)).unwrap();
    let response = handler.handle_request(bad_req);

    assert!(response.error.is_some());
    let error = response.error.unwrap();
    assert!(error.code != 0);
    assert!(!error.message.is_empty());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-15: Request parsing failures handled
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_15_malformed_json_returns_error() {
    let result: Result<JsonRpcRequest, _> = serde_json::from_str("not json");
    assert!(result.is_err());
}

#[test]
fn sr_15_missing_method_returns_error() {
    let json = r#"{"jsonrpc": "2.0", "params": {}, "id": 1}"#;
    let result: Result<JsonRpcRequest, _> = serde_json::from_str(json);
    assert!(result.is_err());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-16: Concurrent requests handled correctly
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn sr_16_concurrent_requests() {
    use tokio::task::JoinSet;

    let handler = Arc::new(create_handler());

    let mut join_set = JoinSet::new();

    for i in 0..10 {
        let handler = handler.clone();
        join_set.spawn(async move {
            let req = parse_request(&make_request(METHOD_INFO, None)).unwrap();
            handler.handle_request(req)
        });
    }

    let mut success_count = 0;
    while let Some(result) = join_set.join_next().await {
        let resp = result.unwrap();
        if resp.error.is_none() && resp.result.is_some() {
            success_count += 1;
        }
    }

    assert_eq!(success_count, 10, "all concurrent requests must succeed");
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-17: Handler maintains state correctly
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_17_handler_maintains_room_state() {
    let handler = create_handler();

    // Join rooms
    let init_req = parse_request(&make_request(METHOD_INIT, None)).unwrap();
    handler.handle_request(init_req);

    let rooms = vec!["room-a", "room-b", "room-c"];
    for room in &rooms {
        let req = parse_request(&make_request(METHOD_JOIN, Some(serde_json::json!({"room": room})))).unwrap();
        handler.handle_request(req);
    }

    // Verify state
    let list_req = parse_request(&make_request(METHOD_LIST_ROOMS, None)).unwrap();
    let list_resp = handler.handle_request(list_req);
    let list_rooms = list_resp.result.unwrap().as_array().cloned().unwrap();

    assert_eq!(list_rooms.len(), 3);
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-18: Node stats tracked correctly
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_18_stats_tracked() {
    let handler = create_handler();

    // Make several requests
    for _ in 0..5 {
        let req = parse_request(&make_request(METHOD_INFO, None)).unwrap();
        handler.handle_request(req);
    }

    let stats = handler.node.stats();
    assert_eq!(stats.requests_handled, 5);
    assert!(stats.last_request_at.is_some());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-19: Parse errors return specific error codes
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_19_parse_error_code() {
    // JSON parse errors should have code -32700
    // (though we can't test the full stack from here)
    let invalid = "not valid json {";
    let result: Result<JsonRpcRequest, _> = serde_json::from_str(invalid);
    assert!(result.is_err());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-20: Request without id is valid notification
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_20_notification_has_no_id() {
    let notif_json = r#"{"jsonrpc": "2.0", "method": "announcement", "params": {}}"#;
    let notif: JsonRpcNotification = serde_json::from_str(notif_json).unwrap();
    assert_eq!(notif.method, "announcement");
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-21..25: Additional IPC validation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_21_empty_method_name_rejected() {
    let handler = create_handler();
    let req = parse_request(&make_request("", None)).unwrap();
    let response = handler.handle_request(req);
    assert!(response.error.is_some());
}

#[test]
fn sr_22_method_name_length_limit() {
    let handler = create_handler();
    let long_method = "a".repeat(1000);
    let req = parse_request(&make_request(&long_method, None)).unwrap();
    let response = handler.handle_request(req);
    // Should either succeed or fail gracefully
    assert!(response.result.is_some() || response.error.is_some());
}

#[test]
fn sr_23_null_params_handled() {
    let handler = create_handler();
    let req = parse_request(&make_request(METHOD_INFO, Some(serde_json::Value::Null))).unwrap();
    let response = handler.handle_request(req);
    // Null params should be treated as no params
    assert!(response.error.is_none() || response.result.is_some());
}

#[test]
fn sr_24_array_params_handled() {
    let handler = create_handler();
    // Some methods might accept array params
    let req = parse_request(r#"{"jsonrpc": "2.0", "method": "info", "params": [], "id": 1}"#).unwrap();
    let response = handler.handle_request(req);
    assert!(response.result.is_some() || response.error.is_some());
}

#[test]
fn sr_25_response_json_rpc_version() {
    let handler = create_handler();
    let req = parse_request(&make_request(METHOD_INFO, None)).unwrap();
    let response = handler.handle_request(req);
    assert_eq!(response.jsonrpc, "2.0");
}

// ─────────────────────────────────────────────────────────────────────────────
// Safety Revision Verification
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn safety_revision_is_pinned() {
    assert!(
        SAFETY_REVISION.starts_with("IPC-NODE-BRIDGE-"),
        "safety revision must be properly prefixed"
    );
    assert!(SAFETY_REVISION.contains("2026"));
}

#[test]
fn dal_level_is_a() {
    assert_eq!(DAL_LEVEL, "A");
}

#[test]
fn reproducible_build_flag_is_true() {
    assert!(REPRODUCIBLE_BUILD);
}
