//! Realistic adnet-ssh app example.
//!
//! Walks through how an ADNet node would:
//!
//! 1. Build an ephemeral iroh `IrohSsh` endpoint (with an explicit
//!    `SecretKey` so the demo never touches the host's persistent
//!    identity).
//! 2. Print the invitation a friend would dial.
//! 3. Parse the printed invitation back and confirm the user /
//!    endpoint id round-trip through the parser unchanged.
//! 4. Tear down the endpoint cleanly.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p adnet-ssh --example ssh_app --features iroh
//! ```
//!
//! This example does NOT bind a real SSH-tunnel server (that would
//! require `sshd` running on the host); it only demonstrates the
//! builder + invitation plumbing.

#[cfg(not(feature = "iroh"))]
fn main() {
    eprintln!(
        "adnet-ssh example `ssh_app` requires the `iroh` feature. \
         Rebuild with `--features iroh`."
    );
}

#[cfg(feature = "iroh")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use adnet_ssh::client::proxy::parse_invite;
    use adnet_ssh::IrohSshBuilder;
    use iroh::SecretKey;

    // 1. Generate a fresh ephemeral secret key so the example never
    //    touches the host's persistent identity. Production code
    //    would omit `.secret_key(...)` and let the builder load
    //    (or create) the on-disk identity.
    let ephemeral = SecretKey::generate();

    // 2. Build the endpoint. We do NOT call `.accept_incoming(true)`
    //    because sshd would be required for the pre-flight probe;
    //    this example only exercises the builder + invitation path.
    let ssh = IrohSshBuilder::new(std::env::temp_dir())
        .accept_incoming(false)
        .accept_port(22)
        .secret_key(ephemeral)
        .build()
        .await?;

    // 3. Render the invitation that a friend would dial.
    let banner = adnet_ssh::info::render_invite(std::env::temp_dir().as_path())?;
    println!("{banner}");

    // 4. Build an invitation string `user@<endpoint-id>` and round
    //    trip it through the parser.
    let endpoint_id = ssh.endpoint().id();
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "alice".to_string());
    let invite = format!("{user}@{endpoint_id}");
    println!("Generated invitation: {invite}");

    let parsed = parse_invite(&invite)?;
    assert_eq!(parsed.user, user);
    assert_eq!(parsed.endpoint_id, endpoint_id);
    println!(
        "Round-trip OK: user={}, endpoint_id={}",
        parsed.user, parsed.endpoint_id
    );

    // 5. Print the underlying ALPN for diagnostic purposes.
    println!("Tunnel ALPN: {}", adnet_ssh::builder::SSH_TUNNEL_ALPN.len());
    println!("ALPN bytes: {:?}", adnet_ssh::builder::SSH_TUNNEL_ALPN);

    // 6. Endpoint is dropped here. The `Arc<Endpoint>` is reference
    //    counted; the iroh runtime will close the underlying sockets
    //    once the last reference is gone.
    drop(ssh);
    println!("Endpoint closed.");

    Ok(())
}
