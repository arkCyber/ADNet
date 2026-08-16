//! `a3net rpc <method> [params]` — raw JSON-RPC escape hatch.
//!
//! Sends a single JSON-RPC request to the running daemon (Unix
//! socket or HTTP/11436) and prints the JSON result. Useful for:
//!
//! - Probing RPC methods that don't have a top-level CLI command yet.
//! - Scripting `a3net` calls from shell pipelines without parsing
//!   per-command flags.
//! - Calling multiple methods in one HTTP round-trip via `--batch`.
//!
//! # Examples
//!
//! Call info() with no parameters:
//! ```bash
//! a3net rpc info
//! ```
//!
//! Call list_rooms and pretty-print the result:
//! ```bash
//! a3net rpc list_rooms --pretty
//! ```
//!
//! Pass parameters as a JSON object:
//! ```bash
//! a3net rpc join '{"room":"lobby"}'
//! ```
//!
//! Send a batch of three requests in one HTTP round-trip:
//! ```bash
//! a3net rpc --batch '[{"id":1,"method":"info","params":{}}]'
//! ```

use anyhow::{Context, Result};
use serde_json::Value;

use crate::ipc_client::{BatchRequest, IpcClient};

/// Top-level dispatch — `a3net rpc <sub>`.
pub async fn run_rpc(
    client: &IpcClient,
    method: Option<&str>,
    params: Option<&str>,
    batch: Option<&str>,
    pretty: bool,
    raw: bool,
) -> Result<()> {
    // Batch path: send multiple requests in a single HTTP round-trip
    // (or sequential calls over Unix socket).
    if let Some(batch_json) = batch {
        return run_batch(client, batch_json, pretty, raw).await;
    }

    let method = method.context("rpc: missing <method> (or pass --batch '<json>')")?;
    let params_value: Value = match params {
        None => Value::Object(serde_json::Map::new()),
        Some(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() || trimmed == "{}" {
                Value::Object(serde_json::Map::new())
            } else if trimmed.starts_with('{') || trimmed.starts_with('[') {
                serde_json::from_str(trimmed)
                    .with_context(|| format!("rpc: invalid JSON params: {trimmed}"))?
            } else {
                // Treat as a single string positional argument under key "arg".
                let mut m = serde_json::Map::new();
                m.insert("arg".to_string(), Value::String(trimmed.to_string()));
                Value::Object(m)
            }
        }
    };

    let raw_value = client.call_raw(method, params_value).await?;
    print_value(&raw_value, pretty, raw);
    Ok(())
}

async fn run_batch(client: &IpcClient, batch_json: &str, pretty: bool, raw: bool) -> Result<()> {
    let parsed: Value = serde_json::from_str(batch_json)
        .context("rpc --batch: input must be a JSON array of {id,method,params}")?;
    let arr = parsed
        .as_array()
        .context("rpc --batch: input must be a JSON array")?;
    let mut requests = Vec::with_capacity(arr.len());
    for (idx, v) in arr.iter().enumerate() {
        let id = v
            .get("id")
            .cloned()
            .unwrap_or_else(|| Value::Number(serde_json::Number::from(idx as u64)));
        let method = v
            .get("method")
            .and_then(|m| m.as_str())
            .with_context(|| format!("rpc --batch: item {idx} missing string 'method'"))?
            .to_string();
        let params = v.get("params").cloned().unwrap_or(Value::Object(Default::default()));
        requests.push(BatchRequest { id, method, params });
    }
    let responses = client.call_batch(requests).await?;
    // Preserve the caller's order: call_batch already returns responses
    // in the same order as requests.
    let out: Vec<Value> = responses
        .into_iter()
        .map(|r| {
            let mut obj = serde_json::Map::new();
            obj.insert("id".to_string(), r.id);
            if let Some(result) = r.result {
                obj.insert("result".to_string(), result);
            }
            if let Some(err) = r.error {
                obj.insert(
                    "error".to_string(),
                    serde_json::json!({"code": err.code, "message": err.message}),
                );
            }
            Value::Object(obj)
        })
        .collect();
    print_value(&Value::Array(out), pretty, raw);
    Ok(())
}

fn print_value(value: &Value, pretty: bool, raw: bool) {
    if raw {
        let s = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
        println!("{s}");
        return;
    }
    let s = if pretty {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string())
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
    };
    println!("{s}");
}

/// Render the list of every RPC method supported by the daemon, so
/// `a3net rpc --list` works as a self-documenting discoverability
/// tool. The list is sourced from
/// `a3net_ipc_adapter::NodeRpc::supported_methods()`.
pub fn list_known_methods() -> Vec<&'static str> {
    a3net_ipc_adapter::NodeRpc::supported_methods()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_known_methods_is_non_empty() {
        let m = list_known_methods();
        assert!(m.len() > 10, "should expose many methods, got {}", m.len());
        assert!(m.contains(&"info"));
        assert!(m.contains(&"ping"));
        assert!(m.contains(&"list_rooms"));
        assert!(m.contains(&"join"));
    }

    #[test]
    fn list_known_methods_is_stable() {
        let a = list_known_methods();
        let b = list_known_methods();
        assert_eq!(a, b);
    }

    #[test]
    fn print_value_handles_pretty_and_raw() {
        let v = serde_json::json!({"a": 1});
        print_value(&v, true, false);
        print_value(&v, false, true);
        print_value(&v, false, false);
    }
}
