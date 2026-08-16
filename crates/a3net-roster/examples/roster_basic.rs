//! Minimal example: open a SQLite-backed roster store, insert a
//! human contact and a digit-mapping, then read them back.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-roster --example roster_basic
//! ```

use a3net_roster::{
    Contact, ContactType, DigitMapping, FriendRequestMode, FriendRequestSetting,
    RosterStore, SqliteRosterStore, SqliteRosterStoreConfig,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = SqliteRosterStore::open(SqliteRosterStoreConfig::new(
        dir.path().join("roster.db"),
    ))?;

    // 1. Insert a human contact.
    let mut alice = Contact::new_human("alice", "Alice");
    alice.node_id = "node-alice".into();
    alice.groups = vec!["friends".into()];
    alice.tags = vec!["vip".into()];
    alice.notes = "met at conference".into();
    alice.is_favorite = true;
    store.put_contact(alice).await?;

    let got = store.get_contact("alice").await?.expect("alice");
    println!(
        "alice: name={} node={} favorite={}",
        got.name, got.node_id, got.is_favorite
    );
    assert_eq!(got.name, "Alice");
    assert!(got.is_favorite);

    // 2. Register a 12-digit mapping so peers can dial Alice directly.
    let mapping = DigitMapping::new("123456789012", "node-alice");
    store.put_digit_mapping(mapping).await?;
    let resolved = store
        .resolve_digit_to_node("123456789012")
        .await?
        .expect("digit -> node");
    println!("digit 123456789012 -> {resolved}");
    assert_eq!(resolved, "node-alice");

    // 3. Persist a friend-request preference.
    store
        .put_friend_request_setting(FriendRequestSetting::new(
            "alice",
            FriendRequestMode::RequireConfirmation,
        ))
        .await?;
    let setting = store
        .get_friend_request_setting("alice")
        .await?
        .expect("setting");
    println!("alice friend-request mode: {:?}", setting.parsed_mode());

    // 4. Sanity: list contacts and confirm IoT validation rejects a
    //    contact typed as IoT but missing required IoT fields.
    let bad_iot = Contact::new_human("bad-iot", "Bad Lamp");
    let mut bad = bad_iot;
    bad.contact_type = ContactType::Iot.as_str().to_string();
    let err = store.put_contact(bad).await.unwrap_err();
    println!("iot validation rejected: {err}");
    Ok(())
}