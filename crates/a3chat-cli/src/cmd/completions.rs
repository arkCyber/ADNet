//! `a3chat completions <shell>` — emit shell completion scripts.

use clap::CommandFactory;
use clap_complete::{generate, Shell};

use crate::error::{CliError, CliResult};

/// Print a shell completion script. Each shell has its own dialect,
/// so we dispatch on the value clap_complete accepts.
pub fn run(shell: Shell) -> CliResult<()> {
    let mut cmd = crate::Cli::command();
    let bin = cmd.get_name().to_string();
    let mut buf: Vec<u8> = Vec::new();
    generate(shell, &mut cmd, bin, &mut buf);
    let s = String::from_utf8(buf)
        .map_err(|e| CliError::Internal(format!("non-utf8 completion: {e}")))?;
    println!("{s}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_completion_emits_non_empty_output() {
        let mut cmd = crate::Cli::command();
        let bin = cmd.get_name().to_string();
        let mut buf: Vec<u8> = Vec::new();
        generate(Shell::Bash, &mut cmd, bin, &mut buf);
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.is_empty());
        assert!(s.contains("a3chat") || s.contains("_a3chat"));
    }

    #[test]
    fn fish_completion_emits_function_keyword() {
        let mut cmd = crate::Cli::command();
        let bin = cmd.get_name().to_string();
        let mut buf: Vec<u8> = Vec::new();
        generate(Shell::Fish, &mut cmd, bin, &mut buf);
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("complete"));
    }
}