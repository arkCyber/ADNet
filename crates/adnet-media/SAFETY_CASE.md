# Safety Case — adnet-media (DO-178C DAL-B)

> Revision: **SC-MEDIA-2026-08-11-r4**
> DAL Level: **B**
> Source Pin: `aerospace::SAFETY_REVISION` = `MEDIA-2026-08-11-r4`
> Reproducible Build Required: **Yes**

This safety case applies to the `adnet-media` crate. It is the
short-video ingest, transcode, segment, audio, manifest, and DAG
pipeline. Every clip that the ADNet node serves as a "short
video" passes through this crate.

## 1. System Description

`adnet-media` decomposes a raw PCM + RGB-888 input into a
content-addressed DAG of segments. The DAG itself is a
deterministic binary record whose root is a BLAKE3 digest of all
variant and audio manifest digests, plus the declared duration
and byte size. The pipeline is:

1. Ingest (size / fps / layout validation)
2. Transcode (per-variant: 240p / 480p / 720p / 1080p)
3. Segment (2 s fixed-window slicing)
4. Audio analysis (RMS, peak, silence ratio)
5. Manifest (per-variant + audio + root)
6. DAG (binary record published by the blobstore)

**Failure modes** this safety case must address:

| Failure | Consequence |
|---|---|
| Silent corruption in a segment | Network propagation of garbage → SR-1 |
| Manifest tampering | Wrong DAG addresses served → SR-2 |
| Non-deterministic segment slicing | Re-ingest produces different hashes, dedup-incorrect → SR-3 |
| Corrupt segment completes manifest | Playback failure with no error code → SR-4 |
| Decoder buffer overrun on truncated segment | Kernel panic / heap corruption → SR-6 |
| AV drift -> out-of-sync playback | User-visible glitch → SR-7 |
| Bad timestamp → metadata poisoning | Stale clip served as fresh → SR-7 |

## 2. Hazard Identification (per ARP 4761)

| ID    | Hazard                                   | Severity | Probability | Risk     |
|-------|------------------------------------------|----------|-------------|----------|
| H-1   | Segment corruption (TOCTOU read)         | Major    | Probable    | HAZARDOUS|
| H-2   | Manifest tampering                       | Major    | Remote      | MAJOR    |
| H-3   | Non-deterministic slicing                | Major    | Occasional  | MAJOR    |
| H-4   | Corrupt segment completes manifest       | Hazardous| Probable    | HAZARDOUS|
| H-6   | Decoder overrun on truncated segment     | Hazardous| Remote      | MAJOR    |
| H-7   | AV drift / clock skew / duration lie     | Major    | Occasional  | MAJOR    |
| H-8   | Codec mismatch (declared vs. payload)    | Minor    | Remote      | MINOR    |
| H-9   | Oversized payload OOMs the node          | Minor    | Occasional  | MINOR    |
| H-10  | DAG persistence mutates / loses segments; partial-failure leaves dangling manifest; blobstore bytes diverge from LP-digest index | Hazardous| Probable    | HAZARDOUS|

## 3. Safety Requirements (DO-178C §6.3.1)

| SR    | Statement                                                                                  | Mitigates |
|-------|--------------------------------------------------------------------------------------------|-----------|
| SR-1  | Every segment payload shall be BLAKE3-hashed and the digest recorded in the manifest.      | H-1       |
| SR-2  | Every manifest root shall be a BLAKE3 digest over deterministic canonical serialization.   | H-2       |
| SR-3  | The segmenter slicing boundary shall be a function of segment index only (no input read).  | H-3       |
| SR-4  | The ingest pipeline shall reject any output whose segment hashes do not match the manifest. | H-4       |
| SR-6  | Every segment payload shall be length-prefixed (u32 LE) so a truncated stream is rejected. | H-6       |
| SR-7  | The verifier shall reject manifests whose AV drift, clock skew, or duration cross-check fails. | H-7   |
| SR-8  | Codec tags shall be validated against a closed enum; unknown tags shall be rejected.       | H-8       |
| SR-9  | The ingester shall reject payloads larger than `MAX_MEDIA_BYTES`.                          | H-9       |
| SR-10 | Persisting a `MediaDag` to the blobstore shall not mutate its content hash; every referenced segment shall remain resolvable; reloading the persisted manifest shall yield a structurally identical object. The on-disk segment-index sidecar shall record the LP tag for each segment so `load_segment_with_kind` can re-verify the bytes. The manifest file shall be written LAST so a partial-failure never leaves a manifest pointing at un-indexed segments. The manifest loader shall cross-check `declared_byte_size` and `declared_duration_ms` against the sum of the persisted segments. | H-10 |

## 4. Verification Cross-Reference

| Safety Req | Test (in `tests/aerospace_compliance.rs`)        | Coverage |
|------------|--------------------------------------------------|----------|
| SR-1       | `sr_1_segment_hash_is_deterministic` etc.        | MC/DC    |
| SR-2       | `sr_2_manifest_round_trip` etc.                  | MC/DC    |
| SR-3       | `sr_3_segmenter_is_deterministic`                | Statement|
| SR-4       | `sr_4_manifest_tampering_detected`               | MC/DC    |
| SR-6       | `sr_6_length_prefix_truncated_rejected`          | MC/DC    |
| SR-7       | `sr_7_av_drift_clock_skew_duration_mismatch`     | Branch   |
| SR-8       | `sr_8_unknown_codec_tag_rejected`                | MC/DC    |
| SR-9       | `sr_9_oversized_payload_rejected`                | Statement|
| SR-10      | `sr_10_persist_preserves_manifest_root`, `sr_10_round_trip_manifest_is_byte_equal`, `sr_10_verify_complete_succeeds_after_persist`, `sr_10_verify_complete_fails_when_segment_missing`, `sr_10_verify_complete_fails_when_index_missing_after_persist`, `sr_10_verify_complete_reloads_manifest_from_disk`, `sr_10_verify_complete_rejects_wrong_lp_kind_in_index`, `sr_10_persist_is_idempotent`, `sr_10_tampered_segment_rejected_at_persist_time`, `sr_10_persist_rejects_oversized_segment`, `sr_10_root_mismatch_rejected`, `sr_10_alias_round_trip`, `sr_10_alias_persists_across_restart`, `sr_10_alias_overwrite_logs_warning`, `sr_10_persist_with_empty_alias_rejected`, `sr_10_load_by_alias_rejects_tampered_root`, `sr_10_load_manifest_rejects_path_traversal`, `sr_10_load_manifest_rejects_declared_byte_size_underflow`, `sr_10_segment_round_trip_preserves_payload`, `sr_10_load_segment_round_trips_audio`, `sr_10_load_segment_rejects_invalid_hex`, `sr_10_load_segment_rejects_invalid_kind`, `sr_10_load_segment_rejects_wrong_kind`, `sr_10_load_segment_rejects_tampered_index`, `sr_10_load_segment_rejects_blob_tampering` | MC/DC    |

Run all safety tests with:

```sh
cargo test -p adnet-media --features aerospace --test aerospace_compliance
```

## 5. Independence

Each test exercises a single audited function. The test binary
imports the crate via the public API only — no `#[cfg(test)]`
internals are accessed outside the source file under test.

## 6. Reproducible Build (DO-178C §11.16)

When the `aerospace` feature is enabled, the build must produce
bit-identical artifacts:

```sh
CARGO_NET_OFFLINE=true cargo build --features aerospace --locked
sha256sum target/debug/libadnet_media.rlib
```

Any source change that affects compilation or link order MUST
bump `aerospace::SAFETY_REVISION` and re-baseline the SHA.
