# Safety Case — adnet-blobstore (DO-178C DAL-B)

> Revision: **SC-2026-08-11**
> DAL Level: **B**
> Hazard Register: HR-2026-08-11
> Reproducible Build Required: **Yes**

This document is the safety case for the ADNet blob store
subsystem (`adnet-blobstore`). It identifies hazards, lists the
safety requirements (SR-1 … SR-5) that mitigate them, and cross-
references the verification artefacts (tests) that demonstrate the
mitigations hold.

## 1. System Description

The `adnet-blobstore` crate provides content-addressed chunked
blob storage. Every blob is split into 16 KiB chunks, each with
a BLAKE3 content hash. The store is the disk-backed foundation
for the ADNet peer-to-peer network.

**Failure modes** that DO-178C DAL-B demands we handle:

| Failure          | Consequence                                    |
|------------------|------------------------------------------------|
| Silent corruption| Reads return garbage, network propagates it    |
| Unauth deletion  | Loss of irreplaceable flight ops data         |
| Disk full / ENOSPC| Import hangs (regression: O(N) gauge loop)    |
| Hostile path     | `/proc` or symlink import = kernel state leak  |
| Partial import   | Future reads return corruption, never detected |

## 2. Hazard Identification (per ARP 4761)

| ID  | Hazard                              | Severity | Probability | Risk    |
|-----|-------------------------------------|----------|-------------|---------|
| H-1 | Silent corruption (TOCTOU read)     | Major    | Probable    | HAZARDOUS |
| H-2 | Unauthorised blob deletion          | Major    | Remote      | MAJOR     |
| H-3 | Staging failure on cross-volume     | Major    | Occasional  | MAJOR     |
| H-4 | Corrupt blob remains "complete"     | Hazardous| Probable    | HAZARDOUS |
| H-5 | Bit-rot in chunked storage          | Minor    | Occasional  | MINOR     |
| H-7 | Edge-case filename bypass           | Minor    | Remote      | MINOR     |
| H-8 | Hostile path import                 | Minor    | Remote      | MINOR     |
| H-11| DoS via large-file Gauge loop       | Minor    | Probable    | MINOR     |

## 3. Safety Requirements (DO-178C §6.3.1)

| SR   | Statement                                                      | Mitigates |
|------|----------------------------------------------------------------|-----------|
| SR-1 | Every chunk shall be hash-verified at read time.               | H-1, H-5  |
| SR-2 | Every blob removal shall require explicit completion proof.   | H-2       |
| SR-3 | Every cross-volume staging shall re-verify hash post-move.    | H-3       |
| SR-4 | Every corrupt blob shall be moved to `.quarantine/`.          | H-4       |
| SR-5 | All imports shall reject paths outside the allow-list.        | H-7, H-8  |
| SR-11| Gauge updates for bulk sizes shall be O(1).                   | H-11      |

## 4. Verification Cross-Reference

| Safety Req | Test (in `tests/aerospace_compliance.rs`)      | Coverage |
|------------|------------------------------------------------|----------|
| SR-1       | `sr_1_verified_read_*` (3 tests)               | MC/DC    |
| SR-2       | `sr_2_remove_verified_*` (4 tests)             | MC/DC    |
| SR-3       | `sr_3_import_post_rename_rehash`               | Branch   |
| SR-4       | `sr_4_quarantine_*` (3 tests)                  | MC/DC    |
| SR-5       | `sr_5_import_*` (5 tests)                      | MC/DC    |
| SR-11      | `robustness_256mib_import_completes_quickly`   | Statement|

Run all safety tests with:

```sh
cargo test --features aerospace --test aerospace_compliance
```

## 5. Independence

The test binary exercises every audited function. Build
artifacts under `--features aerospace` are pinned to this
revision (see `aerospace::SAFETY_REVISION`).

## 6. Reproducible Build (DO-178C §11.16)

When the `aerospace` feature is enabled, the build must produce
bit-identical artifacts across hosts:

```sh
CARGO_NET_OFFLINE=true cargo build --features aerospace --locked
sha256sum target/debug/libadnet_blobstore.rlib
```

Any source change that affects compilation or link order MUST
bump `aerospace::SAFETY_REVISION` and re-baseline the SHA.
