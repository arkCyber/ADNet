# `adnet-share`

> Out-of-band file / collection sharing — turns a local file
> tree into a shareable collection ticket, walks the tree with
> the `walk` module, and resolves inbound tickets through the
> `receive` module. Used by the CLI (`/share`) and the
> `adnet-ffi` mobile SDK.

## Modules

| module         | purpose                                              |
|----------------|------------------------------------------------------|
| `lib`          | re-exports                                           |
| `path`         | safe path validation (no escapes, no symlink loops)  |
| `walk`         | directory tree walker with skip-rules                |
| `collection`   | `Collection` manifest + cap-bounded push            |
| `ticket`       | URL-encoded share ticket (base64url + JSON)          |
| `remote`       | remote-side ticket resolution + verification         |
| `receive`      | inbound collection receiver + integrity verifier     |
| `error`        | typed errors                                         |

## Testing

```bash
cargo test -p adnet-share   # 62 tests
```

Includes boundary tests on collection push (cap exceeded),
path-escape rejection, ticket round-trip, and walker
deterministic ordering.

## License

Same as the workspace root.