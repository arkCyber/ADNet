//! `a3net-ffi-js` build script.
//!
//! napi-rs requires the `napi_build` helper to wire up
//! the `napi.h` include path so the proc-macros can
//! generate the right ABI. Mirrors iroh-ffi's
//! `iroh-js/build.rs`.

fn main() {
    napi_build::setup()
}
