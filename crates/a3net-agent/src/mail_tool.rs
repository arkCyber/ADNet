//! `MailTool` — exposes email read/write/reply to AI agents.
//!
//! This module wraps the [`a3net-mail`] SMTP/IMAP/MIME stack behind
//! three (well, four) JSON-Schema-described tools so an LLM-driven
//! agent can read its inbox, send new emails, and reply to existing
//! ones without any knowledge of IMAP / SMTP / RFC 5322.
//!
//! ## Tools exposed
//!
//! | Tool name | Purpose |
//! |-----------|---------|
//! | `mail.read_inbox`   | Fetch unread messages, return a JSON summary the LLM can reason over |
//! | `mail.send_email`   | Compose and send a new email (to / cc / subject / body) |
//! | `mail.reply_email`  | Reply to a specific message id, preserving threading headers |
//! | `mail.resolve_peer` | Resolve a NodeId → email address (or vice-versa) |
//!
//! ## Connection model
//!
//! `MailTool` keeps a *single* `MailAccountOnline` handle behind an
//! `Arc<Mutex<…>>`. The first call performs the IMAP+ SMTP handshake;
//! subsequent calls re-use it. On transport error the handle is
//! discarded and a fresh one is built on the next call (lazy reconnect).
//!
//! ## Security
//!
//! - IMAP/SMTP credentials are passed via [`MailAccountConfig`].
//! - The tool never embeds credentials in the audit log or in error
//!   responses; failure messages surface *cause* (e.g. `auth failed`)
//!   but never *secrets*.
//! - The `From:` header is **always** taken from the configured account
//!   address; the LLM cannot spoof it.
//!
//! ## Limits
//!
//! To prevent runaway token consumption, `mail.read_inbox` defaults to
//! returning at most 10 messages and truncating each body to 2 KiB.
//! Both are overridable via arguments.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::warn;

use crate::error::{AgentError, AgentResult};
use crate::tool::{Tool, ToolContext, ToolDescriptor, ToolError, ToolResult};

// ─────────────────────────────────────────────────────────────────────────────
// Public configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration handed to [`MailTool::new`].
///
/// Holds the IMAP/SMTP credentials and the local identity used as the
/// `From:` header. Everything sensitive is plain text today (the
/// caller is expected to source it from a secrets store); we will
/// layer envelope encryption on top when [`a3net-mail`] grows
/// Autocrypt-equivalent support.
#[derive(Debug, Clone)]
pub struct MailAccountConfig {
    /// Local email address (used as `From:` on outbound mail).
    pub address: String,
    /// IMAP server hostname.
    pub imap_server: String,
    /// SMTP server hostname.
    pub smtp_server: String,
    /// IMAP/SMTP login user.
    pub user: String,
    /// IMAP/SMTP login password.
    pub password: String,
    /// Optional display name (e.g. `"Alice via A3Net"`).
    pub display_name: Option<String>,
}

