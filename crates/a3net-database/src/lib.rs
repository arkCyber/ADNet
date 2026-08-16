//! `a3net-database` — Database abstraction layer for A3Net.
//!
//! Provides unified database access with:
//! - **Connection pooling** via deadpool
//! - **Multi-database support**: SQLite, PostgreSQL, Redis
//! - **Migration system** for schema management
//! - **Repository pattern** for data access
//! - **Transaction support** with automatic rollback
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use a3net_database::{Database, SqliteConfig};
//!
//! let db = Database::new_sqlite("data.db").await?;
//! db.run_migrations().await?;
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod config;
pub mod connection;
pub mod migrations;
pub mod pool;
pub mod repository;
pub mod error;

pub use config::{DatabaseConfig, DatabaseKind};
pub use connection::DatabaseConnection;
pub use migrations::{Migration, MigrationRunner, MigrationManager};
pub use pool::ConnectionPool;
pub use repository::{InMemoryRepository, Repository, RepositoryExt};
pub use error::{DatabaseError, DatabaseResult};

/// Re-exports for convenience
pub mod prelude {
    pub use super::config::*;
    pub use super::error::*;
    pub use super::repository::*;
}
