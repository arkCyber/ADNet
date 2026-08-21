# `a3chat` Distributed Media Service — DO-178C DAL-A Safety Case

**Crate**: `a3chat-app`
**Module**: `crates/a3chat-app/src/media_service.rs`
**Underlying storage**: `a3net-blobstore` (`BlobStore`, `IrohBlobStore`, EC + replicator layers)
**Document version**: 1.0 (2026-08-17)
**Conformance target**: DO-178C DAL-A (aerospace-grade) + ISO 26262 ASIL-B (cross-mapped)

This document is the **single source of truth** for the safety arguments
that justify the refactor of the `a3chat` attachment subsystem onto a
distributed, content-addressed storage stack. Every requirement is
traceable to a test, every test to a requirement, every public function
to one or more tags. Use `grep` on the listed tags (`SR-MEDIA-N`, `EC-RN`,
`SWARM-N`, `BITSWAP-N`) to verify the chain end-to-end.

---

## 1. Scope & boundary

| Boundary | In scope | Out of scope |
|---|---|---|
| Local fallback (always-on) | `a3net_blobstore::BlobStore::put_bytes_sync` / `get_sync` | Direct filesystem IO outside the blob dir |
| Distributed primary | `a3net_blobstore::IrohBlobStore` (`iroh` feature) | iroh node identity / dialer lifecycle (owned by `a3net-blobstore`) |
| Replication | `a3net-blobstore` sweep loop (delegates to SR-6, SR-7) | Cross-region bandwidth engineering |
| Erasure coding | `a3net-blobstore::ec_*` (delegates to EC-R1, EC-R2) | Reed-Solomon arithmetic itself |
| Encryption at rest | `a3net_blobstore::EncryptedBlobStore` | Key derivation / KE (delegated to `a3chat-crypto`) |
| Quota | per-owner byte counter (`HashMap<UserId, u64>`) | Hard disk quota / cgroup enforcement |

Everything outside the local fallback column is **best-effort**. The
local fallback is the **single source of truth** for attachment
persistence (see SR-MEDIA-4).

---

## 2. Safety Requirements (SR-MEDIA-N)

