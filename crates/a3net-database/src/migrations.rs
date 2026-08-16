//! Database migrations.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::connection::DatabaseConnection;
use crate::error::DatabaseResult;

/// Represents a database migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Migration {
    /// Version number (used for ordering)
    pub version: i64,
    /// Human-readable name
    pub name: String,
    /// SQL to apply the migration
    pub up_sql: String,
    /// SQL to rollback the migration
    pub down_sql: String,
}

impl Migration {
    /// Create a new migration.
    pub fn new(version: i64, name: &str, up_sql: &str, down_sql: &str) -> Self {
        Self {
            version,
            name: name.to_string(),
            up_sql: up_sql.to_string(),
            down_sql: down_sql.to_string(),
        }
    }
}

/// Runs migrations against a database.
#[derive(Debug)]
pub struct MigrationRunner {
    migrations: Vec<Migration>,
}

impl MigrationRunner {
    /// Create a new migration runner.
    pub fn new(migrations: Vec<Migration>) -> Self {
        let mut migrations = migrations;
        migrations.sort_by_key(|m| m.version);
        Self { migrations }
    }

    /// Run all pending migrations.
    pub async fn run(&self, conn: &DatabaseConnection) -> DatabaseResult<MigrationResult> {
        let create_table = r#"
            CREATE TABLE IF NOT EXISTS _a3net_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at TEXT NOT NULL
            )
        "#;
        conn.execute(create_table).await?;

        let applied: Vec<AppliedMigration> = conn
            .fetch_all("SELECT version, name, applied_at FROM _a3net_migrations ORDER BY version")
            .await
            .unwrap_or_default();

        let applied_versions: Vec<i64> = applied.iter().map(|m| m.version).collect();
        let mut applied_count = 0;
        let mut errors = Vec::new();

        for migration in &self.migrations {
            if !applied_versions.contains(&migration.version) {
                let tx_result = conn.begin_transaction().await;
                let tx = match tx_result {
                    Ok(tx) => tx,
                    Err(e) => {
                        errors.push(format!(
                            "Migration {} ({}) failed to start transaction: {}",
                            migration.version, migration.name, e
                        ));
                        continue;
                    }
                };

                let result = conn.execute(&migration.up_sql).await;
                match result {
                    Ok(_) => {
                        let now = Utc::now().to_rfc3339();
                        let record_sql = format!(
                            "INSERT INTO _a3net_migrations (version, name, applied_at) VALUES ({}, '{}', '{}')",
                            migration.version,
                            migration.name.replace('\'', "''"),
                            now
                        );
                        let _ = conn.execute(&record_sql).await;

                        if let Err(e) = tx.commit().await {
                            // Transaction already committed or rolled back, log error
                            errors.push(format!(
                                "Migration {} ({}) failed to commit: {}",
                                migration.version, migration.name, e
                            ));
                        } else {
                            applied_count += 1;
                        }
                    }
                    Err(e) => {
                        let _ = tx.rollback().await;
                        errors.push(format!(
                            "Migration {} ({}) failed: {}",
                            migration.version, migration.name, e
                        ));
                    }
                }
            }
        }

        Ok(MigrationResult {
            applied: applied_count,
            errors,
        })
    }

    /// Get all migrations.
    pub fn migrations(&self) -> &[Migration] {
        &self.migrations
    }
}

#[derive(Debug, sqlx::FromRow)]
struct AppliedMigration {
    version: i64,
    name: String,
    applied_at: String,
}

/// Result of running migrations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationResult {
    pub applied: usize,
    pub errors: Vec<String>,
}

impl MigrationResult {
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// Manages migration state.
#[derive(Debug)]
pub struct MigrationManager {
    runner: MigrationRunner,
}

impl MigrationManager {
    pub fn new(migrations: Vec<Migration>) -> Self {
        Self {
            runner: MigrationRunner::new(migrations),
        }
    }

    pub async fn migrate(&self, conn: &DatabaseConnection) -> DatabaseResult<MigrationResult> {
        self.runner.run(conn).await
    }

    pub async fn status(&self, conn: &DatabaseConnection) -> DatabaseResult<Vec<MigrationStatus>> {
        let applied: Vec<AppliedMigration> = conn
            .fetch_all("SELECT version, name, applied_at FROM _a3net_migrations ORDER BY version")
            .await
            .unwrap_or_default();

        let applied_map: std::collections::HashMap<i64, AppliedMigration> = applied
            .into_iter()
            .map(|m| (m.version, m))
            .collect();

        let mut status = Vec::new();
        for migration in self.runner.migrations() {
            if let Some(found) = applied_map.get(&migration.version) {
                status.push(MigrationStatus {
                    version: migration.version,
                    name: migration.name.clone(),
                    applied: true,
                    applied_at: chrono::DateTime::parse_from_rfc3339(&found.applied_at)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                });
            } else {
                status.push(MigrationStatus {
                    version: migration.version,
                    name: migration.name.clone(),
                    applied: false,
                    applied_at: Utc::now(),
                });
            }
        }

        Ok(status)
    }
}

/// Status of a single migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStatus {
    pub version: i64,
    pub name: String,
    pub applied: bool,
    pub applied_at: DateTime<Utc>,
}
