// SPDX-License-Identifier: MIT OR Apache-2.0
//
// `a3net-fuzz` support library.
//
// The real libfuzzer entrypoints live in `src/fuzz_main.rs` (which is
// `#![no_main]` and therefore incompatible with `cargo test`). This
// file exists so the crate has a regular library target that the
// workspace can `cargo check` against without trying to link the
// no-main entrypoint as a unit-test binary.
//
// It re-exports nothing today; fuzz targets import the workspace
// crates directly (`use a3net_types::...`). If shared helpers are
// needed in the future they should land here.

#![allow(dead_code)]