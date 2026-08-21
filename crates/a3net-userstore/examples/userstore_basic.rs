//! Minimal example: open a SQLite-backed user store, insert a
//! user profile with preferences, attach a public key, and resolve
//! the canonical 12-digit ID.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-userstore --example userstore_basic
//! ```

use a3net_userstore::{
    AvatarBlob, DeviceClass, PublicKeyAlgorithm, SqliteUserStore, SqliteUserStoreConfig,
    UserDevice, UserPreferences, UserProfile, UserPublicKey, UserStore,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = SqliteUserStore::open(SqliteUserStoreConfig::new(
        dir.path().join("users.db"),
    ))?;

    // 1. Insert a profile with avatar + preferences.
    let mut alice = UserProfile::new("alice", "alice");
    alice.display_name = "Alice".into();
    alice.bio = "test user".into();
    alice.avatar = Some(AvatarBlob::new(
        "blake3:0123456789abcdef",
        "image/png",
        4096,
    ));
    alice.preferences = UserPreferences {
        theme: "dark".into(),
        locale: "zh-CN".into(),
        notifications_enabled: true,
        read_receipts_enabled: false,
        typing_indicators_enabled: true,
        experimental_json: "{}".into(),
    };
    store.put_profile(alice)?;

    let got = store.get_profile("alice")?.expect("alice");
    println!(
        "alice: display={} bio={} theme={}",
        got.display_name, got.bio, got.preferences.theme
    );
    assert_eq!(got.display_name, "Alice");
    assert!(got.preferences.notifications_enabled);

    // 2. Patch only preferences (preserves the rest of the row).
    let new_prefs = UserPreferences {
        theme: "light".into(),
        ..got.preferences.clone()
    };
    store.put_preferences("alice", new_prefs)?;
    let after = store.get_profile("alice")?.expect("alice");
    println!("alice theme after patch: {}", after.preferences.theme);
    assert_eq!(after.preferences.theme, "light");
    assert_eq!(after.bio, "test user");

    // 3. Bind an Ed25519 public key.
    let key = UserPublicKey {
        key_id: "alice#ed25519#1".into(),
        user_id: "alice".into(),
        algorithm: PublicKeyAlgorithm::Ed25519.as_str().to_string(),
        key_material: "MCowBQYDK2VwAyEARAMPLEKEY".into(),
        label: "primary".into(),
        created_at: 0,
        revoked_at: None,
    };
    store.put_public_key(key)?;
    let keys = store.list_public_keys("alice")?;
    println!("alice has {} public key(s)", keys.len());
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].parsed_algorithm(), PublicKeyAlgorithm::Ed25519);

    // 4. Register a paired device.
    let device = UserDevice {
        device_id: "alice-macbook".into(),
        user_id: "alice".into(),
        node_id: "node-alice".into(),
        pairing_id: Some("pairing-123".into()),
        device_class: DeviceClass::Desktop.as_str().to_string(),
        label: "Alice's MacBook".into(),
        paired_at: 0,
        revoked_at: None,
    };
    store.put_device(device)?;
    let devices = store.list_devices("alice")?;
    println!("alice has {} device(s)", devices.len());
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].parsed_class(), DeviceClass::Desktop);

    // 5. Derive the canonical 12-digit Exodus ID.
    let digit = store.ensure_user_digit("alice")?;
    println!("alice 12-digit id: {digit}");
    assert_eq!(digit.len(), 12);
    let again = store.resolve_user_digit("alice")?.expect("digit");
    assert_eq!(again, digit);
    println!("ok");
    Ok(())
}