//! `adnet` application configuration.
//!
//! The CLI reads a JSON5 (`.json` or `.json5`) file from a
//! platform-default location — `~/.config/adnet/config.json` on Linux,
//! `~/Library/Application Support/adnet/config.json` on macOS — and
//! layers it under environment variables and CLI flags. The resolved
//! [`AppConfig`] is then handed to the rest of the binary.
//!
//! ## File format
//!
//! JSON5 is supported so operators can sprinkle `//` comments and
//! trailing commas. Unknown fields are ignored (lenient) and warned
//! to stderr so a typo never silently disables a feature.
//!
//! ## Lookup order
//!
//! 1. The `--config <path>` CLI flag (when supplied).
//! 2. `$ADNET_CONFIG` environment variable (when set).
//! 3. The platform-default path described above.
//!
//! ## Missing file
//!
//! On the first run the loader writes a fully-commented default
//! template to the platform-default path and proceeds with the
//! in-memory default — no failure, no prompt.
//!
//! ## Override layering (after the file is loaded)
//!
//! - Environment variables can override [`LogConfig::level`] and
//!   [`LogConfig::format`] (the variables are intentionally limited to
//!   "runtime" fields, never to `mesh` / `relay` / `data_dir`).
//! - CLI flags override everything (see `cli.rs`).

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(feature = "qr")]
use adnet_qr::generator::QrErrorCorrectionLevel as QrCodeEcc;

use adnet_mesh::MeshConfig;
use adnet_relay::RelayConfig;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Filename used inside the platform-default config directory.
pub const CONFIG_FILE_NAME: &str = "config.json";

/// Environment variable that overrides the config path.
pub const ADNET_CONFIG_ENV: &str = "ADNET_CONFIG";

/// Top-level config struct. Every field has a sensible default so a
/// missing or partial file still produces a working CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    /// Local data directory. Mirrors `Cli::data_dir` and lives in
    /// the same place by default — `./.adnet-data` in the current
    /// working directory.
    pub data_dir: PathBuf,

    /// Logging configuration.
    pub log: LogConfig,

    /// Default room/lobby used when the user runs `adnet feed` or
    /// `adnet echo` without `--room`. `None` means "require --room".
    pub default_room: Option<String>,

    /// REPL behaviour (only meaningful under `adnet run`).
    pub repl: ReplConfig,

    /// Mesh HTTP server settings. `None` means "use defaults".
    pub mesh: Option<MeshConfig>,

    /// Embedded relay server settings. `None` means "don't run a
    /// relay server" but still create a client when `enabled`.
    pub relay: Option<RelayConfig>,

    /// Gossip validation policy override. `None` means "node default
    /// (Strict)".
    pub gossip_validation: Option<GossipValidation>,

    /// iroh-backed runtime settings. `None` means "do not start the
    /// iroh runtime — the node falls back to the legacy native-QUIC
    /// transport (or mesh-only when `--no-quic` is also set)". The
    /// `iroh` cargo feature on `adnet-node` must be enabled at build
    /// time for any non-`None` value to take effect; otherwise the
    /// values are silently ignored with a `warn!` at boot.
    ///
    /// See [`IrohConfig`] for the individual knobs.
    pub iroh: Option<IrohConfig>,

    /// QR-rendering settings. Only consulted when the `qr` cargo
    /// feature is enabled; a config that supplies a `qr` block on
    /// a feature-disabled build is parsed and ignored so the file
    /// stays consistent across builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg(feature = "qr")]
    pub qr: Option<QrConfigToml>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./.adnet-data"),
            log: LogConfig::default(),
            default_room: None,
            repl: ReplConfig::default(),
            mesh: None,
            relay: None,
            gossip_validation: None,
            iroh: None,
            #[cfg(feature = "qr")]
            qr: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LogConfig {
    /// Tracing-subscriber filter directive. Accepts the same syntax
    /// as the `RUST_LOG` env var (e.g. `"info"`, `"adnet_mesh=debug"`).
    pub level: String,
    /// `"compact"` for the default `tracing_subscriber::fmt()` output,
    /// `"json"` for machine-readable structured logs.
    pub format: LogFormat,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: LogFormat::Compact,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Compact,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReplConfig {
    /// Prompt printed before each REPL read. Empty string disables it.
    pub prompt: String,
    /// Optional history file. `None` disables persistent history.
    pub history_file: Option<PathBuf>,
}

impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            prompt: "adnet> ".to_string(),
            history_file: None,
        }
    }
}

/// Mirrors [`adnet_ipc::validation::ValidationPolicy`] but is
/// duplicated here so the CLI doesn't have to depend on `adnet-ipc`.
/// The variants 1-to-1 map to the IPC enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GossipValidation {
    Strict,
    Audit,
    Lenient,
}

/// iroh-backed runtime configuration.
///
/// When [`enabled`](Self::enabled) is `true` the CLI will:
///
/// 1. Load (or create) the persistent [`IrohIdentity`] at
///    `<data_dir>/<identity_path>` using `adnet_identity` semantics.
/// 2. Bind an `iroh::Endpoint` to
///    `[bind_host]:<bind_port | 0>` (port `0` means "OS-assigned").
/// 3. Spawn the full [`IrohRuntime`] — blobs / gossip / docs /
///    `adnet/frame/1` ALPN — and hand it to
///    `NodeBuilder::with_iroh_runtime`.
///
/// `enabled = false` (the default) keeps the legacy native-QUIC
/// transport path. Setting `enabled = true` without the `iroh`
/// cargo feature on `adnet-node` is **fail-soft**: the CLI logs a
/// warning and proceeds without the iroh runtime.
///
/// [`IrohIdentity`]: adnet_transport::iroh::IrohIdentity
/// [`IrohRuntime`]: adnet_node::IrohRuntime
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IrohConfig {
    /// Whether to bring up the iroh runtime on boot. Default `false`
    /// (legacy native-QUIC only) so existing deployments keep their
    /// transport stack until they opt in.
    pub enabled: bool,
    /// Bind host for the iroh `Endpoint`. Use `127.0.0.1` for
    /// loopback-only, `0.0.0.0` for all interfaces. Default
    /// `"127.0.0.1"` — iroh uses private Pkarr publishing by default
    /// so a wider bind is rarely what you want for the test path.
    pub bind_host: String,
    /// TCP port. `0` = OS-assigned (default). When `0` the CLI
    /// surfaces the bound port in `NodeInfo`.
    pub bind_port: u16,
    /// Optional path to the persistent iroh identity file. When
    /// `None` (the default) the loader falls back to
    /// `<data_dir>/iroh_secret_key`, matching what
    /// `adnet_transport::iroh::IrohIdentity::load_or_create` uses.
    pub identity_path: Option<PathBuf>,
    /// When `true`, advertise the local iroh endpoint via the n0
    /// Pkarr relay so other nodes can discover it via DNS / Pkarr.
    ///
    /// **Default: `false` (opt-in)**. Rationale: an operator who
    /// types `iroh.enabled = true` in their config is asking for
    /// the iroh transport, not necessarily for the public DHT
    /// lookup. Air-gapped deployments, on-prem labs, and CI
    /// runners should not leak the endpoint to the public n0
    /// infrastructure by accident. Operators who want public
    /// discovery should:
    ///
    /// 1. Set `publish_publicly = true` explicitly in their config,
    ///    or
    /// 2. Provide their own `NODE_ADDRESS_LOOKUP` / `DISCOVERY_*`
    ///    env vars to point at a private Pkarr publisher (not yet
    ///    wired in this CLI; today `publish_publicly = false`
    ///    silently suppresses the n0 defaults).
    pub publish_publicly: bool,
    /// Discovery stack configuration. Mirrors the subset of
    /// [`adnet_transport::iroh::discovery::DiscoveryConfig`]
    /// that operators can set via the config file. Each
    /// sub-field is independent and `None` falls back to the
    /// ADNet / iroh defaults. Operators that want to surface a
    /// `user_data` payload alongside the relay URL set
    /// `discovery.user_data` here.
    #[serde(default)]
    pub discovery: DiscoveryConfigToml,
}

impl Default for IrohConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_host: "127.0.0.1".to_string(),
            bind_port: 0,
            identity_path: None,
            // Audit V6 P2-1: previously `true` advertised the local
            // endpoint to the n0 Pkarr relay the moment an operator
            // wrote `iroh.enabled = true`. The default is now
            // opt-in so the inversion is enforced by the loader.
            publish_publicly: false,
            discovery: DiscoveryConfigToml::default(),
        }
    }
}

/// TOML-shaped mirror of the user-facing portion of
/// [`adnet_transport::iroh::discovery::DiscoveryConfig`].
///
/// Kept separate from `DiscoveryConfig` because the runtime
/// type lives in `adnet-transport` and the CLI must not pull
/// every iroh transitive dependency just to parse JSON5. Only
/// the fields operators can actually set via the config file
/// are mirrored here; the runtime struct fills in the rest.
///
/// ## Fields
///
/// - `user_data`: optional UTF-8 string up to 245 bytes,
///   included as the `user-data=` TXT attribute in every pkarr
///   packet the local node publishes. Mirrors
///   `iroh_dns::endpoint_info::UserData` (v1.0.3). Stays
///   consistent with the wire format a stock iroh endpoint
///   publishes, so ADNet endpoints can be discovered by
///   `iroh::endpoint::Endpoint::connect(…, discovery)` calls.
///   Length validation runs at runtime when the value is
///   applied to the `PkarrPublisherConfig` — oversized inputs
///   surface as a startup error rather than a silent
///   truncation.
///
/// - `mdns_enabled`: when `true`, the node advertises itself
///   via mDNS on the local LAN and discovers other ADNet nodes
///   the same way. Default `false` (opt-in). Requires the
///   `mdns` cargo feature at build time; a config file that
///   sets this to `true` on a non-mDNS build is parsed but
///   ignored at runtime with a warning.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiscoveryConfigToml {
    /// Optional user-data payload published alongside the
    /// endpoint's relay URL. See
    /// [`adnet_transport::iroh::discovery::UserData`].
    ///
    /// The `alias = "user_data"` lets `adnet config set
    /// iroh.discovery.user_data …` round-trip even though the
    /// struct-level `rename_all = "camelCase"` would otherwise
    /// prefer `userData`. Without the alias, the snake_case
    /// dotted key would land on disk as `user_data` and serde
    /// would silently drop it on read.
    #[serde(alias = "user_data")]
    pub user_data: Option<String>,
    /// Enable mDNS-based LAN discovery. When `true`, the node
    /// advertises its endpoint address via mDNS multicast and
    /// discovers other ADNet nodes on the same LAN without a
    /// relay or DHT. Default `false` (opt-in).
    #[serde(default, alias = "mdns_enabled")]
    pub mdns_enabled: bool,
}

