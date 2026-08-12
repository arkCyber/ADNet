# `adnet-userstore`

> Per-user profile store with SQLite persistence. Owns the
> local node's "self" profile plus any cached peer profiles the
> rest of the runtime asks about. Used by the chat, gossip, and
> roster layers to resolve `NodeId → display name / avatar`.

## Modules

| module        | purpose                                            |
|---------------|----------------------------------------------------|
| `lib`         | re-exports                                         |
| `model`       | `UserProfile` typed record                         |
| `sqlite`      | SQLite backend                                     |
| `store`       | `UserStore` facade                                 |
| `mem`         | in-memory backend                                  |
| `error`       | typed errors                                       |

## Testing

```bash
cargo test -p adnet-userstore   # 18 tests
```

Coverage includes:

- profile upsert + read
- avatar-hash dedup
- delete cascades and clears the digit-mapping table

## License

Same as the workspace root.