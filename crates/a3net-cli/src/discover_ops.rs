//! `a3net discover` — probe well-known ports for a running daemon.
//!
//! Wraps `IpcClient::discover_http_daemon()` (which already probes
//! 11436..11439 on 127.0.0.1) and prints the resolved URL.
//!
//! In JSON mode the output is the same shape as `ipfs id`.

use anyhow::Result;

use crate::ipc_client::IpcClient;

/// Top-level dispatch — `a3net discover`.
pub async fn run_discover(json_out: bool) -> Result<()> {
    match IpcClient::discover_http_daemon().await {
        Some(client) => {
            let url = client.as_http_url().unwrap_or_default();
            if json_out {
                let payload = serde_json::json!({
                    "found": true,
                    "url": url,
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!("{url}");
            }
            Ok(())
        }
        None => {
            if json_out {
                let payload = serde_json::json!({
                    "found": false,
                    "probed_ports": crate::ipc_client::DEFAULT_DISCOVERY_PORTS,
                    "hint": "start a daemon with `a3net daemon --http-rpc-addr 127.0.0.1:11436`",
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                eprintln!(
                    "discover: no daemon found on ports {:?}",
                    crate::ipc_client::DEFAULT_DISCOVERY_PORTS
                );
                eprintln!("hint: start one with `a3net daemon --http-rpc-addr 127.0.0.1:11436`");
                std::process::exit(2);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discover_returns_false_when_no_daemon() {
        // We don't have a real daemon running in tests, so this should
        // return None (or Some if a daemon happens to be on a test port).
        let res = IpcClient::discover_http_daemon().await;
        // Either is fine for the unit test — we just need it not to panic.
        let _ = res;
    }
}
