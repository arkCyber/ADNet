//! Comprehensive TUI dashboard for A3Net - covers all 70+ CLI commands.

use crate::color::Color;
use crate::widget::{section_header, Table};
use serde::{Deserialize, Serialize};

/// A single CLI command exposed in the menu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub key: char,
    pub label: &'static str,
    pub description: &'static str,
    pub cli_args: &'static str,
}

/// Top-level command categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Node,
    Content,
    Network,
    Rooms,
    Workspace,
    Contacts,
    Identity,
    Media,
    Diagnostics,
    Advanced,
    Misc,
}

impl Category {
    pub fn title(self) -> &'static str {
        match self {
            Category::Node => "Node Operations",
            Category::Content => "Content & Storage",
            Category::Network => "Network & Routing",
            Category::Rooms => "Rooms & Feeds",
            Category::Workspace => "Workspace",
            Category::Contacts => "Contacts & Roster",
            Category::Identity => "Identity & Pairing",
            Category::Media => "Media (Video/DNS)",
            Category::Diagnostics => "Diagnostics & Health",
            Category::Advanced => "Advanced",
            Category::Misc => "Misc",
        }
    }
}

/// Build the full CLI command menu, covering every command in `a3net-cli`.
pub fn build_main_menu() -> Vec<(Category, Vec<CommandSpec>)> {
    vec![
        (
            Category::Node,
            vec![
                cmd('i', "Info", "Show node identity & status", "info"),
                cmd('I', "Init", "Initialize local node identity", "init"),
                cmd('S', "Serve", "Run mesh HTTP server", "serve"),
                cmd('d', "Daemon", "Run long-lived background daemon", "daemon"),
                cmd('s', "Shutdown", "Stop the running daemon", "shutdown"),
                cmd('r', "Roster", "Show node roster", "roster"),
                cmd('H', "Health", "Daemon health check", "health"),
                cmd('S', "Status", "Status snapshot", "status"),
                cmd('c', "Config", "Show/edit configuration", "config show"),
                cmd('P', "Profile", "Profile management", "profile"),
            ],
        ),
        (
            Category::Content,
            vec![
                cmd('A', "Add", "Add a file to local store", "add"),
                cmd('G', "Get", "Retrieve a file from store", "get"),
                cmd('C', "Cat", "Print file contents", "cat"),
                cmd('L', "Ls", "List local store contents", "ls"),
                cmd('P', "Pin", "Pin a CID locally", "pin"),
                cmd('R', "Repo", "Repo stats", "repo stat"),
                cmd('V', "Verify", "Verify blob hashes", "verify"),
                cmd('G', "Gc", "Garbage-collect unpinned blobs", "gc"),
                cmd('B', "Block", "Block content by hash", "block"),
                cmd('U', "Unblock", "Unblock content", "unblock"),
                cmd('F', "Files", "Browse files", "files ls"),
                cmd('S', "Storage", "Storage usage & quota", "storage"),
            ],
        ),
        (
            Category::Network,
            vec![
                cmd('B', "Bootstrap", "Bootstrap the node", "bootstrap"),
                cmd('S', "Swarm", "Swarm peers", "swarm peers"),
                cmd('D', "DHT", "DHT operations", "dht"),
                cmd('B', "Bitswap", "Bitswap stats", "bitswap"),
                cmd('R', "Routing", "Routing table", "routing"),
                cmd('M', "Mesh", "Mesh coordinator", "mesh"),
                cmd('C', "Channel", "Channel peering", "channel"),
                cmd('N', "Name", "Name resolution", "name"),
                cmd('K', "Key", "Key store", "key"),
                cmd('P', "Pubsub", "Pub/sub messaging", "pubsub"),
                cmd('W', "WebDAV", "WebDAV access", "webdav"),
                cmd('R', "Relay", "Relay URLs", "relay-urls"),
                cmd('P', "PortMapProbe", "Probe port mapping", "port-map-probe"),
            ],
        ),
        (
            Category::Rooms,
            vec![
                cmd('L', "Room Ls", "List joined rooms", "room ls"),
                cmd('J', "Join", "Join a room", "room join"),
                cmd('L', "Leave", "Leave a room", "room leave"),
                cmd('P', "Peers", "List peers in room", "room peers"),
                cmd('F', "Feed", "Show room feed", "feed"),
                cmd('A', "Announce", "Announce file to room", "announce"),
                cmd('S', "Subscribe", "Subscribe to room", "room subscribe"),
                cmd('U', "Unsubscribe", "Unsubscribe from room", "room unsubscribe"),
            ],
        ),
        (
            Category::Workspace,
            vec![
                cmd('P', "Publish", "Publish file to workspace", "workspace publish"),
                cmd('L', "Ls", "List workspace entries", "workspace ls"),
                cmd('U', "Unpublish", "Remove from workspace", "workspace unpublish"),
                cmd('V', "Verify", "Verify workspace files", "workspace verify"),
                cmd('P', "Pull", "Pull workspace entry", "workspace pull"),
                cmd('P', "Push", "Push workspace entry", "workspace push"),
                cmd('S', "Sync", "Sync workspace", "workspace sync"),
            ],
        ),
        (
            Category::Contacts,
            vec![
                cmd('A', "Add", "Add contact", "user add"),
                cmd('L', "List", "List contacts", "user list"),
                cmd('S', "Search", "Search contacts", "user search"),
                cmd('S', "Show", "Show contact", "user show"),
                cmd('D', "Delete", "Delete contact", "user delete"),
                cmd('G', "GroupCreate", "Create contact group", "user group-create"),
                cmd('G', "GroupList", "List contact groups", "user group-list"),
                cmd('D', "Digit", "Digit ID operations", "user digit"),
                cmd('D', "DigitAdd", "Add digit ID", "user digit-add"),
                cmd('D', "DigitResolve", "Resolve digit ID", "user digit-resolve"),
            ],
        ),
        (
            Category::Identity,
            vec![
                cmd('P', "Pair", "Pair with peer", "pair"),
                cmd('I', "Invite", "Send/receive invite", "invite"),
                cmd('Q', "QR", "Generate/parse QR", "qr"),
                cmd('R', "Roster", "Identity roster", "roster"),
                cmd('K', "Keys", "Key management", "key"),
            ],
        ),
        (
            Category::Media,
            vec![
                cmd('V', "Video", "Video stream stats", "video"),
                cmd('D', "DNS", "DNS server commands", "dns"),
                cmd('M', "MDNS", "mDNS discovery", "mdns"),
                cmd('V', "Vless", "VLESS proxy", "vless"),
                cmd('B', "Bandwidth", "Bandwidth info", "bandwidth"),
                cmd('W', "WebRTC", "WebRTC stats", "webrtc"),
                cmd('W', "WebTransport", "WebTransport stats", "webtransport"),
            ],
        ),
        (
            Category::Diagnostics,
            vec![
                cmd('D', "Doctor", "Run health diagnostics", "doctor"),
                cmd('D', "Diagnose", "Run diagnostics", "diagnose"),
                cmd('R', "Report", "Generate report", "report"),
                cmd('S', "Stats", "Show statistics", "stats"),
                cmd('M', "Metrics", "Metrics snapshot", "metrics-server"),
                cmd('L', "Logs", "Show logs", "log"),
                cmd('T', "Tail", "Tail logs", "log tail"),
            ],
        ),
        (
            Category::Advanced,
            vec![
                cmd('N', "News", "News feed", "news"),
                cmd('M', "Moments", "Moments (social)", "moments"),
                cmd('W', "Webhook", "Webhook management", "webhook"),
                cmd('M', "Moderation", "Moderation queue", "moderation"),
                cmd('R', "Reputation", "Reputation system", "reputation"),
                cmd('C', "Commands", "List CLI commands", "commands"),
                cmd('T', "Tag", "Tagging system", "tag"),
            ],
        ),
        (
            Category::Misc,
            vec![
                cmd('E', "Echo", "Echo a message", "echo"),
                cmd('F', "Feed", "Room feed reader", "feed"),
                cmd('H', "Help", "Show this menu", "help"),
                cmd('Q', "Quit", "Exit dashboard", "--quit"),
            ],
        ),
    ]
}

