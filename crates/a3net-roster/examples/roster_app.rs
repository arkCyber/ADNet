//! Realistic example: end-to-end roster workflow — create a group,
//! insert a human + IoT contact, search by substring, toggle
//! favorite, then resolve both digit-id directions.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-roster --example roster_app
//! ```

use a3net_roster::{
    Contact, ContactGroup, ContactType, DigitMapping, IoTDeviceType, IoTProtocol, IoTStatus,
    RosterStore, SqliteRosterStore, SqliteRosterStoreConfig,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = SqliteRosterStore::open(SqliteRosterStoreConfig::new(
        dir.path().join("roster.db"),
    ))?;

    // 1. Create a group.
    let group = ContactGroup {
        group_id: "home".into(),
        name: "Home".into(),
        description: "people & devices at home".into(),
        color: "teal".into(),
        created_at: 1,
    };
    store.put_group(group).await?;
    println!("group 'home' created");

    // 2. Add a human contact in the group.
    let mut alice = Contact::new_human("alice", "Alice");
    alice.node_id = "node-alice".into();
    alice.groups = vec!["home".into()];
    alice.tags = vec!["vip".into(), "founder".into()];
    alice.notes = "spouse".into();
    store.put_contact(alice).await?;

    // 3. Add an IoT contact (kitchen lamp) in the group.
    let mut lamp = Contact::new_human("lamp-1", "Kitchen Lamp");
    lamp.contact_type = ContactType::Iot.as_str().to_string();
    lamp.iot_device_type = Some(IoTDeviceType::SmartLight.as_str().into());
    lamp.iot_protocol = Some(IoTProtocol::Matter.as_str().into());
    lamp.iot_status = Some(IoTStatus::Online.as_str().into());
    lamp.iot_capabilities = Some(vec!["on_off".into(), "dimming".into()]);
    lamp.iot_location = Some("kitchen".into());
    lamp.groups = vec!["home".into()];
    store.put_contact(lamp).await?;

    // 4. Search contacts by substring.
    let alice_hits = store.search_contacts("alice").await?;
    println!("search 'alice' returned {} hit(s)", alice_hits.len());
    assert_eq!(alice_hits.len(), 1);
    assert_eq!(alice_hits[0].contact_id, "alice");

    let iot_hits = store.search_contacts("kitchen").await?;
    println!("search 'kitchen' returned {} hit(s)", iot_hits.len());
    assert_eq!(iot_hits.len(), 1);
    assert!(iot_hits[0].is_iot_online());

    // 5. Toggle favorite for Alice (false → true → false).
    let fav0 = store.toggle_favorite("alice").await?;
    let fav1 = store.toggle_favorite("alice").await?;
    println!("alice favorite: {fav0:?} -> {fav1:?}");
    assert_eq!(fav0, Some(false));
    assert_eq!(fav1, Some(true));

    // 6. Register digit mappings for both contacts and resolve in both directions.
    store
        .put_digit_mapping(DigitMapping::new("111111111111", "node-alice"))
        .await?;
    store
        .put_digit_mapping(DigitMapping::new("222222222222", "node-lamp"))
        .await?;

    let node_for_lamp = store
        .resolve_digit_to_node("222222222222")
        .await?
        .expect("digit 222222222222");
    assert_eq!(node_for_lamp, "node-lamp");
    let digit_for_alice = store
        .resolve_node_to_digit("node-alice")
        .await?
        .expect("digit for alice");
    assert_eq!(digit_for_alice, "111111111111");
    println!("mappings: lamp={node_for_lamp} alice_digit={digit_for_alice}");

    let info = store.info();
    println!(
        "roster: backend={} contacts={} groups={} digit_mappings={}",
        info.backend, info.contact_count, info.group_count, info.digit_mapping_count
    );
    assert_eq!(info.contact_count, 2);
    assert_eq!(info.group_count, 1);
    assert_eq!(info.digit_mapping_count, 2);
    println!("ok");
    Ok(())
}