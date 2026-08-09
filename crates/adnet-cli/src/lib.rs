//! Re-export of the CLI module so library consumers (tests, examples,
//! embedding programs) can re-use the same `clap` parser the `adnet`
//! binary uses, without spawning a child process.

pub mod cli;
pub mod feed_view;
pub mod repl;

pub use cli::{Cli, Cmd};
pub use repl::run as run_repl;
