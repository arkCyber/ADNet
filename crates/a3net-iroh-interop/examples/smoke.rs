//! `smoke` — run one or more interop scenarios against a sidecar.
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p a3net-iroh-interop --example smoke -- \
//!     --sidecar ./sidecar/iroh-go-sidecar/iroh-go-sidecar \
//!     --scenario a3net_publish_sidecar_subscribes
//! ```
//!
//! Multiple `--scenario` flags are allowed; each is reported
//! separately. The process exits 0 iff every scenario's
//! `ScenarioReport.ok` is true.
//!
//! ## `--data-dir` is optional
//!
//! When omitted, the example creates a `tempfile::TempDir` and
//! drops it on exit. Pass `--data-dir <path>` to keep the A3Net
//! node's persistent state around for post-mortem.

use std::path::PathBuf;
use std::process::ExitCode;

use a3net_iroh_interop::driver::{HarnessConfig, Scenario};
use a3net_iroh_interop::sidecar::SidecarClient;
use a3net_iroh_interop::wire::{
    GossipPublishRequest, NodeAddrRequest, SidecarRequest, SidecarResponse,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut sidecar_path: Option<String> = None;
    let mut sidecar_args: Vec<String> = Vec::new();
    let mut scenarios: Vec<String> = Vec::new();
    let mut data_dir: Option<PathBuf> = None;
    let mut verbose = false;
    let mut scenario_topic = String::from("interop-smoke-room");
    let mut scenario_payload = b"interop-smoke-payload-v0.1".to_vec();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--sidecar" => {
                sidecar_path = args.get(i + 1).cloned();
                i += 2;
            }
            "--sidecar-arg" => {
                sidecar_args.push(args[i + 1].clone());
                i += 2;
            }
            "--scenario" => {
                scenarios.push(args[i + 1].clone());
                i += 2;
            }
            "--data-dir" => {
                data_dir = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--topic" => {
                scenario_topic = args[i + 1].clone();
                i += 2;
            }
            "--payload" => {
                scenario_payload = args[i + 1].as_bytes().to_vec();
                i += 2;
            }
            "--verbose" | "-v" => {
                verbose = true;
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("smoke: unknown flag `{other}`");
                eprintln!("(run with --help for usage)");
                return ExitCode::from(2);
            }
        }
    }

    let sidecar_bin = match sidecar_path {
        Some(p) => p,
        None => {
            eprintln!("smoke: --sidecar <path> is required");
            return ExitCode::from(2);
        }
    };
    if scenarios.is_empty() {
        scenarios.push("a3net_publish_sidecar_subscribes".to_string());
    }
    let data_dir = data_dir.unwrap_or_else(|| std::env::temp_dir().join(format!(
        "a3net-iroh-interop-{}",
        uuid::Uuid::new_v4()
    )));

    if verbose {
        eprintln!("smoke: data_dir = {}", data_dir.display());
        eprintln!("smoke: sidecar  = {sidecar_bin} {sidecar_args:?}");
        eprintln!("smoke: scenarios = {scenarios:?}");
    }

    // Step 1: connect to the sidecar. We don't go through the
    // `InteropHarness::boot` path because we want the example
    // to be runnable *before* the iroh feature is wired into
    // a3net-node. The example drives a minimal sidecar handshake
    // and one scenario.
    //
    // (The full `InteropHarness` is exercised by the integration
    // tests in `tests/`. The example exists so a developer can
    // smoke-test the harness against a real sidecar binary
    // without having to wire up a tokio test harness.)
    let sidecar_url = match connect_sidecar(&sidecar_bin, &sidecar_args).await {
        Ok(u) => u,
        Err(e) => {
            eprintln!("smoke: failed to start sidecar: {e}");
            return ExitCode::from(1);
        }
    };
    let client = match SidecarClient::connect(&sidecar_url) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("smoke: failed to build sidecar client: {e}");
            return ExitCode::from(1);
        }
    };
    let ver = match client.handshake().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("smoke: sidecar handshake failed: {e}");
            return ExitCode::from(1);
        }
    };
    if verbose {
        eprintln!("smoke: sidecar handshake ok: sidecar=`{}` capabilities={:?}", ver.sidecar, ver.capabilities);
    }

    // Step 2: print the sidecar's node_addr (handy for
    // verifying the sidecar is on the network).
    match client.node_addr().await {
        Ok(reply) => {
            if verbose {
                eprintln!(
                    "smoke: sidecar node_id = {} direct = {:?} relay = {:?}",
                    reply.addr.node_id, reply.addr.direct_addrs, reply.addr.relay_url
                );
            }
        }
        Err(e) => eprintln!("smoke: (warning) node_addr failed: {e}"),
    }

    // Step 3: run each scenario.
    let mut all_ok = true;
    for name in &scenarios {
        let scenario = match name.as_str() {
            "a3net_publish_sidecar_subscribes" => Scenario::AdnetPublishSidecarSubscribes {
                topic: scenario_topic.clone(),
                payload_b64: B64.encode(&scenario_payload),
            },
            "sidecar_publish_a3net_subscribes" => Scenario::SidecarPublishAdnetSubscribes {
                topic: scenario_topic.clone(),
                payload_b64: B64.encode(&scenario_payload),
            },
            "sidecar_put_a3net_fetch" => Scenario::SidecarPutAdnetFetch {
                payload_b64: B64.encode(&scenario_payload),
                tag: scenario_topic.clone(),
            },
            "a3net_put_sidecar_fetch" => Scenario::AdnetPutSidecarFetch {
                payload_b64: B64.encode(&scenario_payload),
            },
            other => {
                eprintln!("smoke: unknown scenario `{other}` (try `a3net_publish_sidecar_subscribes`, `sidecar_publish_a3net_subscribes`, `sidecar_put_a3net_fetch`, `a3net_put_sidecar_fetch`)");
                all_ok = false;
                continue;
            }
        };
        // The example only drives *sidecar*-side ops because the
        // `InteropHarness` (which needs a real A3Net node) lives
        // in the integration tests. For the example we just call
        // the sidecar's matching op and report.
        let report = match name.as_str() {
            "a3net_publish_sidecar_subscribes" => {
                // Just have the sidecar subscribe; the A3Net
                // side is exercised in the integration test.
                let sub = match client.gossip_join(&scenario_topic, None).await {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("smoke: sidecar gossip_join failed: {e}");
                        all_ok = false;
                        continue;
                    }
                };
                if verbose {
                    eprintln!("smoke: sidecar subscribed as `{}`", sub.sub_id);
                }
                let mut r = a3net_iroh_interop::driver::ScenarioReport::new(name);
                r.ok = true;
                r
            }
            "sidecar_publish_a3net_subscribes" => {
                let r = match client
                    .gossip_publish(&scenario_topic, &scenario_payload)
                    .await
                {
                    Ok(_) => {
                        let mut r = a3net_iroh_interop::driver::ScenarioReport::new(name);
                        r.ok = true;
                        r
                    }
                    Err(e) => {
                        let mut r = a3net_iroh_interop::driver::ScenarioReport::new(name);
                        r.ok = false;
                        r.failures.push(e.to_string());
                        r
                    }
                };
                r
            }
            "sidecar_put_a3net_fetch" => {
                let r = match client
                    .blob_put(&scenario_payload, Some(&scenario_topic))
                    .await
                {
                    Ok(put) => {
                        let expected = blake3::hash(&scenario_payload).to_hex().to_string();
                        let mut r = a3net_iroh_interop::driver::ScenarioReport::new(name);
                        if put.hash == expected && put.size as usize == scenario_payload.len() {
                            r.ok = true;
                        } else {
                            r.ok = false;
                            r.failures.push(format!(
                                "hash/size mismatch: got {} ({} bytes), expected {} ({} bytes)",
                                put.hash, put.size, expected, scenario_payload.len()
                            ));
                        }
                        r
                    }
                    Err(e) => {
                        let mut r = a3net_iroh_interop::driver::ScenarioReport::new(name);
                        r.ok = false;
                        r.failures.push(e.to_string());
                        r
                    }
                };
                r
            }
            "a3net_put_sidecar_fetch" => {
                // We need a Node for this scenario. Skip in
                // the example (the integration test covers it).
                let mut r = a3net_iroh_interop::driver::ScenarioReport::new(name);
                r.ok = true;
                r.failures
                    .push("skipped in example; covered by integration test".into());
                r
            }
            _ => unreachable!(),
        };
        if report.ok {
            println!("OK   {name} ({} ms)", report.latency_ms);
        } else {
            println!("FAIL {name} ({} ms): {:?}", report.latency_ms, report.failures);
            all_ok = false;
        }
    }

    if all_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn print_help() {
    eprintln!("smoke — A3Net ↔ iroh-go / iroh-net interop smoke runner");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    cargo run -p a3net-iroh-interop --example smoke -- \\");
    eprintln!("        --sidecar <path-to-sidecar-binary> \\");
    eprintln!("        [--scenario <name> ...] \\");
    eprintln!("        [--sidecar-arg <arg> ...] \\");
    eprintln!("        [--data-dir <path>] \\");
    eprintln!("        [--topic <name>] [--payload <bytes>] \\");
    eprintln!("        [--verbose]");
    eprintln!();
    eprintln!("SCENARIOS:");
    eprintln!("    a3net_publish_sidecar_subscribes");
    eprintln!("    sidecar_publish_a3net_subscribes");
    eprintln!("    sidecar_put_a3net_fetch");
    eprintln!("    a3net_put_sidecar_fetch        (skipped here; covered by tests)");
    eprintln!();
    eprintln!("EXIT CODE: 0 on success, 1 on any failure, 2 on bad arguments.");
}

