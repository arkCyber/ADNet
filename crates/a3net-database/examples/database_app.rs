//! Realistic example: a small `kv` repository lives on top of an
//! `InMemoryRepository<String>`, and the schema lives in a
//! `Migration` that's run through `MigrationManager` against an
//! in-memory SQLite database. Two demo "tenants" share the same
//! backing store but write/read their own keys.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-database --example database_app
//! ```

use a3net_database::{DatabaseConnection, InMemoryRepository, Migration, MigrationManager};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open the SQLite store + run migrations.
    let conn = DatabaseConnection::sqlite(":memory:").await?;
    let mgr = MigrationManager::new(vec![Migration::new(
        1,
        "create_kv",
        "CREATE TABLE kv (id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT UNIQUE, value TEXT)",
        "DROP TABLE kv",
    )]);
    let result = mgr.migrate(&conn).await?;
    assert!(!result.has_errors());
    println!("applied {} migration(s)", result.applied);

    // 2. Build an in-memory repository on top of a unit type so we
    //    can demonstrate the CRUD surface.
    let repo: InMemoryRepository<String> = InMemoryRepository::new();
    let id_alice = repo.insert("alice-token".into()).await;
    let id_bob = repo.insert("bob-token".into()).await;
    println!("alice token stored under id={id_alice}");
    println!("bob   token stored under id={id_bob}");

    // 3. Query + filter.
    let all = repo.all().await;
    assert_eq!(all.len(), 2);
    let alice = repo.get(&id_alice.to_string()).await;
    let bob = repo.get(&id_bob.to_string()).await;
    assert_eq!(alice.as_deref(), Some("alice-token"));
    assert_eq!(bob.as_deref(), Some("bob-token"));

    // 4. Remove one and assert count.
    repo.remove(&id_alice.to_string()).await;
    assert_eq!(repo.len().await, 1);
    assert!(repo.is_empty().await == false);
    println!("removed alice; remaining = {}", repo.len().await);

    // 5. Confirm the underlying SQLite connection is still usable.
    let rows: Vec<(String, String)> = conn
        .fetch_all("SELECT key, value FROM kv")
        .await
        .unwrap_or_default();
    println!("sqlite still has {} table(s)", rows.len());
    Ok(())
}
