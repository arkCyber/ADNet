//! Repository pattern for data access.

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::connection::DatabaseConnection;
use crate::error::{DatabaseError, DatabaseResult};

/// Repository trait for data access objects.
#[async_trait]
pub trait Repository: Send + Sync {
    /// The entity type this repository manages.
    type Entity: Send + Sync;

    /// The ID type for entities.
    type Id: Send + Sync + std::fmt::Debug + Clone;

    /// Get the table name.
    fn table_name(&self) -> &str;

    /// Find by ID.
    async fn find_by_id(&self, id: &Self::Id) -> DatabaseResult<Option<Self::Entity>>;

    /// Find all entities.
    async fn find_all(&self) -> DatabaseResult<Vec<Self::Entity>>;

    /// Find with filter.
    async fn find_with_filter(
        &self,
        filter: &HashMap<String, String>,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> DatabaseResult<Vec<Self::Entity>>;

    /// Create a new entity.
    async fn create(&self, entity: &Self::Entity) -> DatabaseResult<Self::Id>;

    /// Update an entity.
    async fn update(&self, id: &Self::Id, entity: &Self::Entity) -> DatabaseResult<()>;

    /// Delete an entity.
    async fn delete(&self, id: &Self::Id) -> DatabaseResult<bool>;

    /// Count entities.
    async fn count(&self) -> DatabaseResult<i64>;

    /// Check if entity exists.
    async fn exists(&self, id: &Self::Id) -> DatabaseResult<bool>;
}

/// Extension trait for Repository.
pub trait RepositoryExt: Repository {
    /// Get the database connection.
    fn connection(&self) -> &DatabaseConnection;
}

impl<T: Repository + ?Sized> RepositoryExt for T {
    fn connection(&self) -> &DatabaseConnection {
        unimplemented!("Override in implementation")
    }
}

/// Generic in-memory repository for testing.
pub struct InMemoryRepository<T: Clone + Send + Sync + 'static> {
    items: Arc<RwLock<HashMap<String, T>>>,
    next_id: Arc<RwLock<i64>>,
}

impl<T: Clone + Send + Sync + 'static> InMemoryRepository<T> {
    /// Create a new in-memory repository.
    pub fn new() -> Self {
        Self {
            items: Arc::new(RwLock::new(HashMap::new())),
            next_id: Arc::new(RwLock::new(1)),
        }
    }

    /// Insert an item.
    pub async fn insert(&self, item: T) -> i64 {
        let mut next_id = self.next_id.write().await;
        let id = *next_id;
        *next_id += 1;
        drop(next_id);

        let mut items = self.items.write().await;
        items.insert(id.to_string(), item);
        id
    }

    /// Get an item by ID.
    pub async fn get(&self, id: &str) -> Option<T> {
        let items = self.items.read().await;
        items.get(id).cloned()
    }

    /// Get all items.
    pub async fn all(&self) -> Vec<T> {
        let items = self.items.read().await;
        items.values().cloned().collect()
    }

    /// Remove an item.
    pub async fn remove(&self, id: &str) -> Option<T> {
        let mut items = self.items.write().await;
        items.remove(id)
    }

    /// Clear all items.
    pub async fn clear(&self) {
        let mut items = self.items.write().await;
        items.clear();
    }

    /// Get count.
    pub async fn len(&self) -> usize {
        let items = self.items.read().await;
        items.len()
    }

    /// Check if empty.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

impl<T: Clone + Send + Sync + 'static> Default for InMemoryRepository<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for complex queries.
pub struct QueryBuilder {
    table: String,
    conditions: Vec<String>,
    order_by: Vec<String>,
    limit_val: Option<usize>,
    offset_val: Option<usize>,
}

impl QueryBuilder {
    /// Create a new query builder for a table.
    pub fn new(table: &str) -> Self {
        Self {
            table: table.to_string(),
            conditions: Vec::new(),
            order_by: Vec::new(),
            limit_val: None,
            offset_val: None,
        }
    }

    /// Add a WHERE condition.
    pub fn where_eq(mut self, column: &str, value: &str) -> Self {
        self.conditions.push(format!("{} = '{}'", column, value.replace("'", "''")));
        self
    }

    /// Add a WHERE LIKE condition.
    pub fn where_like(mut self, column: &str, pattern: &str) -> Self {
        self.conditions.push(format!("{} LIKE '{}'", column, pattern.replace("'", "''")));
        self
    }

    /// Add ORDER BY clause.
    pub fn order_by(mut self, column: &str, direction: &str) -> Self {
        self.order_by.push(format!("{} {}", column, direction));
        self
    }

    /// Set LIMIT.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit_val = Some(limit);
        self
    }

    /// Set OFFSET.
    pub fn offset(mut self, offset: usize) -> Self {
        self.offset_val = Some(offset);
        self
    }

    /// Build the SELECT query.
    pub fn build_select(&self) -> String {
        let mut sql = format!("SELECT * FROM {}", self.table);

        if !self.conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&self.conditions.join(" AND "));
        }

        if !self.order_by.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&self.order_by.join(", "));
        }

        if let Some(limit) = self.limit_val {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        if let Some(offset) = self.offset_val {
            sql.push_str(&format!(" OFFSET {}", offset));
        }

        sql
    }

    /// Build the COUNT query.
    pub fn build_count(&self) -> String {
        let mut sql = format!("SELECT COUNT(*) FROM {}", self.table);

        if !self.conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&self.conditions.join(" AND "));
        }

        sql
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_repository() {
        let repo: InMemoryRepository<String> = InMemoryRepository::new();

        let id = repo.insert("Hello".to_string()).await;
        assert_eq!(id, 1);

        let item = repo.get("1").await;
        assert_eq!(item, Some("Hello".to_string()));

        let all = repo.all().await;
        assert_eq!(all.len(), 1);

        let removed = repo.remove("1").await;
        assert_eq!(removed, Some("Hello".to_string()));

        assert!(repo.is_empty().await);
    }

    #[test]
    fn test_query_builder() {
        let query = QueryBuilder::new("users")
            .where_eq("name", "Alice")
            .where_like("email", "%@example.com")
            .order_by("created_at", "DESC")
            .limit(10)
            .offset(20)
            .build_select();

        assert!(query.contains("WHERE"));
        assert!(query.contains("name = 'Alice'"));
        assert!(query.contains("LIKE '%@example.com'"));
        assert!(query.contains("ORDER BY created_at DESC"));
        assert!(query.contains("LIMIT 10"));
        assert!(query.contains("OFFSET 20"));
    }
}