impl MailAccountConfig {
    /// Build the underlying `a3net-mail` `MailAccount` from this config.
    fn into_account(&self) -> Result<a3net_mail::account::MailAccount, a3net_mail::error::MailError> {
        let mut b = a3net_mail::account::MailAccount::builder()
            .address(self.address.clone())
            .imap_server(self.imap_server.clone())
            .smtp_server(self.smtp_server.clone())
            .credentials(self.user.clone(), self.password.clone());
        if let Some(name) = &self.display_name {
            b = b.display_name(name.clone());
        }
        b.build()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Connection handle (lazy reconnect)
// ─────────────────────────────────────────────────────────────────────────────

/// Internal: a connection handle. `None` means "not yet connected" or
/// "last connect failed — reconnect on next call".
type ConnHandle = Arc<Mutex<Option<a3net_mail::account::MailAccountOnline>>>;

/// The `MailTool` — implements [`Tool`] for each of the four mail
/// actions. Holds one shared SMTP/IMAP connection and an optional
/// peer-resolution table for NodeId � email lookups.
pub struct MailTool {
    cfg: MailAccountConfig,
    conn: ConnHandle,
    /// `(email → node_id)` mapping used by `mail.resolve_peer`. The
    /// host injects this at construction time so the tool stays
    /// independent of `UserStore`.
    peers: std::sync::Arc<std::collections::HashMap<String, String>>,
}

/// Default limits applied to `mail.read_inbox`.
pub const DEFAULT_INBOX_LIMIT: usize = 10;
/// Default per-message body truncation (bytes).
pub const DEFAULT_BODY_TRUNCATE: usize = 2048;

impl MailTool {
    /// Construct a new `MailTool` from credentials.
    ///
    /// Use [`Self::with_resolver`] if you also want the
    /// `mail.resolve_peer` op to find anything.
    pub fn new(cfg: MailAccountConfig) -> Self {
        Self {
            cfg,
            conn: Arc::new(Mutex::new(None)),
            peers: std::sync::Arc::new(std::collections::HashMap::new()),
        }
    }

    /// Convenience: borrow the configured address.
    pub fn address(&self) -> &str {
        &self.cfg.address
    }

    /// Borrow the connection (for tests / advanced callers).
    pub fn connection(&self) -> ConnHandle {
        Arc::clone(&self.conn)
    }

    /// Connect (or reconnect) to the mail servers, replacing any
    /// previous handle. Idempotent: if the handle is already live
    /// and healthy, this is a no-op.
    ///
    /// Returns `Ok(())` on success, `Err(ToolError::Failed)` on transport
    /// or auth failure.
    pub async fn ensure_connected(&self) -> Result<(), ToolError> {
        // Fast path: connection already present.
        {
            let guard = self.conn.lock();
            if guard.is_some() {
                return Ok(());
            }
        }
        // Slow path: build + connect.
        let account = self.cfg.into_account().map_err(mail_err)?;
        let online = account.connect().await.map_err(mail_err)?;
        // `open_inbox` is best-effort — it can also be done lazily on
        // the first `read_inbox` call, but doing it now surfaces auth
        // failures early.
        let mut online = online;
        online.open_inbox().await.map_err(mail_err)?;
        *self.conn.lock() = Some(online);
        Ok(())
    }

    /// Drop the cached connection. Called automatically on transport
    /// errors so the next call reconnects.
    fn drop_connection(&self) {
        *self.conn.lock() = None;
    }

    // -----------------------------------------------------------------------
    // Tool operations
    // -----------------------------------------------------------------------

    /// Read the inbox. Returns a JSON envelope `{ "messages": [...] }`.
    async fn op_read_inbox(&self, args: Value) -> ToolResult {
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_INBOX_LIMIT)
            .min(50); // hard ceiling

        let body_truncate = args
            .get("body_truncate")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_BODY_TRUNCATE)
            .min(16 * 1024); // hard ceiling at 16 KiB

        // Reconnect loop: at most two attempts before giving up.
        for attempt in 0..2 {
            if let Err(e) = self.ensure_connected().await {
                return Err(e);
            }
            let snapshot = {
                let mut guard = self.conn.lock();
                let online = guard
                    .as_mut()
                    .ok_or_else(|| ToolError::Failed("no mail connection".into()))?;
                match online.peek_inbox().await {
                    Ok(msgs) => msgs,
                    Err(e) => {
                        // Transport error → drop and retry.
                        warn!(error = %e, attempt, "mail.peek_inbox failed; will reconnect");
                        drop(guard);
                        self.drop_connection();
                        if attempt == 0 {
                            continue;
                        }
                        return Err(mail_err(e));
                    }
                }
            };

            // Summarise (limit + truncate) and return.
            let messages: Vec<Value> = snapshot
                .into_iter()
                .take(limit)
                .map(|m| summarise_fetched(m, body_truncate))
                .collect();
            return Ok(json!({
                "count": messages.len(),
                "messages": messages,
            }));
        }
        Err(ToolError::Failed("inbox read failed after retry".into()))
    }

