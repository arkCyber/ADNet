//! `a3net-ffi` build script.
//!
//! Two responsibilities:
//!
//! 1. **cbindgen** — when `ADNET_FFI_REGEN_HEADER=1` is set in
//!    the environment, regenerate `include/a3net_ffi.h` from
//!    the Rust source. The default `cargo build` skips this
//!    step so a developer can compile without installing
//!    `cbindgen`.
//!
//! 2. **uniffi scaffolding** — when the `uniffi` feature is
//!    enabled, emit a `cargo:rerun-if-changed` directive for
//!    `src/a3net.udl` so the uniffi-build step re-runs when the
//!    interface definition changes.
//!
//! Mirrors the iroh-ffi pattern (separate `build.rs` for the
//! `iroh.pc` pkg-config generator + `uniffi` build dependency).
//!
//! ## CI integration
//!
//! The `ci_ffi_headers.yml` workflow runs:
//!
//!   ```bash
//!   cargo install cbindgen --locked
//!   cbindgen --config cbindgen.toml --crate a3net-ffi \
//!       --output include/a3net_ffi.h --verify
//!   ```
//!
//! `--verify` makes cbindgen exit with a non-zero status when
//! the checked-in header does not match the source.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=src/blob_ffi.rs");
    println!("cargo:rerun-if-changed=src/gossip_ffi.rs");
    println!("cargo:rerun-if-changed=src/dial_ffi.rs");
    println!("cargo:rerun-if-changed=src/doctor_ffi.rs");
    println!("cargo:rerun-if-changed=src/news_ffi.rs");
    println!("cargo:rerun-if-changed=src/uniffi_surface.rs");
    println!("cargo:rerun-if-changed=src/version.rs");
    println!("cargo:rerun-if-changed=src/a3net.udl");

    if env::var("ADNET_FFI_REGEN_HEADER").as_deref() == Ok("1") {
        regen_header();
    }
}

/// Run `cbindgen` against the current `lib.rs` and emit
/// `include/a3net_ffi.h`. Fails the build if `cbindgen` is not
/// installed or if the resulting header would change.
fn regen_header() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_path = crate_dir.join("include").join("a3net_ffi.h");
    let config = crate_dir.join("cbindgen.toml");

    let status = Command::new("cbindgen")
        .arg("--config")
        .arg(&config)
        .arg("--crate")
        .arg("a3net-ffi")
        .arg("--output")
        .arg(&out_path)
        .status()
        .expect("cbindgen not installed — run `cargo install cbindgen --locked`");

    if !status.success() {
        panic!(
            "cbindgen failed (exit {:?}); header was NOT regenerated.\n\
             Check the stderr above for the source-level error.",
            status.code()
        );
    }
    println!(
        "cargo:warning=a3net_ffi.h regenerated at {}",
        out_path.display()
    );
}
