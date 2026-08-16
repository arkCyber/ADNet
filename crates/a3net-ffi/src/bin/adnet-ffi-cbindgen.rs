//! `a3net-ffi-cbindgen` — administrative binary that
//! regenerates the C header from the Rust source.
//!
//! Mirrors iroh-ffi's `make_kotlin.sh` (which calls `uniffi
//! bindgen`); we keep the same pattern for the C header so
//! developers don't have to install `cbindgen` separately.
//!
//! Usage:
//!
//! ```bash
//! cargo run -p a3net-ffi --bin a3net-ffi-cbindgen --
//!     --config crates/a3net-ffi/cbindgen.toml
//!     --output crates/a3net-ffi/include/a3net_ffi.h
//! ```
//!
//! The binary is a thin wrapper around `cbindgen`'s Rust API
//! so the project can pin both the version and the config
//! file without depending on a system-installed `cbindgen`
//! binary.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut config_path = PathBuf::from("crates/a3net-ffi/cbindgen.toml");
    let mut output_path = PathBuf::from("crates/a3net-ffi/include/a3net_ffi.h");
    let mut crate_dir = PathBuf::from("crates/a3net-ffi");
    let mut verify = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" if i + 1 < args.len() => {
                config_path = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--output" if i + 1 < args.len() => {
                output_path = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--crate-dir" if i + 1 < args.len() => {
                crate_dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--verify" => {
                verify = true;
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("a3net-ffi-cbindgen: unknown flag `{other}`");
                print_help();
                return ExitCode::from(2);
            }
        }
    }

    // The `cbindgen` crate is a build-time tool; pull it in
    // here rather than as a regular dependency so a
    // `cargo build -p a3net-ffi` doesn't incur the extra
    // compile cost.
    let config = match cbindgen::Config::from_file(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "a3net-ffi-cbindgen: failed to load config from {}: {e}",
                config_path.display()
            );
            return ExitCode::from(1);
        }
    };

    let result = cbindgen::generate_with_config(&crate_dir, config);
    let bindings = match result {
        Ok(b) => b,
        Err(e) => {
            eprintln!("a3net-ffi-cbindgen: generation failed: {e:?}");
            return ExitCode::from(1);
        }
    };

    if verify {
        // `--verify` semantics: re-generate into an in-memory
        // buffer, compare with the on-disk file, fail if they
        // differ. We use `Vec<u8>` as the buffer because the
        // `Bindings` type doesn't expose a `to_string()` method.
        let mut generated = Vec::new();
        bindings.write(&mut generated);
        let generated_str = match std::str::from_utf8(&generated) {
            Ok(s) => s.to_string(),
            Err(e) => {
                eprintln!(
                    "a3net-ffi-cbindgen: generated header is not valid UTF-8: {e}"
                );
                return ExitCode::from(1);
            }
        };
        let existing = match std::fs::read_to_string(&output_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "a3net-ffi-cbindgen: cannot read {}: {e}",
                    output_path.display()
                );
                return ExitCode::from(1);
            }
        };
        if generated_str != existing {
            eprintln!(
                "a3net-ffi-cbindgen: drift between Rust source and {}.\n\
                 Re-run `cargo make bindgen` (or `cargo run -p a3net-ffi \n\
                 --bin a3net-ffi-cbindgen`) to regenerate.",
                output_path.display()
            );
            return ExitCode::from(1);
        }
        println!("a3net-ffi-cbindgen: header at {} is up-to-date", output_path.display());
        return ExitCode::SUCCESS;
    }

    if !bindings.write_to_file(&output_path) {
        eprintln!(
            "a3net-ffi-cbindgen: failed to write {}",
            output_path.display()
        );
        return ExitCode::from(1);
    }
    println!("a3net-ffi-cbindgen: wrote {}", output_path.display());
    ExitCode::SUCCESS
}

fn print_help() {
    eprintln!(
        "a3net-ffi-cbindgen — regenerate the C header from the Rust source.\n\
         \n\
         USAGE:\n    \
         cargo run -p a3net-ffi --bin a3net-ffi-cbindgen -- [FLAGS]\n\
         \n\
         FLAGS:\n    \
         --config <PATH>      cbindgen.toml path.\n                              \
         default: crates/a3net-ffi/cbindgen.toml\n    \
         --crate-dir <PATH>    directory containing the crate's Cargo.toml.\n     \
         default: crates/a3net-ffi\n    \
         --output <PATH>       header output path.\n                            \
         default: crates/a3net-ffi/include/a3net_ffi.h\n    \
         --verify              exit non-zero if the on-disk header drifts.\n  \
         -h, --help            print this help."
    );
}
