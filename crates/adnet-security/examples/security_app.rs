//! Realistic example: a small ADNet "node" combines ACL, audit log,
//! and key rotation. User actions (login, blob write) are checked
//! against the ACL and recorded in the audit log; encryption keys are
//! rotated on demand.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-security --example security_app
//! ```

use chrono::Duration;

use adnet_security::acl::{Permission, Resource, ResourceType, Subject};
use adnet_security::audit::{
    AuditConfig, AuditEventType, AuditLog, AuditOutcome, AuditRecord,
};
use adnet_security::key_management::{KeyRotationPolicy, KeyStore, KeyType};
use adnet_security::{
    AccessControl, AccessLevel, AclEntry, AclPolicy, AuditSeverity,
};

#[tokio::main]
async fn main() {
    // 1. ACL: deny-by-default, with one allow rule for `bob`.
    let acl = AccessControl::default_config();
    let mut policy = AclPolicy::new("default".into());
    policy.default_access_level = AccessLevel::DenyAll;
    policy.add_entry(AclEntry::new(
        Subject::new_user("bob".into()),
        Resource::new(ResourceType::Blob, "*.pdf".into()),
        vec![Permission::Read],
        false,
    ));
    let policy_id = policy.id.clone();
    acl.add_policy(policy).await.expect("add policy");
    acl.set_default_policy(policy_id).await.expect("set default");

    // 2. Audit log: only `Info` and above are kept.
    let audit = AuditLog::new(AuditConfig {
        min_severity: AuditSeverity::Info,
        max_in_memory: 1024,
        ..Default::default()
    });

    // 3. Key store: register one symmetric key with a 24h rotation
    //    policy. The store is in-memory for this demo.
    let store = KeyStore::memory();
    let rotation = KeyRotationPolicy::new(
        "node-encryption".into(),
        KeyType::Symmetric,
        Duration::days(1),
    );
    let key_id = store
        .create_key(
            "node-encryption".into(),
            KeyType::Symmetric,
            b"initial-key-bytes".to_vec(),
            Some(rotation),
        )
        .await
        .expect("create key");
    let v0 = store
        .get_key(&key_id)
        .await
        .expect("get key")
        .active_version()
        .cloned()
        .expect("active version");
    println!(
        "initial key: id={key_id} version={} active={}",
        v0.version, v0.is_active
    );

    // 4. Simulate a few user actions. The ACL is checked first; the
    //    outcome is recorded into the audit log.
    let bob = Subject::new_user("bob".into());
    let mallory = Subject::new_user("mallory".into());
    let report = Resource::new(ResourceType::Blob, "report.pdf".into());

    let bob_can_read = acl.can(&bob, &report, Permission::Read).await;
    audit
        .record(AuditRecord::new(
            AuditEventType::AccessGranted,
            "read report.pdf".into(),
            if bob_can_read {
                AuditOutcome::Success
            } else {
                AuditOutcome::Failure
            },
        ))
        .await;
    println!("bob    -> report.pdf read: {bob_can_read}");

    let mallory_can_read = acl.can(&mallory, &report, Permission::Read).await;
    audit
        .record(AuditRecord::new(
            AuditEventType::AccessDenied,
            "read report.pdf".into(),
            if mallory_can_read {
                AuditOutcome::Success
            } else {
                AuditOutcome::Failure
            },
        ))
        .await;
    println!("mal    -> report.pdf read: {mallory_can_read}");

    let stats = audit.stats().await;
    println!(
        "\naudit stats: total={} by_severity={:?}",
        stats.total_records, stats.by_severity
    );
    assert!(stats.total_records >= 2);
    assert!(bob_can_read);
    assert!(!mallory_can_read);

    // 5. Force a rotation. The store returns the new active key bytes.
    let v1 = store
        .rotate_key(&key_id, b"rotated-key-bytes".to_vec())
        .await
        .expect("rotate key");
    println!(
        "\nrotated key: version={} active={}",
        v1.version, v1.is_active
    );

    let active = store
        .get_active_key_data(&key_id)
        .await
        .expect("active data");
    println!("active key data: {} bytes", active.len());
    assert_eq!(active, b"rotated-key-bytes".to_vec());
}