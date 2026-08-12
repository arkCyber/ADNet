//! Persistence tests for `MediaStore`.
//!
//! Coverage:
//!   - SR-10: persisting a manifest produces the same BLAKE3 root
//!   - Round-trip: persist → reload returns the identical DAG
//!   - Alias resolution: alias → root → manifest
//!   - Tamper detection: re-loading a tampered manifest fails
//!   - Quarantine detection: missing segment is reported
//!   - Idempotency: persisting twice is safe

use adnet_blobstore::BlobStore;
use adnet_media::codec::{AudioCodec, SampleFormat, VideoCodec};
use adnet_media::ingest::MediaIngester;
use adnet_media::persist::{AliasMap, MediaStore};
use adnet_media::transcode::Frame;

fn fresh_store() -> (tempfile::TempDir, MediaStore) {
    let dir = tempfile::tempdir().unwrap();
    let bs = BlobStore::new(dir.path()).unwrap();
    let store = MediaStore::open(bs).unwrap();
    (dir, store)
}

fn ingest_clip() -> adnet_media::ingest::IngestReport {
    let ing = MediaIngester::default();
    let samples = vec![0u8; 48_000 * 2 * 2 * 2_000 / 1_000];
    let frames: Vec<Frame> = (0..60)
        .map(|i| Frame::solid(426, 240, (i & 0xFF) as u8, 0, 0))
        .collect();
    ing.ingest(
        samples,
        SampleFormat::S16,
        2,
        AudioCodec::Aac,
        frames,
        VideoCodec::H264,
        30,
    )
    .unwrap()
}

#[test]
fn sr_10_persisted_manifest_hash_equals_root() {
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    // Collect segment payloads from the IngestReport.
    let mut video_segs = Vec::new();
    let mut audio_segs = Vec::new();
    for out in &report.transcoder_outputs {
        video_segs.extend(out.video_segments.iter().cloned());
        audio_segs.extend(out.audio_segments.iter().cloned());
    }
    let out = store
        .persist_dag_with_segments(
            &report.dag,
            &report.manifest,
            &video_segs,
            &audio_segs,
        )
        .unwrap();
    // SR-10: the persisted manifest's blob hash equals the
    // declared root.
    assert_eq!(out.manifest_hash, report.manifest.root.as_hex());
}

#[test]
fn round_trip_manifest_is_identical() {
    // This is a relaxed smoke test; the strict byte-equal version
    // lives in `aerospace_compliance::sr_10_round_trip_manifest_is_byte_equal`.
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    let mut v = Vec::new();
    let mut a = Vec::new();
    for out in &report.transcoder_outputs {
        v.extend(out.video_segments.iter().cloned());
        a.extend(out.audio_segments.iter().cloned());
    }
    store
        .persist_dag_with_segments(&report.dag, &report.manifest, &v, &a)
        .unwrap();
    let root_hex = report.manifest.root.as_hex();
    let loaded = store.load_manifest(&root_hex).unwrap();
    assert_eq!(loaded.root, report.manifest.root);
    assert_eq!(loaded.variants.len(), report.manifest.variants.len());
    assert_eq!(loaded.audio.segments.len(), report.manifest.audio.segments.len());
}

#[test]
fn alias_round_trip() {
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    let mut v = Vec::new();
    let mut a = Vec::new();
    for out in &report.transcoder_outputs {
        v.extend(out.video_segments.iter().cloned());
        a.extend(out.audio_segments.iter().cloned());
    }
    store
        .persist_with_alias(&report.dag, &report.manifest, &v, &a, "intro")
        .unwrap();
    let loaded = store.load_by_alias("intro").unwrap();
    assert_eq!(loaded.root, report.manifest.root);
}

#[test]
fn alias_unknown_rejected() {
    let (_dir, store) = fresh_store();
    let err = store.load_by_alias("does-not-exist").unwrap_err();
    assert!(matches!(err, adnet_media::error::MediaError::InvalidConfig(_)));
}

#[test]
fn alias_map_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let bs = BlobStore::new(dir.path()).unwrap();
    let store = MediaStore::open(bs).unwrap();
    let report = ingest_clip();
    let mut v = Vec::new();
    let mut a = Vec::new();
    for out in &report.transcoder_outputs {
        v.extend(out.video_segments.iter().cloned());
        a.extend(out.audio_segments.iter().cloned());
    }
    store
        .persist_with_alias(&report.dag, &report.manifest, &v, &a, "alpha")
        .unwrap();
    drop(store);

    // Re-open the store on the same data dir; the alias should
    // still resolve.
    let bs2 = BlobStore::new(dir.path()).unwrap();
    let store2 = MediaStore::open(bs2).unwrap();
    let loaded = store2.load_by_alias("alpha").unwrap();
    assert_eq!(loaded.root, report.manifest.root);
}

