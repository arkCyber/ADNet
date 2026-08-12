//! Realistic example: create a profile with a public key + device,
//! revoke the key, register a second device, list remaining keys
//! (excluding revoked), and confirm the canonical 12-digit ID
//! remains stable across re-opens.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-userstore --example userstore_app
//! ```

use adnet_userstore::{
    DeviceClass, PublicKeyAlgorithm, SqliteUserStore, SqliteUserStoreConfig, UserDevice,
    UserPreferences, UserProfile, UserPublicKey, UserStore,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("users.db");

    // Phase 1: write data.
    let store = SqliteUserStore::open(SqliteUserStoreConfig::new(&db_path))?;
    let mut bob = UserProfile::new("bob", "bob");
    bob.display_name = "Bob".into();
    bob.preferences = UserPreferences {
        theme: "auto".into(),
        locale: "en-US".into(),
        notifications_enabled: true,
        read_receipts_enabled: true,
        typing_indicators_enabled: false,
        experimental_json: "{}".into(),
    };
    store.put_profile(bob).await?;

    // First key (will be revoked below).
    store
        .put_public_key(UserPublicKey {
            key_id: "bob#ed25519#1".into(),
            user_id: "bob".into(),
            algorithm: PublicKeyAlgorithm::Ed25519.as_str().to_string(),
            key_material: "MCowBQYDK2VwAyEAKEY1".into(),
            created_at: 1,
            revoked_at: None,
        })
        .await?;

    // Second key (active).
    store
        .put_public_key(UserPublicKey {
            key_id: "bob#ed25519#2".into(),
            user_id: "bob".into(),
            algorithm: PublicKeyAlgorithm::Ed25519.as_str().to_string(),
            key_material: "MCowBQYDK2VwAyEAKEY2".into(),
            created_at: 2,
            revoked_at: None,
        })
        .await?;

    // Two paired devices.
    store
        .put_device(UserDevice {
            device_id: "bob-mac".into(),
            user_id: "bob".into(),
            node_id: "node-bob-mac".into(),
            pairing_id: Some("pair-1".into()),
            device_class: DeviceClass::Desktop.as_str().to_string(),
            label: "Bob's iMac".into(),
            paired_at: 10,
            revoked_at: None,
        })
        .await?;
    store
        .put_device(UserDevice {
            device_id: "bob-phone".into(),
            user_id: "bob".into(),
            node_id: "node-bob-phone".into(),
            pairing_id: None,
            device_class: DeviceClass::Mobile.as_str().to_string(),
            label: "Bob's iPhone".into(),
            paired_at: 20,
            revoked_at: None,
        })
        .await?;

    // Revoke the first key + the Mac device.
    store.revoke_public_key("bob#ed25519#1").await?;
    store.revoke_device("bob-mac").await?;
    let keys = store.list_public_keys("bob").await?;
    let devices = store.list_devices("bob").await?;
    let revoked_keys = keys.iter().filter(|k| k.revoked_at.is_some()).count();
    let active_keys = keys.iter().filter(|k| k.revoked_at.is_none()).count();
    println!(
        "after revoke: keys active={active_keys} revoked={revoked_keys} devices total={}",
        devices.len()
    );
    assert_eq!(active_keys, 1);
    assert_eq!(revoked_keys, 1);
    assert_eq!(devices.len(), 2);

    // Pin the canonical 12-digit ID.
    let digit1 = store.ensure_user_digit("bob").await?;
    println!("bob 12-digit id: {digit1} (len = {})", digit1.len());
    assert_eq!(digit1.len(), 12);

    // Phase 2: re-open and confirm the ID is stable.
    drop(store);
    let reopened = SqliteUserStore::open(SqliteUserStoreConfig::new(&db_path))?;
    let digit2 = reopened.resolve_user_digit("bob").await?.expect("digit");
    println!("bob 12-digit id after re-open: {digit2}");
    assert_eq!(digit1, digit2);

    let info = reopened.info();
    println!(
        "userstore: backend={} profiles={}",
        info.backend, info.profile_count
    );
    assert_eq!(info.profile_count, 1);
    println!("ok");
    Ok(())
}