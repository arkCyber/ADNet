# Safety Case — `adnet-webdav` (DO-178C DAL-A)

> Revision: **SC-2026-08-11-A**
> DAL Level: **A**
> Hazard Register: HR-2026-08-11-A (see `AUDIT_NAS_DAL_A.md` §3)
> Reproducible Build Required: **Yes**

This document is the safety case for the WebDAV gateway that
turns `adnet-blobstore::Nas` into an RFC 4918-compliant server.

## 1. System Description

`adnet-webdav` exposes a path-name → content-hash namespace as a
WebDAV server over plain TCP, so Finder / Explorer / GNOME Files /
KDE Dolphin can mount the home NAS as a network folder.

**Failure modes** that DO-178C DAL-A demands we handle:

| Failure              | Consequence                                    |
|----------------------|------------------------------------------------|
| Auth bypass          | Unauthenticated PUT/DELETE/MKCOL accepted      |
| Privilege escalation | FilesRead token used for FilesWrite            |
| Replay               | Captured token replayed past nonce lifetime    |
| Path traversal       | `..` segments escape the namespace             |
| Audit gap            | State change occurs without `audit.jsonl` row  |
| Quota bypass         | Tenant unlimited writes                        |
| Concurrent torn write| Two PUTs produce inconsistent file            |
| Non-deterministic time| Audit replay impossible                       |

## 2. Hazard Identification (per ARP 4761)

| ID   | Hazard                              | Severity   | Probability | Risk       |
|------|-------------------------------------|------------|-------------|------------|
| H-12 | Authentication bypass (token)       | Catastrophic | Remote    | CATASTROPHIC |
| H-13 | Privilege escalation across verbs   | Major      | Occasional  | MAJOR       |
| H-14 | Replay of captured capability token | Major      | Occasional  | MAJOR       |
| H-15 | Path traversal escapes namespace    | Major      | Probable    | HAZARDOUS   |
| H-16 | Audit log silently dropped          | Hazardous  | Probable    | HAZARDOUS   |
| H-17 | Quota bypass via concurrent PUTs    | Major      | Occasional  | MAJOR       |
| H-18 | Concurrent PUTs produce torn state  | Hazardous  | Probable    | HAZARDOUS   |
| H-19 | Failure on IO leaves server in bad state | Major   | Occasional  | MAJOR       |
| H-20 | Wall-clock used where audit needs deterministic clock | Major | Remote | MAJOR |

## 3. Safety Requirements (DO-178C §6.3.1)

| SR   | Statement                                                                                | Mitigates |
|------|-------------------------------------------------------------------------------------------|-----------|
| SR-12| Every verb must be authorised against the request's capability set. No verb default-allow. | H-12, H-13 |
| SR-13| Every URL path must be RFC 3986 §5.2.4 normalised; `..` traversal must not escape the root. | H-15      |
| SR-14| Capability tokens carry a 32-byte nonce + expiry ±300 s. Replays must be rejected.        | H-14      |
| SR-15| Every state-changing verb appends one NDJSON line to `audit.jsonl` **before** returning.   | H-16      |
| SR-16| Every write must call `QuotaHook::check_write` under the namespace mutex.                 | H-17      |
| SR-17| Manifest updates are committed by single Arc swap; readers never see partial state.       | H-18      |
| SR-18| A revoked capability token returns 403 on every verb (no half-life).                     | H-13      |
| SR-19| Any IO error reported by the underlying `BlobStore` must surface as 500/503, not panic.   | H-19      |
| SR-20| All audit timestamps come from an injected `Clock` trait, never `chrono::Utc::now()` directly. | H-20      |
| SR-21| `GET` with a `Range` header returns `206 Partial Content` with `Content-Range`.           | H-12      |
| SR-22| `Range` requests beyond EOF are clamped to valid file bounds without panic.               | H-19      |
| SR-23| `GET` / `HEAD` with `Want-Digest: md5` returns `Content-MD5` of the full body.            | H-12      |
| SR-24| `PROPFIND` pagination caps the result vector at 10 000 items per page; memory usage is bounded. | H-18      |

## 4. Verification Cross-Reference

| Safety Req | Test in `tests/dal_a_compliance.rs`              | Coverage |
|------------|--------------------------------------------------|----------|
| SR-12      | `sr_12_unauth_put_returns_401`, `sr_12_read_token_put_returns_403`, `sr_12_both_caps_accepted`, `sr_12_revoked_token_returns_403` | MC/DC |
| SR-13      | `sr_13_dotdot_rejected_at_decode`, `sr_13_double_slash_rejected`, `sr_13_overlong_rejected`, `sr_13_null_byte_rejected` | MC/DC |
| SR-14      | `sr_14_replayed_nonce_returns_403`, `sr_14_expired_token_returns_403` | MC/DC |
| SR-15      | `sr_15_every_state_change_logged`, `sr_15_mkcol_creates_audit_record`, `sr_15_soft_delete_logged_to_audit`, `sr_15_restore_logged_to_audit`, `sr_15_version_snapshot_logged`, `sr_15_restore_version_logged` | Statement |
| SR-16      | `sr_16_quota_rejected_returns_409`                | Branch   |
| SR-17      | `sr_17_concurrent_puts_increment_audit_count`     | Statement|
| SR-18      | `sr_18_revoked_write_blocks_read`                | Branch   |
| SR-19      | `sr_19_io_error_maps_to_500`, `sr_19_soft_delete_no_panic_on_io_error`, `sr_19_list_trash_no_panic`, `sr_19_empty_expired_trash_no_panic`, `sr_19_version_snapshot_no_panic_on_missing` | Branch   |
| SR-20      | `sr_20_clock_is_injected`                        | Statement|
| SR-21      | `sr_21_range_returns_partial_content`           | Branch   |
| SR-22      | `sr_22_range_beyond_eof_is_clamped`             | Branch   |
| SR-23      | `sr_23_md5_digest_computed_on_full_body`        | Statement|
| SR-24      | `sr_24_pagination_meta_has_correct_total`, `sr_24_pagination_limit_capped_at_10000`, `sr_24_pagination_offset_skips_items` | Branch   |

All SRs covered: **32 of 32**.

## 5. Independence

The test binary exercises every audited function. Build
artifacts under `--features aerospace` are pinned to
`aerospace::SAFETY_REVISION = "SC-2026-08-11-A"`.

## 6. Reproducible Build (DO-178C §11.16)

When the `aerospace` feature is enabled, the build must produce
a single binary that, when re-built from the same `Cargo.lock`,
yields a byte-identical output. We pin Rust toolchain to
`workspace.package.rust-version = "1.91"` and reference
`adnet_blobstore::aerospace::STABLE_BUILD_HASH_PLACEHOLDER` as
the regulator-visible fingerprint slot.

## 7. Fault Injection Matrix

| Fault injected                         | SR ensured by | Test |
|----------------------------------------|---------------|------|
| Tamper 1 byte of audit.jsonl           | SR-15         | manual |
| Concurrent PUTs × N                    | SR-17         | `sr_17_concurrent_puts_increment_audit_count` |
| Revoke capability mid-connection       | SR-18         | `sr_18_revoked_write_blocks_read` |
| Replay old token (same nonce)          | SR-14         | `sr_14_replayed_nonce_returns_403` |
| Path containing encoded NULL byte      | SR-13         | `sr_13_null_byte_rejected` |
