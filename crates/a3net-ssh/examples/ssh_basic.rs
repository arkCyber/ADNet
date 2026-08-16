//! Minimal a3net-ssh example.
//!
//! Prints the local SSH-tunnel invitation information and parses a
//! sample `user@<endpoint>` invite token. This is the smallest
//! useful program that exercises the public API without spinning up
//! a real iroh endpoint.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p a3net-ssh --example ssh_basic --features iroh
//! ```
//!
//! Note: this example prints the persistent identity of the host
//! process. In a CI environment, point `ADNET_DATA_DIR` at a tmp
//! directory to avoid leaking the host's identity.

#[cfg(not(feature = "iroh"))]
fn main() {
    eprintln!(
        "a3net-ssh example `ssh_basic` requires the `iroh` feature. \
         Rebuild with `--features iroh`."
    );
}

#[cfg(feature = "iroh")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use a3net_ssh::client::proxy::parse_invite;
    use a3net_ssh::info::render_invite;
    use std::path::PathBuf;

    // 1. Pick a data dir. Use $ADNET_DATA_DIR if set, else a fresh
    //    tmp dir so the example is hermetic.
    let data_dir: PathBuf = std::env::var("ADNET_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!("a3net-ssh-basic-{}", std::process::id()))
        });
    std::fs::create_dir_all(&data_dir)?;

    // 2. Print the invitation banner. With the `iroh` feature
    //    enabled this resolves the persistent identity.
    let banner = render_invite(&data_dir)?;
    println!("{banner}");

    // 3. Parse a synthetic invite token. The endpoint id is a
    //    real-format placeholder (64 hex chars) — it might not be
    //    a reachable peer, but it parses cleanly.
    let demo_token =
        "alice@0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    match parse_invite(demo_token) {
        Ok(parsed) => println!(
            "Parsed invite: user={}, endpoint_id={}",
            parsed.user, parsed.endpoint_id
        ),
        Err(e) => println!("Invite parse error: {e}"),
    }

    // 4. Show the negative path — a malformed token.
    let bad = "no-at-sign-here";
    match parse_invite(bad) {
        Ok(_) => println!("unexpected: {bad} parsed successfully"),
        Err(e) => println!("Rejecting malformed token `{bad}`: {e}"),
    }

    Ok(())
}