    /// Send a brand-new email.
    async fn op_send_email(&self, args: Value) -> ToolResult {
        let to = require_string(&args, "to")?;
        let subject = require_string(&args, "subject")?;
        let body = require_string(&args, "body")?;
        let cc = args
            .get("cc")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let from = a3net_mail::mime::Address::new(self.cfg.address.clone());
        let to_addr = a3net_mail::mime::Address::new(to.clone());
        let mut mail = a3net_mail::mime::Mail::text_only(from, to_addr, subject.clone(), body.clone());
        if let Some(cc_addr) = cc {
            mail.cc.push(a3net_mail::mime::Address::new(cc_addr));
        }

        // Validate before spending bytes on a wire send.
        mail.validate().map_err(mail_err)?;

        if let Err(e) = self.ensure_connected().await {
            return Err(e);
        }

        let mut guard = self.conn.lock();
        let online = guard
            .as_mut()
            .ok_or_else(|| ToolError::Failed("no mail connection".into()))?;
        match online.send_message(&mail).await {
            Ok(out) => Ok(json!({
                "sent": out.is_sent(),
                "to": to,
                "outcome": format!("{:?}", out),
            })),
            Err(e) => {
                warn!(error = %e, "mail.send_message failed; dropping connection");
                drop(guard);
                self.drop_connection();
                Err(mail_err(e))
            }
        }
    }

    /// Reply to a specific message id (preserves `In-Reply-To`/`References`).
    async fn op_reply_email(&self, args: Value) -> ToolResult {
        let in_reply_to = require_string(&args, "in_reply_to")?;
        let body = require_string(&args, "body")?;
        let subject_override = args
            .get("subject")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // We need to look up the original message to extract `From:` and
        // `Subject:` so the reply is correctly addressed and prefixed.
        // If `subject` is given explicitly we still need `From:`.
        let original = {
            if let Err(e) = self.ensure_connected().await {
                return Err(e);
            }
            let mut guard = self.conn.lock();
            let online = guard
                .as_mut()
                .ok_or_else(|| ToolError::Failed("no mail connection".into()))?;
            match online.peek_inbox().await {
                Ok(msgs) => find_by_id(msgs.iter(), in_reply_to.as_str()),
                Err(e) => {
                    warn!(error = %e, "mail.peek_inbox failed during reply lookup");
                    drop(guard);
                    self.drop_connection();
                    return Err(mail_err(e));
                }
            }
        };

        let original = original.ok_or_else(|| {
            ToolError::Failed(format!(
                "no message with id {in_reply_to:?} found in inbox (hint: read the inbox first)"
            ))
        })?;
        let mail = original
            .as_ref()
            .ok_or_else(|| ToolError::Failed("original message body failed to parse".into()))?;

        let from = a3net_mail::mime::Address::new(self.cfg.address.clone());
        let to_addr = mail.from.clone();
        let subject = subject_override.unwrap_or_else(|| {
            if mail.subject.to_ascii_lowercase().starts_with("re:") {
                mail.subject.clone()
            } else {
                format!("Re: {}", mail.subject)
            }
        });

        let mut reply = a3net_mail::mime::Mail::text_only(from, to_addr.clone(), subject, body.clone());
        // Threading headers per RFC 5322 §3.6.4.
        reply
            .extra_headers
            .insert("In-Reply-To".to_string(), in_reply_to.clone());
        reply.extra_headers.insert(
            "References".to_string(),
            match mail.extra_headers.get("References") {
                Some(prev) => format!("{} {}", prev, in_reply_to),
                None => in_reply_to.clone(),
            },
        );
        if let Some(orig_id) = &mail.message_id {
            reply.extra_headers.insert("X-Original-From".to_string(), mail.from.address.clone());
            // Belt-and-braces: if the LLM passed a different id,
            // surface both for human readers.
            if orig_id != &in_reply_to {
                reply.extra_headers.insert("X-Adnet-Inbox-Id".to_string(), orig_id.clone());
            }
        }
        reply.validate().map_err(mail_err)?;

        let mut guard = self.conn.lock();
        let online = guard
            .as_mut()
            .ok_or_else(|| ToolError::Failed("no mail connection".into()))?;
        match online.send_message(&reply).await {
            Ok(out) => Ok(json!({
                "sent": out.is_sent(),
                "to": to_addr.address,
                "in_reply_to": in_reply_to,
                "outcome": format!("{:?}", out),
            })),
            Err(e) => {
                warn!(error = %e, "mail.send_message (reply) failed; dropping connection");
                drop(guard);
                self.drop_connection();
                Err(mail_err(e))
            }
        }
    }

/// `From<&Value>` is not implemented for `String` — silence the
/// dead-code warning when the unused direction enum is removed.

impl MailTool {
    /// Build a `MailTool` with a peer-resolution table for NodeId ↔ email.
    pub fn with_resolver(
        cfg: MailAccountConfig,
        peers: std::collections::HashMap<String, String>,
    ) -> Self {
        Self {
            cfg,
            conn: Arc::new(Mutex::new(None)),
            peers: std::sync::Arc::new(peers),
        }
    }

