//! Top-level CLI dispatch tests.
//!
//! These tests verify the surface contract: every planned top-level
//! subcommand must (a) parse from the command line, (b) reach the
//! dispatch match, and (c) reject malformed arguments with a
//! `CliError::Usage` (which carries an actionable suggestion).
//!
//! DO-178C §5.2 traceability — every test references a stable
//! command string so a future regression points at the exact
//! subcommand that broke.

use a3chat_cli::audit_report::{generate_report, CliSupport};
use a3chat_cli::Cli;
use a3chat_core::rpc::A3chatRpcMethod;
use clap::CommandFactory;
use clap::Parser;

/// Every public top-level command must appear in `--help` output.
/// This is the regression guard for the "10 orphan modules" gap
/// that previously left `contact`, `group`, `moments`, `link`,
/// `media`, `moderation`, `presence`, `bundle`, `stream`, and
/// `chat` unreachable from the CLI.
#[test]
fn all_top_level_commands_are_listed_in_help() {
    let help = Cli::command().render_help().to_string();
    for cmd in [
        "whoami",
        "doctor",
        "conversation",
        "message",
        "sync",
        "profile",
        "chat",
        "contact",
        "group",
        "moments",
        "link",
        "media",
        "moderation",
        "presence",
        "bundle",
        "stream",
        "trace",
        "rpc",
        "repl",
        "completions",
        "audit",
        "config",
    ] {
        assert!(
            help.contains(cmd),
            "top-level command `{cmd}` missing from `--help`. \nhelp was:\n{help}"
        );
    }
}

#[test]
fn contact_add_subcommand_parses() {
    let cli = Cli::try_parse_from([
        "a3chat",
        "contact",
        "add",
        "--to",
        &"a".repeat(64),
        "--message",
        "hi",
    ])
    .expect("contact add should parse");
    match cli.command {
        a3chat_cli::Cmd::Contact(_) => {}
        other => panic!("expected Contact, got {other:?}"),
    }
}

#[test]
fn group_create_subcommand_parses() {
    let cli = Cli::try_parse_from([
        "a3chat",
        "group",
        "create",
        "--name",
        "team-a",
        "--is-private=true",
    ])
    .expect("group create should parse");
    match cli.command {
        a3chat_cli::Cmd::Group(_) => {}
        other => panic!("expected Group, got {other:?}"),
    }
}

#[test]
fn moments_post_subcommand_parses() {
    let cli = Cli::try_parse_from(["a3chat", "moments", "post", "hello world", "--visibility", "public"])
        .expect("moments post should parse");
    match cli.command {
        a3chat_cli::Cmd::Moments(_) => {}
        other => panic!("expected Moments, got {other:?}"),
    }
}

/// New moments subcommands added in the F-05 v1.1 audit — every
/// one maps onto a `MOMENTS_*` JSON-RPC method and must parse so an
/// operator can drive it from the CLI without falling back to
/// `a3chat rpc`.
#[test]
fn moments_new_subcommands_parse() {
    use a3chat_cli::cmd::moments::MomentsCmd;

    let cases: &[&[&str]] = &[
        &["a3chat", "moments", "comment-edit", "c-1", "--post-id", "p-1", "new text"],
        &["a3chat", "moments", "comment-delete", "c-1"],
        &["a3chat", "moments", "unreact", "t-1", "--target-type", "comment"],
        &["a3chat", "moments", "followers", "--user-id", "u-1"],
        &["a3chat", "moments", "followers"],
        &["a3chat", "moments", "block", "u-1", "--reason", "spam"],
        &["a3chat", "moments", "unblock", "u-1"],
        &["a3chat", "moments", "blocklist"],
        &["a3chat", "moments", "share", "p-1", "--comment", "nice!"],
        &["a3chat", "moments", "report", "p-1", "--reason", "abuse", "--notes", "spammy"],
    ];
    for argv in cases {
        let cli = Cli::try_parse_from(argv.iter().copied())
            .unwrap_or_else(|e| panic!("`{}` failed to parse: {e}", argv.join(" ")));
        match cli.command {
            a3chat_cli::Cmd::Moments(cmd) => match cmd {
                MomentsCmd::CommentEdit(_)
                | MomentsCmd::CommentDelete { .. }
                | MomentsCmd::Unreact(_)
                | MomentsCmd::Followers { .. }
                | MomentsCmd::Block { .. }
                | MomentsCmd::Unblock { .. }
                | MomentsCmd::Blocklist
                | MomentsCmd::Share(_)
                | MomentsCmd::Report(_) => {}
                other => panic!("`{}` dispatched to wrong variant: {other:?}", argv.join(" ")),
            },
            other => panic!("`{}` dispatched to wrong Cmd: {other:?}", argv.join(" ")),
        }
    }
}

