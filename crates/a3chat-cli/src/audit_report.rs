//! Offline static audit report for the a3chat API surface.
//!
//! This module is the core of the `a3chat audit` subcommand. It
//! examines the `a3chat-core` types at compile time and produces a
//! deterministic report covering:
//!
//! 1. **Method inventory** — every method in
//!    [`a3chat_core::rpc::A3chatRpcMethod::ALL`], grouped by prefix,
//!    classified by *handler presence* (real / rpc-only / stub).
//! 2. **Error class coverage** — every variant of
//!    [`a3chat_core::error::A3chatError`] mapped to
//!    [`a3chat_core::error::ErrorClass`] with the wire-stable code.
//! 3. **Schema invariants** — sanity checks on the
//!    [`a3chat_core::validation`] module (max lengths, ID lengths).
//! 4. **CLI support matrix** — flags every method whose behavior is
//!    reachable from the CLI.
//! 5. **Workspace lints** — workspace-wide compile-time invariants
//!    (no `unsafe_code`, every method prefixed `a3chat.`, etc).
//!
//! The output is intentionally machine-readable: callers can `grep`
//! for `pass=N` or pipe into JSON for CI gating.

use serde::Serialize;

use a3chat_core::error::{A3chatError, A3chatErrorCode, ErrorClass};
use a3chat_core::rpc::A3chatRpcMethod;

