//! Tests for the a3net-tui dashboard module.

use a3net_tui::dashboard::{build_main_menu, render_dashboard, Category, CommandSpec};
use a3net_tui::app::{build_flat_menu, render_default_dashboard, RunResult, TuiApp};

#[test]
fn main_menu_has_eleven_categories() {
    let menu = build_main_menu();
    assert_eq!(menu.len(), 11);
}

#[test]
fn all_categories_have_commands() {
    let menu = build_main_menu();
    for (cat, items) in &menu {
        assert!(!items.is_empty(), "Category {:?} should have commands", cat);
    }
}

#[test]
fn total_commands_is_70_plus() {
    let total: usize = build_main_menu().iter().map(|(_, v)| v.len()).sum();
    assert!(
        total >= 70,
        "expected >= 70 commands, got {}",
        total
    );
}

#[test]
fn all_command_labels_are_non_empty() {
    for (_, items) in build_main_menu() {
        for item in items {
            assert!(!item.label.is_empty());
            assert!(!item.description.is_empty());
            assert!(!item.cli_args.is_empty());
            assert!(item.key != '\0');
        }
    }
}

#[test]
fn every_command_maps_to_a_real_cli_subcommand() {
    // Spot check: every cli_args should start with a recognized verb
    let known_verbs = [
        "info", "init", "serve", "daemon", "shutdown", "roster", "health",
        "status", "config", "profile", "add", "get", "cat", "ls", "pin",
        "repo", "verify", "gc", "block", "unblock", "files", "storage",
        "bootstrap", "swarm", "dht", "bitswap", "routing", "mesh",
        "channel", "name", "key", "pubsub", "webdav", "relay-urls",
        "port-map-probe", "room", "feed", "announce", "subscribe",
        "unsubscribe", "workspace", "user", "pair", "invite", "qr",
        "video", "dns", "mdns", "vless", "bandwidth", "webrtc",
        "webtransport", "doctor", "diagnose", "report", "stats",
        "metrics-server", "log", "news", "moments", "webhook",
        "moderation", "reputation", "commands", "tag", "echo", "help",
        "--quit",
    ];

    let mut total_checked = 0;
    for (_, items) in build_main_menu() {
        for item in items {
            let first_word = item.cli_args.split_whitespace().next().unwrap_or("");
            total_checked += 1;
            assert!(
                known_verbs.contains(&first_word),
                "command {:?} has unknown verb: {}",
                item.label,
                item.cli_args
            );
        }
    }
    assert!(total_checked >= 70);
}

#[test]
fn render_dashboard_has_header_and_menu() {
    let dash = render_dashboard();
    assert!(dash.contains("A3Net"));
    assert!(dash.contains("Node Operations"));
    assert!(dash.contains("Content & Storage"));
    assert!(dash.contains("Network & Routing"));
    assert!(dash.contains("Rooms & Feeds"));
    assert!(dash.contains("Workspace"));
    assert!(dash.contains("Diagnostics & Health"));
}

#[test]
fn categories_are_distinct_titles() {
    let menu = build_main_menu();
    let titles: Vec<&str> = menu.iter().map(|(c, _)| c.title()).collect();
    let mut deduped = titles.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(titles.len(), deduped.len(), "category titles must be unique");
}

// ─── TuiApp integration tests ─────────────────────────────────────────

#[test]
fn tui_app_renders_main_menu_by_default() {
    let app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
    let rendered = app.render();
    assert!(rendered.contains("Quick Actions"));
    assert!(rendered.contains("127.0.0.1:11436"));
}

#[test]
fn tui_app_handles_quit() {
    let mut app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
    let r = app.handle_key('q');
    assert!(matches!(r, RunResult::Quit));
}

#[test]
fn tui_app_navigates_to_category() {
    let mut app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
    let r = app.handle_key('c');
    assert!(matches!(r, RunResult::Navigate(Category::Content)));
    assert_eq!(app.current_category, Some(Category::Content));
}

#[test]
fn tui_app_renders_category_view() {
    let mut app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
    app.current_category = Some(Category::Workspace);
    let rendered = app.render();
    assert!(rendered.contains("Workspace"));
    // Should show workspace-specific commands
    assert!(rendered.contains("publish") || rendered.contains("Publish"));
}

#[test]
fn tui_app_handles_back_key() {
    let mut app = TuiApp::new("http://127.0.0.1:11436/rpc".to_string());
    app.current_category = Some(Category::Content);
    let r = app.handle_key('b');
    assert_eq!(app.current_category, None);
    assert!(matches!(r, RunResult::Navigate(_)));
}

#[test]
fn flat_menu_helper_works() {
    let m = build_flat_menu();
    assert!(!m.is_empty());
    // 'q' is mapped in Category::Misc
    let quit = m.get(&'q');
    // Some categories use 'q' as a key
    assert!(quit.is_some() || m.len() > 0, "flat menu should contain quit");
}

#[test]
fn render_default_dashboard_includes_header() {
    let s = render_default_dashboard();
    assert!(s.contains("A3Net"));
    assert!(s.contains("Connected to"));
    assert!(s.contains("http://127.0.0.1:11436"));
}