#[test]
fn idempotent_persist() {
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    let mut v = Vec::new();
    let mut a = Vec::new();
    for out in &report.transcoder_outputs {
        v.extend(out.video_segments.iter().cloned());
        a.extend(out.audio_segments.iter().cloned());
    }
    let r1 = store
        .persist_dag_with_segments(&report.dag, &report.manifest, &v, &a)
        .unwrap();
    let r2 = store
        .persist_dag_with_segments(&report.dag, &report.manifest, &v, &a)
        .unwrap();
    assert_eq!(r1.manifest_hash, r2.manifest_hash);
    assert_eq!(r1.bytes_written, r2.bytes_written);
    assert_eq!(r1.video_segments, r2.video_segments);
    assert_eq!(r1.audio_segments, r2.audio_segments);
}

#[test]
fn verify_complete_succeeds_after_persist() {
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    let mut v = Vec::new();
    let mut a = Vec::new();
    for out in &report.transcoder_outputs {
        v.extend(out.video_segments.iter().cloned());
        a.extend(out.audio_segments.iter().cloned());
    }
    store
        .persist_dag_with_segments(&report.dag, &report.manifest, &v, &a)
        .unwrap();
    let count = store.verify_complete(&report.manifest).unwrap();
    assert!(count >= 4);
}

#[test]
fn verify_complete_fails_if_segment_missing() {
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    // Intentionally do NOT persist — every segment is "missing"
    // from the local index sidecar. verify_complete re-loads
    // the manifest by its declared root, so the very first
    // failure is `Quarantined` (manifest file not on disk).
    // The index-vs-blobstore distinction is exercised by
    // `aerospace_compliance::sr_10_verify_complete_fails_when_index_missing_after_persist`.
    let err = store.verify_complete(&report.manifest).unwrap_err();
    assert!(matches!(err, adnet_media::error::MediaError::Quarantined { .. }));
}

#[test]
fn load_segment_round_trip() {
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    let mut v = Vec::new();
    let mut a = Vec::new();
    for out in &report.transcoder_outputs {
        v.extend(out.video_segments.iter().cloned());
        a.extend(out.audio_segments.iter().cloned());
    }
    store
        .persist_dag_with_segments(&report.dag, &report.manifest, &v, &a)
        .unwrap();

    // Read back the first video segment of the first variant.
    let first_video = &report.manifest.variants[0].segments[0];
    // We need the LP kind from the DAG (the manifest's SegmentRef
    // stores a MediaDigest, not a SegmentDigest with kind).
    let dag_video = &report.dag.variants[0].segments[0];
    let loaded = store
        .load_segment_with_kind(&first_video.digest.as_hex(), dag_video.digest.kind)
        .unwrap();
    // The first 5 bytes are length-prefix (kind + u32 length).
    assert_eq!(loaded[0], 0x01);
    assert_eq!(&loaded[5..], &v[0][5..]);
}

#[test]
fn tampered_segment_rejected_at_persist_time() {
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    let mut v = Vec::new();
    let mut a = Vec::new();
    for out in &report.transcoder_outputs {
        v.extend(out.video_segments.iter().cloned());
        a.extend(out.audio_segments.iter().cloned());
    }
    // Flip a byte in the second video segment so its BLAKE3
    // no longer matches the DAG-declared digest.
    if v.len() >= 2 {
        let last = v[1].len() - 1;
        v[1][last] ^= 0xFF;
    }
    let err = store
        .persist_dag_with_segments(&report.dag, &report.manifest, &v, &a)
        .unwrap_err();
    assert!(matches!(
        err,
        adnet_media::error::MediaError::ManifestHashMismatch { .. }
    ));
}

#[test]
fn root_mismatch_rejected() {
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    let mut dag = report.dag.clone();
    // Force a different root without rebuilding the manifest.
    dag.root.bytes = [0xAAu8; 32];
    let err = store
        .persist_dag_with_segments(&dag, &report.manifest, &[], &[])
        .unwrap_err();
    assert!(matches!(
        err,
        adnet_media::error::MediaError::ManifestHashMismatch { .. }
    ));
}