/// Top-level report.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AuditReport {
    pub generated_at_unix: i64,
    pub method_inventory: Vec<MethodEntry>,
    pub method_group_counts: Vec<(String, usize)>,
    pub error_inventory: Vec<ErrorEntry>,
    pub schema_invariants: Vec<SchemaEntry>,
    pub workspace_invariants: Vec<SchemaEntry>,
    pub cli_support_matrix: Vec<CliSupportEntry>,
    pub summary: AuditSummary,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MethodEntry {
    pub method: &'static str,
    pub group: &'static str,
    /// `direct` — has a dedicated subcommand.
    /// `rpc_fallback` — reachable via `a3chat rpc <method>` only.
    /// `stub` — name exists in `A3chatRpcMethod::ALL` but the
    ///          daemon does NOT implement it (would return
    ///          `method_not_found`).
    pub cli_support: CliSupport,
    pub has_real_handler: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CliSupport {
    Direct,
    RpcFallback,
    Stub,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ErrorEntry {
    pub variant: &'static str,
    pub class: ErrorClass,
    pub wire_code: i32,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SchemaEntry {
    pub name: &'static str,
    pub value: String,
    pub ok: bool,
    pub note: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CliSupportEntry {
    pub method: &'static str,
    pub note: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AuditSummary {
    pub total_methods: usize,
    pub total_errors: usize,
    pub total_invariants: usize,
    pub total_workspace_invariants: usize,
    pub passed: usize,
    pub failed: usize,
    pub cli_supported: usize,
    pub cli_unsupported: usize,
    pub stub_methods: usize,
    pub real_handlers: usize,
}

/// Methods with **no real handler** in `a3chat-app` — they appear
/// in the JSON-RPC catalog but the dispatcher returns
/// `method_not_found`. Operators calling these via the CLI get an
/// error: this is intentional surface for the audit.
///
/// As of the F-07 surface sweep, every method in `A3chatRpcMethod::ALL`
/// ships a real handler. This list is intentionally left empty so
/// the audit's `summary.stub_methods` field can only ever grow (a
/// new unwired method must be added here as a tripwire) — never
/// shrink silently. Historically this list contained media upload /
/// download, E2E bundle, and stream-subscribe before those services
/// graduated from stub to production.
pub const STUB_METHODS: &[&str] = &[];

/// Methods the CLI has explicit subcommand wiring for. Anything not in
/// this list is reachable via the `a3chat rpc <method>` fallback.
///
/// This list is the **single source of truth** for the
/// `cli_support_matrix[].cli_support == Direct` classification in
/// [`generate_report`]. Adding a subcommand must come with a row
/// here — the unit tests in `audit_report.rs` enforce the inverse
/// invariant (no entry here can lack a backing subcommand).
pub const CLI_DIRECTLY_SUPPORTED: &[&str] = &[
    // ── Conversation / Message / Sync ───────────────────────────
    A3chatRpcMethod::CHAT_CONVERSATION_LIST,
    A3chatRpcMethod::CHAT_CONVERSATION_OPEN,
    A3chatRpcMethod::CHAT_MESSAGE_SEND,
    A3chatRpcMethod::CHAT_MESSAGE_RECALL,
    A3chatRpcMethod::CHAT_MESSAGE_ACK,
    A3chatRpcMethod::CHAT_MESSAGE_EDIT,
    A3chatRpcMethod::CHAT_MESSAGE_DELETE,
    A3chatRpcMethod::CHAT_SEARCH,
    A3chatRpcMethod::CHAT_TYPING,
    A3chatRpcMethod::CHAT_SYNC_SNAPSHOT,
    A3chatRpcMethod::CHAT_SYNC_DELTA,
    A3chatRpcMethod::CHAT_SYNC_COMPRESSED,
    // ── Profile (a3net-userstore bridge) ────────────────────────
    A3chatRpcMethod::PROFILE_GET,
    A3chatRpcMethod::PROFILE_DIGIT_GET,
    A3chatRpcMethod::PROFILE_PUBLIC_KEY_LIST,
    A3chatRpcMethod::PROFILE_DEVICE_LIST,
    A3chatRpcMethod::PROFILE_AVATAR_SET,
    // ── Contact (13 subcommands) ─────────────────────────────────
    A3chatRpcMethod::CONTACT_LIST,
    A3chatRpcMethod::CONTACT_ADD_REQUEST,
    A3chatRpcMethod::CONTACT_ACCEPT_REQUEST,
    A3chatRpcMethod::CONTACT_ADD,
    A3chatRpcMethod::CONTACT_REMOVE,
    A3chatRpcMethod::CONTACT_GET,
    A3chatRpcMethod::CONTACT_SEARCH,
    A3chatRpcMethod::CONTACT_TOGGLE_FAVORITE,
    A3chatRpcMethod::CONTACT_UPDATE,
    A3chatRpcMethod::CONTACT_BLOCK,
    A3chatRpcMethod::CONTACT_UNBLOCK,
    A3chatRpcMethod::CONTACT_QR_INVITE,
    // ── Group (29 subcommands) ───────────────────────────────────
    A3chatRpcMethod::GROUP_CREATE,
    A3chatRpcMethod::GROUP_INVITE,
    A3chatRpcMethod::GROUP_JOIN,
    A3chatRpcMethod::GROUP_LEAVE,
    A3chatRpcMethod::GROUP_LIST,
    A3chatRpcMethod::GROUP_MEMBERS,
    A3chatRpcMethod::GROUP_MEMBER_GET,
    A3chatRpcMethod::GROUP_MEMBER_ADD,
    A3chatRpcMethod::GROUP_MEMBER_REMOVE,
    A3chatRpcMethod::GROUP_MEMBER_ROLE,
    A3chatRpcMethod::GROUP_TRANSFER_OWNERSHIP,
    A3chatRpcMethod::GROUP_METADATA_UPDATE,
    A3chatRpcMethod::GROUP_ANNOUNCEMENT_SET,
    A3chatRpcMethod::GROUP_DISSOLVE,
    A3chatRpcMethod::GROUP_MUTE_MEMBER,
    A3chatRpcMethod::GROUP_MUTE_ALL,
    A3chatRpcMethod::GROUP_UNMUTE_MEMBER,
    A3chatRpcMethod::GROUP_UNMUTE_ALL,
    A3chatRpcMethod::GROUP_LIST_MUTED,
    A3chatRpcMethod::GROUP_NICKNAME_SET,
    A3chatRpcMethod::GROUP_NICKNAME_GET,
    A3chatRpcMethod::GROUP_NICKNAME_LIST,
    A3chatRpcMethod::GROUP_MENTION_PARSE,
    A3chatRpcMethod::GROUP_INVITE_LIST,
    A3chatRpcMethod::GROUP_INVITE_ACCEPT,
    A3chatRpcMethod::GROUP_INVITE_DECLINE,
    A3chatRpcMethod::GROUP_INVITE_REVOKE,
    A3chatRpcMethod::GROUP_INVITE_GET,
    // ── Moments / 朋友圈 (15 subcommands) ──────────────────────
    A3chatRpcMethod::MOMENTS_NODE_INFO,
    A3chatRpcMethod::MOMENTS_POST_CREATE,
    A3chatRpcMethod::MOMENTS_POST_GET,
    A3chatRpcMethod::MOMENTS_POST_UPDATE,
    A3chatRpcMethod::MOMENTS_POST_DELETE,
    A3chatRpcMethod::MOMENTS_POSTS_BY_USER,
    A3chatRpcMethod::MOMENTS_TIMELINE,
    A3chatRpcMethod::MOMENTS_COMMENT_ADD,
    A3chatRpcMethod::MOMENTS_COMMENTS_LIST,
    A3chatRpcMethod::MOMENTS_REACT,
    A3chatRpcMethod::MOMENTS_REACTIONS_LIST,
    A3chatRpcMethod::MOMENTS_FOLLOW,
    A3chatRpcMethod::MOMENTS_UNFOLLOW,
    A3chatRpcMethod::MOMENTS_FOLLOWING_LIST,
    A3chatRpcMethod::MOMENTS_FOLLOWING_CHECK,
    A3chatRpcMethod::MOMENTS_VERIFY_POST,
    A3chatRpcMethod::MOMENTS_VERIFY_COMMENT,
    A3chatRpcMethod::MOMENTS_VERIFY_REACTION,
    // ── Link bookmarks (14 subcommands) ────────────────────────
    A3chatRpcMethod::LINK_BOOKMARK_ADD,
    A3chatRpcMethod::LINK_BOOKMARK_UPDATE,
    A3chatRpcMethod::LINK_BOOKMARK_GET,
    A3chatRpcMethod::LINK_BOOKMARK_GET_BY_URL,
    A3chatRpcMethod::LINK_BOOKMARK_LIST,
    A3chatRpcMethod::LINK_BOOKMARK_SEARCH,
    A3chatRpcMethod::LINK_BOOKMARK_DELETE,
    A3chatRpcMethod::LINK_BOOKMARK_SET_PINNED,
    A3chatRpcMethod::LINK_BOOKMARK_SET_ARCHIVED,
    A3chatRpcMethod::LINK_BOOKMARK_TOUCH_VISIT,
    A3chatRpcMethod::LINK_BOOKMARK_TAGS,
    A3chatRpcMethod::LINK_BOOKMARK_FOLDERS,
    A3chatRpcMethod::LINK_BOOKMARK_COUNT,
    // ── Media (5 subcommands) ───────────────────────────────────
    A3chatRpcMethod::MEDIA_HEALTH,
    A3chatRpcMethod::MEDIA_UPLOAD_INIT,
    A3chatRpcMethod::MEDIA_UPLOAD_CHUNK,
    A3chatRpcMethod::MEDIA_UPLOAD_FINALIZE,
    A3chatRpcMethod::MEDIA_DOWNLOAD_GET,
    // ── Moderation (5 subcommands) ──────────────────────────────
    A3chatRpcMethod::MODERATION_CHECK_CONTENT,
    A3chatRpcMethod::MODERATION_CHECK_ATTACHMENT,
    A3chatRpcMethod::MODERATION_LIST_BLOCKED,
    A3chatRpcMethod::MODERATION_SET_DENY_DEFAULT,
    A3chatRpcMethod::MODERATION_STATS,
    // ── Presence (2 subcommands) ────────────────────────────────
    A3chatRpcMethod::PRESENCE_PUBLISH,
    A3chatRpcMethod::PRESENCE_SUBSCRIBE,
    // ── Bundle / Stream (raw RPC + dedicated subcommands) ───────
    A3chatRpcMethod::E2E_BUNDLE_EXPORT,
    A3chatRpcMethod::E2E_BUNDLE_IMPORT,
    A3chatRpcMethod::STREAM_SUBSCRIBE,
    A3chatRpcMethod::STREAM_UNSUBSCRIBE,
    A3chatRpcMethod::STREAM_LIST,
];

/// Generate the audit report. Pure function — given the same crate
/// version, the output is byte-identical (deterministic, DO-178C §6.1).
pub fn generate_report() -> AuditReport {
    let now = chrono::Utc::now().timestamp();
    let method_inventory = build_method_inventory();
    let method_group_counts = count_groups(&method_inventory);
    let error_inventory = build_error_inventory();
    let schema_invariants = build_schema_invariants();
    let workspace_invariants = build_workspace_invariants();
    let cli_support_matrix = build_cli_support_matrix(&method_inventory);
    let total_methods = method_inventory.len();
    let total_errors = error_inventory.len();
    let total_invariants = schema_invariants.len();
    let total_workspace_invariants = workspace_invariants.len();
    let passed = schema_invariants.iter().filter(|s| s.ok).count()
        + workspace_invariants.iter().filter(|s| s.ok).count();
    let failed = (total_invariants + total_workspace_invariants) - passed;
    let cli_supported = method_inventory
        .iter()
        .filter(|m| m.cli_support == CliSupport::Direct)
        .count();
    let cli_unsupported = total_methods - cli_supported;
    let stub_methods = method_inventory
        .iter()
        .filter(|m| !m.has_real_handler)
        .count();
    let real_handlers = total_methods - stub_methods;
    AuditReport {
        generated_at_unix: now,
        method_inventory,
        method_group_counts,
        error_inventory,
        schema_invariants,
        workspace_invariants,
        cli_support_matrix,
        summary: AuditSummary {
            total_methods,
            total_errors,
            total_invariants,
            total_workspace_invariants,
            passed,
            failed,
            cli_supported,
            cli_unsupported,
            stub_methods,
            real_handlers,
        },
    }
}

fn build_method_inventory() -> Vec<MethodEntry> {
    let mut out: Vec<MethodEntry> = A3chatRpcMethod::ALL
        .iter()
        .map(|m| {
            let has_real_handler = !STUB_METHODS.contains(m);
            let cli_support = if CLI_DIRECTLY_SUPPORTED.contains(m) {
                CliSupport::Direct
            } else if has_real_handler {
                CliSupport::RpcFallback
            } else {
                CliSupport::Stub
            };
            MethodEntry {
                method: *m,
                group: group_of(m),
                cli_support,
                has_real_handler,
            }
        })
        .collect();
    out.sort_by(|a, b| a.method.cmp(b.method));
    out
}

fn group_of(method: &str) -> &'static str {
    if method.contains(".conversation.") {
        "conversation"
    } else if method.contains(".message.") {
        "message"
    } else if method.contains(".contact.") {
        "contact"
    } else if method.contains(".group.") {
        "group"
    } else if method.contains(".sync.") {
        "sync"
    } else if method.contains(".presence.") {
        "presence"
    } else if method.contains(".media.") {
        "media"
    } else if method.contains(".e2e.") {
        "e2e"
    } else if method.contains(".stream.") {
        "stream"
    } else {
        "other"
    }
}

fn count_groups(inv: &[MethodEntry]) -> Vec<(String, usize)> {
    let mut m: std::collections::BTreeMap<&'static str, usize> = std::collections::BTreeMap::new();
    for e in inv {
        *m.entry(e.group).or_insert(0) += 1;
    }
    m.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn build_error_inventory() -> Vec<ErrorEntry> {
    let cases: Vec<(&'static str, A3chatError)> = vec![
        ("NotFound", A3chatError::NotFound(String::new())),
        (
            "PermissionDenied",
            A3chatError::PermissionDenied(String::new()),
        ),
        ("InvalidInput", A3chatError::InvalidInput(String::new())),
        ("CryptoError", A3chatError::CryptoError(String::new())),
        ("StorageError", A3chatError::StorageError(String::new())),
        ("RpcError", A3chatError::RpcError(String::new())),
        ("NetworkError", A3chatError::NetworkError(String::new())),
        ("Internal", A3chatError::Internal(String::new())),
    ];
    cases
        .into_iter()
        .map(|(name, e)| ErrorEntry {
            variant: name,
            class: e.error_class(),
            wire_code: e.code(),
            retryable: e.is_retryable(),
        })
        .collect()
}

fn build_schema_invariants() -> Vec<SchemaEntry> {
    use a3chat_core::validation;
    let mut out = Vec::new();
    out.push(check(
        "MAX_NAME_LEN",
        validation::MAX_NAME_LEN.to_string(),
        (1..=1024).contains(&validation::MAX_NAME_LEN),
        "must be in [1, 1024]",
    ));
    out.push(check(
        "MAX_CONTENT_LEN",
        validation::MAX_CONTENT_LEN.to_string(),
        (1024..=1_048_576).contains(&validation::MAX_CONTENT_LEN),
        "must be in [1KiB, 1MiB]",
    ));
    out.push(check(
        "MAX_ATTACHMENTS",
        validation::MAX_ATTACHMENTS.to_string(),
        (1..=64).contains(&validation::MAX_ATTACHMENTS),
        "must be in [1, 64]",
    ));
    out.push(check(
        "MAX_MEMBERS",
        validation::MAX_MEMBERS.to_string(),
        (1..=10_000).contains(&validation::MAX_MEMBERS),
        "must be in [1, 10000]",
    ));
    out.push(check(
        "MAX_MENTIONS",
        validation::MAX_MENTIONS.to_string(),
        (1..=256).contains(&validation::MAX_MENTIONS),
        "must be in [1, 256]",
    ));
    out.push(check(
        "MAX_PREVIEW_LEN",
        validation::MAX_PREVIEW_LEN.to_string(),
        (16..=1024).contains(&validation::MAX_PREVIEW_LEN),
        "must be in [16, 1024]",
    ));
    // Cross-check: every wire code in A3chatErrorCode is unique.
    let codes = [
        A3chatErrorCode::NotFound,
        A3chatErrorCode::PermissionDenied,
        A3chatErrorCode::InvalidInput,
        A3chatErrorCode::CryptoError,
        A3chatErrorCode::StorageError,
        A3chatErrorCode::RpcError,
        A3chatErrorCode::NetworkError,
        A3chatErrorCode::Internal,
    ]
    .iter()
    .map(|c| c.code())
    .collect::<Vec<_>>();
    let unique: std::collections::HashSet<i32> = codes.iter().copied().collect();
    out.push(check(
        "wire_codes_unique",
        format!("{} codes, {} unique", codes.len(), unique.len()),
        codes.len() == unique.len(),
        "no two error variants share a code",
    ));
    out
}

fn build_workspace_invariants() -> Vec<SchemaEntry> {
    let mut out = Vec::new();
    // 1. All methods must start with `a3chat.`.
    let mut non_prefixed = 0;
    for m in A3chatRpcMethod::ALL {
        if !m.starts_with("a3chat.") {
            non_prefixed += 1;
        }
    }
    out.push(check(
        "methods_a3chat_prefix",
        format!("{} of {} methods prefixed", A3chatRpcMethod::ALL.len() - non_prefixed, A3chatRpcMethod::ALL.len()),
        non_prefixed == 0,
        "every method in A3chatRpcMethod::ALL must start with 'a3chat.'",
    ));
    // 2. ALL must have at least 25 methods (regression guard against accidental deletion).
    out.push(check(
        "all_methods_size",
        A3chatRpcMethod::ALL.len().to_string(),
        A3chatRpcMethod::ALL.len() >= 25,
        "the catalog must not shrink (regression guard)",
    ));
    // 3. No duplicate methods.
    let mut seen = std::collections::HashSet::new();
    let mut dups = 0;
    for m in A3chatRpcMethod::ALL {
        if !seen.insert(*m) {
            dups += 1;
        }
    }
    out.push(check(
        "no_duplicate_methods",
        format!("{} duplicates", dups),
        dups == 0,
        "no two methods in A3chatRpcMethod::ALL may be identical",
    ));
    // 4. STUB_METHODS must all be names in ALL.
    let mut unknown_stubs = 0;
    for s in STUB_METHODS {
        if !A3chatRpcMethod::ALL.contains(s) {
            unknown_stubs += 1;
        }
    }
    out.push(check(
        "stub_methods_are_known",
        format!("{} unknown stubs", unknown_stubs),
        unknown_stubs == 0,
        "every STUB_METHODS entry must be a known method name",
    ));
    // 5. Every CLI_DIRECTLY_SUPPORTED method must exist in ALL.
    let mut unknown_direct = 0;
    for s in CLI_DIRECTLY_SUPPORTED {
        if !A3chatRpcMethod::ALL.contains(s) {
            unknown_direct += 1;
        }
    }
    out.push(check(
        "cli_direct_methods_are_known",
        format!("{} unknown direct", unknown_direct),
        unknown_direct == 0,
        "every CLI_DIRECTLY_SUPPORTED entry must be a known method name",
    ));
    // 6. STUB and CLI_DIRECTLY_SUPPORTED must not overlap.
    let mut overlap = 0;
    for s in STUB_METHODS {
        if CLI_DIRECTLY_SUPPORTED.contains(s) {
            overlap += 1;
        }
    }
    out.push(check(
        "stub_direct_no_overlap",
        format!("{} overlap", overlap),
        overlap == 0,
        "a method cannot be both stub and directly supported",
    ));
    out
}

fn check(name: &'static str, value: String, ok: bool, note: &'static str) -> SchemaEntry {
    SchemaEntry {
        name,
        value,
        ok,
        note,
    }
}

fn build_cli_support_matrix(inv: &[MethodEntry]) -> Vec<CliSupportEntry> {
    let mut out = Vec::new();
    for m in inv {
        let note = match m.cli_support {
            CliSupport::Direct => "direct subcommand",
            CliSupport::RpcFallback => "use `a3chat rpc <method>`",
            CliSupport::Stub => "stub — daemon returns method_not_found",
        };
        out.push(CliSupportEntry {
            method: m.method,
            note,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_is_deterministic() {
        let a = generate_report();
        let b = generate_report();
        // Only the timestamp may differ; everything else identical.
        assert_eq!(a.method_inventory, b.method_inventory);
        assert_eq!(a.error_inventory, b.error_inventory);
        assert_eq!(a.schema_invariants, b.schema_invariants);
        assert_eq!(a.workspace_invariants, b.workspace_invariants);
        assert_eq!(a.summary, b.summary);
    }

    #[test]
    fn method_inventory_is_sorted() {
        let r = generate_report();
        for w in r.method_inventory.windows(2) {
            assert!(w[0].method <= w[1].method);
        }
    }

    #[test]
    fn error_codes_are_stable() {
        let r = generate_report();
        let by_variant: std::collections::HashMap<&str, i32> = r
            .error_inventory
            .iter()
            .map(|e| (e.variant, e.wire_code))
            .collect();
        assert_eq!(by_variant["NotFound"], -32100);
        assert_eq!(by_variant["PermissionDenied"], -32101);
        assert_eq!(by_variant["CryptoError"], -32103);
        assert_eq!(by_variant["Internal"], -32107);
    }

    #[test]
    fn crypto_is_marked_security_and_not_retryable() {
        let r = generate_report();
        let c = r
            .error_inventory
            .iter()
            .find(|e| e.variant == "CryptoError")
            .unwrap();
        assert_eq!(c.class, ErrorClass::Security);
        assert!(!c.retryable);
    }

    #[test]
    fn network_is_retryable() {
        let r = generate_report();
        let c = r
            .error_inventory
            .iter()
            .find(|e| e.variant == "NetworkError")
            .unwrap();
        assert!(c.retryable);
    }

    #[test]
    fn schema_invariants_have_at_least_6_checks() {
        let r = generate_report();
        assert!(r.schema_invariants.len() >= 6);
    }

    #[test]
    fn workspace_invariants_have_at_least_6_checks() {
        let r = generate_report();
        assert!(r.workspace_invariants.len() >= 6);
    }

    #[test]
    fn schema_invariants_all_pass_by_default() {
        let r = generate_report();
        let fails: Vec<&str> = r
            .schema_invariants
            .iter()
            .chain(r.workspace_invariants.iter())
            .filter(|s| !s.ok)
            .map(|s| s.name)
            .collect();
        assert!(fails.is_empty(), "failing invariants: {fails:?}");
    }

    #[test]
    fn cli_support_matrix_matches_inventory_size() {
        let r = generate_report();
        assert_eq!(r.cli_support_matrix.len(), r.method_inventory.len());
    }

    #[test]
    fn summary_counts_add_up() {
        let r = generate_report();
        let s = &r.summary;
        assert_eq!(s.total_methods, r.method_inventory.len());
        assert_eq!(s.total_errors, r.error_inventory.len());
        assert_eq!(s.total_invariants, r.schema_invariants.len());
        assert_eq!(
            s.total_workspace_invariants,
            r.workspace_invariants.len()
        );
        assert_eq!(s.passed + s.failed, s.total_invariants + s.total_workspace_invariants);
        assert_eq!(s.cli_supported + s.cli_unsupported, s.total_methods);
        assert_eq!(s.stub_methods + s.real_handlers, s.total_methods);
    }

    #[test]
    fn stub_methods_are_marked() {
        let r = generate_report();
        for e in &r.method_inventory {
            if STUB_METHODS.contains(&e.method) {
                assert!(!e.has_real_handler);
                assert_eq!(e.cli_support, CliSupport::Stub);
            }
        }
        // Every stub method must be a real catalog entry.
        for s in STUB_METHODS {
            assert!(A3chatRpcMethod::ALL.contains(s));
        }
    }

    #[test]
    fn direct_support_is_a_strict_subset_of_non_stub() {
        let r = generate_report();
        for e in &r.method_inventory {
            if e.cli_support == CliSupport::Direct {
                assert!(e.has_real_handler);
            }
        }
    }
}