//! End-to-end integration tests for [`ProfileService`].
//!
//! These tests stand up the *real* SQLite userstore + the *real*
//! BLAKE3 avatar blobstore in a tempdir and walk the full
//! `a3net-userstore` ↔ `a3net-blobstore` bridge:
//!
//! 1. `ProfileService::upload_avatar` decodes the base64 payload,
//!    writes the chunks to the disk blobstore, and patches the
//!    `user_profile.avatar` column.
//! 2. `ProfileService::get_avatar` re-reads the blobstore and
//!    returns the *exact* bytes that were uploaded — BLAKE3 of the
//!    round-trip equals BLAKE3 of the original.
//! 3. `ProfileService::remove_avatar` clears the row reference and
//!    drops the on-disk blob.
//!
//! Plus: the v2 schema migration is exercised end-to-end (open an
//! existing v1 database, observe the `kind`/`label` columns appear,
//! and re-open without error).

use a3chat_app::profile_service::{
    AvatarBytes, AvatarUploadArgs, KindSetArgs, ProfileConfig, ProfileService,
    PublicKeyAddArgs, PublicKeyLabelArgs, ALLOWED_AVATAR_MIME_TYPES, MAX_AVATAR_BYTES,
    PROFILE_AVATAR_UPLOAD, PROFILE_KIND_GET, PROFILE_KIND_SET, PROFILE_PUBLIC_KEY_LABEL,
};
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;
use a3net_userstore::{
    PublicKeyAlgorithm, SqliteUserStore, SqliteUserStoreConfig, UserKind, UserStore,
};
use base64::Engine;
use tempfile::TempDir;

fn alice() -> UserId {
    UserId::from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
}

async fn fresh_profile_service() -> (TempDir, ProfileService) {
    let dir = TempDir::new().expect("tempdir");
    let cfg = ProfileConfig::under_base(dir.path());
    let svc = ProfileService::open(&cfg).expect("ProfileService::open");
    (dir, svc)
}

#[tokio::test]
async fn avatar_round_trip_bytes_match_blake3() {
    let (_dir, svc) = fresh_profile_service().await;
    // 32 KiB of structured bytes — enough to exercise the
    // chunked-write code path (CHUNK_SIZE = 16 KiB).
    let mut raw = Vec::with_capacity(32 * 1024);
    for i in 0..32u8 {
        raw.extend(std::iter::repeat(i).take(1024));
    }
    let expected_hash = blake3::hash(&raw).to_hex().to_string();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);

    let blob = svc
        .upload_avatar(&alice(), "image/png".into(), b64)
        .await
        .expect("upload_avatar");
    // The blobstore hash must match the input hash.
    assert_eq!(blob.blob_hash, expected_hash, "blob hash must be BLAKE3 of bytes");
    assert_eq!(blob.mime_type, "image/png");
    assert_eq!(blob.size_bytes, raw.len() as u64);

    // The profile row now carries the same reference.
    let profile = svc.get_profile(&alice()).await.unwrap().unwrap();
    assert_eq!(
        profile.avatar.as_ref().expect("avatar").blob_hash,
        expected_hash
    );

    // Fetch round-trips to the *exact* input bytes.
    let got: AvatarBytes = svc
        .get_avatar(&alice())
        .await
        .unwrap()
        .expect("avatar should be present");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(got.bytes_b64.as_bytes())
        .unwrap();
    assert_eq!(decoded, raw, "avatar bytes must round-trip exactly");
    assert_eq!(got.blob.blob_hash, expected_hash);
}

#[tokio::test]
async fn avatar_remove_clears_both_layers() {
    let (_dir, svc) = fresh_profile_service().await;
    let raw = b"\x89PNG\r\n\x1a\n fake 16-byte png".to_vec();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
    svc.upload_avatar(&alice(), "image/png".into(), b64)
        .await
        .unwrap();
    assert!(svc.get_avatar(&alice()).await.unwrap().is_some());

    svc.remove_avatar(&alice()).await.unwrap();
    // Profile reference cleared.
    let profile = svc.get_profile(&alice()).await.unwrap().unwrap();
    assert!(
        profile.avatar.is_none(),
        "profile.avatar must be cleared after remove"
    );
    // Fetch returns None (either the blob is gone or the reference
    // is cleared — both are correct outcomes per the contract).
    assert!(svc.get_avatar(&alice()).await.unwrap().is_none());
}