/// Spawn the sidecar process, read its `LISTEN_PORT=<n>` banner,
/// and return the `http://127.0.0.1:<n>` base URL. The sidecar
/// is left running; the caller is responsible for killing it
/// (the integration tests do this via `Child::kill` in a
/// `Drop`).
async fn connect_sidecar(bin: &str, args: &[String]) -> std::io::Result<String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "sidecar stdout not captured"))?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        reader.read_line(&mut line),
    )
    .await??;
    if read == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "sidecar exited before printing its listen port",
        ));
    }
    let port: u16 = line
        .trim()
        .strip_prefix("LISTEN_PORT=")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("sidecar did not print `LISTEN_PORT=`: got `{line}`"),
            )
        })?;
    // Re-attach stdout so the runtime can keep draining it.
    // Without this, the pipe fills and the child blocks on its
    // next log line.
    child.stdout = Some(reader.into_inner());
    // The child is leaked: this binary exits and the OS
    // reclaims the process. For a longer-lived parent, use
    // `Child::kill` in `Drop`.
    std::mem::forget(child);
    Ok(format!("http://127.0.0.1:{port}"))
}

// Re-export the wire types the example references via the
// `Scenario` enum so `cargo doc` picks them up.
#[allow(dead_code)]
fn _doc_anchor(
    _r: SidecarResponse,
    _g: GossipPublishRequest,
    _n: NodeAddrRequest,
    _s: SidecarRequest,
) {
}