const fn cmd(
    key: char,
    label: &'static str,
    description: &'static str,
    cli_args: &'static str,
) -> CommandSpec {
    CommandSpec {
        key,
        label,
        description,
        cli_args,
    }
}

/// Render the main dashboard header.
pub fn render_header() -> String {
    let mut out = String::new();
    out.push_str(&section_header("A3Net - Interactive Dashboard"));
    out.push('\n');
    out.push_str(&format!(
        "  {} commands, {} categories\n",
        count_commands(),
        build_main_menu().len()
    ));
    out.push_str("  Connected to: http://127.0.0.1:11436/rpc\n");
    out.push('\n');
    out
}

/// Render the full menu listing.
pub fn render_menu() -> String {
    let mut out = String::new();
    for (idx, (cat, items)) in build_main_menu().iter().enumerate() {
        out.push_str(&format!(
            "{} [{}] {}\n",
            Color::Cyan.paint(format!("{:>2}", idx + 1)),
            cat.title(),
            Color::Bright.paint(&format!("({} cmds)", items.len()))
        ));

        for item in items.iter() {
            out.push_str(&format!(
                "      {:<14} - {}\n",
                Color::Yellow.paint(item.label),
                Color::Bright.paint(item.description)
            ));
        }
        out.push('\n');
    }
    out
}