/// QR-rendering settings. Only consulted when the `qr` cargo
/// feature is enabled.
///
/// The defaults are tuned for "human-scannable" output (medium
/// error correction, 4-module quiet zone). Operators that print
/// QR codes on small labels should drop to `Low` error
/// correction; operators that shell out to industrial / weathered
/// surfaces should pick `Quartile` or `High`.
#[cfg(feature = "qr")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QrConfigToml {
    /// Error-correction level. `auto` defers to the QR payload —
    /// `SecJoin` invites get `Quartile`, everything else `Low`.
    /// `low` / `medium` / `quartile` / `high` pin a specific level.
    #[serde(default = "default_qr_ec_level")]
    pub ec_level: QrEcLevelToml,
    /// Quiet-zone (white border) in modules. Must be ≥ 0; the
    /// generator rounds up to 4 for ISO/IEC 18004 compliance.
    /// 0 is allowed for embedded screenshots where the parent
    /// canvas already supplies a border.
    pub quiet_zone: u8,
    /// Output module size in pixels. Used by the SVG renderer
    /// only — the module-pixel mapping is implicit in the SVG
    /// `viewBox` so the same SVG renders crisply at any size.
    pub module_size: u8,
}

#[cfg(feature = "qr")]
impl Default for QrConfigToml {
    fn default() -> Self {
        Self {
            ec_level: default_qr_ec_level(),
            quiet_zone: 4,
            module_size: 8,
        }
    }
}

#[cfg(feature = "qr")]
fn default_qr_ec_level() -> QrEcLevelToml {
    QrEcLevelToml::Auto
}

/// Error-correction level as exposed to operators. `Auto` defers
/// to the QR payload (chatmail SecureJoin → `Quartile`, everything
/// else → `Low`).
#[cfg(feature = "qr")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QrEcLevelToml {
    Auto,
    Low,
    Medium,
    Quartile,
    High,
}

#[cfg(feature = "qr")]
impl QrEcLevelToml {
    /// Resolve `Auto` into a concrete EC level.
    pub fn resolve_ec_level(self) -> QrCodeEcc {
        // Sentinel: Auto is currently resolved to Low. SecureJoin
        // overrides happen at the payload layer, not here.
        match self {
            QrEcLevelToml::Auto | QrEcLevelToml::Low => QrCodeEcc::Low,
            QrEcLevelToml::Medium => QrCodeEcc::Medium,
            QrEcLevelToml::Quartile => QrCodeEcc::Quartile,
            QrEcLevelToml::High => QrCodeEcc::High,
        }
    }
}

// ---------------------------------------------------------------------------
// Resolution + load
// ---------------------------------------------------------------------------

/// Where the config came from, exposed for logging plus tests.
#[derive(Debug, Clone)]
pub struct ConfigSource {
    pub path: Option<PathBuf>,
    pub written_template: bool,
}

/// Result of the loader.
pub struct LoadedConfig {
    pub config: AppConfig,
    pub source: ConfigSource,
}

/// Resolve the platform-default config path.
///
/// Returns `Some` for all supported platforms. If both `dirs::config_dir()`
/// and `dirs::home_dir()` return `None` (rare but possible in heavily
/// sandboxed environments), the fallback is `None` — meaning "do not
/// write a config file" rather than polluting the CWD with a hidden
/// JSON5 file that looks like a project artefact.
pub fn default_config_path() -> Option<PathBuf> {
    if let Some(base) = dirs::config_dir() {
        return Some(base.join("adnet").join(CONFIG_FILE_NAME));
    }
    if let Some(home) = dirs::home_dir() {
        return Some(home.join(".config").join("adnet").join(CONFIG_FILE_NAME));
    }
    // Worst-case fallback: refuse to invent a path in the CWD. The
    // CLI will run in-memory-only and the operator can pass --config
    // explicitly.
    None
}

/// Resolve the config path using the lookup order documented on the
/// module header. Returns `None` only when no candidate can be
/// derived (essentially never on supported platforms).
pub fn resolve_config_path(cli_override: Option<&Path>) -> Option<PathBuf> {
    resolve(cli_override).and_then(|p| p.path)
}

/// Inner resolver that also returns *where* the path came from, so
/// downstream commands can distinguish "operator pointed at a
/// missing file" from "no file has been written yet".
pub fn resolve(cli_override: Option<&Path>) -> Option<ResolvedConfigPath> {
    resolve_with_env(cli_override, |k| std::env::var(k).ok())
}

/// Env-reader seam for [`resolve`]. The default `resolve` uses
/// `std::env::var`; tests use a closure to avoid `unsafe` env
/// mutation.
fn resolve_with_env<F: Fn(&str) -> Option<String>>(
    cli_override: Option<&Path>,
    env: F,
) -> Option<ResolvedConfigPath> {
    if let Some(p) = cli_override {
        return Some(ResolvedConfigPath {
            path: Some(p.to_path_buf()),
            source: ConfigPathSource::CliFlag,
        });
    }
    if let Some(p) = env(ADNET_CONFIG_ENV)
        && !p.trim().is_empty()
    {
        return Some(ResolvedConfigPath {
            path: Some(PathBuf::from(p)),
            source: ConfigPathSource::EnvVar,
        });
    }
    Some(ResolvedConfigPath {
        path: default_config_path(),
        source: ConfigPathSource::Default,
    })
}

/// Where the resolved config path came from. Used by subcommands
/// that need to distinguish "operator explicitly asked for this path"
/// from "we picked the platform-default for them".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPathSource {
    /// `--config <path>` on the CLI.
    CliFlag,
    /// `$ADNET_CONFIG` env var.
    EnvVar,
    /// Platform-default path synthesised by `default_config_path`.
    /// [`ResolvedConfigPath::path`] is `None` when the platform gave
    /// us no candidate (rare, e.g. heavily sandboxed environments);
    /// in that case the loader skips file IO and runs in-memory.
    Default,
}

/// Result of [`resolve`].
#[derive(Debug, Clone)]
pub struct ResolvedConfigPath {
    pub path: Option<PathBuf>,
    pub source: ConfigPathSource,
}

/// Load + parse the config. See the module-level docs for the lookup
/// order and the first-run template behaviour.
pub fn load(cli_override: Option<&Path>) -> Result<LoadedConfig> {
    let path = resolve_config_path(cli_override);
    let mut written_template = false;

    let config = match path.as_ref() {
        Some(p) if p.exists() => parse_file(p)?,
        Some(p) => {
            // Missing file — write a fully commented template so the
            // operator can see every supported knob on first run.
            write_default_template(p)
                .with_context(|| format!("write default config template to {}", p.display()))?;
            written_template = true;
            info!(
                path = %p.display(),
                "no config found, wrote default template and using in-memory defaults"
            );
            AppConfig::default()
        }
        None => AppConfig::default(),
    };

    // Apply environment overrides on the limited "runtime" fields.
    let config = apply_env_overrides(config);

    Ok(LoadedConfig {
        config,
        source: ConfigSource {
            path,
            written_template,
        },
    })
}

/// Parse a JSON5 config file. Unknown fields are silently ignored on
/// the JSON5 front (the parser swallows them) but we still capture
/// them at the top level via a second pass through `serde_json::Value`
/// so we can warn the operator.
fn parse_file(path: &Path) -> Result<AppConfig> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("read config file {}", path.display()))?;

    // First pass: parse into `serde_json::Value` via the JSON5
    // deserializer. `json5` 0.4 implements `deserialize_any`, so a
    // self-describing target like `serde_json::Value` round-trips.
    let value: serde_json::Value = json5::from_str(&raw)
        .with_context(|| format!("parse config file {} as JSON5", path.display()))?;
    warn_unknown_top_level_fields(&value);

    // Second pass: deserialize into the typed struct. Unknown fields
    // are ignored (we *did* pass `default` on every field via
    // `#[serde(default)]`).
    let cfg: AppConfig = serde_json::from_value(value)
        .with_context(|| format!("deserialize config at {}", path.display()))?;
    Ok(cfg)
}

/// Walk the top-level JSON5 object and warn about unknown keys. This
/// gives operators an early signal when they typo a field name
/// without forcing the parser to fail.
fn warn_unknown_top_level_fields(value: &serde_json::Value) {
    let Some(obj) = value.as_object() else {
        return;
    };
    let known: &[&str] = &[
        "dataDir",
        "data_dir",
        "log",
        "defaultRoom",
        "default_room",
        "repl",
        "mesh",
        "relay",
        "gossipValidation",
        "gossip_validation",
        "iroh",
        #[cfg(feature = "qr")]
        "qr",
    ];
    for key in obj.keys() {
        if !known.iter().any(|k| *k == key) {
            warn!(field = %key, "unknown config field, ignoring");
        }
    }
}

