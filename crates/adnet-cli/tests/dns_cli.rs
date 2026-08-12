//! Integration tests for the `ipfs dns` CLI surface.
//!
//! These tests verify that the `ipfs dns` subcommand is correctly
//! registered in `IpfsCmd` and accepts the expected flags. The actual
//! resolution path is covered exhaustively in `adnet-namespace`'s
//! `dnslink` module unit tests.

use clap::Parser;

#[derive(clap::Parser)]
struct Wrap {
    #[command(subcommand)]
    cmd: adnet_cli::ipfs::IpfsCmd,
}

#[test]
fn dns_subcommand_parses() {
    let w = Wrap::parse_from(["test", "dns", "example.com"]);
    match w.cmd {
        adnet_cli::ipfs::IpfsCmd::Dns { domain, json } => {
            assert_eq!(domain, "example.com");
            assert!(!json);
        }
        other => panic!("expected Dns variant, got {:?}", other),
    }
}

#[test]
fn dns_subcommand_parses_with_json_flag() {
    let w = Wrap::parse_from(["test", "dns", "example.com", "--json"]);
    match w.cmd {
        adnet_cli::ipfs::IpfsCmd::Dns { domain, json } => {
            assert_eq!(domain, "example.com");
            assert!(json);
        }
        other => panic!("expected Dns variant, got {:?}", other),
    }
}
