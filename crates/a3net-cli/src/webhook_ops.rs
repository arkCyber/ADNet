//! `a3net webhook <sub>` handlers.
//!
//! Thin wrappers around `a3net-webhook`'s file-based config
//! helpers. The CLI does **not** run the HTTP delivery loop
//! itself; that lives in the long-lived `a3net serve` process
//! and reads the same JSON files these subcommands write. The
//! CLI's job here is to make the config readable, writable,
//! and verifiable from the shell.
//!
//! - `webhook list <file>`  — load and pretty-print a JSON
//!   webhook config so operators can audit the active list.
//! - `webhook save <file>`  — read JSON endpoints from stdin
//!   (one per line) and persist them as a JSON array.
//! - `webhook test <file>`  — emit a synthetic
//!   `AdnetEvent::Announcement` to every endpoint in the file
//!   so a freshly deployed receiver can be smoke-tested
//!   without going through the gossip bus.

use std::path::Path;

use a3net_webhook::{
    load_endpoints, save_endpoints, AdnetEvent, EndpointConfig, EventSink, WebhookSink,
};
use anyhow::{Context, Result};

use crate::cli::WebhookCmd;

pub async fn run_webhook(sub: &WebhookCmd, data_dir: &Path) -> Result<()> {
    match sub {
        WebhookCmd::List { config } => {
            let endpoints = load_endpoints(config).with_context(|| {
                format!("load webhook endpoints from {}", config.display())
            })?;
            println!("{} endpoint(s) in {}:", endpoints.len(), config.display());
            for (i, ep) in endpoints.iter().enumerate() {
                println!(
                    "  [{i}] url={} room={} timeout={:?}",
                    ep.url,
                    ep.room_filter.as_deref().unwrap_or("*"),
                    ep.request_timeout
                );
            }
            Ok(())
        }
        WebhookCmd::Save { output } => {
            use std::io::BufRead;
            let stdin = std::io::stdin();
            let handle = stdin.lock();
            let mut endpoints: Vec<EndpointConfig> = Vec::new();
            for line in handle.lines() {
                let line = line?;
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let ep: EndpointConfig = serde_json::from_str(line)
                    .with_context(|| format!("parse stdin line: {line}"))?;
                endpoints.push(ep);
            }
            save_endpoints(output, &endpoints).with_context(|| {
                format!("save webhook config to {}", output.display())
            })?;
            println!(
                "wrote {} endpoint(s) to {}",
                endpoints.len(),
                output.display()
            );
            Ok(())
        }
        WebhookCmd::Test { config, room } => {
            let endpoints = load_endpoints(config).with_context(|| {
                format!("load webhook endpoints from {}", config.display())
            })?;
            if endpoints.is_empty() {
                println!("(no endpoints in {})", config.display());
                return Ok(());
            }
            // Use the spool directory derived from the data
            // dir so a test delivery that fails to reach its
            // receiver does not silently disappear.
            let spool_path = data_dir.join("webhook-spool.jsonl");
            let sink = WebhookSink::with_spool(endpoints.clone(), spool_path)
                .context("construct webhook sink")?;
            let event = AdnetEvent::Announcement {
                payload: serde_json::json!({
                    "room_id": room,
                    "node_id": "cli-test",
                    "title": "webhook smoke test",
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }),
            };
            let delivery_id = format!(
                "cli-test-{}",
                chrono::Utc::now().timestamp_millis()
            );
            sink.deliver(&event, &delivery_id).await?;
            println!(
                "delivered synthetic event (id={delivery_id}) to {} endpoint(s)",
                endpoints.len()
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_endpoints_file_missing_errors() {
        let result = load_endpoints(Path::new("/nonexistent/hooks.json"));
        assert!(result.is_err());
    }
}