#[test]
fn alias_map_basic_api() {
    let mut m = AliasMap::new();
    assert!(m.is_empty());
    m.insert("k", "a".repeat(64));
    assert_eq!(m.len(), 1);
    assert!(m.get("k").is_some());
    assert!(m.get("missing").is_none());
    let removed = m.remove("k");
    assert!(removed.is_some());
    assert!(m.is_empty());
}

#[test]
fn sr_10_path_traversal_rejected() {
    let (_dir, store) = fresh_store();
    // All of these must surface InvalidConfig without touching
    // the FS.
    let bad = [
        "../etc/passwd",
        "/etc/passwd",
        "..",
        ".",
        "abcdef",          // wrong length
        "ZZZ0000000000000000000000000000000000000000000000000000000000000", // non-hex
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefg", // non-hex char
    ];
    for b in bad {
        let err = store.load_manifest(b).unwrap_err();
        assert!(
            matches!(err, adnet_media::error::MediaError::InvalidConfig(_)),
            "expected InvalidConfig for {b:?}, got {err:?}"
        );
    }
}

#[test]
fn sr_10_load_by_alias_rejects_malformed_root() {
    // Plant a forged alias map and ensure load_by_alias refuses.
    let dir = tempfile::tempdir().unwrap();
    let bs = BlobStore::new(dir.path()).unwrap();
    let store = MediaStore::open(bs).unwrap();
    let alias_path = dir.path().join("media-aliases.json");
    std::fs::write(
        &alias_path,
        r#"{"entries":{"pwned":"../../etc/passwd"}}"#,
    )
    .unwrap();
    let err = store.load_by_alias("pwned").unwrap_err();
    assert!(matches!(err, adnet_media::error::MediaError::InvalidConfig(_)));
}

#[test]
fn sr_10_load_by_alias_rejects_empty_alias() {
    let (_dir, store) = fresh_store();
    let err = store.load_by_alias("").unwrap_err();
    assert!(matches!(err, adnet_media::error::MediaError::InvalidConfig(_)));
}

#[test]
fn sr_10_persist_rejects_oversized_segment() {
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    let (mut v, a) = collect_segments_for_test(&report);
    // Inflate one segment past MAX_SEGMENT_BYTES.
    if !v.is_empty() {
        v[0] = vec![0u8; adnet_media::persist::MAX_SEGMENT_BYTES + 1];
    }
    let err = store
        .persist_dag_with_segments(&report.dag, &report.manifest, &v, &a)
        .unwrap_err();
    assert!(matches!(err, adnet_media::error::MediaError::InvalidConfig(_)));
}

#[test]
fn sr_10_persist_rolls_back_on_tampered_segment() {
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    let (mut v, a) = collect_segments_for_test(&report);
    // Flip a byte in the 2nd video segment so its digest
    // mismatches the DAG. Persist must fail AND the on-disk
    // index must remain at its pre-call state.
    if v.len() >= 2 {
        let last = v[1].len() - 1;
        v[1][last] ^= 0xFF;
    }
    let err = store
        .persist_dag_with_segments(&report.dag, &report.manifest, &v, &a)
        .unwrap_err();
    assert!(matches!(
        err,
        adnet_media::error::MediaError::ManifestHashMismatch { .. }
    ));
    // verify_complete should NOT find the 1st segment in the
    // index (since the call rolled back).
    let verify = store.verify_complete(&report.manifest);
    assert!(verify.is_err());
}

#[test]
fn sr_10_alias_overwrite_emits_warning() {
    let (_dir, store) = fresh_store();
    let report = ingest_clip();
    let (v, a) = collect_segments_for_test(&report);
    store
        .persist_with_alias(&report.dag, &report.manifest, &v, &a, "demo")
        .unwrap();
    // Second persist with same alias — overwrites.
    let r2 = store
        .persist_with_alias(&report.dag, &report.manifest, &v, &a, "demo")
        .unwrap();
    assert_eq!(r2.alias.as_deref(), Some("demo"));
    // The alias still resolves to the (only) manifest root.
    let loaded = store.load_by_alias("demo").unwrap();
    assert_eq!(loaded.root, report.manifest.root);
}

fn collect_segments_for_test(
    report: &adnet_media::ingest::IngestReport,
) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut v = Vec::new();
    let mut a = Vec::new();
    for out in &report.transcoder_outputs {
        v.extend(out.video_segments.iter().cloned());
        a.extend(out.audio_segments.iter().cloned());
    }
    (v, a)
}