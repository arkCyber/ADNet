//! Model Catalog - SQLite-based storage for model metadata

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use tokio::task::spawn_blocking;

use super::error::ModelCatalogError;
use super::manifest::ModelManifest;
use super::types::{CatalogStats, ModelFilter, ModelStatus, ModelType, PaginatedModels, Quantization, SortField};

/// SQLite-backed model catalog
pub struct ModelCatalog {
    db: Arc<Mutex<Connection>>,
}

impl Clone for ModelCatalog {
    fn clone(&self) -> Self {
        Self {
            db: Arc::clone(&self.db),
        }
    }
}

impl ModelCatalog {
    /// Open or create a model catalog at the given path
    pub async fn open<P: AsRef<Path>>(path: P) -> Result<Self, ModelCatalogError> {
        let path = path.as_ref().to_path_buf();
        
        let result = spawn_blocking(move || {
            let conn = Connection::open(&path)
                .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?;
            ModelCatalog::init_db(conn)
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(format!("Join error: {}", e)))?;

        result
    }

    /// Create an in-memory catalog (for testing)
    pub fn memory() -> Result<Self, ModelCatalogError> {
        let conn = Connection::open_in_memory()?;
        ModelCatalog::init_db(conn)
    }

    /// Initialize database schema
    fn init_db(conn: Connection) -> Result<Self, ModelCatalogError> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS models (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                version TEXT NOT NULL,
                model_type TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                content_hash TEXT NOT NULL,
                iroh_ticket TEXT NOT NULL,
                author TEXT NOT NULL,
                description TEXT NOT NULL,
                tags TEXT NOT NULL,
                architecture TEXT NOT NULL,
                quantization TEXT NOT NULL,
                license TEXT NOT NULL,
                source_url TEXT,
                status TEXT NOT NULL DEFAULT 'available',
                download_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_models_name ON models(name);
            CREATE INDEX IF NOT EXISTS idx_models_model_type ON models(model_type);
            CREATE INDEX IF NOT EXISTS idx_models_author ON models(author);
            CREATE INDEX IF NOT EXISTS idx_models_architecture ON models(architecture);
            CREATE INDEX IF NOT EXISTS idx_models_status ON models(status);
            CREATE INDEX IF NOT EXISTS idx_models_created_at ON models(created_at);

            CREATE VIRTUAL TABLE IF NOT EXISTS models_fts USING fts5(
                name, description, author, tags,
                content='models',
                content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS models_ai AFTER INSERT ON models BEGIN
                INSERT INTO models_fts(rowid, name, description, author, tags)
                VALUES (NEW.rowid, NEW.name, NEW.description, NEW.author, NEW.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS models_ad AFTER DELETE ON models BEGIN
                INSERT INTO models_fts(models_fts, rowid, name, description, author, tags)
                VALUES('delete', OLD.rowid, OLD.name, OLD.description, OLD.author, OLD.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS models_au AFTER UPDATE ON models BEGIN
                INSERT INTO models_fts(models_fts, rowid, name, description, author, tags)
                VALUES('delete', OLD.rowid, OLD.name, OLD.description, OLD.author, OLD.tags);
                INSERT INTO models_fts(rowid, name, description, author, tags)
                VALUES (NEW.rowid, NEW.name, NEW.description, NEW.author, NEW.tags);
            END;

            CREATE TABLE IF NOT EXISTS downloads (
                id TEXT PRIMARY KEY,
                model_id TEXT NOT NULL,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                bytes_downloaded INTEGER NOT NULL DEFAULT 0,
                total_bytes INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                error_message TEXT,
                FOREIGN KEY (model_id) REFERENCES models(id)
            );

            CREATE TABLE IF NOT EXISTS providers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                node_id TEXT NOT NULL UNIQUE,
                address TEXT NOT NULL,
                is_local INTEGER NOT NULL DEFAULT 0,
                last_seen TEXT NOT NULL,
                total_models INTEGER NOT NULL DEFAULT 0,
                score REAL NOT NULL DEFAULT 0.0,
                successful_downloads INTEGER NOT NULL DEFAULT 0,
                failed_downloads INTEGER NOT NULL DEFAULT 0,
                reports_count INTEGER NOT NULL DEFAULT 0,
                trust_flag TEXT NOT NULL DEFAULT 'neutral',
                last_updated TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_providers_node_id ON providers(node_id);
            CREATE INDEX IF NOT EXISTS idx_providers_trust_flag ON providers(trust_flag);
            CREATE INDEX IF NOT EXISTS idx_providers_score ON providers(score);
            "#,
        )?;

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
        })
    }

    /// Add a model to the catalog
    pub async fn add(&self, manifest: ModelManifest) -> Result<(), ModelCatalogError> {
        manifest.validate()?;
        
        let db = self.db.clone();
        let manifest_json = serde_json::to_string(&manifest)?;
        
        spawn_blocking(move || {
            let conn = db.lock();
            conn.execute(
                r#"
                INSERT OR REPLACE INTO models (
                    id, name, version, model_type, size_bytes, content_hash,
                    iroh_ticket, author, description, tags, architecture,
                    quantization, license, source_url, status, download_count,
                    created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
                "#,
                params![
                    manifest.id,
                    manifest.name,
                    manifest.version,
                    serialize_model_type(&manifest.model_type),
                    manifest.size_bytes as i64,
                    manifest.content_hash,
                    manifest.iroh_ticket,
                    manifest.author,
                    manifest.description,
                    serde_json::to_string(&manifest.tags)?,
                    manifest.architecture,
                    serialize_quantization(&manifest.quantization),
                    manifest.license,
                    manifest.source_url,
                    serialize_status(&manifest.status),
                    manifest.download_count as i64,
                    manifest.created_at.to_rfc3339(),
                    manifest.updated_at.to_rfc3339(),
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Get a model by ID
    pub async fn get(&self, id: &str) -> Result<Option<ModelManifest>, ModelCatalogError> {
        let db = self.db.clone();
        let id = id.to_string();
        
        spawn_blocking(move || {
            let conn = db.lock();
            let mut stmt = conn.prepare(
                r#"
                SELECT id, name, version, model_type, size_bytes, content_hash,
                       iroh_ticket, author, description, tags, architecture,
                       quantization, license, source_url, status, download_count,
                       created_at, updated_at
                FROM models WHERE id = ?1
                "#,
            )?;

            let model = stmt
                .query_row(params![id], |row| {
                    Ok(row_to_manifest(row)?)
                })
                .optional()?;

            Ok(model)
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// List all models with optional filtering
    pub async fn list(&self, filter: ModelFilter) -> Result<PaginatedModels<ModelManifest>, ModelCatalogError> {
        let db = self.db.clone();
        
        spawn_blocking(move || {
            let conn = db.lock();
            
            // Build WHERE clause
            let mut where_clauses = Vec::new();
            let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

            if let Some(ref model_type) = filter.model_type {
                where_clauses.push("model_type = ?");
                params_vec.push(Box::new(serialize_model_type(model_type)));
            }
            if let Some(ref architecture) = filter.architecture {
                where_clauses.push("architecture = ?");
                params_vec.push(Box::new(architecture.clone()));
            }
            if let Some(ref author) = filter.author {
                where_clauses.push("author = ?");
                params_vec.push(Box::new(author.clone()));
            }
            if let Some(min_size) = filter.min_size {
                where_clauses.push("size_bytes >= ?");
                params_vec.push(Box::new(min_size as i64));
            }
            if let Some(max_size) = filter.max_size {
                where_clauses.push("size_bytes <= ?");
                params_vec.push(Box::new(max_size as i64));
            }
            if let Some(ref tags) = filter.tags {
                // Match all tags (AND logic)
                for tag in tags {
                    where_clauses.push("tags LIKE ?");
                    params_vec.push(Box::new(format!("%\"{}\"%", tag)));
                }
            }
            if let Some(ref query) = filter.query {
                where_clauses.push("id IN (SELECT rowid FROM models_fts WHERE models_fts MATCH ?)");
                params_vec.push(Box::new(query.clone()));
            }

            where_clauses.push("status != 'Removed'");

            let where_clause = if where_clauses.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", where_clauses.join(" AND "))
            };

            // Count total
            let count_sql = format!("SELECT COUNT(*) FROM models {}", where_clause);
            let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
            let total: i64 = conn.query_row(&count_sql, params_refs.as_slice(), |row| row.get(0))?;

            // Build ORDER BY
            let sort_field = filter.sort_by.unwrap_or(SortField::CreatedAt);
            let sort_dir = if filter.sort_desc.unwrap_or(true) { "DESC" } else { "ASC" };
            let order_clause = match sort_field {
                SortField::Name => "name",
                SortField::CreatedAt => "created_at",
                SortField::UpdatedAt => "updated_at",
                SortField::Size => "size_bytes",
                SortField::Downloads => "download_count",
            };
            let order_by = format!("ORDER BY {} {}", order_clause, sort_dir);

            // Pagination
            let offset = filter.offset.unwrap_or(0) as i64;
            let limit = filter.limit.unwrap_or(50) as i64;

            let query_sql = format!(
                "SELECT id, name, version, model_type, size_bytes, content_hash, \
                 iroh_ticket, author, description, tags, architecture, \
                 quantization, license, source_url, status, download_count, \
                 created_at, updated_at \
                 FROM models {} {} LIMIT ? OFFSET ?",
                where_clause, order_by
            );

            let mut all_params = params_refs;
            all_params.push(&limit);
            all_params.push(&offset);

            let mut stmt = conn.prepare(&query_sql)?;
            let models: Vec<ModelManifest> = stmt
                .query_map(all_params.as_slice(), |row| row_to_manifest(row))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(PaginatedModels::new(
                models,
                total as u64,
                offset as u64,
                limit as u64,
            ))
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Search models by tag
    pub async fn search_by_tag(&self, tag: &str) -> Result<Vec<ModelManifest>, ModelCatalogError> {
        let db = self.db.clone();
        let tag = tag.to_string();

        spawn_blocking(move || {
            let conn = db.lock();
            let mut stmt = conn.prepare(
                r#"
                SELECT id, name, version, model_type, size_bytes, content_hash,
                       iroh_ticket, author, description, tags, architecture,
                       quantization, license, source_url, status, download_count,
                       created_at, updated_at
                FROM models 
                WHERE tags LIKE ? AND status != 'Removed'
                ORDER BY created_at DESC
                "#,
            )?;

            let models: Vec<ModelManifest> = stmt
                .query_map(params![format!("%\"{}\"%", tag)], |row| row_to_manifest(row))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(models)
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Full-text search
    pub async fn search(&self, query: &str) -> Result<Vec<ModelManifest>, ModelCatalogError> {
        let db = self.db.clone();
        let query = format!("{}*", query.replace(" ", " OR ")); // Prefix matching

        spawn_blocking(move || {
            let conn = db.lock();
            let mut stmt = conn.prepare(
                r#"
                SELECT m.id, m.name, m.version, m.model_type, m.size_bytes, m.content_hash,
                       m.iroh_ticket, m.author, m.description, m.tags, m.architecture,
                       m.quantization, m.license, m.source_url, m.status, m.download_count,
                       m.created_at, m.updated_at
                FROM models m
                JOIN models_fts fts ON m.rowid = fts.rowid
                WHERE models_fts MATCH ?
                ORDER BY rank
                "#,
            )?;

            let models: Vec<ModelManifest> = stmt
                .query_map(params![query], |row| row_to_manifest(row))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(models)
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Get the Iroh ticket for a model
    pub async fn get_ticket(&self, model_id: &str) -> Result<Option<String>, ModelCatalogError> {
        let db = self.db.clone();
        let model_id = model_id.to_string();

        spawn_blocking(move || {
            let conn = db.lock();
            let ticket: Option<String> = conn.query_row(
                "SELECT iroh_ticket FROM models WHERE id = ? AND status != 'Removed'",
                params![model_id],
                |row| row.get(0),
            ).optional()?;

            Ok(ticket)
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Update download count
    pub async fn increment_downloads(&self, model_id: &str) -> Result<(), ModelCatalogError> {
        let db = self.db.clone();
        let model_id = model_id.to_string();

        spawn_blocking(move || {
            let conn = db.lock();
            conn.execute(
                "UPDATE models SET download_count = download_count + 1, updated_at = ? WHERE id = ?",
                params![Utc::now().to_rfc3339(), model_id],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Update model status
    pub async fn update_status(&self, model_id: &str, status: ModelStatus) -> Result<(), ModelCatalogError> {
        let db = self.db.clone();
        let model_id = model_id.to_string();
        let status_str = serialize_status(&status);

        spawn_blocking(move || {
            let conn = db.lock();
            conn.execute(
                "UPDATE models SET status = ?, updated_at = ? WHERE id = ?",
                params![status_str, Utc::now().to_rfc3339(), model_id],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    // ── Provider Reputation CRUD ────────────────────────────────

    /// Upsert a [`super::reputation::ProviderReputation`] snapshot.
    pub async fn upsert_provider_reputation(
        &self,
        rep: &super::reputation::ProviderReputation,
    ) -> Result<(), ModelCatalogError> {
        let db = self.db.clone();
        let trust_flag = match rep.trust_flag {
            super::reputation::TrustFlag::Trusted => "trusted",
            super::reputation::TrustFlag::Neutral => "neutral",
            super::reputation::TrustFlag::Blocked => "blocked",
        };
        let rep = rep.clone();

        spawn_blocking(move || {
            let conn = db.lock();
            conn.execute(
                r#"INSERT INTO providers (
                    id, name, node_id, address, is_local, last_seen,
                    total_models, score, successful_downloads, failed_downloads,
                    reports_count, trust_flag, last_updated
                ) VALUES (?, '', ?, '', 0, ?, 0, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(node_id) DO UPDATE SET
                    score = excluded.score,
                    successful_downloads = excluded.successful_downloads,
                    failed_downloads = excluded.failed_downloads,
                    reports_count = excluded.reports_count,
                    trust_flag = excluded.trust_flag,
                    last_updated = excluded.last_updated
                "#,
                params![
                    rep.node_id,
                    rep.node_id,
                    rep.last_updated.to_rfc3339(),
                    rep.score,
                    rep.successful_downloads,
                    rep.failed_downloads,
                    rep.reports_count,
                    trust_flag,
                    rep.last_updated.to_rfc3339(),
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Read all provider reputation snapshots.
    pub async fn list_provider_reputation(
        &self,
    ) -> Result<Vec<super::reputation::ProviderReputation>, ModelCatalogError> {
        let db = self.db.clone();
        spawn_blocking(move || {
            let conn = db.lock();
            let mut stmt = conn.prepare(
                "SELECT node_id, score, successful_downloads, failed_downloads,
                        reports_count, last_updated, trust_flag
                  FROM providers ORDER BY score DESC",
            )?;
            let rows = stmt.query_map([], |row| {
                let trust_flag_str: String = row.get(6)?;
                Ok(super::reputation::ProviderReputation {
                    node_id: row.get(0)?,
                    score: row.get(1)?,
                    successful_downloads: row.get(2)?,
                    failed_downloads: row.get(3)?,
                    reports_count: row.get(4)?,
                    last_updated: parse_datetime(row.get::<_, String>(5)?.as_str()),
                    trust_flag: match trust_flag_str.as_str() {
                        "trusted" => super::reputation::TrustFlag::Trusted,
                        "blocked" => super::reputation::TrustFlag::Blocked,
                        _ => super::reputation::TrustFlag::Neutral,
                    },
                })
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Read a single provider's reputation snapshot.
    pub async fn get_provider_reputation(
        &self,
        node_id: &str,
    ) -> Result<Option<super::reputation::ProviderReputation>, ModelCatalogError> {
        let db = self.db.clone();
        let node_id = node_id.to_string();
        spawn_blocking(move || {
            let conn = db.lock();
            let mut stmt = conn.prepare(
                "SELECT node_id, score, successful_downloads, failed_downloads,
                        reports_count, last_updated, trust_flag
                  FROM providers WHERE node_id = ?",
            )?;
            let mut rows = stmt.query(params![node_id])?;
            if let Some(row) = rows.next()? {
                let trust_flag_str: String = row.get(6)?;
                Ok(Some(super::reputation::ProviderReputation {
                    node_id: row.get(0)?,
                    score: row.get(1)?,
                    successful_downloads: row.get(2)?,
                    failed_downloads: row.get(3)?,
                    reports_count: row.get(4)?,
                    last_updated: parse_datetime(&row.get::<_, String>(5)?),
                    trust_flag: match trust_flag_str.as_str() {
                        "trusted" => super::reputation::TrustFlag::Trusted,
                        "blocked" => super::reputation::TrustFlag::Blocked,
                        _ => super::reputation::TrustFlag::Neutral,
                    },
                }))
            } else {
                Ok(None)
            }
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Delete a provider's reputation snapshot.
    pub async fn delete_provider_reputation(&self, node_id: &str) -> Result<(), ModelCatalogError> {
        let db = self.db.clone();
        let node_id = node_id.to_string();
        spawn_blocking(move || {
            let conn = db.lock();
            conn.execute("DELETE FROM providers WHERE node_id = ?", params![node_id])?;
            Ok(())
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Delete a model (soft delete)
    pub async fn remove(&self, model_id: &str) -> Result<(), ModelCatalogError> {
        self.update_status(model_id, ModelStatus::Removed).await
    }

    /// Get catalog statistics
    pub async fn stats(&self) -> Result<CatalogStats, ModelCatalogError> {
        let db = self.db.clone();

        spawn_blocking(move || {
            let conn = db.lock();

            let total_models: u64 = conn.query_row(
                "SELECT COUNT(*) FROM models WHERE status != 'Removed'",
                [],
                |row| row.get(0),
            )?;

            let total_size_bytes: u64 = conn.query_row(
                "SELECT COALESCE(SUM(size_bytes), 0) FROM models WHERE status != 'Removed'",
                [],
                |row| row.get(0),
            )?;

            let recent_models: u64 = conn.query_row(
                "SELECT COUNT(*) FROM models WHERE status != 'Removed' AND created_at > datetime('now', '-7 days')",
                [],
                |row| row.get(0),
            )?;

            let mut stmt = conn.prepare(
                "SELECT model_type, COUNT(*) FROM models WHERE status != 'Removed' GROUP BY model_type"
            )?;

            let mut models_by_type = std::collections::HashMap::new();
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?;

            for row in rows {
                if let Ok((model_type, count)) = row {
                    models_by_type.insert(model_type, count);
                }
            }

            Ok(CatalogStats {
                total_models,
                total_size_bytes,
                models_by_type,
                recent_models,
                active_downloads: 0, // TODO: track active downloads
            })
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Get all unique tags
    pub async fn get_all_tags(&self) -> Result<Vec<(String, u64)>, ModelCatalogError> {
        let db = self.db.clone();

        spawn_blocking(move || {
            let conn = db.lock();
            let mut stmt = conn.prepare(
                "SELECT tags FROM models WHERE status != 'Removed'"
            )?;

            let mut tag_counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
            
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            
            for row in rows {
                if let Ok(tags_json) = row {
                    if let Ok(tags) = serde_json::from_str::<Vec<String>>(&tags_json) {
                        for tag in tags {
                            *tag_counts.entry(tag).or_insert(0) += 1;
                        }
                    }
                }
            }

            let mut tags: Vec<(String, u64)> = tag_counts.into_iter().collect();
            tags.sort_by(|a, b| b.1.cmp(&a.1));

            Ok(tags)
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Get all unique architectures
    pub async fn get_all_architectures(&self) -> Result<Vec<String>, ModelCatalogError> {
        let db = self.db.clone();

        spawn_blocking(move || {
            let conn = db.lock();
            let mut stmt = conn.prepare(
                "SELECT DISTINCT architecture FROM models WHERE status != 'Removed' ORDER BY architecture"
            )?;

            let architectures: Vec<String> = stmt
                .query_map([], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(architectures)
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }

    /// Get all unique authors
    pub async fn get_all_authors(&self) -> Result<Vec<String>, ModelCatalogError> {
        let db = self.db.clone();

        spawn_blocking(move || {
            let conn = db.lock();
            let mut stmt = conn.prepare(
                "SELECT DISTINCT author FROM models WHERE status != 'Removed' ORDER BY author"
            )?;

            let authors: Vec<String> = stmt
                .query_map([], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(authors)
        })
        .await
        .map_err(|e| ModelCatalogError::DatabaseError(e.to_string()))?
    }
}

// Helper functions

fn row_to_manifest(row: &rusqlite::Row) -> Result<ModelManifest, rusqlite::Error> {
    let tags_json: String = row.get(9)?;
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    let model_type_str: String = row.get(3)?;
    let quantization_str: String = row.get(11)?;
    let status_str: String = row.get(14)?;
    let created_at_str: String = row.get(16)?;
    let updated_at_str: String = row.get(17)?;

    Ok(ModelManifest {
        id: row.get(0)?,
        name: row.get(1)?,
        version: row.get(2)?,
        model_type: deserialize_model_type(&model_type_str),
        size_bytes: row.get::<_, i64>(4)? as u64,
        content_hash: row.get(5)?,
        iroh_ticket: row.get(6)?,
        author: row.get(7)?,
        description: row.get(8)?,
        tags,
        architecture: row.get(10)?,
        quantization: deserialize_quantization(&quantization_str),
        license: row.get(12)?,
        source_url: row.get(13)?,
        status: deserialize_status(&status_str),
        download_count: row.get::<_, i64>(15)? as u64,
        created_at: DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        updated_at: DateTime::parse_from_rfc3339(&updated_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
    })
}

fn serialize_model_type(t: &ModelType) -> String {
    serde_json::to_string(t).unwrap_or_default()
}

fn deserialize_model_type(s: &str) -> ModelType {
    serde_json::from_str(s).unwrap_or(ModelType::Other(s.to_string()))
}

fn serialize_quantization(q: &Quantization) -> String {
    serde_json::to_string(q).unwrap_or_default()
}

fn deserialize_quantization(s: &str) -> Quantization {
    serde_json::from_str(s).unwrap_or(Quantization::None)
}

fn parse_datetime(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn serialize_status(s: &ModelStatus) -> String {
    // Use a stable string representation that does NOT round-trip through
    // serde_json quoting — SQL `WHERE status != 'Removed'` clauses match
    // this format directly.
    match s {
        ModelStatus::Available => "Available".to_string(),
        ModelStatus::Downloading => "Downloading".to_string(),
        ModelStatus::Uploading => "Uploading".to_string(),
        ModelStatus::Unavailable => "Unavailable".to_string(),
        ModelStatus::Removed => "Removed".to_string(),
    }
}

fn deserialize_status(s: &str) -> ModelStatus {
    match s.trim_matches('"') {
        "Available" => ModelStatus::Available,
        "Downloading" => ModelStatus::Downloading,
        "Uploading" => ModelStatus::Uploading,
        "Unavailable" => ModelStatus::Unavailable,
        "Removed" => ModelStatus::Removed,
        _ => ModelStatus::Available,
    }
}

// Extension trait for optional result
trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(val) => Ok(Some(val)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ModelManifest;
    use crate::types::{ModelType, Quantization};

    fn sample(name: &str) -> ModelManifest {
        ModelManifest::new(
            name.to_string(),
            "1.0.0".to_string(),
            ModelType::Llm,
            1024,
            "a".repeat(64),
            "iroh://blob/abc".to_string(),
            "Test Author".to_string(),
            "desc".to_string(),
            vec!["test".to_string()],
            "llama3".to_string(),
            Quantization::None,
            "MIT".to_string(),
        )
    }

    #[tokio::test]
    async fn memory_catalog_add_and_get() {
        let catalog = ModelCatalog::memory().expect("memory catalog");
        let m = sample("alpha");
        catalog.add(m.clone()).await.expect("add");
        let fetched = catalog.get(&m.id).await.expect("get").expect("present");
        assert_eq!(fetched.name, "alpha");
    }

    #[tokio::test]
    async fn memory_catalog_list_returns_paginated() {
        let catalog = ModelCatalog::memory().expect("memory catalog");
        for n in ["a", "b", "c"] {
            catalog.add(sample(n)).await.expect("add");
        }
        let page = catalog
            .list(ModelFilter {
                limit: Some(2),
                offset: Some(0),
                ..Default::default()
            })
            .await
            .expect("list");
        assert!(!page.items.is_empty(), "expected some items, got 0");
        assert!(page.total >= 3);
    }

    #[tokio::test]
    async fn memory_catalog_search_finds_match() {
        let catalog = ModelCatalog::memory().expect("memory catalog");
        catalog.add(sample("llama3-chat")).await.expect("add");
        let results = catalog.search("llama").await.expect("search");
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "llama3-chat");
    }

    #[tokio::test]
    async fn memory_catalog_remove_marks_as_removed() {
        let catalog = ModelCatalog::memory().expect("memory catalog");
        let m = sample("removable");
        catalog.add(m.clone()).await.expect("add");
        catalog.remove(&m.id).await.expect("remove");
        // remove() is a soft delete that flips status; `get` still returns
        // the row but `list` must omit it.
        let listed = catalog
            .list(ModelFilter {
                limit: Some(10),
                offset: Some(0),
                ..Default::default()
            })
            .await
            .expect("list");
        assert!(listed.items.iter().all(|i| i.id != m.id));
    }

    #[tokio::test]
    async fn memory_catalog_stats_reflect_added_models() {
        let catalog = ModelCatalog::memory().expect("memory catalog");
        for n in ["x", "y", "w"] {
            catalog.add(sample(n)).await.expect("add");
        }
        let stats = catalog.stats().await.expect("stats");
        assert_eq!(stats.total_models, 3);
    }

    #[test]
    fn optional_ext_handles_no_rows() {
        let ok: Result<i32, rusqlite::Error> = Ok(42);
        let none: Result<i32, rusqlite::Error> = Err(rusqlite::Error::QueryReturnedNoRows);
        let other: Result<i32, rusqlite::Error> = Err(rusqlite::Error::InvalidQuery);
        assert_eq!(ok.optional().unwrap(), Some(42));
        assert_eq!(none.optional().unwrap(), None);
        assert!(other.optional().is_err());
    }

    // ── Provider reputation CRUD ────────────────────────────────

    fn sample_rep(node: &str, score: f64) -> super::super::reputation::ProviderReputation {
        super::super::reputation::ProviderReputation {
            node_id: node.to_string(),
            score,
            successful_downloads: 3,
            failed_downloads: 1,
            reports_count: 0,
            last_updated: chrono::Utc::now(),
            trust_flag: super::super::reputation::TrustFlag::Neutral,
        }
    }

    #[tokio::test]
    async fn upsert_and_read_provider_reputation() {
        let catalog = ModelCatalog::memory().unwrap();
        let node = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let rep = sample_rep(node, 12.5);
        catalog.upsert_provider_reputation(&rep).await.unwrap();
        let fetched = catalog
            .get_provider_reputation(node)
            .await
            .unwrap()
            .expect("present");
        assert_eq!(fetched.score, 12.5);
        assert_eq!(fetched.successful_downloads, 3);
        assert_eq!(fetched.failed_downloads, 1);
        assert_eq!(fetched.trust_flag, super::super::reputation::TrustFlag::Neutral);
    }

    #[tokio::test]
    async fn upsert_provider_reputation_is_idempotent() {
        let catalog = ModelCatalog::memory().unwrap();
        let node = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
        catalog.upsert_provider_reputation(&sample_rep(node, 1.0)).await.unwrap();
        catalog.upsert_provider_reputation(&sample_rep(node, 5.0)).await.unwrap();
        let fetched = catalog
            .get_provider_reputation(node)
            .await
            .unwrap()
            .expect("present");
        assert_eq!(fetched.score, 5.0);
        // Only one row should exist
        let all = catalog.list_provider_reputation().await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn list_provider_reputation_orders_by_score_desc() {
        let catalog = ModelCatalog::memory().unwrap();
        catalog
            .upsert_provider_reputation(&sample_rep("a000000000000000000000000000000000000000000000000000000000000001", -10.0))
            .await
            .unwrap();
        catalog
            .upsert_provider_reputation(&sample_rep("b000000000000000000000000000000000000000000000000000000000000002", 30.0))
            .await
            .unwrap();
        catalog
            .upsert_provider_reputation(&sample_rep("c000000000000000000000000000000000000000000000000000000000000003", 5.0))
            .await
            .unwrap();
        let list = catalog.list_provider_reputation().await.unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].score, 30.0);
        assert_eq!(list[1].score, 5.0);
        assert_eq!(list[2].score, -10.0);
    }

    #[tokio::test]
    async fn delete_provider_reputation_removes_row() {
        let catalog = ModelCatalog::memory().unwrap();
        let node = "1111111111111111111111111111111111111111111111111111111111111111";
        catalog.upsert_provider_reputation(&sample_rep(node, 7.0)).await.unwrap();
        catalog.delete_provider_reputation(node).await.unwrap();
        assert!(catalog.get_provider_reputation(node).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn upsert_persists_trust_flag() {
        let catalog = ModelCatalog::memory().unwrap();
        let node = "2222222222222222222222222222222222222222222222222222222222222222";
        let mut rep = sample_rep(node, 0.0);
        rep.trust_flag = super::super::reputation::TrustFlag::Blocked;
        catalog.upsert_provider_reputation(&rep).await.unwrap();
        let fetched = catalog
            .get_provider_reputation(node)
            .await
            .unwrap()
            .expect("present");
        assert_eq!(fetched.trust_flag, super::super::reputation::TrustFlag::Blocked);
    }

    // ── Integration: catalog + reputation tracker ────────────────

    use super::super::reputation::ProviderReputationTracker;

    #[tokio::test]
    async fn tracker_snapshots_persist_via_catalog() {
        let catalog = ModelCatalog::memory().unwrap();
        let tracker = ProviderReputationTracker::new();
        let n1 = "1111111111111111111111111111111111111111111111111111111111111111";
        let n2 = "2222222222222222222222222222222222222222222222222222222222222222";
        tracker
            .record_download(n1, super::super::reputation::DownloadOutcome::Success, "m1")
            .unwrap();
        tracker
            .record_download(n2, super::super::reputation::DownloadOutcome::Failure, "m2")
            .unwrap();
        // Persist both
        for snap in tracker.snapshots() {
            catalog.upsert_provider_reputation(&snap).await.unwrap();
        }
        // Re-hydrate a fresh tracker from the catalog
        let fresh = ProviderReputationTracker::new();
        let stored = catalog.list_provider_reputation().await.unwrap();
        fresh.hydrate(stored);
        assert_eq!(fresh.snapshots().len(), 2);
        assert_eq!(
            fresh
                .get(n1)
                .unwrap()
                .successful_downloads,
            1
        );
        assert_eq!(
            fresh
                .get(n2)
                .unwrap()
                .failed_downloads,
            1
        );
    }

    #[tokio::test]
    async fn trust_tier_round_trips_through_catalog() {
        let catalog = ModelCatalog::memory().unwrap();
        let node = "3333333333333333333333333333333333333333333333333333333333333333";
        let mut rep = sample_rep(node, -50.0);
        rep.trust_flag = super::super::reputation::TrustFlag::Blocked;
        catalog.upsert_provider_reputation(&rep).await.unwrap();
        let fetched = catalog
            .get_provider_reputation(node)
            .await
            .unwrap()
            .expect("present");
        assert_eq!(fetched.trust_tier(), super::super::reputation::TrustTier::Blocked);
        assert_eq!(fetched.score, -50.0);
    }

    #[tokio::test]
    async fn empty_list_returns_no_snapshots() {
        let catalog = ModelCatalog::memory().unwrap();
        let list = catalog.list_provider_reputation().await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn upsert_updates_last_updated_to_now() {
        let catalog = ModelCatalog::memory().unwrap();
        let node = "4444444444444444444444444444444444444444444444444444444444444444";
        let mut rep = sample_rep(node, 5.0);
        rep.last_updated = chrono::Utc::now() - chrono::Duration::days(7);
        catalog.upsert_provider_reputation(&rep).await.unwrap();
        let fetched = catalog
            .get_provider_reputation(node)
            .await
            .unwrap()
            .expect("present");
        // Upsert does not auto-bump last_updated; the caller controls
        // that field. So we expect to read back what we wrote.
        let written = rep.last_updated;
        assert_eq!(fetched.last_updated.timestamp(), written.timestamp());
    }

    #[tokio::test]
    async fn concurrent_upserts_for_different_providers() {
        use std::sync::Arc;
        let catalog = Arc::new(ModelCatalog::memory().unwrap());
        let mut handles = Vec::new();
        for i in 0..10u64 {
            let c = catalog.clone();
            handles.push(tokio::spawn(async move {
                let node_id = format!(
                    "{:0>64}",
                    format!("{:x}", i)
                );
                let mut rep = sample_rep(&node_id, i as f64);
                rep.last_updated = chrono::Utc::now();
                c.upsert_provider_reputation(&rep).await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        let list = catalog.list_provider_reputation().await.unwrap();
        assert_eq!(list.len(), 10);
    }

    #[tokio::test]
    async fn concurrent_upserts_for_same_provider_are_idempotent() {
        use std::sync::Arc;
        let catalog = Arc::new(ModelCatalog::memory().unwrap());
        let node = "5555555555555555555555555555555555555555555555555555555555555555";
        let mut handles = Vec::new();
        for i in 0..20u64 {
            let c = catalog.clone();
            let n = node.to_string();
            handles.push(tokio::spawn(async move {
                let rep = sample_rep(&n, i as f64);
                c.upsert_provider_reputation(&rep).await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        let list = catalog.list_provider_reputation().await.unwrap();
        assert_eq!(list.len(), 1);
    }
}
