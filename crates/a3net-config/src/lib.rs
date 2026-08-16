//! `a3net-config` — Unified configuration system for A3Net.
//!
//! DO-178C DAL-B: Provides centralized configuration management with:
//! - Schema validation
//! - Environment variable overrides
//! - Hot reload via file watching
//! - Type-safe configuration access
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    ConfigManager                            │
//! │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐   │
//! │  │   Schema    │  │   Source    │  │  ConfigWatcher  │   │
//! │  │ Validation  │  │  Priority  │  │ (hot reload)   │   │
//! │  └─────────────┘  └─────────────┘  └─────────────────┘   │
//! └─────────────────────────────────────────────────────────────┘
//!         │                 │                   │
//!         ▼                 ▼                   ▼
//! ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐
//! │    File     │  │   Env Var   │  │   CLI Flags     │
//! │  (JSON5)   │  │  (prefix)   │  │  (override)    │
//! └─────────────┘  └─────────────┘  └─────────────────┘
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod manager;
pub mod schema;
pub mod source;
pub mod watcher;

pub use error::{ConfigError, ConfigResult};
pub use manager::{ConfigManager, HotReloadConfig};
pub use schema::{ConfigKey, ConfigValue, ConfigSchema, SchemaValidator, SchemaType};
pub use source::{ConfigSource, EnvSource, FileSource};
pub use watcher::ConfigWatcher;
