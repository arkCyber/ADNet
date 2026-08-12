//! Connection pool management using sqlx built-in pooling.

use sqlx::SqlitePool;

use crate::config::{DatabaseConfig, DatabaseKind, SqliteConfig};
use crate::error::{DatabaseError, DatabaseResult};

/// Unified connection pool for all database backends.
pub struct ConnectionPool {
    inner: PoolInner,
}

enum PoolInner {
    Sqlite(SqlitePool),
}

impl ConnectionPool {
    /// Create a SQLite connection pool.
    pub async fn sqlite(config: &SqliteConfig) -> DatabaseResult<Self> {
        let database_url = if config.path.starts_with(':') {
            format!("sqlite:{}?mode=memory", config.path)
        } else if config.path.contains(':') {
            format!("sqlite:{}", config.path)
        } else {
            format!("sqlite:{}", config.path)
        };

        let pool = SqlitePool::connect(&database_url)
            .await
            .map_err(|e| DatabaseError::Pool { reason: e.to_string() })?;

        Ok(Self {
            inner: PoolInner::Sqlite(pool),
        })
    }

    /// Create from unified config.
    pub async fn from_config(config: &DatabaseConfig) -> DatabaseResult<Self> {
        match config.kind {
            DatabaseKind::Sqlite => {
                let sqlite_config = config.sqlite.as_ref().ok_or_else(|| DatabaseError::Config {
                    reason: "SQLite config required".to_string(),
                })?;
                Self::sqlite(sqlite_config).await
            }
            DatabaseKind::Postgres => {
                Err(DatabaseError::Config {
                    reason: "PostgreSQL support requires postgres feature".to_string(),
                })
            }
            DatabaseKind::Redis => Err(DatabaseError::Config {
                reason: "Redis not supported in this pool".to_string(),
            }),
        }
    }

    /// Get a connection from the pool.
    pub async fn get(&self) -> DatabaseResult<PooledConnection> {
        match &self.inner {
            PoolInner::Sqlite(pool) => {
                Ok(PooledConnection::Sqlite(pool.clone()))
            }
        }
    }

    /// Get pool statistics.
    pub async fn stats(&self) -> PoolStats {
        match &self.inner {
            PoolInner::Sqlite(pool) => {
                PoolStats {
                    total_connections: pool.size() as u32,
                    idle_connections: pool.num_idle() as u32,
                    used_connections: (pool.size() as u32).saturating_sub(pool.num_idle() as u32),
                    max_connections: pool.size() as u32,
                }
            }
        }
    }

    /// Close the pool.
    pub async fn close(self) {
        match self.inner {
            PoolInner::Sqlite(pool) => pool.close().await,
        }
    }

    /// Get the underlying SQLite pool.
    pub fn sqlite_pool(&self) -> Option<&SqlitePool> {
        match &self.inner {
            PoolInner::Sqlite(pool) => Some(pool),
        }
    }
}

/// Pooled connection wrapper.
pub enum PooledConnection {
    Sqlite(SqlitePool),
}

impl PooledConnection {
    /// Execute a query.
    pub async fn execute(&self, sql: &str) -> DatabaseResult<u64> {
        match self {
            PooledConnection::Sqlite(pool) => {
                let result = sqlx::query(sql)
                    .execute(pool)
                    .await
                    .map_err(|e| DatabaseError::Query { reason: e.to_string() })?;
                Ok(result.rows_affected())
            }
        }
    }

    /// Fetch all rows.
    pub async fn fetch_all<T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin>(
        &self,
        sql: &str,
    ) -> DatabaseResult<Vec<T>> {
        match self {
            PooledConnection::Sqlite(pool) => {
                sqlx::query_as::<_, T>(sql)
                    .fetch_all(pool)
                    .await
                    .map_err(|e| DatabaseError::Query { reason: e.to_string() })
            }
        }
    }

    /// Fetch one row.
    pub async fn fetch_one<T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin>(
        &self,
        sql: &str,
    ) -> DatabaseResult<Option<T>> {
        match self {
            PooledConnection::Sqlite(pool) => {
                sqlx::query_as::<_, T>(sql)
                    .fetch_optional(pool)
                    .await
                    .map_err(|e| DatabaseError::Query { reason: e.to_string() })
            }
        }
    }

    /// Close the connection (return to pool).
    pub async fn close(self) {
        match self {
            PooledConnection::Sqlite(_) => {
                // Connection is returned to pool
            }
        }
    }
}

/// Pool statistics.
#[derive(Debug, Clone)]
pub struct PoolStats {
    pub total_connections: u32,
    pub idle_connections: u32,
    pub used_connections: u32,
    pub max_connections: u32,
}
