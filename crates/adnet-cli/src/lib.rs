//! Re-export of the CLI module so library consumers (tests, examples,
//! embedding programs) can re-use the same `clap` parser the `adnet`
//! binary uses, without spawning a child process.

pub mod bandwidth;
pub mod bitswap_ops;
pub mod bytes;
pub mod channel_ops;
pub mod cli;
pub mod config;
pub mod config_wizard;
pub mod diagnostics;
pub mod dht_cli;
pub mod feed_view;
pub mod file_ops;
pub mod ipns_ops;
pub mod mdns;
pub mod moments;
pub mod news;
pub mod pairing_ops;
pub mod profile;
pub mod repl;
pub mod roster;
pub mod routing_ops;
pub mod share;
pub mod storage;
pub mod status;
pub mod userstore;
pub mod webhook_ops;

pub use cli::{
    BitswapCmd, ChannelCmd, ConfigCmd, DhtExtraCmd, InviteCmd, KeyCmd, MdnsCmd, MeshCmd,
    MomentsCmd, NameCmd, NewsCmd, PairCmd, PinCmd, ProfileCmd, QrCmd, RepoCmd, RosterCmd,
    RoutingCmd, ShareCmd, ShareResumeCmd, StorageCmd, SwarmCmd, UserCmd, WebhookCmd, Cli, Cmd,
};
pub use pairing_ops::{run_invite, run_mesh, run_pair, run_qr};
pub use repl::run as run_repl;