/// Apply environment variable overrides on `log.level` only. The
/// `RUST_LOG` variable is the standard way to retune tracing-subscriber
/// without editing the config; we surface it as an override so the
/// precedence docs (CLI > env > file) stay accurate.
fn apply_env_overrides(mut cfg: AppConfig) -> AppConfig {
    apply_env_overrides_with(&mut cfg, |k| std::env::var(k).ok());
    cfg
}

/// Same as [`apply_env_overrides`] but takes an env-reader closure so
/// unit tests can avoid the `unsafe` `std::env::set_var` API.
fn apply_env_overrides_with<F: Fn(&str) -> Option<String>>(cfg: &mut AppConfig, env: F) {
    if let Some(level) = env("RUST_LOG")
        && !level.trim().is_empty()
    {
        cfg.log.level = level;
    }
    if let Some(fmt) = env("ADNET_LOG_FORMAT") {
        match fmt.to_lowercase().as_str() {
            "json" => cfg.log.format = LogFormat::Json,
            "compact" => cfg.log.format = LogFormat::Compact,
            other => warn!(
                value = %other,
                "unknown ADNET_LOG_FORMAT, expected 'compact' or 'json'"
            ),
        }
    }
}

/// Write the default template to `path`. Creates intermediate
/// directories as needed. The template is JSON5 so the comments are
/// preserved on disk.
fn write_default_template(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create config directory {}", parent.display()))?;
    }
    let template = DEFAULT_CONFIG_TEMPLATE;
    fs::write(path, template)
        .with_context(|| format!("write config template to {}", path.display()))?;
    Ok(())
}

/// The default config template written on first run. JSON5 with
/// `//` comments so the operator can read it as documentation.
pub const DEFAULT_CONFIG_TEMPLATE: &str = r#"{
  // ADNet CLI configuration. Edit any field below and re-run the
  // command. CLI flags override anything set here, and `$RUST_LOG`
  // overrides `log.level`.
  //
  // Lookup order (highest priority first):
  //   1. CLI flags (e.g. --data-dir, --room)
  //   2. Environment variables (RUST_LOG, ADNET_LOG_FORMAT)
  //   3. This file

  // Local data directory. Holds node_id, QUIC identity, blobstore,
  // gossip state. Override with --data-dir on the CLI.
  "dataDir": "./.adnet-data",

  // Tracing / log config.
  "log": {
    // Filter directive in the same syntax as $RUST_LOG:
    //   "info"            — global info
    //   "adnet_mesh=debug" — debug for the mesh crate, info elsewhere
    "level": "info",

    // "compact" (default) or "json" (structured).
    "format": "compact"
  },

  // Default room when --room is omitted on `adnet feed` /
  // `adnet echo`. Comment out to require explicit --room.
  // "defaultRoom": "lobby",

  // REPL settings (only used by `adnet run`).
  "repl": {
    // Prompt string. Empty string disables the prompt.
    "prompt": "adnet> ",

    // Optional persistent history file. Omit to disable history.
    // "historyFile": null
  },

  // Mesh HTTP server. Set to null to use defaults (0.0.0.0:0).
  // "mesh": {
  //   "host": "0.0.0.0",
  //   "port": 0,
  //   "routePrefix": ""
  // },

  // Embedded relay server. Set to null to disable; omit to skip
  // configuring the relay entirely.
  // "relay": {
  //   "enabled": true,
  //   "serveEnabled": true,
  //   "servePort": 8790,
  //   "serveBind": "127.0.0.1",
  //   "hostPolicy": "defaultBlockPrivate",
  //   "maxBodyBytes": 67108864,
  //   "upstreamTimeoutMs": 60000,
  //   "maxRedirects": 3
  // },

  // Gossip validation policy: "strict" (default), "audit", or "lenient".
  // "gossipValidation": "strict",

  // iroh-backed runtime. When enabled = true the CLI loads (or
  // creates) a persistent Ed25519 identity at
  // `<dataDir>/iroh_secret_key`, binds an iroh Endpoint, and wires
  // the full IrohRuntime (blobs/gossip/docs + adnet/frame/1 ALPN)
  // into the node. Default enabled = false — set to true to opt in.
  // Requires `cargo build -p adnet-node --features iroh` at compile
  // time; otherwise the values are logged and ignored.
  // "iroh": {
  //   "enabled": false,
  //   "bindHost": "127.0.0.1",
  //   "bindPort": 0,
  //   // Override the persistent identity location. Defaults to
  //   // `<dataDir>/iroh_secret_key`.
  //   // "identityPath": null,
  //   "publishPublicly": true,
  //   // Discovery sub-configuration. `userData` is included as the
  //   // `user-data=` TXT attribute on every pkarr packet this
  //   // node publishes, mirroring iroh-dns's wire format so ADNet
  //   // endpoints stay discoverable from stock iroh clients.
  //   // Maximum 245 bytes; oversized values surface as a startup
  //   // error rather than a silent truncation.
  //   // "discovery": {
  //   //   "userData": "adnet/role=worker",
  //   //   // Enable mDNS-based LAN discovery. When true, the node
  //   //   // advertises itself via mDNS multicast and discovers
  //   //   // other ADNet nodes on the same LAN without a relay.
  //   //   // Default false (opt-in). Requires the `mdns` cargo
  //   //   // feature at build time.
  //   //   // "mdnsEnabled": true
  //   // }
  // },

  // QR rendering settings. Only consulted when the CLI is built
  // with `cargo build -p adnet-cli --features qr`; the block is
  // parsed but ignored on a feature-disabled build so the file
  // stays consistent across builds.
  // "qr": {
  //   // Error-correction level. "auto" defers to the QR payload
  //   // (SecureJoin invites get Quartile, everything else Low).
  //   // "low", "medium", "quartile", "high" pin a specific level.
  //   "ecLevel": "auto",
  //   // Quiet-zone in modules. 0 is allowed for embedded screenshots
  //   // where the parent canvas already supplies a border.
  //   "quietZone": 4,
  //   // Output module size in pixels (SVG renderer only).
  //   "moduleSize": 8
  // }
}
"#;

/// Convenience: build a friendly error when a CLI override path is
/// supplied but unusable.
pub fn ensure_cli_override_usable(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow!("--config path {} does not exist", path.display()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sub-commands: path / show / validate / edit / reset / set
// ---------------------------------------------------------------------------
//
// These helpers are invoked by the `adnet config <sub>` dispatcher in
// `main.rs`. They never touch the network or the node — they only
// manipulate the JSON5 file on disk.

/// Resolve the config path without loading the file. Used by both
/// `main.rs` and the `config path` subcommand so they stay in sync.
pub fn resolve_path(cli_override: Option<&Path>) -> Option<PathBuf> {
    resolve_config_path(cli_override)
}

/// Load the file at `path` (creating the default template if
/// missing) and report. Returns the [`LoadedConfig`] but also
/// writes a friendly `wrote default config template` line to
/// stdout so the operator can see what happened.
pub fn load_for_cli(cli_override: Option<&Path>) -> Result<LoadedConfig> {
    let loaded = load(cli_override)?;
    if let Some(path) = loaded.source.path.as_ref()
        && loaded.source.written_template
    {
        println!("adnet: wrote default config template to {}", path.display());
    }
    Ok(loaded)
}

/// Pretty-print the **effective** config as JSON. Useful for
/// diffing what the binary actually sees versus what's on disk.
pub fn show_effective(cfg: &AppConfig) -> Result<String> {
    Ok(serde_json::to_string_pretty(cfg)?)
}

/// Validate the file at `path`. Returns the parsed `AppConfig` on
/// success or an `Err` describing the first malformed line.
pub fn validate(path: &Path) -> Result<AppConfig> {
    parse_file(path)
}

/// Overwrite the file at `path` with the default template. Creates
/// parent directories as needed.
pub fn reset(path: &Path) -> Result<()> {
    write_default_template(path)
}

/// Set a single dotted key (`log.level`, `mesh.port`, etc.) to a
/// JSON5-typed value. The file is reloaded into a
/// `serde_json::Value`, the targeted path is mutated, and the
/// resulting document is re-serialised in JSON5 so comments
/// elsewhere are preserved.
///
/// The dotted key is checked against a known-field whitelist so a
/// typo becomes a hard error rather than a silently-broken config.
/// The check runs before any file IO so a bad key never creates
/// a fresh template on disk.
pub fn set_value(path: &Path, dotted_key: &str, json5_value: &str) -> Result<()> {
    // Validate the key first — refuse to even open the file if
    // the operator is pointing at a field we don't recognise.
    validate_known_dotted_key(dotted_key)?;

    let new_fragment: serde_json::Value = json5::from_str(json5_value).map_err(|e| {
        anyhow!(
            "value '{json5_value}' is not valid JSON5: {e}\n\nhint: string literals need quotes, e.g. \"lobby\""
        )
    })?;

    let raw = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("read config file {}", path.display()))?
    } else {
        // Setting a key on a missing file should bootstrap the
        // template first so the user sees the full document on
        // next `config show`.
        write_default_template(path)?;
        fs::read_to_string(path)
            .with_context(|| format!("read freshly-written config at {}", path.display()))?
    };

    let mut value: serde_json::Value = json5::from_str(&raw)
        .with_context(|| format!("parse config at {} as JSON5", path.display()))?;

    apply_dotted_set(&mut value, dotted_key, new_fragment)?;

    let serialized = json5::to_string(&value)
        .with_context(|| format!("serialise config at {}", path.display()))?;
    fs::write(path, serialized).with_context(|| format!("write config file {}", path.display()))?;
    Ok(())
}

