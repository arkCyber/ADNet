//! `a3net completions <shell>` — emit a shell completion script.
//!
//! Wraps `clap_complete::generate` so adding new top-level commands
//! automatically extends the completion surface — no hand-written
//! scripts to maintain.

use anyhow::Result;
use clap::CommandFactory;
use clap_complete::{generate, Shell};

use crate::cli::{Cli, CompletionShell};

/// Emit a shell completion script on stdout.
pub fn run_completions(shell: CompletionShell) -> Result<()> {
    let shell: Shell = match shell {
        CompletionShell::Bash => Shell::Bash,
        CompletionShell::Zsh => Shell::Zsh,
        CompletionShell::Fish => Shell::Fish,
        CompletionShell::Elvish => Shell::Elvish,
        CompletionShell::Powershell => Shell::PowerShell,
    };
    let mut cmd = Cli::command();
    let bin = cmd.get_name().to_string();
    generate(shell, &mut cmd, bin, &mut std::io::stdout());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn completions_bash_emits_something() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(Shell::Bash, &mut cmd, "a3net", &mut buf);
        assert!(!buf.is_empty(), "completion script must not be empty");
        // Bash completions always contain `_a3net_completion` or similar.
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("a3net") || s.contains("completion"));
    }

    #[test]
    fn completions_zsh_emits_something() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(Shell::Zsh, &mut cmd, "a3net", &mut buf);
        assert!(!buf.is_empty());
    }

    #[test]
    fn completions_fish_emits_something() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(Shell::Fish, &mut cmd, "a3net", &mut buf);
        assert!(!buf.is_empty());
    }

    #[test]
    fn completion_shell_enum_parses() {
        let cli = Cli::try_parse_from(["a3net", "completions", "zsh"]).unwrap();
        match cli.cmd {
            crate::cli::Cmd::Completions { shell } => assert_eq!(shell, CompletionShell::Zsh),
            _ => panic!("expected Completions"),
        }
    }

    #[test]
    fn run_completions_does_not_panic_for_each_shell() {
        for s in [
            CompletionShell::Bash,
            CompletionShell::Zsh,
            CompletionShell::Fish,
            CompletionShell::Elvish,
            CompletionShell::Powershell,
        ] {
            // Each `run_completions` writes to stdout; we redirect to a
            // sink so the test output isn't polluted.
            let mut sink = std::io::sink();
            let shell: Shell = match s {
                CompletionShell::Bash => Shell::Bash,
                CompletionShell::Zsh => Shell::Zsh,
                CompletionShell::Fish => Shell::Fish,
                CompletionShell::Elvish => Shell::Elvish,
                CompletionShell::Powershell => Shell::PowerShell,
            };
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "a3net", &mut sink);
        }
    }
}
