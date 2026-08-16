Node.js bindings for a3net-ffi
==============================

This is the napi-rs binding for Node.js. Mirrors iroh-ffi's
`iroh-js/` member crate.

```
crates/a3net-ffi-js/
├── Cargo.toml          # Rust crate (cdylib)
├── build.rs            # napi-build glue
├── src/lib.rs          # napi-rs annotated surface
├── package.json        # npm package manifest
├── index.d.ts          # TypeScript definitions
├── index.js            # Platform-arch loader
├── test/               # Node.js integration tests
│   └── basic.test.mjs
└── README.md
```

Building
========

```bash
# 1. Compile the Rust cdylib (produces a `.node` binary
#    per (target_triple, node_api_version) pair)
cd crates/a3net-ffi-js
napi build --platform --release --strip

# 2. Run the tests
node --test test/basic.test.mjs
```

The prebuilt `.node` binaries are distributed via npm
`optionalDependencies` for every supported triple. The
`index.js` loader resolves the right binary for the
host `(platform, arch)` combination.

Node.js (TypeScript)
====================

```typescript
import { AdnetHandle } from '@arksong/a3net'

const handle = await AdnetHandle.new('/var/lib/a3net')
await handle.ensureBooted()
console.log('node id:', handle.nodeId())

const put = await handle.putBytes(Buffer.from('hello-node'))
console.log('hash:', put.hash)

await handle.destroy()
```

npm
===

The package is published as `@arksong/a3net` (mirrors the
iroh-ffi `@number0/iroh` package name). The CI workflow
`ci_js.yml` runs `napi build` for every supported triple
and uploads the artifacts via `napi pre-publish -t npm`.

Reference
=========

* iroh-ffi `iroh-js/`: <https://github.com/n0-computer/iroh-ffi/tree/main/iroh-js>
* napi-rs: <https://napi.rs/>