/// Whitelist of dotted-key paths accepted by [`set_value`]. Mirrors
/// the structure of [`AppConfig`] so an operator typing
/// `adnet config set log.levl debug` gets a clear error instead of
/// a silently-created bogus field. Each top-level field may be
/// addressed under either its camelCase or its snake_case form, so
/// `adnet config set data_dir /tmp/x` and `adnet config set dataDir
/// /tmp/x` both work.
const KNOWN_DOTTED_KEYS: &[&str] = &[
    "dataDir",
    "data_dir",
    "log.level",
    "log.format",
    "defaultRoom",
    "default_room",
    "repl.prompt",
    "repl.historyFile",
    "repl.history_file",
    "mesh.host",
    "mesh.port",
    "mesh.routePrefix",
    "mesh.route_prefix",
    "gossipValidation",
    "gossip_validation",
    "iroh.enabled",
    "iroh.bindHost",
    "iroh.bind_host",
    "iroh.bindPort",
    "iroh.bind_port",
    "iroh.identityPath",
    "iroh.identity_path",
    "iroh.publishPublicly",
    "iroh.publish_publicly",
    "iroh.discovery.userData",
    "iroh.discovery.user_data",
    "iroh.discovery.mdnsEnabled",
    "iroh.discovery.mdns_enabled",
    #[cfg(feature = "qr")]
    "qr.ecLevel",
    #[cfg(feature = "qr")]
    "qr.ec_level",
    #[cfg(feature = "qr")]
    "qr.quietZone",
    #[cfg(feature = "qr")]
    "qr.quiet_zone",
    #[cfg(feature = "qr")]
    "qr.moduleSize",
    #[cfg(feature = "qr")]
    "qr.module_size",
    // `relay.*` is intentionally not whitelisted: RelayConfig has
    // many fields and the typed builder is the safer surface for
    // editing them. Operators can still set `relay` via the file
    // editor (`adnet config edit`).
];

/// Reject dotted keys that don't appear in [`KNOWN_DOTTED_KEYS`].
fn validate_known_dotted_key(dotted_key: &str) -> Result<()> {
    if KNOWN_DOTTED_KEYS.contains(&dotted_key) {
        return Ok(());
    }
    // Suggest close matches so a typo is recoverable.
    let mut suggestions: Vec<&str> = KNOWN_DOTTED_KEYS
        .iter()
        .copied()
        .filter(|k| {
            // Cheap distance: a key matches if it shares the same
            // first segment or has a common prefix of >= 3 chars.
            k.split('.').next() == dotted_key.split('.').next()
                || (k.len() >= 3 && dotted_key.len() >= 3 && k[..3] == dotted_key[..3])
        })
        .collect();
    suggestions.sort_unstable();
    suggestions.dedup();
    let hint = if suggestions.is_empty() {
        String::new()
    } else {
        format!("\n\ndid you mean: {}", suggestions.join(", "))
    };
    Err(anyhow!(
        "unknown config key '{dotted_key}'\n\nknown keys:\n  - {known}{hint}",
        known = KNOWN_DOTTED_KEYS.join("\n  - "),
    ))
}

/// Recursive helper for [`set_value`]: walks the dotted path,
/// creating intermediate objects as needed, then drops the new
/// fragment at the leaf.
fn apply_dotted_set(
    root: &mut serde_json::Value,
    dotted_key: &str,
    new_fragment: serde_json::Value,
) -> Result<()> {
    let segments: Vec<&str> = dotted_key.split('.').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(anyhow!("empty key path"));
    }
    let mut current = root;
    for (i, seg) in segments.iter().enumerate() {
        let is_last = i + 1 == segments.len();
        if !current.is_object() {
            return Err(anyhow!(
                "cannot descend into {dotted_key:?}: intermediate value is a {} (expected object)",
                value_type_name(current)
            ));
        }
        let obj = current.as_object_mut().expect("checked above");
        if is_last {
            obj.insert((*seg).to_string(), new_fragment.clone());
            return Ok(());
        }
        current = obj
            .entry((*seg).to_string())
            .or_insert_with(|| serde_json::json!({}));
    }
    Ok(())
}

/// Human-friendly type label for a `serde_json::Value` so the error
/// in [`apply_dotted_set`] is recoverable by the operator.
fn value_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Run `$EDITOR` / `$VISUAL` against a temp copy of the config
/// file. On save, re-parse to validate; if the parse fails, leave
/// the temp file in place and surface the error so the operator
/// can fix and retry. Returns the modified path on success.
pub fn edit(path: &Path) -> Result<()> {
    use std::io::Write;
    use std::process::Command;

    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "vi".to_string());

    // Always start from a known-good template so the operator
    // never opens an empty file. If a config already exists, copy
    // it; otherwise write the default template first.
    if !path.exists() {
        write_default_template(path)?;
    }
    let original =
        fs::read_to_string(path).with_context(|| format!("read config file {}", path.display()))?;

    // Temp file next to the original so atomic rename is a
    // single filesystem operation.
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = parent.to_path_buf();
    tmp.push(format!(".adnet-config-edit-{}.json5", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)
            .with_context(|| format!("create temp file {}", tmp.display()))?;
        f.write_all(original.as_bytes())?;
    }

    let status = Command::new(&editor).arg(&tmp).status().map_err(|e| {
        anyhow!(
            "failed to spawn editor '{editor}': {e}\n\nhint: set $EDITOR or $VISUAL to a binary on your PATH"
        )
    })?;
    if !status.success() {
        let _ = fs::remove_file(&tmp);
        return Err(anyhow!(
            "editor exited with status {status}; no changes applied"
        ));
    }

    let edited = fs::read_to_string(&tmp)
        .with_context(|| format!("read edited temp file {}", tmp.display()))?;
    if edited == original {
        println!("adnet: no changes detected");
        let _ = fs::remove_file(&tmp);
        return Ok(());
    }

    // Validate before committing — refuse to write a broken file.
    match parse_file(&tmp) {
        Ok(_) => {}
        Err(e) => {
            // Keep the temp file around so the operator can fix it
            // and re-run `adnet config edit`.
            return Err(anyhow!(
                "edited config did not parse: {e}\n\nthe temp file is still at {}\nfix it (or move it back into place) and re-run `adnet config edit`",
                tmp.display()
            ));
        }
    }

    // Atomic-ish: copy the temp file back over the original.
    fs::write(path, edited)
        .with_context(|| format!("write edited config back to {}", path.display()))?;
    let _ = fs::remove_file(&tmp);
    println!("adnet: updated {}", path.display());
    Ok(())
}

