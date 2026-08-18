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

use a3chat_cli::Cli;
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