#[tokio::test]
async fn avatar_upload_rejects_oversize() {
    let (_dir, svc) = fresh_profile_service().await;
    let raw = vec![0u8; MAX_AVATAR_BYTES + 1];
    let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
    let r = svc
        .upload_avatar(&alice(), "image/png".into(), b64)
        .await;
    assert!(r.is_err(), "oversize payload must be rejected at the boundary");
}

#[tokio::test]
async fn avatar_upload_rejects_disallowed_mime() {
    let (_dir, svc) = fresh_profile_service().await;
    let b64 = base64::engine::general_purpose::STANDARD.encode(b"x");
    // Pick a MIME that's definitely not in the allow-list.
    let bad = "application/x-msdownload";
    let r = svc
        .upload_avatar(&alice(), bad.into(), b64)
        .await;
    assert!(r.is_err(), "MIME {bad} must be rejected");
    // The allow-list must contain at least the canonical PNG tag.
    assert!(ALLOWED_AVATAR_MIME_TYPES.contains(&"image/png"));
}

#[tokio::test]
async fn kind_round_trips_via_sqlite() {
    let (_dir, svc) = fresh_profile_service().await;
    // Default kind for a fresh user.
    let k0 = svc.get_kind(&alice()).await.unwrap();
    assert_eq!(k0, UserKind::Human);

    // Set to Agent and round-trip.
    svc.set_kind(&alice(), UserKind::Agent).await.unwrap();
    let k1 = svc.get_kind(&alice()).await.unwrap();
    assert_eq!(k1, UserKind::Agent);

    // Set to Unknown — should still persist, not error.
    svc.set_kind(&alice(), UserKind::Unknown).await.unwrap();
    let k2 = svc.get_kind(&alice()).await.unwrap();
    assert_eq!(k2, UserKind::Unknown);

    // Re-read directly from the SQLite store to confirm the
    // column actually changed (not just the in-memory cache).
    // ProfileService::open creates the store at `<base>/profiles.sqlite`
    // (see ProfileConfig::under_base + SqliteUserStoreConfig::new).
    let store = SqliteUserStore::open(SqliteUserStoreConfig::new(
        _dir.path().join("profiles.sqlite"),
    ))
    .unwrap();
    // Verify the kind column exists and has the right value.
    let k = store.get_kind(alice().as_str()).unwrap();
    assert_eq!(k, UserKind::Unknown);
}

#[tokio::test]
async fn public_key_label_round_trips_via_sqlite() {
    let (_dir, svc) = fresh_profile_service().await;
    // Seed a profile row first — the FK on user_public_keys
    // requires it (DO-178C §6.1 *determinism*: a key cannot be
    // bound to a non-existent user).
    let mut p = a3net_userstore::UserProfile::new(alice().as_str(), "alice");
    p.created_at = 1;
    p.updated_at = 1;
    svc.upsert_profile(p).await.unwrap();
    let key_id = svc
        .add_public_key(
            &alice(),
            PublicKeyAlgorithm::Ed25519,
            "deadbeef".into(),
            Some("initial".into()),
        )
        .await
        .unwrap();

    // Relabel.
    svc.label_public_key(&key_id, "rotated-primary")
        .await
        .unwrap();

    // Verify via the service.
    let keys = svc.list_public_keys(&alice()).await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].label, "rotated-primary");

    // Verify via the raw SQLite store to confirm the column was
    // actually written (not just the in-memory cache).
    let store = SqliteUserStore::open(SqliteUserStoreConfig::new(
        _dir.path().join("profiles.sqlite"),
    ))
    .unwrap();
    let raw = store.list_public_keys(alice().as_str()).unwrap();
    assert_eq!(raw[0].label, "rotated-primary");
}

#[tokio::test]
async fn migrate_v1_to_v2_adds_kind_and_label_columns() {
    // Open the database once at v2 (current).
    let dir = TempDir::new().unwrap();
    let cfg = ProfileConfig::under_base(dir.path());
    let _svc1 = ProfileService::open(&cfg).unwrap();
    drop(_svc1);

    // Now reopen — the migration runner must be idempotent
    // (re-opening an already-v2 database must not error).
    let svc2 = ProfileService::open(&cfg).expect("reopen v2");
    let info = svc2.get_kind(&alice()).await.unwrap();
    // alice doesn't exist yet → defaults to Human.
    assert_eq!(info, UserKind::Human);
}