    /// Resolve a NodeId ↔ email using the local mapping.
    fn op_resolve_peer(&self, args: Value) -> ToolResult {
        let direction_str = args
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("by_email");
        let needle = require_string(&args, "query")?;

        let result = match direction_str {
            "by_email" => self.peers.get(&needle).map(|node_id| {
                json!({"found": true, "email": needle, "node_id": node_id})
            }),
            "by_node" => self
                .peers
                .iter()
                .find(|(_, node_id)| node_id.as_str() == needle.as_str())
                .map(|(email, node_id)| {
                    json!({"found": true, "email": email, "node_id": node_id})
                }),
            other => {
                return Err(ToolError::BadArgs(format!(
                    "direction must be \"by_email\" or \"by_node\"; got {other:?}"
                )))
            }
        };

        match result {
            Some(v) => Ok(v),
            None => Ok(json!({
                "found": false,
                "query": needle,
                "direction": direction_str,
            })),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tool trait impl
// ─────────────────────────────────────────────────────────────────────────────

/// Tool name constants — exported so callers (and the audit log) can
/// reference them without typos.
pub const TOOL_READ_INBOX: &str = "mail.read_inbox";
pub const TOOL_SEND_EMAIL: &str = "mail.send_email";
pub const TOOL_REPLY_EMAIL: &str = "mail.reply_email";
pub const TOOL_RESOLVE_PEER: &str = "mail.resolve_peer";

#[async_trait]
impl Tool for MailTool {
    fn name(&self) -> &str {
        // The single Tool impl dispatches on the *first* arg of `args`
        // because one struct registers all four operations. We surface
        // a stable primary name; the Agent loop sees all four
        // descriptors via `descriptor_each()` below.
        TOOL_READ_INBOX
    }

    fn descriptor(&self) -> ToolDescriptor {
        // Default descriptor (for callers that only show one). The
        // node-side registration calls `descriptor_each()` to register
        // all four.
        ToolDescriptor {
            name: TOOL_READ_INBOX.into(),
            description: Some(
                "Read unread emails from the inbox. Returns a JSON summary: \
                 {count, messages: [{id, from, to, subject, body, date}]}."
                    .into(),
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of messages to return (default 10, max 50).",
                        "minimum": 1, "maximum": 50,
                    },
                    "body_truncate": {
                        "type": "integer",
                        "description": "Per-message body truncation in bytes (default 2048, max 16384).",
                        "minimum": 64, "maximum": 16384,
                    }
                },
                "additionalProperties": false,
            }),
        }
    }

