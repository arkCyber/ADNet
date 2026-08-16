//! Interactive TUI runner - the entry point for `a3net tui`.
//!
//! This module connects `a3net-tui::dashboard` to the running daemon via
//! the HTTP RPC endpoint (default `127.0.0.1:11436`). It renders the menu
//! and handles user keystrokes, shell-out'ing to the appropriate `a3net`
//! subcommand when the user picks one.

use std::collections::HashMap;
use std::process::Command;

use crate::box_drawing::Box as TuiBox;
use crate::color::Color;
use crate::dashboard::{build_main_menu, render_dashboard, Category, CommandSpec};
use crate::widget::{section_header, Table};

/// Result of running a single command in the TUI.
#[derive(Debug, Clone)]
pub enum RunResult {
    /// Command completed successfully.
    Ok(String),
    /// Command failed with an error message.
    Err(String),
    /// User asked to quit.
    Quit,
    /// Menu level changed (e.g. navigated to a sub-menu).
    Navigate(Category),
}

/// State for the TUI runtime.
pub struct TuiApp {
    pub current_category: Option<Category>,
    pub rpc_url: String,
    pub history: Vec<String>,
    pub status_message: String,
}

impl TuiApp {
    /// Create a new TUI app.
    pub fn new(rpc_url: String) -> Self {
        Self {
            current_category: None,
            rpc_url,
            history: Vec::new(),
            status_message: "Ready".to_string(),
        }
    }

    /// Render the current state.
    pub fn render(&self) -> String {
        match self.current_category {
            None => self.render_main_menu(),
            Some(cat) => self.render_category(cat),
        }
    }

    /// Render the top-level menu.
    fn render_main_menu(&self) -> String {
        let mut out = render_dashboard();
        out.push('\n');
        out.push_str(&format!(
            "\n{}\n",
            Color::Bright.paint(&format!("Connected to {}", self.rpc_url))
        ));
        if !self.status_message.is_empty() {
            out.push_str(&format!(
                "\n{}\n",
                Color::Green.paint(&format!(">> {}", self.status_message))
            ));
        }
        out
    }

    /// Render a specific category's commands.
    fn render_category(&self, cat: Category) -> String {
        let mut out = String::new();
        out.push_str(&section_header(cat.title()));
        out.push('\n');

        let menu = build_main_menu();
        let items: Vec<_> = menu
            .iter()
            .find(|(c, _)| *c == cat)
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        let mut table = Table::with_headers(vec!["Key", "Label", "Description", "CLI"]);
        for item in &items {
            table.add_row(vec![
                Color::Cyan.paint(format!("[{}]", item.key)).to_string(),
                Color::Yellow.paint(item.label).to_string(),
                item.description.to_string(),
                Color::Bright.paint(format!("a3net {}", item.cli_args)).to_string(),
            ]);
        }
        out.push_str(&format!("{}\n", table));

        out.push_str("\n[B] Back to main menu    [Q] Quit\n");
        out
    }

    /// Handle a key press.
    pub fn handle_key(&mut self, key: char) -> RunResult {
        self.status_message = format!("Pressed '{}'", key);

        // Global keys
        match key.to_ascii_lowercase() {
            'q' => return RunResult::Quit,
            'b' => {
                self.current_category = None;
                return RunResult::Navigate(Category::Misc);
            }
            'h' => {
                self.current_category = Some(Category::Diagnostics);
                return RunResult::Navigate(Category::Diagnostics);
            }
            'i' => {
                self.current_category = Some(Category::Node);
                return RunResult::Navigate(Category::Node);
            }
            'c' => {
                self.current_category = Some(Category::Content);
                return RunResult::Navigate(Category::Content);
            }
            'r' => {
                self.current_category = Some(Category::Rooms);
                return RunResult::Navigate(Category::Rooms);
            }
            'w' => {
                self.current_category = Some(Category::Workspace);
                return RunResult::Navigate(Category::Workspace);
            }
            'n' => {
                self.current_category = Some(Category::Network);
                return RunResult::Navigate(Category::Network);
            }
            _ => {}
        }

        // Otherwise let them pick a category by number 1-9
        if let Some(digit) = key.to_digit(10) {
            let menu = build_main_menu();
            if let Some((cat, _)) = menu.get((digit as usize).saturating_sub(1)) {
                self.current_category = Some(*cat);
                return RunResult::Navigate(*cat);
            }
        }

        RunResult::Ok(format!(
            "Unknown key '{}' - try h, i, c, r, w, n, or q",
            key
        ))
    }