/// Render the quick-access panel.
pub fn render_quick_panel() -> String {
    let panel = crate::box_drawing::Box::with_title("Quick Actions")
        .add_field("[I] Info", "Show node identity & join status")
        .add_field("[H] Health", "Daemon health (HTTP RPC 11436)")
        .add_field("[F] Feed", "Current room's announcements")
        .add_field("[L] Ls", "List local store / workspace")
        .add_field("[D] Doctor", "Run full diagnostics")
        .add_field("[Q] Quit", "Exit dashboard");
    format!("{}\n", panel)
}

/// Render a 2-column category navigator.
pub fn render_categories() -> String {
    let cats = build_main_menu();
    let half = cats.len().div_ceil(2);
    let left_col = &cats[..half];
    let right_col = &cats[half..];

    let mut table = Table::with_headers(vec!["Category", "Category"]);
    for (l, r) in left_col
        .iter()
        .zip(right_col.iter().chain(std::iter::repeat(&(
            Category::Misc,
            Vec::new(),
        ))))
    {
        let left = format!(
            "{} {} ({})",
            Color::Cyan.paint("["),
            l.0.title(),
            Color::Bright.paint(&format!("{}", l.1.len())),
        );
        let right = if r.1.is_empty() {
            String::new()
        } else {
            format!(
                "{} {} ({})",
                Color::Cyan.paint("["),
                r.0.title(),
                Color::Bright.paint(&format!("{}", r.1.len())),
            )
        };
        table.add_row(vec![left, right]);
    }
    format!("{}\n", table)
}

/// Find command spec by key.
pub fn find_command(key: char) -> Option<CommandSpec> {
    for (_, items) in build_main_menu() {
        for item in items {
            if item.key == key {
                return Some(item);
            }
        }
    }
    None
}

/// Get all unique cli args strings for shell-out.
pub fn all_cli_commands() -> Vec<&'static str> {
    let mut out = Vec::new();
    for (_, items) in build_main_menu() {
        for item in items {
            out.push(item.cli_args);
        }
    }
    out
}

fn count_commands() -> usize {
    build_main_menu().iter().map(|(_, v)| v.len()).sum()
}

/// Style helper for headings.
pub fn heading(s: &str) -> String {
    Color::Cyan.paint(s).to_string()
}

/// Style helper for subheadings.
pub fn subheading(s: &str) -> String {
    Color::Bright.paint(s).to_string()
}

/// Render a dashboard view - the entry point for `a3net tui`.
pub fn render_dashboard() -> String {
    let mut out = String::new();
    out.push_str(&render_header());
    out.push_str(&render_quick_panel());
    out.push_str(&render_categories());
    out.push_str(&render_menu());
    out.push_str(&format!(
        "\n{}\n",
        Color::Bright.paint(
            "Tip: Most commands support --http 127.0.0.1 --http-port 11436 to talk to the daemon over HTTP RPC."
        )
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_menu_has_all_categories() {
        let menu = build_main_menu();
        assert_eq!(menu.len(), 11, "expected 11 categories");
    }

    #[test]
    fn all_commands_have_keys() {
        for (_, items) in build_main_menu() {
            assert!(!items.is_empty(), "category should have at least one command");
            for item in items {
                assert!(!item.label.is_empty());
                assert!(!item.description.is_empty());
                assert!(!item.cli_args.is_empty());
            }
        }
    }

    #[test]
    fn total_commands_count() {
        let total = count_commands();
        assert!(total >= 70, "expected at least 70 commands, got {}", total);
    }

    #[test]
    fn find_command_by_key() {
        assert!(find_command('i').is_some());
        assert!(find_command('Q').is_some());
        assert!(find_command('z').is_none());
    }

    #[test]
    fn render_dashboard_works() {
        let dash = render_dashboard();
        assert!(dash.contains("A3Net"));
        assert!(dash.contains("Quick Actions"));
        assert!(dash.contains("Node Operations"));
        assert!(dash.contains("Content & Storage"));
    }

    #[test]
    fn cli_commands_iter() {
        let cmds = all_cli_commands();
        assert!(cmds.contains(&"info"));
        assert!(cmds.contains(&"daemon"));
        assert!(cmds.contains(&"health"));
    }

    #[test]
    fn render_categories_well_formed() {
        let s = render_categories();
        assert!(s.contains("Node"));
        assert!(s.contains("Category"));
    }

    #[test]
    fn heading_and_subheading() {
        assert!(!heading("test").is_empty());
        assert!(!subheading("test").is_empty());
    }
}
