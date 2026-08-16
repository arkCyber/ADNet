//! `a3net stream [--filter <event>]` — subscribe to the daemon's
//! Server-Sent-Events stream (`GET /rpc/stream`) and print events
//! as they arrive.
//!
//! Requires the HTTP transport (port 11436); on Unix socket the
//! command errors out with a hint pointing at the IPC adapter.

use anyhow::Result;
use futures::StreamExt;

use crate::ipc_client::IpcClient;

/// Top-level dispatch — `a3net stream`.
pub async fn run_stream(client: &IpcClient, filter: Option<String>, max_events: u64) -> Result<()> {
    let mut stream = client.subscribe_events()?;
    let mut count: u64 = 0;
    while let Some(ev) = stream.next().await {
        if let Some(f) = filter.as_ref() {
            if !ev.event.contains(f) {
                continue;
            }
        }
        let line = format!(
            "[{}] event={} data={}",
            chrono::Utc::now().format("%H:%M:%S%.3f"),
            ev.event,
            ev.data
        );
        println!("{line}");
        count += 1;
        if max_events > 0 && count >= max_events {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc_client::{IpcClient, Transport};

    #[tokio::test]
    async fn stream_rejects_unix_socket() {
        let c = IpcClient::with_transport(Transport::UnixSocket("/tmp/nope".into()));
        let res = c.subscribe_events();
        assert!(res.is_err(), "subscribe_events must reject Unix socket transport");
    }
}