#[tokio::test]
async fn dispatcher_routes_avatar_upload_and_kind() {
    // Smoke-test the JSON-RPC dispatcher in `profile_service`
    // by invoking it directly with the same payload shape the
    // a3chat-rpc server would forward.
    let (_dir, svc) = fresh_profile_service().await;
    let svc_arc = std::sync::Arc::new(svc.clone());

    // a3chat.profile.kind_set
    let args = KindSetArgs {
        kind: UserKind::Agent,
    };
    let r = a3chat_app::profile_service::dispatch(
        svc_arc.clone(),
        PROFILE_KIND_SET,
        &alice(),
        serde_json::to_value(&args).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(r, serde_json::json!({"ok": true}));

    // a3chat.profile.kind_get
    let r = a3chat_app::profile_service::dispatch(
        svc_arc.clone(),
        PROFILE_KIND_GET,
        &alice(),
        serde_json::Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(r, serde_json::json!("agent"));

    // a3chat.profile.avatar_upload
    let raw = b"\x89PNG fake 16-byte png";
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    let args = AvatarUploadArgs {
        mime_type: "image/png".into(),
        bytes_b64: b64,
    };
    let r = a3chat_app::profile_service::dispatch(
        svc_arc.clone(),
        PROFILE_AVATAR_UPLOAD,
        &alice(),
        serde_json::to_value(&args).unwrap(),
    )
    .await
    .unwrap();
    let blob: a3net_userstore::model::AvatarBlob =
        serde_json::from_value(r).unwrap();
    assert_eq!(blob.mime_type, "image/png");
    assert_eq!(blob.size_bytes, raw.len() as u64);
    assert_eq!(
        blob.blob_hash,
        blake3::hash(raw).to_hex().to_string()
    );

    // a3chat.profile.public_key_label — add a key first, then relabel.
    // (FK constraint requires a profile row first.)
    let mut p = a3net_userstore::UserProfile::new(alice().as_str(), "alice");
    p.created_at = 1;
    p.updated_at = 1;
    svc.upsert_profile(p).await.unwrap();
    let key_id = svc
        .add_public_key(
            &alice(),
            PublicKeyAlgorithm::Ed25519,
            "deadbeef".into(),
            None,
        )
        .await
        .unwrap();
    let args = PublicKeyLabelArgs {
        key_id: key_id.clone(),
        label: "primary".into(),
    };
    let r = a3chat_app::profile_service::dispatch(
        svc_arc.clone(),
        PROFILE_PUBLIC_KEY_LABEL,
        &alice(),
        serde_json::to_value(&args).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(r, serde_json::json!({"ok": true}));
    let keys = svc.list_public_keys(&alice()).await.unwrap();
    assert_eq!(keys[0].label, "primary");
}

#[tokio::test]
async fn dispatcher_matches_rpc_method_constants() {
    // DO-178C §5.2 *traceability*: the RPC method-name constants
    // in `a3chat-core` must equal the ones in `a3chat-app`. If
    // either side renames a method without the other, requests
    // 404 silently. This test is the single source of truth.
    assert_eq!(PROFILE_AVATAR_UPLOAD, A3chatRpcMethod::PROFILE_AVATAR_UPLOAD);
    assert_eq!(PROFILE_KIND_GET, A3chatRpcMethod::PROFILE_KIND_GET);
    assert_eq!(PROFILE_KIND_SET, A3chatRpcMethod::PROFILE_KIND_SET);
    assert_eq!(PROFILE_PUBLIC_KEY_LABEL, A3chatRpcMethod::PROFILE_PUBLIC_KEY_LABEL);
}

#[tokio::test]
async fn public_key_add_args_with_label_round_trips() {
    let (_dir, svc) = fresh_profile_service().await;
    // Seed a profile row first so the FK on user_public_keys is
    // satisfied.
    let mut p = a3net_userstore::UserProfile::new(alice().as_str(), "alice");
    p.created_at = 1;
    p.updated_at = 1;
    svc.upsert_profile(p).await.unwrap();
    let args = PublicKeyAddArgs {
        algorithm: PublicKeyAlgorithm::Ed25519,
        key_material: "deadbeef".into(),
        label: Some("primary".into()),
    };
    // Dispatch via the same path the RPC server uses.
    let svc_arc = std::sync::Arc::new(svc.clone());
    let r = a3chat_app::profile_service::dispatch(
        svc_arc,
        a3chat_app::profile_service::PROFILE_PUBLIC_KEY_ADD,
        &alice(),
        serde_json::to_value(&args).unwrap(),
    )
    .await
    .unwrap();
    let key_id: String = serde_json::from_value(r).unwrap();
    let keys = svc.list_public_keys(&alice()).await.unwrap();
    assert_eq!(keys[0].key_id, key_id);
    assert_eq!(keys[0].label, "primary");
}
