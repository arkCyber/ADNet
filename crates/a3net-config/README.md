# `a3net-config`

> Unified, hot-reloadable configuration system for A3Net.
>
> **DO-178C DAL-B** — Configuration layer sits at Design Assurance Level B, the
> second-highest rigor tier in DO-178C (above D/E, below A). Every module in
> this crate carries a `SR-N` traceability marker (`SR-1` ... `SR-12`) that
> maps to the requirements table in `docs/traceability.md` of the
> workspace root (planned; see "Tracking" below).

## What it does

`a3net-config` provides a single `ConfigManager` that:

1. Loads values from a JSON file (highest stability), then environment
   variables with a configurable prefix (`ADNET_HOST` →
   `host`), then runtime `set()` overrides.
2. Validates the merged result against an optional schema
   (`SchemaType::Integer { min, max }`, regex-lite
   patterns, etc.).
3. Watches the on-disk config file and re-applies the load
   pipeline on change (`notify` + `notify-debouncer-mini`).
4. Exposes typed getters: `get_string`, `get_i64`, `get_f64`,
   `get_bool`. Anything missing returns `None` instead of panicking.

## Layered architecture

```
                ┌────────────────────────────┐
                │      ConfigManager          │
                │  values : HashMap<String,   │
                │           ConfigValue>      │
                └─────────┬──────────────────┘
                          │
   ┌────────────┬─────────┼───────────────┐
   ▼            ▼         ▼               ▼
 Schema      Source     Watcher      HotReloadConfig
 Validation  Priority   (notify)     background loop
```

- **`error`** — `ConfigError` + `ConfigResult`. Categorises IO,
  parse, validation, schema, watcher failures.
- **`manager`** — `ConfigManager` + `HotReloadConfig`. Orchestrator.
- **`schema`** — `ConfigKey`, `ConfigValue`, `SchemaType`, `ConfigSchema`,
  `SchemaValidator`. Pure data + validator.
- **`source`** — `FileSource`, `EnvSource`, `ConfigSource` (with priorities).
- **`watcher`** — `ConfigWatcher` + `ConfigWatcherEvent`. Wraps `notify`.

## Quick start

```rust
use a3net_config::{
    ConfigManager, ConfigValue, ConfigSchema, SchemaType,
};
use std::path::Path;

let manager = ConfigManager::new("config.json", "ADNET")
    .with_schema(
        ConfigSchema::new()
            .required_field("host", SchemaType::Any)
            .required_field("port", SchemaType::Integer {
                min: Some(1024), max: Some(65535),
            }),
    );

manager.load()?;
assert_eq!(manager.get_string("host"), Some("localhost".into()));
assert_eq!(manager.get_i64("port"), Some(8080));

manager.start_watcher()?;
// … later, when shutdown:
manager.stop_watcher()?;
```

## Status / open work

| Item                              | State                                       |
|-----------------------------------|---------------------------------------------|
| Module-level unit tests           | **in place** (see `manager::tests`, `source::tests`, `schema::tests`). |
| End-to-end environment overrides  | Covered by `test_config_manager_env_override`. |
| Schema-driven validation path     | Covered for string / int only. Array + Object branches in `validate_value` are reachable but not yet asserted on; see issue tracker. |
| File-watcher reload under load    | Not yet exercised by `cargo test`; needs a `notify` integration test under `tempfile`. |
| `docs/traceability.md` mapping    | **TODO** — `SR-N` markers exist but the cross-reference table is not yet authored. |
| DO-178C DAL-A escalation          | **Not required.** Configuration is DAL-B; promotion to DAL-A would require formal verification we are not budgeting for. |

## Why we still have `#[warn(missing_docs)]`

`lib.rs:28` enables `missing_docs` to keep the public surface area
intentional. New types added to the crate that don't carry a `///`
doc comment will produce a warning at build time. This is by design,
not a bug.