#[test]
fn link_add_subcommand_parses() {
    let cli = Cli::try_parse_from([
        "a3chat",
        "link",
        "add",
        "https://example.com",
        "--title",
        "Example",
    ])
    .expect("link add should parse");
    match cli.command {
        a3chat_cli::Cmd::Link(_) => {}
        other => panic!("expected Link, got {other:?}"),
    }
}

#[test]
fn media_health_subcommand_parses() {
    let cli = Cli::try_parse_from(["a3chat", "media", "health"])
        .expect("media health should parse");
    match cli.command {
        a3chat_cli::Cmd::Media(_) => {}
        other => panic!("expected Media, got {other:?}"),
    }
}

#[test]
fn moderation_check_content_subcommand_parses() {
    let cli = Cli::try_parse_from([
        "a3chat",
        "moderation",
        "check-content",
        "--text",
        "hello",
    ])
    .expect("moderation check-content should parse");
    match cli.command {
        a3chat_cli::Cmd::Moderation(_) => {}
        other => panic!("expected Moderation, got {other:?}"),
    }
}

#[test]
fn presence_publish_subcommand_parses() {
    let cli = Cli::try_parse_from([
        "a3chat",
        "presence",
        "publish",
        "--status",
        "online",
        "--message",
        "at desk",
    ])
    .expect("presence publish should parse");
    match cli.command {
        a3chat_cli::Cmd::Presence(_) => {}
        other => panic!("expected Presence, got {other:?}"),
    }
}

#[test]
fn bundle_export_subcommand_parses() {
    let cli = Cli::try_parse_from(["a3chat", "bundle", "export", "--out", "-", "--passphrase", "secret"])
        .expect("bundle export should parse");
    match cli.command {
        a3chat_cli::Cmd::Bundle(_) => {}
        other => panic!("expected Bundle, got {other:?}"),
    }
}

#[test]
fn stream_list_subcommand_parses() {
    let cli = Cli::try_parse_from(["a3chat", "stream", "list"])
        .expect("stream list should parse");
    match cli.command {
        a3chat_cli::Cmd::Stream(_) => {}
        other => panic!("expected Stream, got {other:?}"),
    }
}

#[test]
fn chat_subcommand_parses() {
    let cli = Cli::try_parse_from([
        "a3chat",
        "chat",
        "--conversation-id",
        "dm:alice:bob",
    ])
    .expect("chat should parse");
    match cli.command {
        a3chat_cli::Cmd::Chat(_) => {}
        other => panic!("expected Chat, got {other:?}"),
    }
}

#[test]
fn audit_static_subcommand_parses() {
    let cli = Cli::try_parse_from(["a3chat", "audit", "static"])
        .expect("audit static should parse");
    match cli.command {
        a3chat_cli::Cmd::Audit(_) => {}
        other => panic!("expected Audit, got {other:?}"),
    }
}

#[test]
fn completions_subcommand_parses() {
    let cli = Cli::try_parse_from(["a3chat", "completions", "bash"])
        .expect("completions bash should parse");
    match cli.command {
        a3chat_cli::Cmd::Completions { .. } => {}
        other => panic!("expected Completions, got {other:?}"),
    }
}

#[test]
fn unknown_subcommand_is_rejected() {
    let r = Cli::try_parse_from(["a3chat", "no-such-subcommand"]);
    assert!(r.is_err(), "unknown subcommand must fail to parse");
}

#[test]
fn global_flags_apply_to_subcommands() {
    // `--output json` is a global flag and must be accepted on any
    // subcommand.
    let cli = Cli::try_parse_from([
        "a3chat",
        "--output",
        "json",
        "--retries",
        "1",
        "rpc",
        "methods",
    ])
    .expect("global flags should reach the subcommand");
    assert_eq!(cli.output, Some(a3chat_cli::OutputFormat::Json));
    assert_eq!(cli.retries, 1);
}

