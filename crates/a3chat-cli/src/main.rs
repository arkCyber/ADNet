//! `a3chat` CLI entry point. Defers everything to [`a3chat_cli::run`].

use a3chat_cli::{run, Cli};
use clap::Parser;
use std::process::ExitCode;

/// Top-level entry. We use `tokio::main` so subcommands can `await`
/// freely; signals are handled by the runtime.
#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    // Tracing init is best-effort; never panic the CLI if it fails.
    init_tracing(cli.verbose);
    run(cli).await
}

/// Initialize tracing with the requested verbosity. Honors
/// `RUST_LOG` if set; otherwise uses the `-v` count.
fn init_tracing(verbosity: u8) {
    use tracing_subscriber::{fmt, EnvFilter};
    let default = match verbosity {
        0 => "a3chat_cli=warn",
        1 => "a3chat_cli=info",
        2 => "a3chat_cli=debug",
        _ => "a3chat_cli=trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = fmt().with_env_filter(filter).try_init();
}