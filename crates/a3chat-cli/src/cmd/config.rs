//! `a3chat config …` — inspect resolved configuration.

use clap::Subcommand;

use crate::config::CliConfig;
use crate::error::CliResult;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Print the resolved config as TOML.
    Show,
    /// Print the platform-default config path.
    Path,
}

pub fn run(cmd: ConfigCmd, cfg: &CliConfig) -> CliResult<()> {
    match cmd {
        ConfigCmd::Show => show(cfg),
        ConfigCmd::Path => path(cfg),
    }
}

fn show(cfg: &CliConfig) -> CliResult<()> {
    // Render as JSON for round-trippability across formats.
    let v = serde_json::json!({
        "daemon_url": cfg.effective_daemon_url(),
        "owner": cfg.effective_owner(),
        "output": format!("{:?}", cfg.effective_output()).to_lowercase(),
        "retries": cfg.effective_retries(),
        "timeout_ms": cfg.effective_timeout_ms(),
    });
    output::print(crate::config::OutputFormat::Json, &v)
}

fn path(_cfg: &CliConfig) -> CliResult<()> {
    let p = crate::config::default_config_path()?;
    println!("{}", p.display());
    Ok(())
}