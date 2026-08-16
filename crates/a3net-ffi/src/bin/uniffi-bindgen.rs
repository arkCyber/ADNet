// uniffi-bindgen binary — minimal Rust wrapper that delegates
// to `uniffi::uniffi_bindgen_main()`.
//
// Mirrors iroh-ffi's `uniffi-bindgen.rs` (48 bytes). The bin
// lets us run `cargo run -p a3net-ffi --bin uniffi-bindgen --features uniffi --
// uniffi generate src/a3net.udl --language <swift|kotlin|python>
// --out-dir <bindings-dir>` without dragging the
// `uniffi_bindgen` CLI crate into the workspace as a member.
//
// Built only when the `uniffi` feature is enabled — the
// default build (no uniffi) doesn't pull in the bindgen tool.

#[cfg(feature = "uniffi")]
fn main() {
    uniffi::uniffi_bindgen_main()
}

#[cfg(not(feature = "uniffi"))]
fn main() {
    eprintln!("a3net-ffi: this binary requires the `uniffi` feature.");
    eprintln!("Re-run with: cargo run -p a3net-ffi --features uniffi --bin uniffi-bindgen -- <args>");
    std::process::exit(2);
}
