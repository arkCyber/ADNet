# `adnet-database`

> ADNet 数据库抽象层 —— 基于 sqlx 的连接池 + 多数据库支持(SQLite / PostgreSQL / Redis)+ 版本化迁移 + Repository 模式。
>
> ADNet database abstraction layer — sqlx-based connection pool with multi-database support (SQLite / PostgreSQL / Redis), versioned migrations, and the Repository pattern.

## 概览(Overview)

`adnet-database` 给 ADNet 体系内的其他 crate 提供一个"统一、能装不同后端、能跑迁移"的数据库层。它把连接、事务、迁移、查询这四件事压成一致的 API,让上层不必每次都重新选 sqlx vs deadpool vs diesel。

设计上分三层。**Config 层**:`DatabaseConfig` 把 `SqliteConfig` / `PostgresConfig` / `RedisConfig` 三种后端参数装进同一个 enum,允许一个节点在不同子系统里使用不同后端(SQLite 用于本地 profile,Redis 用于 gossip 缓存)。**连接池层**:`ConnectionPool` 在背后跑 sqlx pool,暴露 `get() -> PooledConnection` 和 `stats()`。**仓储模式层**:`Repository` trait + `InMemoryRepository<T>` 给单测提供零开销的替代,`QueryBuilder` 用于动态拼装条件。

迁移系统独立于连接,`Migration` / `MigrationRunner` / `MigrationManager` 三件套允许运营方追踪 schema 版本,启动时按版本号顺序跑未应用的 `up_sql`,失败自动 rollback。错误统一为 `DatabaseError`(Connection / Query / Transaction / Migration / Pool / NotFound / ConstraintViolation / Serialization / Config / Unknown),带 `is_not_found` / `is_retryable` 辅助方法。

## 特性(Features)

- **多数据库支持**:`DatabaseKind::{Sqlite, Postgres, Redis}` + 对应 `SqliteConfig` / `PostgresConfig` / `RedisConfig`,同一 API 切换后端。
- **`ConnectionPool`**:内部包 sqlx `SqlitePool`,上限由 `SqlitePoolOptions` 控制,暴露 `PoolStats { total, idle, used, max }`。
- **仓储 trait**:`Repository<Entity, Id>`,每个实现管理一张表或一类集合,提供 `find_by_id / find_all / find_with_filter / create / update / delete / count / exists`。
- **`InMemoryRepository<T>`**:`tokio::sync::RwLock<HashMap>` 实现,自增 id,专为单测设计。
- **`QueryBuilder`**:流式 API 拼装 `SELECT *` / `COUNT(*)`,支持 `WHERE ... AND ...` / `LIKE` / `ORDER BY` / `LIMIT` / `OFFSET`,自动转义单引号。
- **版本化迁移**:`Migration { version, name, up_sql, down_sql }`,`MigrationRunner` 自动跳过已应用版本,事务包装保证原子性,`MigrationManager` 提供 `migrate` / `status` 两个公开方法。
- **事务支持**:`DatabaseConnection::begin_transaction() -> Transaction<'a>`,显式 `commit()` / `rollback()`。
- **健康检查**:`DatabaseConnection::health_check()` 跑 `SELECT 1` 探活。

## 安装(Installation)

工作空间内 path 依赖:

```toml
# 你的 crate 的 Cargo.toml
adnet-database = { workspace = true }
```

```rust
use adnet_database::{
    DatabaseConfig, DatabaseConnection, ConnectionPool,
    Migration, MigrationRunner, MigrationManager,
    Repository, InMemoryRepository, QueryBuilder,
};
```

## 使用(Usage)

### 1. 打开 SQLite 连接

```rust
use adnet_database::{DatabaseConfig, DatabaseConnection};
let cfg = DatabaseConfig::sqlite("data.db");
let conn = DatabaseConnection::from_config(&cfg).await?;
conn.health_check().await?;
```

### 2. 用 ConnectionPool 拿一次性连接

```rust
use adnet_database::ConnectionPool;
let pool = ConnectionPool::sqlite(&adnet_database::config::SqliteConfig::default()).await?;
let conn = pool.get().await?;
let rows = conn.execute("CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT)").await?;
let stats = pool.stats().await;
println!("pool: total={} idle={}", stats.total_connections, stats.idle_connections);
```

### 3. 跑一组迁移

```rust
use adnet_database::{Migration, MigrationManager};
let mgr = MigrationManager::new(vec![
    Migration::new(1, "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
        "DROP TABLE users"),
    Migration::new(2, "create_posts",
        "CREATE TABLE posts (id INTEGER PRIMARY KEY, body TEXT, user_id INTEGER)",
        "DROP TABLE posts"),
]);
let result = mgr.migrate(&conn).await?;
println!("applied {} migrations", result.applied);
```

### 4. 用 InMemoryRepository 做单测

```rust
use adnet_database::InMemoryRepository;
let repo: InMemoryRepository<String> = InMemoryRepository::new();
let id = repo.insert("hello".to_string()).await;
let item = repo.get(&id.to_string()).await;
assert_eq!(item, Some("hello".into()));
```

### 5. 用 QueryBuilder 拼 SELECT

```rust
use adnet_database::QueryBuilder;
let sql = QueryBuilder::new("users")
    .where_eq("active", "true")
    .order_by("created_at", "DESC")
    .limit(50)
    .build_select();
// SELECT * FROM users WHERE active = 'true' ORDER BY created_at DESC LIMIT 50
```

## 应用案例(Use Cases / Examples)

- **多子系统多后端**:同一进程用 SQLite 存用户 profile,Postgres 存审计日志,Redis 存 gossip 计数器 —— 只需在 `DatabaseConfig` 里启三个 pool。
- **嵌入式 NAS 节点**:默认走 `DatabaseKind::Sqlite`,无需任何外部服务就能跑起来;验证通过后再切到 Postgres。
- **Schema 版本管理**:`MigrationManager::status` 输出当前哪些 migration 已应用,CI 可以断言"所有 migration 都已应用"。
- **单测加速**:业务测试用 `InMemoryRepository<String>` 替代真实 SQLite,避免每个测试都打开 DB 文件。
- **运营遥测**:运营脚本通过 `QueryBuilder` 临时拼 `SELECT COUNT(*)` 看表行数,无需新增 SQL 文件。

## 许可(License)

MIT OR Apache-2.0
