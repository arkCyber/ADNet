//! Minimal example: implement `RpcHandler`, start a `JsonRpcServer` on a
//! temp Unix socket, fire a `json_rpc_call` against it, and shut it down.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-ipc --example ipc_basic
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use adnet_ipc::{JsonRpcServer, RpcHandler, json_rpc_call};
use async_trait::async_trait;
use serde_json::{Value, json};

struct Adder;

#[async_trait]
impl RpcHandler for Adder {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "add" => {
                let a = params.get("a").and_then(|v| v.as_i64()).ok_or("missing a")?;
                let b = params.get("b").and_then(|v| v.as_i64()).ok_or("missing b")?;
                Ok(json!(a + b))
            }
            "echo" => Ok(params),
            other => Err(format!("unknown method: {other}")),
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock: PathBuf = tmp.path().join("adder.sock");

    let handler = Arc::new(Adder);
    let handle = JsonRpcServer::start(sock.clone(), handler)
        .await
        .expect("start server");

    // 1. Method: `add`. Returns the sum.
    let sum = json_rpc_call(&sock, "adder", "add", json!({"a": 2, "b": 3}))
        .await
        .expect("add call");
    println!("add(2, 3) -> {sum}");
    assert_eq!(sum, json!(5));

    // 2. Method: `echo`. Returns the params verbatim.
    let echoed = json_rpc_call(&sock, "adder", "echo", json!({"hello": "world"}))
        .await
        .expect("echo call");
    println!("echo -> {echoed}");
    assert_eq!(echoed["hello"], "world");

    // 3. Unknown method should produce a server-side error.
    let err = json_rpc_call(&sock, "adder", "no_such_method", json!({}))
        .await
        .unwrap_err();
    println!("no_such_method -> {err}");

    handle.shutdown();
}