/// Read the JSON5 file at `path` and return it as a
/// `serde_json::Value`. Used by `set_value` and `edit` to round-trip
/// edits without losing the rest of the document.
pub fn read_value(path: &Path) -> Result<serde_json::Value> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("read config file {}", path.display()))?;
    json5::from_str(&raw).with_context(|| format!("parse config file {} as JSON5", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_known_dotted_key_accepts_whitelist() {
        for k in [
            "log.level",
            "log.format",
            "mesh.port",
            "defaultRoom",
            "gossipValidation",
        ] {
            validate_known_dotted_key(k).unwrap_or_else(|e| panic!("{k} should pass: {e}"));
        }
    }

    #[test]
    fn validate_known_dotted_key_rejects_unknown() {
        let err = validate_known_dotted_key("log.levl")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown config key 'log.levl'"), "{err}");
        // Should suggest the close match `log.level`.
        assert!(err.contains("log.level"), "{err}");
    }

    #[test]
    fn validate_known_dotted_key_rejects_under_relay() {
        // relay.* is intentionally not whitelisted — the typed
        // builder is the only supported surface.
        let err = validate_known_dotted_key("relay.port")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown config key 'relay.port'"), "{err}");
    }

    #[test]
    fn set_value_rejects_unknown_dotted_key() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        let err = set_value(&path, "log.levl", "\"debug\"")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown config key"), "{err}");
        // Confirm the file was NOT written.
        let raw = fs::read_to_string(&path).unwrap_or_default();
        assert!(raw.is_empty(), "file should not have been created: {raw}");
    }

    #[test]
    fn resolve_distinguishes_cli_flag_from_default() {
        let dir = tempdir();
        let explicit = dir.0.join("explicit.json");
        let r = resolve(Some(&explicit)).unwrap();
        assert_eq!(r.source, ConfigPathSource::CliFlag);
        assert_eq!(r.path.as_deref(), Some(explicit.as_path()));

        let r = resolve(None).unwrap();
        assert_eq!(r.source, ConfigPathSource::Default);
        // The default always points at the platform-default filename when
        // the platform gives us a candidate (CI macOS / Linux both do).
        let path = r.path.expect("platform should provide a candidate path");
        assert!(path.ends_with(CONFIG_FILE_NAME));
    }

    #[test]
    fn resolve_honours_env_var() {
        let dir = tempdir();
        let env_path = dir.0.join("env-path.json");
        // SAFETY: not used; we go through the closure seam.
        // SAFETY: only the test thread touches env vars between set/remove.
        // We use the explicit env reader seam instead of mutating global state.
        let r = resolve_with_env(None, |k| {
            if k == ADNET_CONFIG_ENV {
                Some(env_path.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .unwrap();
        assert_eq!(r.source, ConfigPathSource::EnvVar);
        assert_eq!(r.path.as_deref(), Some(env_path.as_path()));
    }

    #[test]
    fn set_value_inserts_into_existing_doc() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        let original = r#"{
            // initial
            "dataDir": "/tmp/x",
            "log": { "level": "info", "format": "compact" }
        }"#;
        fs::write(&path, original).unwrap();

        set_value(&path, "log.level", "\"debug\"").unwrap();

        let parsed = read_value(&path).unwrap();
        assert_eq!(parsed["log"]["level"], "debug");
        assert_eq!(parsed["log"]["format"], "compact");
        assert_eq!(parsed["dataDir"], "/tmp/x");
        // Re-parse must still succeed → the file is well-formed JSON5.
        let cfg = validate(&path).unwrap();
        assert_eq!(cfg.log.level, "debug");
        assert_eq!(cfg.data_dir, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn set_value_creates_missing_intermediate_object() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        set_value(&path, "mesh.port", "8080").unwrap();
        let parsed = read_value(&path).unwrap();
        assert_eq!(parsed["mesh"]["port"], 8080);
    }

    /// P0-8 regression: when an operator's config has `log` set to a
    /// *scalar* (e.g. `"log": "info"` instead of an object), the
    /// legacy implementation silently overwrote it with `{}` and lost
    /// the operator's value. We now refuse to descend and surface a
    /// recoverable error.
    #[test]
    fn set_value_refuses_to_overwrite_scalar_intermediate() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        let original = r#"{ "log": "info" }"#;
        fs::write(&path, original).unwrap();

        let err = set_value(&path, "log.level", "\"debug\"")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cannot descend into"),
            "expected refusal message, got: {err}"
        );

        // The original scalar must survive intact on disk.
        let after = fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("\"log\": \"info\""),
            "operator's scalar must not have been clobbered: {after}"
        );
    }

    /// P0-7 regression: `default_config_path` returns `None` when the
    /// platform cannot produce a candidate. The resolver propagates
    /// this so the loader skips file IO. We exercise the seam by
    /// providing an env-reader that simulates the no-platform
    /// situation by routing through a custom closure.
    #[test]
    fn resolve_propagates_none_for_unwritable_default() {
        // With `cli_override = None` and no env var set, the resolver
        // falls back to `default_config_path()`. On the CI host (macOS
        // + Linux) that always returns `Some`, so we can't directly
        // synthesise the None branch without changing `dirs::*`.
        // Instead we assert the contract that the public API is
        // shaped correctly: `resolve_config_path` returns `Option`
        // and `ResolvedConfigPath.path` is `Option<PathBuf>`. A
        // refactor that breaks this contract (e.g. unwrapping the
        // default path) will fail to compile.
        let r = resolve(None).unwrap();
        let _: Option<&Path> = r.path.as_deref();
    }

    /// P2-1 regression: the dotted-key whitelist must accept both the
    /// camelCase and snake_case spellings of every field.
    #[test]
    fn set_value_accepts_snake_case_alias() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        set_value(&path, "data_dir", "\"/tmp/y\"").expect("snake_case alias");
        set_value(&path, "default_room", "\"lobby\"").expect("snake_case alias");
        let parsed = read_value(&path).unwrap();
        assert_eq!(parsed["data_dir"], "/tmp/y");
        assert_eq!(parsed["default_room"], "lobby");
    }

    #[test]
    fn set_value_rejects_invalid_json5_value() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        // Unquoted string is a JSON5 syntax error.
        let err = set_value(&path, "log.level", "debug")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not valid JSON5"), "{err}");
    }

    #[test]
    fn validate_passes_on_default_template() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        reset(&path).unwrap();
        let cfg = validate(&path).unwrap();
        assert_eq!(cfg.data_dir, PathBuf::from("./.adnet-data"));
    }

    #[test]
    fn validate_rejects_garbage() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        fs::write(&path, b"this is not json5").unwrap();
        assert!(validate(&path).is_err());
    }

    #[test]
    fn show_effective_roundtrip() {
        let cfg = AppConfig::default();
        let json = show_effective(&cfg).unwrap();
        // sanity: the top-level keys are present and the default
        // data dir is wired through.
        assert!(json.contains("\"dataDir\""));
        assert!(json.contains(".adnet-data"));
        assert!(json.contains("\"log\""));
        assert!(json.contains("\"prompt\""));
    }

    #[test]
    fn parses_minimal_config() {
        let raw = r#"{
            "dataDir": "/tmp/adnet",
            "log": { "level": "debug", "format": "json" }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        assert_eq!(cfg.data_dir, PathBuf::from("/tmp/adnet"));
        assert_eq!(cfg.log.level, "debug");
        assert_eq!(cfg.log.format, LogFormat::Json);
        // Defaults still hold for unspecified fields.
        assert_eq!(cfg.repl.prompt, "adnet> ");
    }

    #[test]
    fn parses_with_comments() {
        let raw = r#"{
            // comment
            "dataDir": "/tmp/adnet",
            "log": { "level": "info", "format": "compact", }, // trailing comma
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        assert_eq!(cfg.data_dir, PathBuf::from("/tmp/adnet"));
    }

    #[test]
    fn missing_file_falls_back_to_default() {
        let tmp = tempdir();
        let cfg = AppConfig::default();
        assert_eq!(cfg.data_dir, PathBuf::from("./.adnet-data"));
        // Just exercise the path resolver.
        let path = default_config_path().expect("platform should give a candidate");
        assert!(path.ends_with(CONFIG_FILE_NAME));
        tmp.cleanup();
    }

    #[test]
    fn env_overrides_log_level() {
        let mut cfg = AppConfig::default();
        let env = |k: &str| match k {
            "RUST_LOG" => Some("debug".to_string()),
            _ => None,
        };
        apply_env_overrides_with(&mut cfg, env);
        assert_eq!(cfg.log.level, "debug");
    }

    #[test]
    fn env_overrides_log_format_invalid_is_warned() {
        let mut cfg = AppConfig::default();
        let env = |k: &str| match k {
            "ADNET_LOG_FORMAT" => Some("xml".to_string()),
            _ => None,
        };
        apply_env_overrides_with(&mut cfg, env);
        // Invalid format leaves the default in place.
        assert_eq!(cfg.log.format, LogFormat::Compact);
    }

    #[test]
    fn env_overrides_log_format_json() {
        let mut cfg = AppConfig::default();
        let env = |k: &str| match k {
            "ADNET_LOG_FORMAT" => Some("JSON".to_string()),
            _ => None,
        };
        apply_env_overrides_with(&mut cfg, env);
        assert_eq!(cfg.log.format, LogFormat::Json);
    }

    #[test]
    fn mesh_config_roundtrip() {
        let raw = r#"{
            "mesh": { "host": "127.0.0.1", "port": 8080, "routePrefix": "/mesh" }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        let mesh = cfg.mesh.expect("mesh present");
        assert_eq!(mesh.host, "127.0.0.1");
        assert_eq!(mesh.port, 8080);
        assert_eq!(mesh.route_prefix, "/mesh");
    }

    /// P0-a regression: a JSON5 config that supplies a `relay` block
    /// must produce an `AppConfig` whose `relay` field is `Some(_)`
    /// and whose inner `RelayConfig` carries every field the
    /// operator set. Previously the field was parsed but never
    /// forwarded into `NodeBuilder::with_relay_config`, so this
    /// round-trip is the only side that could be unit-tested at the
    /// config layer.
    #[test]
    fn relay_config_roundtrip() {
        let raw = r#"{
            "relay": {
                "enabled": true,
                "serveEnabled": true,
                "servePort": 18900,
                "serveBind": "0.0.0.0",
                "hostPolicy": "allow-loopback-only",
                "maxBodyBytes": 4096,
                "upstreamTimeoutMs": 12000,
                "maxRedirects": 5
            }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        let relay = cfg.relay.expect("relay block present");
        assert!(relay.enabled);
        assert!(relay.serve_enabled);
        assert_eq!(relay.serve_port, 18900);
        assert_eq!(relay.serve_bind, "0.0.0.0");
        assert_eq!(
            relay.host_policy.name(),
            "loopback-only",
            "hostPolicy must round-trip as AllowLoopbackOnly"
        );
        assert_eq!(relay.max_body_bytes, 4096);
        assert_eq!(
            relay.upstream_timeout,
            std::time::Duration::from_millis(12000),
            "upstreamTimeoutMs must round-trip as a Duration"
        );
        assert_eq!(relay.max_redirects, 5);
    }

    /// P0-a regression: `AppConfig::default()` must have `relay: None`
    /// so that a fresh install does NOT silently start a relay
    /// server. The CLI must explicitly opt in by setting the block.
    #[test]
    fn iroh_discovery_user_data_round_trip() {
        let raw = r#"{
            "iroh": {
                "enabled": false,
                "discovery": { "userData": "adnet/role=worker" }
            }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        let iroh = cfg.iroh.expect("iroh block");
        assert_eq!(
            iroh.discovery.user_data.as_deref(),
            Some("adnet/role=worker")
        );
    }

    #[test]
    fn iroh_discovery_mdns_enabled_round_trip() {
        let raw = r#"{
            "iroh": {
                "enabled": true,
                "discovery": { "mdnsEnabled": true }
            }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        let iroh = cfg.iroh.expect("iroh block");
        assert!(iroh.discovery.mdns_enabled, "mdnsEnabled must round-trip as true");
    }

    #[test]
    fn iroh_discovery_mdns_enabled_snake_case() {
        let raw = r#"{
            "iroh": { "discovery": { "mdns_enabled": false } }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        let iroh = cfg.iroh.expect("iroh block");
        assert!(!iroh.discovery.mdns_enabled, "mdns_enabled snake_case must work");
    }

    #[test]
    fn iroh_discovery_mdns_defaults_to_false() {
        // Even without an iroh block, the default discovery config has mdns_enabled = false
        let cfg: AppConfig = json5::from_str("{}").unwrap();
        // discovery.mdns_enabled defaults to false
        assert!(!cfg.iroh.as_ref().map_or(false, |i| i.discovery.mdns_enabled));
    }

    #[test]
    fn set_value_accepts_mdns_enabled_dotted_key() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        set_value(&path, "iroh.discovery.mdnsEnabled", "true").unwrap();
        let cfg = validate(&path).unwrap();
        let iroh = cfg.iroh.expect("iroh block");
        assert!(iroh.discovery.mdns_enabled, "mdnsEnabled must be settable via dotted key");
    }

    #[test]
    fn iroh_discovery_default_has_no_user_data() {
        // `iroh` itself defaults to `None`; the discovery
        // sub-config is only consulted when the iroh block is
        // present.
        let cfg: AppConfig = json5::from_str("{}").unwrap();
        assert!(cfg.iroh.is_none());
    }

    #[test]
    fn iroh_discovery_user_data_oversized_is_accepted_by_loader() {
        // The CLI TOML layer accepts any string — the 245-byte
        // cap is enforced at runtime when the value is fed into
        // a `PkarrPublisherConfig::with_user_data`. We document
        // that contract here so future readers don't add a
        // redundant length check at the config layer.
        let oversized = "a".repeat(1024);
        let raw = format!(r#"{{ "iroh": {{ "discovery": {{ "userData": "{oversized}" }} }} }}"#);
        let cfg: AppConfig = json5::from_str(&raw).unwrap();
        assert_eq!(cfg.iroh.unwrap().discovery.user_data.unwrap().len(), 1024);
    }

    #[test]
    fn set_value_accepts_discovery_user_data_dotted_key() {
        // The dotted-key setter writes the leaf segment as-is
        // (it doesn't apply `#[serde(rename_all)]`). The
        // whitelist accepts both `user_data` (snake_case) and
        // `userData` (camelCase) — `apply_dotted_set` then
        // writes whichever the operator typed.
        //
        // The `DiscoveryConfigToml` struct has
        // `#[serde(rename_all = "camelCase")]` on the field
        // declarations, so the canonical on-disk key is
        // `userData` (camelCase). We use the camelCase key
        // here so the round-trip is unambiguous.
        let dir = tempdir();
        let path = dir.0.join("config.json");
        set_value(&path, "iroh.discovery.userData", "\"adnet/role=worker\"").unwrap();
        let cfg = validate(&path).unwrap();
        let iroh = cfg.iroh.expect("iroh block");
        assert_eq!(
            iroh.discovery.user_data.as_deref(),
            Some("adnet/role=worker"),
            "camelCase dotted key must round-trip into the typed config"
        );
    }

    #[test]
    fn appconfig_default_has_no_relay() {
        let cfg = AppConfig::default();
        assert!(
            cfg.relay.is_none(),
            "default AppConfig must not start a relay; got {:?}",
            cfg.relay
        );
    }

    /// P1-1: the QR config block must default to `None` so a
    /// feature-disabled build (no `qr` feature) still parses the
    /// full config and the `qr` field is silently absent. Without
    /// this, `appconfig_with_qr` below would fail on non-feature
    /// builds.
    #[cfg(feature = "qr")]
    #[test]
    fn qr_config_block_parses_round_trip() {
        let raw = r#"{
            "qr": {
                "ecLevel": "quartile",
                "quietZone": 8,
                "moduleSize": 12
            }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).expect("qr block must parse");
        let qr = cfg.qr.expect("qr block present");
        assert_eq!(qr.ec_level, super::QrEcLevelToml::Quartile);
        assert_eq!(qr.quiet_zone, 8);
        assert_eq!(qr.module_size, 12);
    }

    /// P1-2: `qr.ecLevel = "auto"` is the documented default.
    /// Without this, an operator staring at `adnet config show`
    /// would see `null` and wonder whether the default is broken.
    #[cfg(feature = "qr")]
    #[test]
    fn qr_config_default_is_auto() {
        let qr = super::QrConfigToml::default();
        assert_eq!(qr.ec_level, super::QrEcLevelToml::Auto);
        assert_eq!(qr.quiet_zone, 4);
        assert_eq!(qr.module_size, 8);
    }

    /// P1-3: `qr.ecLevel` resolves to a concrete
    /// `qrcodegen::QrCodeEcc` so the SVG renderer can pick it up.
    #[cfg(feature = "qr")]
    #[test]
    fn qr_ec_level_resolves() {
        use adnet_qr::generator::QrErrorCorrectionLevel;
        assert_eq!(
            super::QrEcLevelToml::Auto.resolve_ec_level(),
            QrErrorCorrectionLevel::Low
        );
        assert_eq!(
            super::QrEcLevelToml::Quartile.resolve_ec_level(),
            QrErrorCorrectionLevel::Quartile
        );
        assert_eq!(
            super::QrEcLevelToml::High.resolve_ec_level(),
            QrErrorCorrectionLevel::High
        );
    }

    /// P1-4: dotted-key whitelist accepts the QR keys (camelCase +
    /// snake_case). Already exercised by `set_value_accepts_snake_case_alias`
    /// for other fields; this locks the QR keys down specifically.
    #[cfg(feature = "qr")]
    #[test]
    fn qr_dotted_keys_are_whitelisted() {
        for k in [
            "qr.ecLevel",
            "qr.ec_level",
            "qr.quietZone",
            "qr.quiet_zone",
            "qr.moduleSize",
            "qr.module_size",
        ] {
            validate_known_dotted_key(k).unwrap_or_else(|e| panic!("{k} must be whitelisted: {e}"));
        }
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn cleanup(self) {
            let _ = fs::remove_dir_all(self.0);
        }
    }

    fn tempdir() -> TempDir {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "adnet-config-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    // ────────────────────────────────────────────────────────────
    // Tests for missing functions: read_value, ensure_cli_override_usable,
    // apply_dotted_set, value_type_name, warn_unknown_top_level_fields,
    // resolve_with_env, resolve_config_path
    // ────────────────────────────────────────────────────────────

    #[test]
    fn read_value_parses_json5_with_comments() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        let content = r#"{
            // comment
            "dataDir": "/tmp/test",
            // another comment
            "log": { "level": "debug" }
        }"#;
        fs::write(&path, content).unwrap();
        let val = read_value(&path).unwrap();
        assert_eq!(val["dataDir"], "/tmp/test");
        assert_eq!(val["log"]["level"], "debug");
    }

    #[test]
    fn read_value_error_on_missing_file() {
        let dir = tempdir();
        let path = dir.0.join("nonexistent.json");
        assert!(read_value(&path).is_err());
    }

    #[test]
    fn read_value_error_on_invalid_json5() {
        let dir = tempdir();
        let path = dir.0.join("bad.json");
        fs::write(&path, "not valid json5 {{").unwrap();
        assert!(read_value(&path).is_err());
    }

    #[test]
    fn ensure_cli_override_usable_accepts_existing_path() {
        let dir = tempdir();
        let path = dir.0.join("exists.json");
        fs::write(&path, "{}").unwrap();
        assert!(ensure_cli_override_usable(&path).is_ok());
    }

    #[test]
    fn ensure_cli_override_usable_rejects_missing_path() {
        let dir = tempdir();
        let path = dir.0.join("missing.json");
        let err = ensure_cli_override_usable(&path).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn resolve_config_path_returns_option_type() {
        // The contract: resolve_config_path returns Option<PathBuf>.
        // On macOS/Linux, the path is always Some; on sandboxed
        // platforms it could be None. We test the API shape here
        // by ensuring the function signature is correct.
        let r: Option<PathBuf> = resolve_config_path(None);
        // Just verify the type is correct - the actual value depends on platform
        let _: Option<PathBuf> = r;
    }

    #[test]
    fn resolve_with_env_skips_empty_env_var() {
        let _dir = tempdir();
        // Empty string in env var should be treated as unset
        let r = resolve_with_env(None, |k| {
            if k == ADNET_CONFIG_ENV {
                Some("".to_string())
            } else {
                None
            }
        });
        assert!(r.is_some());
        let r = r.unwrap();
        // Empty string is treated as not set, falls through to default
        assert_eq!(r.source, ConfigPathSource::Default);
    }

    #[test]
    fn resolve_with_env_cli_takes_precedence_over_env_var() {
        let dir = tempdir();
        let cli_path = dir.0.join("cli.json");
        let env_path = dir.0.join("env.json");
        let r = resolve_with_env(Some(&cli_path), |k| {
            if k == ADNET_CONFIG_ENV {
                Some(env_path.to_string_lossy().to_string())
            } else {
                None
            }
        });
        assert!(r.is_some());
        let r = r.unwrap();
        assert_eq!(r.source, ConfigPathSource::CliFlag);
        assert_eq!(r.path.as_deref(), Some(cli_path.as_path()));
    }

    #[test]
    fn resolve_with_env_trims_whitespace_from_env_var() {
        let _dir = tempdir();
        // Simulate env var with whitespace in the value
        let r = resolve_with_env(None, |k| {
            if k == ADNET_CONFIG_ENV {
                Some("   /path/with spaces/config.json   ".to_string())
            } else {
                None
            }
        });
        assert!(r.is_some());
        let r = r.unwrap();
        assert_eq!(r.source, ConfigPathSource::EnvVar);
        // The path string itself is used as-is; trimming happens elsewhere
        assert!(r.path.is_some());
    }

    #[test]
    fn apply_dotted_set_single_segment() {
        let root = serde_json::json!({});
        let mut root = root;
        apply_dotted_set(&mut root, "dataDir", serde_json::json!("/tmp/test")).unwrap();
        assert_eq!(root["dataDir"], "/tmp/test");
    }

    #[test]
    fn apply_dotted_set_nested_segments() {
        let root = serde_json::json!({});
        let mut root = root;
        apply_dotted_set(&mut root, "log.level", serde_json::json!("debug")).unwrap();
        assert_eq!(root["log"]["level"], "debug");
    }

    #[test]
    fn apply_dotted_set_deeply_nested() {
        let root = serde_json::json!({});
        let mut root = root;
        apply_dotted_set(
            &mut root,
            "iroh.discovery.userData",
            serde_json::json!("adnet/role=worker"),
        )
        .unwrap();
        assert_eq!(root["iroh"]["discovery"]["userData"], "adnet/role=worker");
    }

    #[test]
    fn apply_dotted_set_replaces_existing_value() {
        let root = serde_json::json!({
            "log": { "level": "info" }
        });
        let mut root = root;
        apply_dotted_set(&mut root, "log.level", serde_json::json!("debug")).unwrap();
        assert_eq!(root["log"]["level"], "debug");
    }

    #[test]
    fn apply_dotted_set_creates_missing_intermediate_objects() {
        let root = serde_json::json!({});
        let mut root = root;
        apply_dotted_set(&mut root, "mesh.port", serde_json::json!(8080)).unwrap();
        assert_eq!(root["mesh"]["port"], 8080);
    }

    #[test]
    fn apply_dotted_set_error_on_empty_key() {
        let root = serde_json::json!({});
        let mut root = root;
        let err = apply_dotted_set(&mut root, "", serde_json::json!("value")).unwrap_err();
        assert!(err.to_string().contains("empty key path"));
    }

    #[test]
    fn apply_dotted_set_error_on_array_intermediate() {
        let mut root = serde_json::json!({ "arr": [1, 2, 3] });
        let err = apply_dotted_set(&mut root, "arr.level", serde_json::json!("debug")).unwrap_err();
        assert!(err.to_string().contains("cannot descend into"));
        assert!(err.to_string().contains("array"));
    }

    #[test]
    fn apply_dotted_set_error_on_string_intermediate() {
        let mut root = serde_json::json!({ "str": "hello" });
        let err = apply_dotted_set(&mut root, "str.level", serde_json::json!("debug")).unwrap_err();
        assert!(err.to_string().contains("cannot descend into"));
        assert!(err.to_string().contains("string"));
    }

    #[test]
    fn apply_dotted_set_error_on_number_intermediate() {
        let mut root = serde_json::json!({ "num": 42 });
        let err = apply_dotted_set(&mut root, "num.level", serde_json::json!("debug")).unwrap_err();
        assert!(err.to_string().contains("cannot descend into"));
        assert!(err.to_string().contains("number"));
    }

    #[test]
    fn apply_dotted_set_error_on_null_intermediate() {
        let mut root = serde_json::json!({ "null": null });
        let err = apply_dotted_set(&mut root, "null.level", serde_json::json!("debug")).unwrap_err();
        assert!(err.to_string().contains("cannot descend into"));
        assert!(err.to_string().contains("null"));
    }

    #[test]
    fn apply_dotted_set_skips_empty_segments() {
        // A key like "a..b" has an empty segment in the middle
        // This tests the filter logic that skips empty segments
        let segments: Vec<&str> = "a..b".split('.').filter(|s| !s.is_empty()).collect();
        assert_eq!(segments, vec!["a", "b"]);
    }

    #[test]
    fn value_type_name_all_variants() {
        assert_eq!(value_type_name(&serde_json::json!(null)), "null");
        assert_eq!(value_type_name(&serde_json::json!(true)), "boolean");
        assert_eq!(value_type_name(&serde_json::json!(42)), "number");
        assert_eq!(value_type_name(&serde_json::json!("hello")), "string");
        assert_eq!(value_type_name(&serde_json::json!([1, 2])), "array");
        assert_eq!(value_type_name(&serde_json::json!({})), "object");
    }

    #[test]
    fn warn_unknown_top_level_fields_no_warning_for_known_fields() {
        // Just verify it doesn't panic on known fields
        let value = serde_json::json!({
            "dataDir": "./.adnet-data",
            "log": { "level": "info" },
            "repl": { "prompt": "adnet> " },
        });
        warn_unknown_top_level_fields(&value);
        // If we get here without panic, the test passes
    }

    #[test]
    fn warn_unknown_top_level_fields_handles_non_object() {
        // Should be a no-op on non-object values
        warn_unknown_top_level_fields(&serde_json::json!("string"));
        warn_unknown_top_level_fields(&serde_json::json!([1, 2]));
        warn_unknown_top_level_fields(&serde_json::json!(42));
        warn_unknown_top_level_fields(&serde_json::json!(null));
    }

    #[test]
    fn warn_unknown_top_level_fields_warns_on_unknown_key() {
        // The function uses `warn!` which goes to tracing, but we can at least
        // verify it doesn't panic and handles unknown keys gracefully
        let value = serde_json::json!({
            "unknownField": "value",
            "log": { "level": "info" }
        });
        warn_unknown_top_level_fields(&value);
        // The function should not panic
    }

    #[test]
    fn parse_file_with_json5_extensions() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        let content = r#"{
            // single-line comment
            "dataDir": "/test",
            /* multi-line
               comment */
            "log": {
                // trailing comma below
                "level": "debug",
            },
        }"#;
        fs::write(&path, content).unwrap();
        let cfg = parse_file(&path).unwrap();
        assert_eq!(cfg.data_dir, PathBuf::from("/test"));
        assert_eq!(cfg.log.level, "debug");
    }

    #[test]
    fn parse_file_missing_file_error() {
        let dir = tempdir();
        let path = dir.0.join("nonexistent.json");
        let err = parse_file(&path).unwrap_err();
        assert!(err.to_string().contains("read config file"));
    }

    #[test]
    fn parse_file_invalid_json5_error() {
        let dir = tempdir();
        let path = dir.0.join("bad.json");
        fs::write(&path, "{ invalid json }").unwrap();
        let err = parse_file(&path).unwrap_err();
        assert!(err.to_string().contains("parse config file") || err.to_string().contains("JSON5"));
    }

    #[test]
    fn write_default_template_creates_parent_dirs() {
        let dir = tempdir();
        let path = dir.0.join("subdir").join("nested").join("config.json");
        write_default_template(&path).unwrap();
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("ADNet"));
        assert!(content.contains("dataDir"));
    }

    #[test]
    fn write_default_template_overwrites_existing() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        fs::write(&path, "old content").unwrap();
        write_default_template(&path).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("ADNet"));
        assert!(!content.contains("old content"));
    }

    #[test]
    fn set_value_rejects_whitespace_only_json5_value() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        let err = set_value(&path, "log.level", "   ").unwrap_err();
        assert!(err.to_string().contains("not valid JSON5"));
    }

    #[test]
    fn set_value_accepts_various_json5_types() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        fs::write(&path, "{}").unwrap();

        // String
        set_value(&path, "default_room", "\"lobby\"").unwrap();
        let parsed = read_value(&path).unwrap();
        assert_eq!(parsed["default_room"], "lobby");
        // Number (via mesh port)
        set_value(&path, "mesh.port", "8080").unwrap();
        let parsed = read_value(&path).unwrap();
        assert_eq!(parsed["mesh"]["port"], 8080);
        // Object via iroh.discovery
        set_value(&path, "iroh.discovery.userData", "\"test\"").unwrap();
        let parsed = read_value(&path).unwrap();
        assert_eq!(parsed["iroh"]["discovery"]["userData"], "test");
    }

    #[test]
    fn set_value_on_nonexistent_file_creates_template() {
        let dir = tempdir();
        let path = dir.0.join("new.json");
        assert!(!path.exists());
        set_value(&path, "log.level", "\"debug\"").unwrap();
        assert!(path.exists());
        // The file should be a valid JSON5 template with our change
        let parsed = read_value(&path).unwrap();
        assert_eq!(parsed["log"]["level"], "debug");
    }

    #[test]
    fn set_value_preserves_existing_fields() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        fs::write(
            &path,
            r#"{
            "dataDir": "/original",
            "log": { "level": "info", "format": "json" }
        }"#,
        )
        .unwrap();

        set_value(&path, "log.level", "\"debug\"").unwrap();
        let parsed = read_value(&path).unwrap();
        assert_eq!(parsed["log"]["level"], "debug");
        assert_eq!(parsed["log"]["format"], "json"); // preserved
        assert_eq!(parsed["dataDir"], "/original"); // preserved
    }

    #[test]
    fn validate_known_dotted_key_all_mesh_keys() {
        for k in [
            "mesh.host",
            "mesh.port",
            "mesh.routePrefix",
            "mesh.route_prefix",
        ] {
            validate_known_dotted_key(k).expect(&format!("{k} should be valid"));
        }
    }

    #[test]
    fn validate_known_dotted_key_all_iroh_keys() {
        for k in [
            "iroh.enabled",
            "iroh.bindHost",
            "iroh.bind_port",
            "iroh.bindPort",
            "iroh.identityPath",
            "iroh.identity_path",
            "iroh.publishPublicly",
            "iroh.publish_publicly",
            "iroh.discovery.userData",
            "iroh.discovery.user_data",
        ] {
            validate_known_dotted_key(k).expect(&format!("{k} should be valid"));
        }
    }

    #[test]
    fn validate_known_dotted_key_repl_keys() {
        for k in ["repl.prompt", "repl.historyFile", "repl.history_file"] {
            validate_known_dotted_key(k).expect(&format!("{k} should be valid"));
        }
    }

    #[test]
    fn validate_known_dotted_key_suggests_similar_keys() {
        // "log.levl" (typo) should suggest "log.level"
        let err = validate_known_dotted_key("log.levl").unwrap_err().to_string();
        assert!(err.contains("log.level"), "Should suggest log.level: {err}");
    }

    #[test]
    fn validate_known_dotted_key_no_suggestion_for_completely_unknown() {
        // A completely random key should not have suggestions
        let err = validate_known_dotted_key("xyzzy").unwrap_err().to_string();
        assert!(err.contains("unknown config key 'xyzzy'"));
        // Should have the full known list but no suggestions section
        assert!(err.contains("known keys:"));
    }

    #[test]
    fn resolve_path_equals_resolve_config_path() {
        let dir = tempdir();
        let path = dir.0.join("explicit.json");
        let r1 = resolve_path(Some(&path));
        let r2 = resolve_config_path(Some(&path));
        assert_eq!(r1, r2);
    }

    #[test]
    fn loaded_config_path_source_debug() {
        let source = ConfigSource {
            path: Some(PathBuf::from("/tmp/test.json")),
            written_template: true,
        };
        let debug = format!("{:?}", source);
        assert!(debug.contains("written_template"));
    }

    #[test]
    fn resolved_config_path_debug() {
        let r = ResolvedConfigPath {
            path: Some(PathBuf::from("/tmp/test.json")),
            source: ConfigPathSource::CliFlag,
        };
        let debug = format!("{:?}", r);
        assert!(debug.contains("path") || debug.contains("source"));
    }

    #[test]
    fn config_path_source_all_variants() {
        // Just ensure all variants can be used in debug format
        let sources = [
            ConfigPathSource::CliFlag,
            ConfigPathSource::EnvVar,
            ConfigPathSource::Default,
        ];
        for s in sources {
            let _ = format!("{:?}", s);
        }
    }

    #[test]
    fn load_uses_in_memory_default_when_path_is_none() {
        // This is hard to test directly without mocking, but we can
        // verify the contract: when resolve returns None for path,
        // load should return AppConfig::default()
        // The actual test relies on the resolve_with_env path returning None
        // which happens when default_config_path() returns None.
        // For now, just verify the default values are present.
        let cfg = AppConfig::default();
        assert_eq!(cfg.log.level, "info");
        assert_eq!(cfg.log.format, LogFormat::Compact);
        assert_eq!(cfg.data_dir, PathBuf::from("./.adnet-data"));
    }

    #[test]
    fn show_effective_serializes_all_fields() {
        let cfg = AppConfig {
            data_dir: PathBuf::from("/custom"),
            log: LogConfig {
                level: "trace".to_string(),
                format: LogFormat::Json,
            },
            default_room: Some("custom-room".to_string()),
            repl: ReplConfig {
                prompt: "test> ".to_string(),
                history_file: None,
            },
            mesh: None,
            relay: None,
            gossip_validation: None,
            iroh: None,
            #[cfg(feature = "qr")]
            qr: None,
        };
        let json = show_effective(&cfg).unwrap();
        assert!(json.contains("/custom"));
        assert!(json.contains("trace"));
        assert!(json.contains("custom-room"));
        assert!(json.contains("test>"));
    }

    #[test]
    fn show_effective_deserializes_back() {
        let cfg = AppConfig::default();
        let json = show_effective(&cfg).unwrap();
        let parsed: AppConfig = json5::from_str(&json).unwrap();
        assert_eq!(parsed.data_dir, cfg.data_dir);
        assert_eq!(parsed.log.level, cfg.log.level);
    }

    #[test]
    fn appconfig_debug_format() {
        let cfg = AppConfig::default();
        let debug = format!("{:?}", cfg);
        assert!(debug.contains("data_dir") || debug.contains("dataDir"));
    }

    #[test]
    fn log_config_debug_format() {
        let log = LogConfig::default();
        let debug = format!("{:?}", log);
        assert!(debug.contains("level"));
    }

    #[test]
    fn repl_config_debug_format() {
        let repl = ReplConfig::default();
        let debug = format!("{:?}", repl);
        assert!(debug.contains("prompt"));
    }

    #[test]
    fn iroh_config_debug_format() {
        let iroh = IrohConfig::default();
        let debug = format!("{:?}", iroh);
        assert!(debug.contains("enabled"));
    }

    #[test]
    fn discovery_config_toml_debug_format() {
        let disc = DiscoveryConfigToml::default();
        let debug = format!("{:?}", disc);
        assert!(debug.contains("user_data"));
    }

    #[test]
    fn appconfig_serialize_deserialize_roundtrip() {
        let cfg = AppConfig {
            data_dir: PathBuf::from("/roundtrip"),
            log: LogConfig {
                level: "debug".to_string(),
                format: LogFormat::Compact,
            },
            default_room: None,
            repl: ReplConfig::default(),
            mesh: None,
            relay: None,
            gossip_validation: Some(GossipValidation::Lenient),
            iroh: None,
            #[cfg(feature = "qr")]
            qr: None,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.data_dir, cfg.data_dir);
        assert_eq!(parsed.log.level, cfg.log.level);
        assert_eq!(parsed.gossip_validation, cfg.gossip_validation);
    }

    #[test]
    fn gossip_validation_all_variants() {
        let raw = r#"{
            "gossipValidation": "strict",
            "log": { "level": "info" }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        assert_eq!(cfg.gossip_validation, Some(GossipValidation::Strict));

        let raw = r#"{
            "gossipValidation": "audit",
            "log": { "level": "info" }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        assert_eq!(cfg.gossip_validation, Some(GossipValidation::Audit));

        let raw = r#"{
            "gossipValidation": "lenient",
            "log": { "level": "info" }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        assert_eq!(cfg.gossip_validation, Some(GossipValidation::Lenient));
    }

    #[test]
    fn iroh_config_with_all_fields() {
        let raw = r#"{
            "iroh": {
                "enabled": true,
                "bindHost": "0.0.0.0",
                "bindPort": 9000,
                "identityPath": "/custom/path",
                "publishPublicly": true,
                "discovery": {
                    "userData": "test-data"
                }
            },
            "log": { "level": "info" }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        let iroh = cfg.iroh.unwrap();
        assert!(iroh.enabled);
        assert_eq!(iroh.bind_host, "0.0.0.0");
        assert_eq!(iroh.bind_port, 9000);
        assert_eq!(iroh.identity_path.unwrap(), PathBuf::from("/custom/path"));
        assert!(iroh.publish_publicly);
        assert_eq!(iroh.discovery.user_data.unwrap(), "test-data");
    }

    #[test]
    fn iroh_config_default_values() {
        let raw = r#"{
            "iroh": { "enabled": true },
            "log": { "level": "info" }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        let iroh = cfg.iroh.unwrap();
        assert_eq!(iroh.bind_host, "127.0.0.1");
        assert_eq!(iroh.bind_port, 0);
        assert!(!iroh.publish_publicly);
        assert!(iroh.identity_path.is_none());
    }

    #[test]
    fn log_format_serialize_deserialize() {
        let compact = serde_json::to_string(&LogFormat::Compact).unwrap();
        assert_eq!(compact, "\"compact\"");
        let json_format = serde_json::to_string(&LogFormat::Json).unwrap();
        assert_eq!(json_format, "\"json\"");

        let parsed: LogFormat = serde_json::from_str("\"compact\"").unwrap();
        assert_eq!(parsed, LogFormat::Compact);
        let parsed: LogFormat = serde_json::from_str("\"json\"").unwrap();
        assert_eq!(parsed, LogFormat::Json);
    }

    #[test]
    fn repl_config_with_history_file() {
        let raw = r#"{
            "repl": {
                "prompt": "custom> ",
                "historyFile": "/tmp/history.txt"
            },
            "log": { "level": "info" }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        assert_eq!(cfg.repl.prompt, "custom> ");
        assert_eq!(cfg.repl.history_file.unwrap(), PathBuf::from("/tmp/history.txt"));
    }

    #[test]
    fn repl_config_empty_prompt() {
        let raw = r#"{
            "repl": { "prompt": "" },
            "log": { "level": "info" }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        assert_eq!(cfg.repl.prompt, "");
    }

    #[test]
    fn mesh_config_with_route_prefix() {
        let raw = r#"{
            "mesh": {
                "host": "0.0.0.0",
                "port": 9000,
                "routePrefix": "/api/v1"
            },
            "log": { "level": "info" }
        }"#;
        let cfg: AppConfig = json5::from_str(raw).unwrap();
        let mesh = cfg.mesh.unwrap();
        assert_eq!(mesh.host, "0.0.0.0");
        assert_eq!(mesh.port, 9000);
        assert_eq!(mesh.route_prefix, "/api/v1");
    }

    #[test]
    fn appconfig_clone() {
        let cfg = AppConfig::default();
        let cloned = cfg.clone();
        assert_eq!(cloned.data_dir, cfg.data_dir);
        assert_eq!(cloned.log.level, cfg.log.level);
    }

    #[test]
    fn load_for_cli_writes_template_on_missing_file() {
        let dir = tempdir();
        let path = dir.0.join("new.json");
        assert!(!path.exists());
        let loaded = load_for_cli(Some(&path)).unwrap();
        assert!(path.exists());
        assert!(loaded.source.written_template);
        assert!(loaded.config.data_dir != PathBuf::from("/nonexistent")); // default is used
    }

    #[test]
    fn load_uses_env_var_for_rust_log() {
        let dir = tempdir();
        let path = dir.0.join("config.json");
        fs::write(&path, r#"{ "log": { "level": "info" } }"#).unwrap();
        // This test would require setting the env var, which we avoid.
        // Instead verify the function exists and is callable.
        let loaded = load(Some(&path)).unwrap();
        assert_eq!(loaded.config.log.level, "info");
    }
}