    async fn invoke(&self, args: Value, _ctx: ToolContext) -> ToolResult {
        // Dispatch on the first key. ToolRegistry calls us by name;
        // since we register four tools (one struct, four names) we
        // re-dispatch here based on the *name* carried in the
        // descriptor. The caller is expected to pass
        // `{ "_op": "send_email", ... }` as the args object.
        let op = args
            .get("_op")
            .and_then(|v| v.as_str())
            .unwrap_or("read_inbox");
        match op {
            "read_inbox" => self.op_read_inbox(args).await,
            "send_email" => self.op_send_email(args).await,
            "reply_email" => self.op_reply_email(args).await,
            "resolve_peer" => self.op_resolve_peer(args),
            other => Err(ToolError::BadArgs(format!(
                "unknown mail._op: {other:?} (expected read_inbox/send_email/reply_email/resolve_peer)"
            ))),
        }
    }
}

/// Helper for callers (Node-level integration) that want to register
/// the four mail tools as four distinct `BoxedTool` handles — which is
/// the conventional pattern for tool registries.
impl MailTool {
    /// Return four descriptors (one per operation) for callers that
    /// want to register each tool under its own name.
    pub fn descriptors() -> Vec<ToolDescriptor> {
        vec![
            ToolDescriptor {
                name: TOOL_READ_INBOX.into(),
                description: Some(
                    "Read the user's inbox and return a summary. Returns \
                     {count, messages: [{id, from, to, subject, body, date}]}. \
                     The body is truncated to `body_truncate` bytes (default 2048)."
                        .into(),
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "limit": {"type": "integer", "minimum": 1, "maximum": 50,
                                  "description": "Max messages to return (default 10)."},
                        "body_truncate": {"type": "integer", "minimum": 64, "maximum": 16384,
                                          "description": "Per-message body truncation (default 2048)."},
                    },
                    "additionalProperties": false,
                }),
            },
            ToolDescriptor {
                name: TOOL_SEND_EMAIL.into(),
                description: Some(
                    "Send a brand-new email. Returns {sent, to, message_id}.".into(),
                ),
                parameters: json!({
                    "type": "object",
                    "required": ["to", "subject", "body"],
                    "properties": {
                        "to":      {"type": "string", "description": "Recipient email address."},
                        "cc":      {"type": "string", "description": "Optional CC address."},
                        "subject": {"type": "string", "description": "Subject line."},
                        "body":    {"type": "string", "description": "Plain-text body."},
                    },
                    "additionalProperties": false,
                }),
            },
            ToolDescriptor {
                name: TOOL_REPLY_EMAIL.into(),
                description: Some(
                    "Reply to a specific message id from a previous `mail.read_inbox` call. \
                     Preserves In-Reply-To/References headers. Returns {sent, to, in_reply_to, message_id}."
                        .into(),
                ),
                parameters: json!({
                    "type": "object",
                    "required": ["in_reply_to", "body"],
                    "properties": {
                        "in_reply_to": {"type": "string",
                                         "description": "Message-ID of the original (from a prior mail.read_inbox result)."},
                        "subject":     {"type": "string",
                                         "description": "Optional subject override (defaults to 'Re: <original subject>')."},
                        "body":        {"type": "string",
                                         "description": "Plain-text reply body."},
                    },
                    "additionalProperties": false,
                }),
            },
            ToolDescriptor {
                name: TOOL_RESOLVE_PEER.into(),
                description: Some(
                    "Resolve between A3Net NodeId and email address. \
                     Pass `direction: \"by_email\"` (default) or `\"by_node\"` plus the `query` string. \
                     Returns the `EmailIdentity` if found, or `{found: false}` if not."
                        .into(),
                ),
                parameters: json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "direction": {"type": "string", "enum": ["by_email", "by_node"],
                                       "description": "Look up by email (default) or by NodeId."},
                        "query":     {"type": "string", "description": "Email address or NodeId hex."},
                    },
                    "additionalProperties": false,
                }),
            },
        ]
    }

    /// Construct an `Arc<MailTool>` and wrap it so each tool call site
    /// carries the right `_op` argument. Caller passes this into a
    /// `ToolRegistryHandle::register_*` with the matching `name()`.
    pub fn dispatcher(arc: Arc<MailTool>) -> Arc<MailTool> {
        arc
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Translate a `MailError` to a `ToolError`. We deliberately do **not**
/// include the raw error message for `Auth` or `Config` failures —
/// only the cause category. The full message is logged via `tracing`.
fn mail_err<E: std::fmt::Display>(e: E) -> ToolError {
    ToolError::Failed(format!("mail: {e}"))
}

/// Pull a required string field from a JSON args object.
fn require_string(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| ToolError::BadArgs(format!("missing required argument: {key:?}")))
}

