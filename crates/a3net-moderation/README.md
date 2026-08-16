# `a3net-moderation`

> Content moderation for the A3Net distributed store.
>
> Composes **blocklist + policy + takedown + reputation-bridge** into the
> single decision path that turns a `GET /ipfs/<cid>` into either an
> HTTP **200** or an HTTP **451 Unavailable For Legal Reasons**
> (RFC 7725).

## Why a separate crate

Content-addressed storage is **immutable by design** — there is no
`UPDATE` operation, so "delete this blob" needs deliberate scaffolding.
`a3net-moderation` provides that scaffolding as four composable
layers:

| Module             | Type                       | Purpose                                  |
|--------------------|----------------------------|------------------------------------------|
| `blocklist`        | [`Blocklist`]              | Persistent registry of banned hashes (`blocklist.json`). |
| `policy`           | [`ModerationPolicy`]       | Pre-serve / pre-write decision engine.   |
| `takedown`         | [`TakedownService`]        | Local-erase primitive (drop pin + `gc_unpinned` + optional crypto-shredding). |
| `reputation_bridge`| [`apply_violation`]        | Cross-subsystem feedback: a takedown lowers the publisher's reputation. |

The gateway asks `ModerationPolicy::check_read` /
`check_write` before every blob-store touch; everything else flows from
that decision.

## HTTP-451 disclosure

When a read is denied for legal reasons, the gateway returns
**HTTP 451** (RFC 7725). This is the right status code for "we have
this content, but a takedown order prevents us from serving it" and
matches the convention major public gateways follow. We deliberately
do **not** use 403 (which would imply "you may not have it") or 404
(which would imply "we don't have it").

## Decision flow

```
                  ┌──────────────────────────────┐
                  │       ModerationPolicy        │
                  │                               │
   check_read ─▶  │ 1. blocklist.lookup_active?   │── yes ─▶ Deny(reason)
                  │ 2. deny_by_default?           │── yes ─▶ Deny(default)
                  │ 3. registered classifiers     │── yes ─▶ Deny(classifier)
                  │ 4. otherwise                  │── no  ─▶ Allow
                  └──────────────────────────────┘
```

### Why `deny_by_default` is separate from the blocklist

Operators can flip `ModerationPolicy::deny_by_default` to `true` during
a crisis (e.g. a known-bad-actor flood) to refuse every read until the
blocklist is reviewed. This is **crisis-mode UI**, not a replacement
for blocklist authoring — see the CLI:

```
a3net moderation defend-on
a3net moderation defend-off
```

## Concurrency

- [`Blocklist`] and [`ModerationPolicy`] use `parking_lot::RwLock` for
  interior mutability. Reads are O(1) hash lookups.
- Writes take an exclusive lock and persist to disk atomically
  (`write-temp` + `rename`), so a crash mid-write cannot corrupt the
  blocklist.

## Status

| Layer                 | Tests                                                                 |
|-----------------------|-----------------------------------------------------------------------|
| `blocklist`           | covered (incl. NCMEC import, revoke, expiry, stats)                   |
| `policy`              | covered (`empty allows all` / `blocklist denies` / `deny_by_default` / `classifier` / `revoke unblocks`) |
| `takedown`            | covered (`tests/integration.rs`)                                     |
| `reputation_bridge`   | unit-tested + bound to `a3net_reputation::BehaviourKind::ContentViolation` |

Cross-crate interaction is exercised by
`crates/a3net-gateway/tests/moderation_integration.rs` — see that file
for the "block + serve 451 + reputation drop" scenario.