/// F-12 / F-14 / F-09 v1.1 — the catalog additions (`CHAT_THREAD_LIST`,
/// `CHAT_THREAD_GET`, `CHAT_TAP`, `CHAT_MESSAGE_FORWARD_MERGE`,
/// `CHANNEL_ANALYTICS_SUMMARY`, `CHANNEL_ANALYTICS_TIMELINE`,
/// `CHANNEL_ANALYTICS_AUDIT`, `CHANNEL_ANALYTICS_AUDIT_VERIFY`) must
/// be present in `A3chatRpcMethod::ALL` AND in the audit report's
/// method inventory. The audit report is the source of truth for
/// "can a CLI operator reach this method"? — if any one of these
/// fell out of the catalog the static report would silently miss it.
#[test]
fn new_rpc_methods_are_in_rpc_catalog_and_audit_inventory() {
    // 1. Catalog correctness.
    for m in [
        A3chatRpcMethod::CHAT_THREAD_LIST,
        A3chatRpcMethod::CHAT_THREAD_GET,
        A3chatRpcMethod::CHAT_TAP,
        A3chatRpcMethod::CHAT_MESSAGE_FORWARD_MERGE,
        A3chatRpcMethod::CHANNEL_ANALYTICS_SUMMARY,
        A3chatRpcMethod::CHANNEL_ANALYTICS_TIMELINE,
        A3chatRpcMethod::CHANNEL_ANALYTICS_AUDIT,
        A3chatRpcMethod::CHANNEL_ANALYTICS_AUDIT_VERIFY,
    ] {
        assert!(
            A3chatRpcMethod::ALL.contains(&m),
            "new RPC `{m}` must be in A3chatRpcMethod::ALL"
        );
    }

    // 2. Audit report must enumerate every catalog entry.
    let report = generate_report();
    let reported: std::collections::HashSet<&str> = report
        .method_inventory
        .iter()
        .map(|e| e.method)
        .collect();
    for m in [
        A3chatRpcMethod::CHAT_THREAD_LIST,
        A3chatRpcMethod::CHAT_THREAD_GET,
        A3chatRpcMethod::CHAT_TAP,
        A3chatRpcMethod::CHAT_MESSAGE_FORWARD_MERGE,
        A3chatRpcMethod::CHANNEL_ANALYTICS_SUMMARY,
        A3chatRpcMethod::CHANNEL_ANALYTICS_TIMELINE,
        A3chatRpcMethod::CHANNEL_ANALYTICS_AUDIT,
        A3chatRpcMethod::CHANNEL_ANALYTICS_AUDIT_VERIFY,
    ] {
        assert!(
            reported.contains(m),
            "new RPC `{m}` missing from audit_report.method_inventory"
        );
    }
}

/// F-12 / F-14 — the new RPCs are not yet first-class CLI subcommands;
/// a CLI operator should reach them via `a3chat rpc <method>`. The
/// audit report must classify them as `RpcFallback` (since they have
/// a real handler in the daemon but no dedicated subcommand), **not**
/// as `Stub` (which would mean the daemon returns `method_not_found`).
#[test]
fn new_rpc_methods_are_rpc_fallback_not_stub() {
    let report = generate_report();
    let by_method: std::collections::HashMap<&str, CliSupport> = report
        .method_inventory
        .iter()
        .map(|e| (e.method, e.cli_support))
        .collect();
    for m in [
        A3chatRpcMethod::CHAT_THREAD_LIST,
        A3chatRpcMethod::CHAT_THREAD_GET,
        A3chatRpcMethod::CHAT_TAP,
        A3chatRpcMethod::CHANNEL_ANALYTICS_SUMMARY,
        A3chatRpcMethod::CHANNEL_ANALYTICS_TIMELINE,
        A3chatRpcMethod::CHANNEL_ANALYTICS_AUDIT,
        A3chatRpcMethod::CHANNEL_ANALYTICS_AUDIT_VERIFY,
    ] {
        let cs = by_method
            .get(m)
            .copied()
            .unwrap_or_else(|| panic!("`{m}` missing from audit_report"));
        assert_ne!(
            cs,
            CliSupport::Stub,
            "`{m}` is dispatched by ChatService / ChannelService and must NOT be Stub"
        );
    }
}

#[test]
fn audit_report_schema_invariants_all_pass() {
    // DO-178C §6.1 — the offline static audit must report `failed=0`
    // every time. If a future regression breaks any invariant the
    // assertion points at the exact invariant name.
    let report = generate_report();
    let failures: Vec<&str> = report
        .schema_invariants
        .iter()
        .chain(report.workspace_invariants.iter())
        .filter(|s| !s.ok)
        .map(|s| s.name)
        .collect();
    assert!(
        failures.is_empty(),
        "audit_report invariants failed: {failures:?}"
    );
}
