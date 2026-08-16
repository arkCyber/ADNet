//! Minimal example: open an in-memory SQLite connection, create
//! a `kv` table, write a couple of rows, and read them back via
//! the unified `DatabaseConnection` API.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-database --example database_basic
//! ```

use a3net_database::DatabaseConnection;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open an in-memory SQLite database.
    let conn = DatabaseConnection::sqlite(":memory:").await?;
    conn.health_check().await?;
    println!("connected + health_check ok");

    // 2. Create a table.
    conn.execute(
        "CREATE TABLE kv (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            key TEXT NOT NULL UNIQUE, \
            value TEXT NOT NULL\
        )",
    )
    .await?;

    // 3. Insert two rows.
    conn.execute("INSERT INTO kv (key, value) VALUES ('name', 'a3net')")
        .await?;
    conn.execute("INSERT INTO kv (key, value) VALUES ('lang', 'rust')")
        .await?;

    // 4. Read back via a tiny ad-hoc struct.
    #[derive(sqlx::FromRow)]
    struct Row {
        key: String,
        value: String,
    }
    let rows: Vec<Row> = conn
        .fetch_all("SELECT key, value FROM kv ORDER BY key")
        .await?;

    for r in &rows {
        println!("{} = {}", r.key, r.value);
    }
    assert_eq!(rows.len(), 2);
    println!("ok");
    Ok(())
}
