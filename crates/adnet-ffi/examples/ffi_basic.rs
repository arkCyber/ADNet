//! Minimal example — exercise the always-available `adnet-ffi`
//! public surface (result envelope, status codes, error mapping
//! and a real (non-FFI) hash round-trip mirroring what
//! `adnet_ffi_hash_bytes` does internally).
//!
//! Run with:
//!   cargo run -p adnet-ffi --example ffi_basic

use adnet_ffi::{
    AdnetFfiError, ADNET_FFI_E_INVALID_ARG, ADNET_FFI_E_UTF8, ADNET_FFI_OK, FfiResult,
};

fn main() {
    println!("=== adnet-ffi basic example ===");

    // ─── 1. Status code stability ────────────────────────────────
    // The embedder (Swift / Kotlin) may switch on these codes;
    // pin the values down so a stray `derive(Enum)` reorder
    // breaks the build, not the mobile app.
    let invalid_arg = AdnetFfiError::InvalidArg("missing nodeId".into()).status();
    let bad_utf8 = AdnetFfiError::Utf8("0xFF byte".into()).status();
    let bad_json = AdnetFfiError::Json("trailing comma".into()).status();
    assert_eq!(invalid_arg, ADNET_FFI_E_INVALID_ARG);
    assert_eq!(bad_utf8, ADNET_FFI_E_UTF8);
    assert_eq!(bad_json, -3);
    assert_eq!(ADNET_FFI_OK, 0);
    println!("status codes:  ok={ADNET_FFI_OK}  invalid={invalid_arg}  utf8={bad_utf8}  json={bad_json}");

    // ─── 2. JSON envelope ────────────────────────────────────────
    // `FfiResult::ok(value)` always serialises with a `value`
    // field; `FfiResult::err(e)` always emits the human-readable
    // error. The mobile caller decodes one struct.
    let ok: FfiResult<&str> = FfiResult::ok("hello embedder");
    let err: FfiResult<()> = FfiResult::err(AdnetFfiError::Feature("iroh disabled".into()));
    println!(
        "ok envelope    : {}",
        serde_json::to_string(&ok).expect("encode ok")
    );
    println!(
        "err envelope   : {}",
        serde_json::to_string(&err).expect("encode err")
    );
    // `unit_ok()` is the smallest valid response — useful for
    // ack-style calls.
    println!(
        "unit_ok        : {}",
        serde_json::to_string(&FfiResult::<()>::unit_ok()).unwrap()
    );

    // ─── 3. Real BLAKE3 round-trip ───────────────────────────────
    // `adnet_ffi_hash_bytes` on the C side is a thin shim around
    // `adnet_types::ContentHash::from_bytes`. Running the same
    // call from Rust lets the embedder (or a CI golden test)
    // pin the deterministic hash.
    let hash = adnet_types::ContentHash::from_bytes(b"hello embedder");
    let hex = hash.as_hex().to_string();
    let short = hash.short().to_string();
    println!("blake3(\"hello embedder\") = {hex}");
    println!("short hash         : {short}");
    assert_eq!(short.len(), 8);

    // Backwards — `ContentHash::from_hex` rejects malformed
    // inputs before they reach the node layer; the same checks
    // run on both sides of the FFI.
    for bad in ["not 64 chars", &"g".repeat(64), "DEADBEEF"] {
        assert!(
            adnet_types::ContentHash::from_hex(bad).is_err(),
            "should reject {bad:?}"
        );
    }
    println!("malformed-hex rejections: ok");

    // ─── 4. NodeId round-trip (mirrors what the iroh build's ─────
    //     `adnet_ffi_dial` decodes from the embedder's hex)
    let node_id = adnet_types::NodeId::random();
    let hex = node_id.as_hex().to_string();
    let parsed = adnet_types::NodeId::from_hex(&hex).expect("hex parse");
    assert_eq!(parsed.as_hex(), hex);
    println!("node id round-trip: {hex}");

    // ─── 5. Feature gating (so embedders fail fast) ─────────────
    // When the FFI was built without `--features iroh`, the
    // iroh-build-only exports (`adnet_ffi_node_addr`,
    // `adnet_ffi_dial`) are absent. The companion pure-Rust
    // check `adnet_ffi::ADNET_FFI_E_FEATURE` is the documented
    // fallback so the Swift / Kotlin layer can match on a
    // stable constant instead of `dlerror`.
    let feature_err = AdnetFfiError::Feature("iroh transport not built".into());
    assert_eq!(feature_err.status(), -6);
    println!(
        "feature-not-built status = {} (recoverable in embedder)",
        feature_err.status()
    );
}
