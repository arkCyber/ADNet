//! Minimal example: spin up a `WebdavServer` on a free port against
//! an in-memory NAS namespace, register a read capability, sign a
//! capability token with the same HMAC key the server uses, and
//! verify it round-trips through `Authorization: Capability …`.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-webdav --example webdav_basic
//! ```

use adnet_blobstore::Nas;
use adnet_pairing::CapabilitySet;
use adnet_webdav::{
    CapabilityToken, HandlerState, ResolvedCapability, StaticCapabilityResolver, TokenVerifier,
    WebdavConfig, WebdavServer,
};
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let nas = Nas::open(dir.path())?;

    // Wire a static capability resolver and a single HMAC-SHA256 key.
    let resolver = Arc::new(StaticCapabilityResolver::new());
    let key = [0xAB; 32];
    resolver.register(
        "device-1".into(),
        ResolvedCapability {
            caps: CapabilitySet::from_names(["files.read"]),
            nonce: [1u8; 32],
            expires_unix_ms: 9_999_999_999_999,
            revoked: false,
        },
    );

    // Keep the verifier around so we can sign tokens with the same
    // HMAC key the server validates against.
    let verifier = TokenVerifier::new(key);
    let signing_verifier = verifier.clone();
    let state = Arc::new(HandlerState::new(nas, resolver, verifier));

    // Pick a free port so the example doesn't collide with anything.
    let config = WebdavConfig {
        host: "127.0.0.1".into(),
        port: 0,
    };
    let server = WebdavServer::new(config, state);
    let handle = server.start().await?;
    let addr = handle.local_addr();
    println!("WebDAV listening on http://{addr}");

    // Sign a token with the same key and round-trip through the
    // `Authorization: Capability <b64url>` header.
    let token: CapabilityToken = signing_verifier.sign("device-1", [0xCC; 32], 9_999_999_999_999);
    let header = token.to_header();
    let parsed = CapabilityToken::from_header(&header)?;
    println!(
        "parsed token: id={} nonce={}",
        parsed.capability_id,
        hex::encode(parsed.nonce)
    );
    signing_verifier.verify(&parsed)?;
    println!("token verified ok");

    handle.shutdown();
    Ok(())
}