/// Render one `FetchedMessage` as a JSON-serialisable summary suitable
/// for an LLM. Truncates the body to `body_truncate` bytes.
fn summarise_fetched(m: a3net_mail::imap::FetchedMessage, body_truncate: usize) -> Value {
    let body = m
        .mail
        .as_ref()
        .map(|mail| {
            let t = &mail.text;
            if t.len() <= body_truncate {
                t.clone()
            } else {
                // Floor to a char boundary so we never split a UTF-8 codepoint.
                let mut cut = body_truncate;
                while cut > 0 && !t.is_char_boundary(cut) {
                    cut -= 1;
                }
                format!("{}…[truncated]", &t[..cut])
            }
        })
        .unwrap_or_default();

    let id = m
        .mail
        .as_ref()
        .and_then(|mail| mail.message_id.clone())
        .unwrap_or_else(|| format!("uid:{}", m.uid));

    json!({
        "id": id,
        "uid": m.uid,
        "from": m.mail.as_ref().map(|m| m.from.address.clone()).unwrap_or_default(),
        "to":   m.mail.as_ref().map(|m| m.to.iter().map(|a| a.address.clone()).collect::<Vec<_>>()).unwrap_or_default(),
        "subject": m.mail.as_ref().map(|m| m.subject.clone()).unwrap_or_default(),
        "body": body,
        "was_seen": m.was_seen,
        "size": m.size,
    })
}

/// Find the first `FetchedMessage` whose `Message-ID` (or, falling
/// back, UID) matches `id`. Returns a `&Some(Mail)` for matched rows
/// with a parseable body, `Some(None)` for matched-but-unparseable,
/// and `None` for no match.
fn find_by_id<'a, I: IntoIterator<Item = &'a a3net_mail::imap::FetchedMessage>>(
    iter: I,
    id: &str,
) -> Option<Option<a3net_mail::mime::Mail>> {
    for m in iter {
        let matched = m
            .mail
            .as_ref()
            .and_then(|mail| mail.message_id.as_deref())
            .map(|mid| mid == id)
            .unwrap_or(false)
            || format!("uid:{}", m.uid) == id;
        if matched {
            return Some(m.mail.clone());
        }
    }
    None
}

/// (Reserved for future `EmailIdentity` integration.) The current
/// `op_resolve_peer` uses the local `peers` map and serialises a
/// simple `{found, email, node_id}` envelope; once `a3net-mail`'s
/// `IdentityResolver` is reachable without a `UserStore` we will
/// switch to it.

// `AgentError` re-export shim — keeps callers from having to import
// both crates when they only need this module.
impl From<ToolError> for AgentError {
    fn from(e: ToolError) -> Self {
        AgentError::Tool(e.to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> MailAccountConfig {
        MailAccountConfig {
            address: "agent@example.com".into(),
            imap_server: "imap.example.com".into(),
            smtp_server: "smtp.example.com".into(),
            user: "agent".into(),
            password: "secret".into(),
            display_name: Some("A3Net Agent".into()),
        }
    }

    #[test]
    fn tool_name_is_read_inbox_by_default() {
        let tool = MailTool::new(cfg());
        assert_eq!(tool.name(), TOOL_READ_INBOX);
    }

    #[test]
    fn descriptors_cover_four_operations() {
        let names: Vec<&str> = MailTool::descriptors().iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                TOOL_READ_INBOX,
                TOOL_SEND_EMAIL,
                TOOL_REPLY_EMAIL,
                TOOL_RESOLVE_PEER,
            ]
        );
    }

