//! `adnet user …` — offline user-profile subcommands.
//!
//! Mirrors `roster.rs`: each invocation opens the SQLite-backed
//! [`SqliteUserStore`] under `<data_dir>/userstore.sqlite`, runs
//! the operation, and prints either a one-line message or a
//! JSON payload.
//!
//! The same `futures::executor::block_on` boundary is used at
//! the dispatcher in `main.rs`; this module is `async` because
//! the underlying [`UserStore`] trait is `async_trait`-shaped.

use std::io::Read;
use std::path::Path;

use adnet_userstore::{SqliteUserStore, SqliteUserStoreConfig, UserProfile, UserStore};
use anyhow::{Context, Result, bail};

/// Maximum length for a `user_id` supplied on the CLI. The store
/// does not enforce this; we cap early so a 1 MB input is
/// rejected without a SQL round-trip.
const MAX_USER_ID_LEN: usize = 256;

fn validate_cli_user_id(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("user_id must be non-empty");
    }
    if trimmed.len() > MAX_USER_ID_LEN {
        bail!(
            "user_id is {n} bytes; the CLI rejects ids longer than {max}",
            n = trimmed.len(),
            max = MAX_USER_ID_LEN
        );
    }
    Ok(trimmed.to_string())
}

/// Apply `cmd` against the userstore rooted at `data_dir`.
pub async fn run(cmd: &super::cli::UserCmd, data_dir: &Path) -> Result<()> {
    let store = SqliteUserStore::open(SqliteUserStoreConfig::under_app_data(data_dir))
        .context("opening userstore sqlite")?;
    match cmd {
        super::cli::UserCmd::Add { from_file } => {
            let profile = read_profile_from_arg(from_file.as_deref())?;
            if profile.user_id.is_empty() {
                bail!("profile.userId must be non-empty");
            }
            let id = profile.user_id.clone();
            store.put_profile(profile).await?;
            println!("upserted profile {id}");
        }
        super::cli::UserCmd::Show { user_id, json } => {
            let id = validate_cli_user_id(user_id)?;
            let p = store.get_profile(&id).await?;
            match p {
                Some(p) => {
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&p)?);
                    } else {
                        println!("user_id      = {}", p.user_id);
                        println!("username     = {}", p.username);
                        println!("display_name = {}", p.display_name);
                        println!("bio          = {}", p.bio);
                        if let Some(avatar) = p.avatar {
                            println!(
                                "avatar       = {} ({} bytes, {})",
                                avatar.blob_hash, avatar.size_bytes, avatar.mime_type
                            );
                        }
                        println!("prefs.theme  = {}", p.preferences.theme);
                        println!("prefs.locale = {}", p.preferences.locale);
                    }
                }
                None => bail!("user not found: {id}"),
            }
        }
        super::cli::UserCmd::List { json } => {
            let profiles = store.list_profiles().await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&profiles)?);
            } else {
                println!("{:<24} {:<24} {:<24} created_at", "user_id", "username", "display_name");
                for p in profiles {
                    println!(
                        "{:<24} {:<24} {:<24} {}",
                        truncate(&p.user_id, 24),
                        truncate(&p.username, 24),
                        truncate(&p.display_name, 24),
                        p.created_at
                    );
                }
            }
        }
        super::cli::UserCmd::Delete { user_id, yes } => {
            let id = validate_cli_user_id(user_id)?;
            if !yes {
                eprintln!("adnet: about to delete user {id} (cascade clears keys, devices, digit)");
                eprintln!("hint: pass --yes to skip this prompt");
                eprintln!("continue? [y/N]");
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
                    eprintln!("aborted");
                    return Ok(());
                }
            }
            let removed = store.delete_profile(&id).await?;
            println!("deleted user {id} ({removed} rows cleared)");
        }
        super::cli::UserCmd::Digit { user_id } => {
            let id = validate_cli_user_id(user_id)?;
            let digit = store.ensure_user_digit(&id).await?;
            println!("{id} -> {digit}");
        }
        super::cli::UserCmd::Info => {
            let info = store.info();
            println!("backend: {}", info.backend);
            if let Some(loc) = info.location {
                println!("location: {loc}");
            }
            println!("profiles: {}", info.profile_count);
            println!("public keys: {}", info.public_key_count);
            println!("devices: {}", info.device_count);
        }
    }
    Ok(())
}

fn read_profile_from_arg(path: Option<&str>) -> Result<UserProfile> {
    let raw = if let Some(p) = path {
        std::fs::read_to_string(p).with_context(|| format!("reading {p}"))?
    } else {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            bail!("expected JSON on stdin, got empty input");
        }
        trimmed.to_string()
    };
    serde_json::from_str(&raw).with_context(|| "parsing profile JSON")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_handles_multibyte() {
        let s = "中文测试中文测试";
        let t = truncate(s, 5);
        assert_eq!(t.chars().count(), 5);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn truncate_passes_through_short() {
        assert_eq!(truncate("hi", 5), "hi");
    }

    #[test]
    fn validate_cli_user_id_rejects_empty() {
        let err = validate_cli_user_id("").unwrap_err();
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn validate_cli_user_id_rejects_whitespace_only() {
        let err = validate_cli_user_id("  \t\n ").unwrap_err();
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn validate_cli_user_id_rejects_oversized() {
        let too_long = "a".repeat(MAX_USER_ID_LEN + 1);
        let err = validate_cli_user_id(&too_long).unwrap_err();
        assert!(err.to_string().contains("CLI rejects ids longer than"));
    }

    #[test]
    fn validate_cli_user_id_trims_and_accepts() {
        let got = validate_cli_user_id("  u1  ").unwrap();
        assert_eq!(got, "u1");
    }
}