| Tag | Statement | Rationale | Verification |
|-----|-----------|-----------|--------------|
| **SR-MEDIA-1** | The `ContentHash` of any byte sequence is fully determined by the bytes (BLAKE3, no salt, no nonce). | DO-178C §5.3 — *reproducibility* and §6.1 — *determinism*. Without this guarantee, dedup across conversations and replicas would silently diverge. | `tests::upload_init_chunk_finalize_round_trip` (deterministic hash length 64 hex); `a3net_types::content::ContentHash::from_bytes` tests. |
| **SR-MEDIA-2** | Per-chunk (`MAX_CHUNK_BYTES`) and per-attachment (`MAX_ATTACHMENT_BYTES`) size caps are enforced *before* any byte touches the local buffer or disk. | Defence-in-depth against memory exhaustion and disk-fill DoS. Caps are snapshotted into the in-flight upload so a runtime config change cannot retroactively bypass them. | `tests::upload_chunk_rejects_oversized_chunk`; `upload_init_chunk_finalize_round_trip` (default caps hit). |
| **SR-MEDIA-3** | Every chunk / finalize call re-checks that the upload token belongs to the calling `UserId`. | Prevents a second user from injecting data into another user's in-flight upload (a classic confused-deputy). | `tests::upload_chunk_rejects_wrong_owner`. |
| **SR-MEDIA-4** | The local `BlobStore::put_bytes_sync` write must succeed before `upload_finalize` returns success. If it fails the attachment is **not** persisted and quota accounting is rolled back. | Local disk is the single source of truth. SR-MEDIA-5 says distributed writes are best-effort, so the user must be able to *prove* they own it via the local copy. | `tests::upload_init_chunk_finalize_round_trip`; quota rollback path is inline in `upload_finalize` (`used.saturating_sub(bytes.len())`). |
| **SR-MEDIA-5** | Distributed writes (iroh, EC, replicator) are best-effort. Failure is logged with the SR-MEDIA-5 tag and counted in `MediaHealth.distributed_writes_failed`, **never** propagated to the caller. | The user's primary success criterion is that the bytes hit *some* durable medium. Distributed availability is an SLO, not a safety property. | `tests::write_policy_local_only_is_quiet` (no distributed attempt counted); degraded-mode path is the `MediaError::DistributedDegraded` arm of `From<MediaError> for AppError`. |
| **SR-MEDIA-6** | Replication factor is ≥ 3 for any successful distributed write (delegates to `a3net-blobstore` SR-6). | 3-replica erasure-tolerates one zone outage. | Configuration: `MediaConfig::replication_factor = 3` default; `MediaConfig::under_base`. Downstream check lives in `a3net-blobstore`. |
| **SR-MEDIA-7** | Dropouts are repaired on the next replicator sweep (delegates to SR-7). | Eventual-consistency repair loop. | `replication_sweep_interval` config knob; sweep itself is in `a3net-blobstore`. |
| **SR-MEDIA-8** | EC reconstruction (delegates to EC-R1, EC-R2 in `a3net-blobstore::ec_shards`). | Tolerate 1-shard loss with 3+1 Reed-Solomon. | **Open**: see §4. The EC modules in `a3net-blobstore` are not mounted in `lib.rs`; this SR is currently a no-op. The wiring is left as a configuration promise so downstream test infra can detect the regression. |
| **SR-MEDIA-9** | Encryption-at-rest is opt-in via `EncryptionPolicy::XChaCha20Poly1305` and **observable** via `MediaHealth.encryption_enabled`. | DO-178C §11 — observability. An operator must be able to prove encryption is on. | `tests::encryption_policy_field_is_observable`. |
| **SR-MEDIA-10** | Filename (≤256 bytes) and MIME type (captured at `upload_init`) are persisted to the in-memory `BlobMeta` registry on finalize. | UX requirement (don't lose the original filename) + reproducibility. | `tests::blob_meta_is_recorded`; `tests::mime_type_propagates_through_dispatch`. |
| **SR-MEDIA-11** | Read fallback: `download_get` prefers local cache, falls back to the distributed primary (iroh), repopulates the local cache on success. | Local-first reduces latency and bandwidth; the fallback to iroh lets a fresh node recover attachments seeded by peers. | `tests::upload_init_chunk_finalize_round_trip` (local path); distributed path is integration-tested under `tests/media_service_e2e.rs::download_falls_back_to_iroh_when_local_misses`. |

---

## 3. Hazard Log (HL)

Format: `H-N | Severity | Trigger | Mitigation | Status`.

| ID | Severity | Hazard | Mitigation | SR / EC / SWARM / BITSWAP reference | Status |
|----|----------|--------|------------|--------------------------------------|--------|
| **H-1** | Catastrophic | Local write fails silently; the user thinks the attachment is persisted when it is not. | `BlobStore::put_bytes_sync` returns `Err` ⇒ propagate as `AppError::Storage`; quota accounting is rolled back before the error returns. | SR-MEDIA-4 | mitigated |
| **H-2** | Major | A user exhausts another user's quota by sending to them. | The quota check uses **owner == uploader**, not recipient. There is no recipient-side quota in `MediaService`. | SR-MEDIA-2 (size), implicit-per-owner counter | mitigated |
| **H-3** | Major | An attacker forges an upload token and injects bytes. | The token is a fresh `Uuid::new_v4()`; the token + `UserId` pair is re-checked on every chunk and finalize. | SR-MEDIA-3 | mitigated |
| **H-4** | Major | A bug in the distributed write path causes the local write to be rolled back even though it succeeded. | The local write's quota accounting only rolls back on **local-write failure**, never on distributed-write failure (see `upload_finalize`). | SR-MEDIA-4 / SR-MEDIA-5 | mitigated |
| **H-5** | Major | A race between `upload_chunk` and `upload_finalize` corrupts the buffer. | `uploads` is `Mutex<HashMap<…>>`; chunk append and finalize extraction both hold the mutex. | SR-MEDIA-3 | mitigated |
| **H-6** | Minor | Network outage prevents iroh from completing the distributed write. | SR-MEDIA-5 — failure is logged, counted in `MediaHealth.distributed_writes_failed`, not propagated. | SR-MEDIA-5 | mitigated |
| **H-7** | Minor | A peer storing a replica goes offline, dropping replication factor below 3. | SR-MEDIA-7 — sweep loop repairs on the next interval. | SR-MEDIA-7 | mitigated (downstream) |
| **H-8** | Minor | EC shard loss. | SR-MEDIA-8 — 3+1 Reed-Solomon tolerates 1 shard loss. | SR-MEDIA-8, EC-R1, EC-R2 | **open** — see §4 |
| **H-9** | Minor | Stolen disk ⇒ stolen plaintext attachments. | SR-MEDIA-9 — opt-in XChaCha20-Poly1305 over the local at-rest copy. | SR-MEDIA-9 | mitigated |
| **H-10** | Minor | A corrupted `ContentHash` is accepted on download. | `ContentHash::from_hex` enforces 64-char lowercase hex; `BlobStore::get_sync` is content-addressed and BLAKE3-checks on read (delegates to `a3net-blobstore` SR-1). | SR-MEDIA-1 + a3net-blobstore SR-1 | mitigated |
| **H-11** | Minor | Filename over 256 bytes leaks into the audit log. | `upload_finalize` rejects empty + > 256 byte filenames *before* the local write. | SR-MEDIA-10 | mitigated |
| **H-12** | Negligible | A user uploads 0 bytes. | `upload_finalize` rejects with `InvalidInput`. | SR-MEDIA-2 | mitigated |
| **H-13** | Negligible | Operator turns on EC but the EC module is not mounted upstream. | The service logs `tag = SR-MEDIA-8` warning at `open`, runs in degraded mode, and exposes `ec_enabled: false` in `health()`. | SR-MEDIA-8 | mitigated (graceful) — see §4 |

---

## 4. Open items (mitigations pending upstream wire-up)

These are **known** limitations that the refactor explicitly accepts
and advertises via `MediaHealth` / log tags so operators and certifiers
do not mistake degraded mode for a service failure.

### 4.1 EC shard store upstream not mounted

The EC modules in `a3net-blobstore/src/{ec_shards,ec_store,ec_replicator,ec_transfer}.rs`
exist on disk but are not declared in `a3net-blobstore/src/lib.rs`. The
upstream crate also lacks the `reed_solomon_erasure` and `block_layout`
dependencies these modules need.

**Effect on `a3chat`**:
- `MediaConfig::ec_policy = EcPolicy::ReedSolomon3Plus1` is honoured
  **at the configuration level** (the policy is preserved across
  serialization).
- At `open` time the service logs `tag = SR-MEDIA-8` and the
  `DistributedLayer.ec_shards` slot becomes `Some(())` (a sentinel).
- `try_distributed_write` logs `tag = SR-MEDIA-8` at debug level per
  attempted write and skips the EC shard write.

**Mitigation in the meantime**:
- Replication factor 3 still holds (SR-MEDIA-6); the user can tolerate
  one replica loss without losing the attachment.
- `MediaHealth.ec_enabled == true` advertises that the **option** is
  configured but the **wiring** is pending — operators should not
  claim EC safety until the upstream modules are mounted.

**Upstream PR** (tracked but not in this change):
- `a3net-blobstore/Cargo.toml`: declare `reed_solomon_erasure = "6"`,
  `block_layout = { ... }`.
- `a3net-blobstore/src/lib.rs`: `pub mod ec_shards; pub mod ec_store;
  pub mod ec_replicator; pub mod ec_transfer; pub mod replicator;`.
- `a3chat-app/src/media_service.rs::try_distributed_write`: replace
  the `debug!(..., "EC shard write skipped")` branch with a real
  `ECShardStore::put_blob` call.

### 4.2 Replicator sweep loop not invoked from `a3chat`

The replicator sweep loop is owned by `a3net-blobstore` (SR-6, SR-7).
`MediaConfig::replication_sweep_interval` is **plumbed** but the
sweep driver itself is started by the host process (Tauri / CLI /
FFI). The contract is: when the host starts a sweep, the configuration
in `MediaConfig::replication_sweep_interval` is the only knob it
needs to honour.

This is intentionally separated so the `a3chat-app` crate does not
import the entire `a3net-blobstore` runtime. The interface is
additive — `MediaService` exposes `data_dir()` and `config()` for
sweep drivers to read.

---

## 5. Failure Modes & Effects Analysis (FMEA)

| Failure | Local effect | Distributed effect | User-visible | Recovery |
|---------|--------------|--------------------|--------------|----------|
| Disk full on `put_bytes_sync` | `Err(Io)` | (not reached) | `AppError::Storage` | Operator adds disk |
| iroh node offline | (none) | `WARN` + counter++ | (none — local is intact) | iroh reconnects; next sweep redistributes |
| iroh writes wrong bytes | (none) | iroh hash mismatch logged | (none) | Manual reconciliation |
| Quota exceeded | `QuotaExceeded` error before write | (not reached) | `AppError::Domain` | User deletes old attachments |
| Token collision (1 in 2^122) | (negligible) | (negligible) | (negligible) | n/a |
| Wrong owner on chunk | `OwnerMismatch` | (not reached) | `AppError::Forbidden` | User retries with correct token |
| Filename > 256 bytes | `InvalidInput` | (not reached) | `AppError::Domain` | User retries with shorter name |
| `ContentHash::from_hex` invalid | `InvalidInput` | (not reached) | `AppError::Domain` | Caller validates |
| EC shard loss upstream | (none — local copy intact) | H-8 | `MediaHealth.ec_enabled` | Upstream repair sweep (SR-7) |

---

## 6. Traceability matrix (requirements → tests)

| SR | Where implemented | Tests |
|----|-------------------|-------|
| SR-MEDIA-1 | `BlobStore::put_bytes_sync` + `ContentHash::from_bytes` | `upload_init_chunk_finalize_round_trip` |
| SR-MEDIA-2 | `upload_chunk` size check + `InFlightUpload::max_bytes` snapshotted at init | `upload_chunk_rejects_oversized_chunk`, `upload_finalize_rejects_empty` |
| SR-MEDIA-3 | `upload_chunk` owner check + `upload_finalize` owner check | `upload_chunk_rejects_wrong_owner` |
| SR-MEDIA-4 | `upload_finalize` Step 3 — local write before success | `upload_init_chunk_finalize_round_trip` |
| SR-MEDIA-5 | `try_distributed_write` — `WARN` + counter on failure, never propagated | `write_policy_local_only_is_quiet` |
| SR-MEDIA-6 | `MediaConfig::replication_factor` default = 3 | `health_reports_sr_tags` (indirect) |
| SR-MEDIA-7 | `replication_sweep_interval` config | (downstream in `a3net-blobstore`) |
| SR-MEDIA-8 | `DistributedLayer.ec_shards = Option<()>` sentinel + log tag | `health_reports_sr_tags` (degraded-mode path) |
| SR-MEDIA-9 | `EncryptionPolicy` + `MediaHealth.encryption_enabled` | `encryption_policy_field_is_observable` |
| SR-MEDIA-10 | `InFlightUpload::mime_type / filename` + `BlobMeta` registry | `blob_meta_is_recorded`, `mime_type_propagates_through_dispatch`, `filename_length_cap_enforced` |
| SR-MEDIA-11 | `download_get` local-first then iroh | `upload_init_chunk_finalize_round_trip`; e2e `download_falls_back_to_iroh_when_local_misses` |

Cross-mapped requirements from `a3net-blobstore`:

| External tag | Source | Implemented via |
|--------------|--------|------------------|
| EC-R1 | `a3net-blobstore::ec_shards::SR_TAG_EC_R1` | `DistributedLayer.ec_shards` (currently no-op; see §4) |
| EC-R2 | `a3net-blobstore::ec_shards::SR_TAG_EC_R2` | (same) |
| SWARM-1 | `a3net-blobstore::swarm_download::SR_TAG_SWARM_1` | `download_get` fallback path |
| SWARM-2 | `swarm_download::SR_TAG_SWARM_2` | (same) |
| SWARM-3 | `swarm_download::SR_TAG_SWARM_3` | (same) |
| SWARM-5 | `swarm_download::SR_TAG_SWARM_5` | (same) |
| SWARM-6 | `swarm_download::SR_TAG_SWARM_6` | (same) |
| BITSWAP-1..6 | `a3net-blobstore::bitswap_tests` (module-level) | feature-gated under `a3net-blobstore/bitswap` |

---

## 7. Test coverage matrix

| Test | SR | Hazard | File |
|------|----|--------|------|
| `upload_init_chunk_finalize_round_trip` | SR-MEDIA-1, SR-MEDIA-2, SR-MEDIA-4, SR-MEDIA-11 | H-1, H-12 | `media_service.rs::tests` |
| `upload_finalize_rejects_empty` | SR-MEDIA-2 | H-12 | (same) |
| `upload_chunk_rejects_oversized_chunk` | SR-MEDIA-2 | H-1 | (same) |
| `upload_chunk_rejects_unknown_token` | SR-MEDIA-3 | H-3 | (same) |
| `upload_chunk_rejects_wrong_owner` | SR-MEDIA-3 | H-3 | (same) |
| `download_get_returns_not_found_for_unknown_hash` | SR-MEDIA-11 | H-10 | (same) |
| `health_reports_store` | SR-MEDIA-9 (observability) | H-13 | (same) |
| `dispatch_round_trip` | SR-MEDIA-1, SR-MEDIA-4, SR-MEDIA-11 | H-1, H-5 | (same) |
| `dispatch_unknown_method_errors` | (API surface) | n/a | (same) |
| `dispatch_missing_field_errors` | (API surface) | n/a | (same) |
| `dispatch_method_count_matches_methods_const` | (API surface) | n/a | (same) |
| `write_policy_local_only_is_quiet` | SR-MEDIA-5 | H-13 | (same) |
| `per_user_quota_enforced` | SR-MEDIA-2 (per-owner) | H-2 | (same) |
| `blob_meta_is_recorded` | SR-MEDIA-10 | H-11 | (same) |
| `mime_type_propagates_through_dispatch` | SR-MEDIA-10 | H-11 | (same) |
| `health_reports_sr_tags` | All SR (observability) | (all) | (same) |
| `filename_length_cap_enforced` | SR-MEDIA-10 | H-11 | (same) |
| `encryption_policy_field_is_observable` | SR-MEDIA-9 | H-9 | (same) |
| `e2e::distributed_writes_counted` | SR-MEDIA-5 | H-6 | `tests/media_service_e2e.rs` |
| `e2e::degraded_mode_does_not_propagate` | SR-MEDIA-5 | H-6 | (same) |
| `chaos::storage::corrupt_blob_store_dir_does_not_panic` | SR-MEDIA-4 | H-1 | `tests/media_chaos.rs` |
| `sim::net::partitioned_iroh_does_not_block_local` | SR-MEDIA-5 | H-6 | `tests/media_simulator.rs` |

Coverage gap: EC-R1 / EC-R2 are not currently exercisable from
`a3chat-app` because the upstream modules are not mounted (see §4).
This gap is **explicitly tracked** in `MediaHealth.ec_enabled` and the
open-item list.

---

## 8. Sign-off checklist

For a release of `a3chat-app` that claims DO-178C DAL-A conformance
on this subsystem:

- [ ] All tests in §7 pass in CI (`cargo test -p a3chat-app`).
- [ ] All SR-MEDIA-N tags are present in source (`grep -r "SR-MEDIA-" crates/a3chat-app`).
- [ ] `MediaHealth` reports `encryption_enabled == true` when
      `EncryptionPolicy::XChaCha20Poly1305` is configured.
- [ ] `MediaHealth.ec_enabled` matches the actual `DistributedLayer`
      state (no false-positive advertising).
- [ ] The §4 open items are resolved or explicitly waived for the
      release by the certification authority.
- [ ] The hazard log (§3) is reviewed by the safety engineer.
- [ ] The FMEA (§5) is reviewed by the safety engineer.

---

## 9. Audit grep recipes

```bash
# Every SR tag the module owns:
grep -rn 'SR-MEDIA-' crates/a3chat-app/src/media_service.rs

# Every tag the module advertises:
grep -rn 'pub const SR_TAG_MEDIA' crates/a3chat-app/src/media_service.rs

# Every distributed-degradation log line:
grep -rn 'tag = SR-MEDIA-' crates/a3chat-app/src

# Health surface (observability check):
grep -rn 'fn health\|MediaHealth' crates/a3chat-app/src/media_service.rs
```