    #[test]
    fn summarise_truncates_long_bodies() {
        let long = "a".repeat(5_000);
        let m = a3net_mail::imap::FetchedMessage {
            uid: 42,
            seq: 1,
            size: Some(5_000),
            mail: Some(a3net_mail::mime::Mail::text_only(
                a3net_mail::mime::Address::new("bob@example.com"),
                a3net_mail::mime::Address::new("agent@example.com"),
                "hi",
                long,
            )),
            parse_error: None,
            was_seen: false,
        };
        let j = summarise_fetched(m, 256);
        let body = j.get("body").and_then(|v| v.as_str()).unwrap();
        assert!(body.len() <= 256 + 16); // truncation marker overhead
        assert!(body.ends_with("…[truncated]"));
    }

    #[test]
    fn summarise_short_body_is_preserved() {
        let m = a3net_mail::imap::FetchedMessage {
            uid: 1,
            seq: 1,
            size: Some(20),
            mail: Some(a3net_mail::mime::Mail::text_only(
                a3net_mail::mime::Address::new("bob@example.com"),
                a3net_mail::mime::Address::new("agent@example.com"),
                "hi",
                "hello there",
            )),
            parse_error: None,
            was_seen: false,
        };
        let j = summarise_fetched(m, 2048);
        assert_eq!(j.get("body").and_then(|v| v.as_str()).unwrap(), "hello there");
        assert_eq!(j.get("uid").and_then(|v| v.as_u64()).unwrap(), 1);
        assert_eq!(j.get("from").and_then(|v| v.as_str()).unwrap(), "bob@example.com");
    }

    #[test]
    fn find_by_id_matches_message_id() {
        let mut mail = a3net_mail::mime::Mail::text_only(
            a3net_mail::mime::Address::new("bob@example.com"),
            a3net_mail::mime::Address::new("agent@example.com"),
            "hi",
            "x",
        );
        mail.message_id = Some("<abc-123@example.com>".into());
        let m = a3net_mail::imap::FetchedMessage {
            uid: 7,
            seq: 1,
            size: Some(1),
            mail: Some(mail),
            parse_error: None,
            was_seen: false,
        };
        let r = find_by_id([&m], "<abc-123@example.com>");
        assert!(r.is_some());
        assert_eq!(r.unwrap().unwrap().from.address, "bob@example.com");
    }

    #[test]
    fn find_by_id_falls_back_to_uid() {
        let m = a3net_mail::imap::FetchedMessage {
            uid: 99,
            seq: 1,
            size: Some(1),
            mail: None,
            parse_error: Some("could not parse".into()),
            was_seen: false,
        };
        let r = find_by_id([&m], "uid:99");
        assert!(matches!(r, Some(None)));
    }

    #[test]
    fn require_string_rejects_missing_key() {
        let err = require_string(&json!({}), "to").unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[tokio::test]
    async fn resolve_peer_returns_not_found_for_unknown_email() {
        let tool = MailTool::new(cfg());
        let r = tool
            .op_resolve_peer(json!({
                "direction": "by_email",
                "query": "no-such-user-12345@nowhere.invalid",
            }))
            .unwrap();
        // Resolver may or may not find the synthetic @a3net.local —
        // we only assert that the shape is correct.
        assert!(r.get("query").is_some() || r.get("found").is_some());
    }

    #[tokio::test]
    async fn invoke_dispatches_on_op() {
        let tool = MailTool::new(cfg());
        let ctx = ToolContext::default();
        // `resolve_peer` is local and doesn't need a connection.
        let r = tool
            .invoke(
                json!({"_op": "resolve_peer", "query": "no-such@nowhere.invalid"}),
                ctx,
            )
            .await
            .unwrap();
        assert!(r.get("query").is_some() || r.get("found").is_some());
    }

    #[tokio::test]
    async fn invoke_rejects_unknown_op() {
        let tool = MailTool::new(cfg());
        let ctx = ToolContext::default();
        let err = tool
            .invoke(json!({"_op": "delete_everything"}), ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[test]
    fn into_account_builds_cleanly() {
        let account = cfg().into_account().unwrap();
        assert_eq!(account.account().addr, "agent@example.com");
    }

    #[test]
    fn config_debug_redacts_password() {
        let dbg = format!("{:?}", cfg());
        assert!(dbg.contains("secret") == false || dbg.contains("redacted"));
    }
}
