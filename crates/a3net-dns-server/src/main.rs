//! `a3net-dns-server` — self-hostable authoritative DNS server for A3Net.
//!
//! Subcommands:
//!   * `serve`  — start the DNS + HTTP admin listener (default).
//!   * `print-config` — print the parsed config (handy for ops).
//!
//! The DNS server is authoritative only; recursive resolution is
//! not in scope. Operators can plug the server behind a reverse
//! proxy for TLS termination.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use a3net_dns_server::{
    config::DnsServerConfig,
    http::{serve_http, HttpApi},
    server,
};

#[derive(Parser, Debug)]
#[command(name = "a3net-dns-server", version)]
struct Cli {
    #[arg(long, env = "ADNET_DNS_BIND", default_value = "0.0.0.0:53")]
    bind: SocketAddr,

    #[arg(long, env = "ADNET_DNS_ZONE", default_value = "a3net.local")]
    zone: String,

    #[arg(long, env = "ADNET_DNS_HTTP_BIND", default_value = "127.0.0.1:8081")]
    http_bind: SocketAddr,

    #[arg(long, env = "ADNET_DNS_STATE_FILE")]
    state_file: Option<PathBuf>,

    #[arg(long, env = "ADNET_DNS_PKARR_RELAY")]
    pkarr_relay: Option<String>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Print the parsed config and exit.
    PrintConfig,
    /// Run the DNS server (default subcommand).
    Serve,
}

fn build_config(args: &Cli) -> DnsServerConfig {
    let mut cfg = DnsServerConfig::default()
        .with_bind(args.bind)
        .with_zone(args.zone.clone());
    if let Some(p) = &args.state_file {
        cfg = cfg.with_state_path(p.clone());
    }
    if let Some(relay) = &args.pkarr_relay {
        cfg = cfg.with_pkarr_relay(relay.clone());
    }
    cfg.upstream_timeout = Duration::from_millis(500);
    cfg
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cli = Cli::parse();
    let cfg = build_config(&cli);

    match cli.cmd.as_ref().unwrap_or(&Cmd::Serve) {
        Cmd::PrintConfig => {
            println!("{}", serde_json::to_string_pretty(&cfg)?);
            Ok(())
        }
        Cmd::Serve => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async move {
                let _dns_handle = server::serve(cfg.clone()).await?;
                let api = Arc::new(HttpApi::from_config(cfg)?);
                serve_http(cli.http_bind, api).await?;
                Ok::<(), anyhow::Error>(())
            })?;
            Ok(())
        }
    }
}