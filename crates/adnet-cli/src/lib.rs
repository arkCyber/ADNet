//! Re-export of the CLI module so library consumers (tests, examples,
//! embedding programs) can re-use the same `clap` parser the `adnet`
//! binary uses, without spawning a child process.

pub mod cli;
pub mod file_ops;
pub mod feed_view;
pub mod repl;
pub mod routing_ops;

pub use cli::{Cli, Cmd, DhtExtraCmd, PinCmd, RepoCmd, RoutingCmd, SwarmCmd};
pub use repl::run as run_repl;
