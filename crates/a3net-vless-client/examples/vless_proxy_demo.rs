//! `vless_proxy_demo` — minimal end-to-end demo.
//!
//! Parses a `vless://` URI from the command line, starts the
//! VLESS client, prints the local SOCKS5/HTTP proxy addresses,
//! waits for `Ctrl+C`, and shuts down.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p a3net-vless-client --example vless_proxy_demo -- \
//!     "vless://<uuid>@host:443?security=tls&sni=host#tag"
//! ```

use a3net_vless_client::{
    VlessClient, VlessClientConfig, VlessLink, VlessClientError,
};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let link_str = args.next().ok_or("usage: vless_proxy_demo <vless URI> [--socks5 127.0.0.1:1080] [--http 127.0.0.1:8080]")?;
    let mut socks5: SocketAddr = "127.0.0.1:1080".parse()?;
    let mut http: Option<SocketAddr> = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--socks5" => socks5 = args.next().ok_or("missing --socks5 value")?.parse()?,
            "--http" => http = Some(args.next().ok_or("missing --http value")?.parse()?),
            other => return Err(format!("unknown flag: {other}").into()),
        }
    }

    let link = VlessLink::parse(&link_str).map_err(|e: VlessClientError| -> Box<dyn std::error::Error> { Box::new(e) })?;
    let cfg = VlessClientConfig {
        link,
        listen_socks5: socks5,
        listen_http: http,
        ..VlessClientConfig::from_link(VlessLink::parse(&link_str)?)
    };
    let handle = VlessClient::start(cfg).await?;
    eprintln!(
        "socks5: {}\nhttp:   {}",
        handle.socks5_addr().await.unwrap(),
        handle.http_addr().await.map(|a| a.to_string()).unwrap_or_else(|| "(disabled)".into()),
    );
    eprintln!("Ctrl-C to stop");
    tokio::signal::ctrl_c().await?;
    handle.shutdown().await?;
    Ok(())
}
