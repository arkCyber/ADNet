//! Realistic example: end-to-end ACL gate. Register two
//! capabilities (one read-only, one read-write), authorise several
//! verbs against the AclMiddleware, and confirm the DAL-A decision
//! matrix holds (read → Allow, write on read-only → Deny, missing
//! cred → Unauthorized).
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-webdav --example webdav_app
//! ```

use adnet_pairing::CapabilitySet;
use adnet_webdav::{
    AclDecision, AclMiddleware, CapabilityToken, ResolvedCapability, StaticCapabilityResolver,
    TokenVerifier,
};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Stand up the resolver with two devices:
    //    - `reader` has files.read only.
    //    - `writer` has files.read + files.write.
    let resolver = Arc::new(StaticCapabilityResolver::new());
    resolver.register(
        "reader".into(),
        ResolvedCapability {
            caps: CapabilitySet::from_names(["files.read"]),
            nonce: [0x10; 32],
            expires_unix_ms: 9_999_999_999_999,
            revoked: false,
        },
    );
    resolver.register(
        "writer".into(),
        ResolvedCapability {
            caps: CapabilitySet::from_names(["files.read", "files.write"]),
            nonce: [0x20; 32],
            expires_unix_ms: 9_999_999_999_999,
            revoked: false,
        },
    );

    // 2. ACL gate.
    let mw = AclMiddleware::new(resolver.clone());

    let files_read = CapabilitySet::from_names(["files.read"]);
    let files_write = CapabilitySet::from_names(["files.write"]);

    let d1 = mw.authorise(Some("reader"), "get", &files_read);
    let d2 = mw.authorise(Some("reader"), "put", &files_write);
    let d3 = mw.authorise(Some("writer"), "put", &files_write);
    let d4 = mw.authorise(None, "get", &files_read);
    let d5 = mw.authorise(Some("unknown"), "get", &files_read);
    println!("reader GET       = {d1:?}");
    println!("reader PUT       = {d2:?}");
    println!("writer PUT       = {d3:?}");
    println!("anonymous GET    = {d4:?}");
    println!("unknown GET      = {d5:?}");
    assert_eq!(d1, AclDecision::Allow);
    assert!(matches!(d2, AclDecision::Forbidden(_)));
    assert_eq!(d3, AclDecision::Allow);
    assert_eq!(d4, AclDecision::Unauthenticated);
    assert!(matches!(d5, AclDecision::Rejected(_) | AclDecision::Forbidden(_)));

    // 3. Token round-trip with the same HMAC key.
    let verifier = TokenVerifier::new([0x77; 32]);
    let token: CapabilityToken = verifier.sign("writer", [0x55; 32], 9_999_999_999_999);
    let header = token.to_header();
    let parsed = CapabilityToken::from_header(&header)?;
    assert_eq!(parsed.capability_id, "writer");
    assert_eq!(parsed.nonce, [0x55; 32]);
    verifier.verify(&parsed)?;
    println!("writer token round-trip ok");

    // 4. Tampered token fails verification.
    let mut bad = token.clone();
    bad.signature[0] ^= 0x01;
    assert!(verifier.verify(&bad).is_err());
    println!("tampered token rejected");

    Ok(())
}