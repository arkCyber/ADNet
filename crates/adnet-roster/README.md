# `adnet-roster`

> Per-node contact roster (地址簿) with SQLite persistence,
> short-digit dialing, and group/category tagging. Used by the
> CLI for `/roster` operations and by the FFI for mobile apps.

## Modules

| module        | purpose                                                |
|---------------|--------------------------------------------------------|
| `lib`         | re-exports                                             |
| `model`       | `Contact`, `Group`, `Membership` typed records         |
| `digit`       | short-digit dial-code resolver (e.g. `*100` → alice)    |
| `group`       | group CRUD + tagging                                   |
| `settings`    | per-user preferences (theme, default visibility, …)    |
| `mapping`     | phone / email / alias → `NodeId` resolver              |
| `store`       | public facade `RosterStore`                            |
| `mem`         | in-memory backend for tests                            |
| `sqlite`      | SQLite backend                                         |
| `error`       | typed errors                                           |

## Testing

```bash
cargo test -p adnet-roster   # 34 tests
```

Includes integration tests for contact CRUD, group
membership, digit assignment, alias resolution, and SQLite
cascade behaviour.

## License

Same as the workspace root.