    /// Execute the `a3net` command for a given command spec.
    #[allow(dead_code)]
    fn shell_out(&mut self, item: &CommandSpec) -> RunResult {
        let full_args = format!(
            "{} --http 127.0.0.1 --http-port 11436",
            item.cli_args
        );

        self.history.push(full_args.clone());

        match Command::new("a3net")
            .args(full_args.split_whitespace())
            .output()
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                self.status_message = format!("Command finished: status={}", output.status);
                if output.status.success() {
                    RunResult::Ok(stdout)
                } else {
                    RunResult::Err(format!("{}\n{}", stdout, stderr))
                }
            }
            Err(e) => {
                let msg = format!(
                    "Could not run `a3net {}` automatically ({}). \
                     Run it yourself:  a3net {}",
                    item.cli_args, e, item.cli_args
                );
                self.status_message = msg.clone();
                RunResult::Ok(msg)
            }
        }
    }
}

/// Render a one-line status bar at the bottom of the screen.
pub fn render_status_bar(app: &TuiApp) -> String {
    let history_count = app.history.len();
    let msg = if app.status_message.is_empty() {
        "Ready"
    } else {
        &app.status_message
    };

    let panel = TuiBox::with_title("Status")
        .add_field("RPC", app.rpc_url.clone())
        .add_field("History", format!("{} commands run", history_count))
        .add_field("Message", msg.to_string());

    format!("{}\n", panel)
}

/// Convenience for the CLI - render & return.
pub fn render_default_dashboard() -> String {
    let mut app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
    app.render()
}

/// Build a flat menu mapping from every CommandSpec.
pub fn build_flat_menu() -> HashMap<char, CommandSpec> {
    let mut out = HashMap::new();
    for (_, items) in build_main_menu() {
        for item in items {
            out.insert(item.key, item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_renders_main_menu() {
        let app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
        let rendered = app.render();
        assert!(rendered.contains("A3Net"));
        assert!(rendered.contains("Quick Actions"));
    }

    #[test]
    fn handle_key_quit() {
        let mut app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
        let r = app.handle_key('q');
        assert!(matches!(r, RunResult::Quit));
    }

    #[test]
    fn handle_key_navigates_to_category() {
        let mut app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
        let r = app.handle_key('i');
        assert!(matches!(r, RunResult::Navigate(Category::Node)));
        assert_eq!(app.current_category, Some(Category::Node));
    }

    #[test]
    fn handle_key_unknown() {
        let mut app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
        let r = app.handle_key('z');
        assert!(matches!(r, RunResult::Ok(_)));
    }

    #[test]
    fn handle_digit_selects_category() {
        let mut app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
        let r = app.handle_key('1');
        assert!(matches!(r, RunResult::Navigate(_)));
    }

    #[test]
    fn render_category() {
        let mut app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
        app.current_category = Some(Category::Content);
        let rendered = app.render();
        assert!(rendered.contains("Content & Storage"));
        assert!(rendered.contains("add"));
        assert!(rendered.contains("get"));
    }

    #[test]
    fn status_bar_includes_rpc_url() {
        let app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
        let bar = render_status_bar(&app);
        assert!(bar.contains("127.0.0.1:11436"));
    }

    #[test]
    fn flat_menu_has_many_entries() {
        let m = build_flat_menu();
        assert!(!m.is_empty());
    }

    #[test]
    fn default_dashboard_renders() {
        let s = render_default_dashboard();
        assert!(s.contains("A3Net"));
    }
}
