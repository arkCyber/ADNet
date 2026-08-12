//! Database configuration.

use serde::{Deserialize, Serialize};

/// Supported database backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseKind {
    /// SQLite database
    Sqlite,
    /// PostgreSQL database
    Postgres,
    /// Redis cache
    Redis,
}

impl Default for DatabaseKind {
    fn default() -> Self {
        DatabaseKind::Sqlite
    }
}

/// Configuration for SQLite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqliteConfig {
    /// Path to the database file
    pub path: String,
    /// Enable WAL mode
    pub wal_mode: bool,
    /// Busy timeout in milliseconds
    pub busy_timeout_ms: u64,
    /// Cache size in pages
    pub cache_size: i64,
    /// Enable foreign keys
    pub foreign_keys: bool,
}

impl Default for SqliteConfig {
    fn default() -> Self {
        Self {
            path: "adnet.db".to_string(),
            wal_mode: true,
            busy_timeout_ms: 5000,
            cache_size: 10000,
            foreign_keys: true,
        }
    }
}

/// Configuration for PostgreSQL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresConfig {
    /// Connection string
    pub connection_string: String,
    /// Maximum connections in pool
    pub max_connections: u32,
    /// Minimum connections in pool
    pub min_connections: u32,
    /// Connection acquire timeout
    pub connect_timeout_secs: u64,
    /// Idle timeout
    pub idle_timeout_secs: Option<u64>,
    /// SSL mode
    pub ssl_mode: SslMode,
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            connection_string: String::new(),
            max_connections: 10,
            min_connections: 1,
            connect_timeout_secs: 30,
            idle_timeout_secs: Some(600),
            ssl_mode: SslMode::Prefer,
        }
    }
}

/// SSL mode for PostgreSQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SslMode {
    Disable,
    Allow,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

/// Configuration for Redis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    /// Redis connection string
    pub connection_string: String,
    /// Maximum connections in pool
    pub max_connections: usize,
    /// Default TTL for keys in seconds
    pub default_ttl_secs: Option<u64>,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            connection_string: String::new(),
            max_connections: 16,
            default_ttl_secs: Some(3600),
        }
    }
}

/// Unified database configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Database kind
    pub kind: DatabaseKind,
    /// SQLite configuration
    pub sqlite: Option<SqliteConfig>,
    /// PostgreSQL configuration
    pub postgres: Option<PostgresConfig>,
    /// Redis configuration
    pub redis: Option<RedisConfig>,
    /// Enable metrics
    pub enable_metrics: bool,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            kind: DatabaseKind::Sqlite,
            sqlite: Some(SqliteConfig::default()),
            postgres: None,
            redis: None,
            enable_metrics: false,
        }
    }
}

impl DatabaseConfig {
    /// Create a SQLite configuration.
    pub fn sqlite(path: impl Into<String>) -> Self {
        Self {
            kind: DatabaseKind::Sqlite,
            sqlite: Some(SqliteConfig {
                path: path.into(),
                ..Default::default()
            }),
            postgres: None,
            redis: None,
            enable_metrics: false,
        }
    }

    /// Create a PostgreSQL configuration.
    pub fn postgres(connection_string: impl Into<String>) -> Self {
        Self {
            kind: DatabaseKind::Postgres,
            sqlite: None,
            postgres: Some(PostgresConfig {
                connection_string: connection_string.into(),
                ..Default::default()
            }),
            redis: None,
            enable_metrics: false,
        }
    }

    /// Create a Redis configuration.
    pub fn redis(connection_string: impl Into<String>) -> Self {
        Self {
            kind: DatabaseKind::Redis,
            sqlite: None,
            postgres: None,
            redis: Some(RedisConfig {
                connection_string: connection_string.into(),
                ..Default::default()
            }),
            enable_metrics: false,
        }
    }

    /// Enable metrics collection.
    pub fn with_metrics(mut self, enabled: bool) -> Self {
        self.enable_metrics = enabled;
        self
    }
}
