//! Database connection management for SQLite.

use sqlx::SqlitePool;

use crate::config::{DatabaseConfig, DatabaseKind, SqliteConfig};
use crate::error::{DatabaseError, DatabaseResult};

/// Represents a database connection.
#[derive(Debug, Clone)]
pub struct DatabaseConnection {
    pub(crate) inner: SqlitePool,
}

impl DatabaseConnection {
    /// Create a new SQLite connection.
    pub async fn sqlite(path: &str) -> DatabaseResult<Self> {
        let database_url = if path.starts_with(':') {
            format!("sqlite:{}?mode=memory", path)
        } else if path.contains(':') {
            format!("sqlite:{}", path)
        } else {
            format!("sqlite:{}", path)
        };

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .map_err(|e| DatabaseError::Connection {
                reason: e.to_string(),
            })?;

        Ok(Self { inner: pool })
    }

    /// Create from configuration.
    pub async fn from_config(config: &DatabaseConfig) -> DatabaseResult<Self> {
        match config.kind {
            DatabaseKind::Sqlite => {
                let path = config
                    .sqlite
                    .as_ref()
                    .map(|c| c.path.as_str())
                    .unwrap_or("a3net.db");
                Self::sqlite(path).await
            }
            DatabaseKind::Postgres => {
                Err(DatabaseError::Config {
                    reason: "PostgreSQL requires sqlx with postgres feature".to_string(),
                })
            }
            DatabaseKind::Redis => {
                Err(DatabaseError::Config {
                    reason: "Redis support requires redis crate".to_string(),
                })
            }
        }
    }

    /// Get the underlying SQLite pool.
    pub fn sqlite_pool(&self) -> &SqlitePool {
        &self.inner
    }

    /// Execute a query that doesn't return results.
    pub async fn execute(&self, query: &str) -> DatabaseResult<u64> {
        let result = sqlx::query(query)
            .execute(&self.inner)
            .await
            .map_err(|e| DatabaseError::Query { reason: e.to_string() })?;
        Ok(result.rows_affected())
    }

    /// Fetch a single row.
    pub async fn fetch_one<T: Send + Unpin + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>>(
        &self,
        query: &str,
    ) -> DatabaseResult<T> {
        sqlx::query_as::<_, T>(query)
            .fetch_one(&self.inner)
            .await
            .map_err(|e| DatabaseError::Query { reason: e.to_string() })
    }

    /// Fetch multiple rows.
    pub async fn fetch_all<T: Send + Unpin + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>>(
        &self,
        query: &str,
    ) -> DatabaseResult<Vec<T>> {
        sqlx::query_as::<_, T>(query)
            .fetch_all(&self.inner)
            .await
            .map_err(|e| DatabaseError::Query { reason: e.to_string() })
    }

    /// Fetch optional row.
    pub async fn fetch_optional<T: Send + Unpin + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>>(
        &self,
        query: &str,
    ) -> DatabaseResult<Option<T>> {
        sqlx::query_as::<_, T>(query)
            .fetch_optional(&self.inner)
            .await
            .map_err(|e| DatabaseError::Query { reason: e.to_string() })
    }

    /// Begin a transaction.
    pub async fn begin_transaction(&self) -> DatabaseResult<Transaction> {
        let tx = self.inner
            .begin()
            .await
            .map_err(|e| DatabaseError::Transaction { reason: e.to_string() })?;
        Ok(Transaction { inner: Some(tx) })
    }

    /// Check if the connection is healthy.
    pub async fn health_check(&self) -> DatabaseResult<()> {
        sqlx::query("SELECT 1")
            .execute(&self.inner)
            .await
            .map_err(|e| DatabaseError::Connection { reason: e.to_string() })?;
        Ok(())
    }

    /// Close the connection pool.
    pub async fn close(self) {
        self.inner.close().await;
    }
}

/// Represents a database transaction.
pub struct Transaction<'a> {
    inner: Option<sqlx::Transaction<'a, sqlx::Sqlite>>,
}

impl<'a> Transaction<'a> {
    /// Commit the transaction.
    pub async fn commit(mut self) -> DatabaseResult<()> {
        if let Some(tx) = self.inner.take() {
            tx.commit()
                .await
                .map_err(|e| DatabaseError::Transaction { reason: e.to_string() })
        } else {
            Ok(())
        }
    }

    /// Rollback the transaction.
    pub async fn rollback(mut self) -> DatabaseResult<()> {
        if let Some(tx) = self.inner.take() {
            tx.rollback()
                .await
                .map_err(|e| DatabaseError::Transaction { reason: e.to_string() })
        } else {
            Ok(())
        }
    }
}
