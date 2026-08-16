//! Reuse the same parsing + dispatch path the `a3net` binary uses, but
//! without spawning a CLI — useful for embedding the CLI logic inside
//! integration tests or other programs.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-cli --example programmatic
//! ```

use std::path::PathBuf;

use a3net_cli::{Cli, Cmd};
use a3net_types::{BlobTicket, NodeAddr, NodeId};
use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    // --- 1. Parsing arbitrary argv -------------------------------------
    let args = vec![
        "a3net",
        "--data-dir",
        "/tmp/a3net-demo",
        "echo",
        "--room",
        "lobby",
    ];
    let cli = Cli::try_parse_from(args)?;
    println!("data-dir : {}", cli.data_dir);
    match cli.cmd {
        Cmd::Echo { room } => println!("echo into: {room}"),
        other => println!("unexpected subcommand: {other:?}"),
    }

    // --- 2. Building the `announce` payload by hand (no FS) ------------
    let me = NodeId::random();
    let addr = NodeAddr::new(me.clone());
    let hash = a3net_types::ContentHash::from_bytes(b"a3net-cli demo");
    let ticket = BlobTicket::whole(&me, &addr, &hash);
    println!(
        "\nhand-built ticket:\n  {ticket_raw}",
        ticket_raw = ticket.encode()
    );

    // --- 3. Roundtrip parse --------------------------------------------
    let raw = ticket.encode();
    let parsed = BlobTicket::parse(&raw).expect("parse");
    assert_eq!(parsed.node_id, me);
    assert_eq!(parsed.content_hash, hash);
    println!("parse ok (node_id matches)");

    // --- 4. Initialise a fake data dir and list an empty feed ----------
    let dir = PathBuf::from("/tmp/a3net-cli-demo");
    std::fs::create_dir_all(&dir).ok();
    let feed_path = dir.join("empty-feed.json");
    if !feed_path.exists() {
        std::fs::write(&feed_path, b"[]").ok();
    }
    println!("feed stub : {}", feed_path.display());

    println!("\nALL OK");
    Ok(())
}
