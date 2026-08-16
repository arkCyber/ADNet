//! `a3net-ffi` cross-language integration smoke test.
//!
//! This is the **headline** integration test for the FFI
//! surface. It mirrors iroh-ffi's `iroh-ffi/tests/ffi.rs`:
//! a real C compiler drives the public `a3net_ffi.h`
//! header against the compiled `liba3net_ffi.{a,so,dylib,
//! lib}` binary, exercising the ABI contract from a
//! non-Rust language.
//!
//! Why a C test, not a Rust test?
//!
//! - It catches **linker-level** problems (missing
//!   `#[no_mangle]`, wrong name mangling, missed
//!   `extern "C"`).
//! - It catches **header drift** — we re-read the header
//!   in the C file, so any field rename or constant
//!   change in Rust is reflected here.
//! - It catches **JSON shape drift** — the C test parses
//!   the response body with `strstr` so a rename in the
//!   Rust `FfiResult::ok` serialisation shows up here.
//!
//! We don't run the C test in the default `cargo test`
//! run because it requires a working C compiler. To opt
//! in, set `ADNET_FFI_C_SMOKE=1`.
//!
//! ## CI
//!
//! The `.github/workflows/ff.yml` matrix sets the env
//! var on the `cbindgen` job; passing it through to
//! `cargo test --test c_smoke` runs the C test on every
//! supported platform.

#[cfg(test)]
mod tests {
    use std::env;
    use std::path::PathBuf;
    use std::process::Command;

    /// Compile the C smoke test against the freshly-built
    /// `liba3net_ffi` and execute it. The test is gated on
    /// `ADNET_FFI_C_SMOKE=1` so the default Rust CI stays
    /// fast.
    #[test]
    fn c_smoke_test() {
        if env::var("ADNET_FFI_C_SMOKE").as_deref() != Ok("1") {
            eprintln!("SKIPPED: set ADNET_FFI_C_SMOKE=1 to run the C smoke test");
            return;
        }

        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        let header = manifest_dir.join("include").join("a3net_ffi.h");
        let c_src = manifest_dir.join("tests").join("c_smoke.c");

        assert!(header.exists(), "missing header at {}", header.display());
        assert!(c_src.exists(), "missing C source at {}", c_src.display());

        // Re-build the crate so the .a / .so is up to date.
        let build = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()))
            .args(["build", "--quiet"])
            .current_dir(&manifest_dir)
            .status()
            .expect("failed to invoke cargo build");
        assert!(build.success(), "cargo build failed");

        // Locate the library. Cargo places it in
        // `target/<debug|release>/`. The test runner's
        // OUT_DIR is `target/debug/deps/`, so we strip
        // the last two segments.
        let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
        let target_dir = manifest_dir
            .parent() // crates/
            .and_then(|p| p.parent()) // repo root
            .map(|p| p.join("target").join(profile))
            .expect("failed to locate target/");

        let lib_name = if cfg!(target_os = "windows") {
            "a3net_ffi.dll"
        } else if cfg!(target_os = "macos") {
            "liba3net_ffi.dylib"
        } else {
            "liba3net_ffi.so"
        };
        let lib_path = target_dir.join(lib_name);
        assert!(
            lib_path.exists(),
            "expected library at {} (run `cargo build` first)",
            lib_path.display()
        );

        // Compile the C source.
        let cc = env::var("CC").unwrap_or_else(|_| "cc".to_string());
        let bin_path = target_dir.join("a3net_ffi_c_smoke");
        let compile = Command::new(&cc)
            .arg(&c_src)
            .arg("-I")
            .arg(manifest_dir.join("include"))
            .arg("-L")
            .arg(&target_dir)
            .arg("-la3net_ffi")
            .arg("-lpthread")
            .arg("-ldl")
            .arg("-lm")
            .arg("-o")
            .arg(&bin_path)
            .status()
            .expect("failed to invoke cc");
        assert!(compile.success(), "cc failed to compile c_smoke.c");

        // On Linux, the runtime linker needs to find the
        // shared library; point LD_LIBRARY_PATH at the
        // target dir. On macOS, DYLD_LIBRARY_PATH.
        let libpath = target_dir.to_string_lossy().to_string();
        let run = Command::new(&bin_path)
            .env(if cfg!(target_os = "macos") {
                "DYLD_LIBRARY_PATH"
            } else {
                "LD_LIBRARY_PATH"
            }, &libpath)
            .status()
            .expect("failed to run c_smoke binary");
        assert!(run.success(), "c_smoke binary exited with {:?}", run.code());
    }

    /// Pin the C smoke test's expectations to the public
    /// header. A rename of `a3net_ffi_node_id` in
    /// `src/lib.rs` would break the C test, which is
    /// the point: this is the second line of defence
    /// after cbindgen's `--verify`.
    #[test]
    fn header_pin_node_id_symbol() {
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        let header = manifest_dir.join("include").join("a3net_ffi.h");
        let body = std::fs::read_to_string(&header)
            .unwrap_or_else(|e| panic!("cannot read header: {e}"));
        // Once the header is generated, the symbol must
        // be present. If the comment `node_id` slot is
        // empty, the generator failed.
        assert!(
            body.contains("a3net_ffi_node_id"),
            "header missing a3net_ffi_node_id: header is stale or cbindgen didn't run"
        );
    }
}
