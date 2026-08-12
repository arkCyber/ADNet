//! Tiny example: stand up a small `AccessControl` with one policy,
//! grant a user read access to a blob, and ask whether the user can
//! read and write.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-security --example security_basic
//! ```

use adnet_security::{
    AccessControl, AccessLevel, AclEntry, AclPolicy, Permission, Resource, ResourceType, Subject,
};

#[tokio::main]
async fn main() {
    let acl = AccessControl::default_config();

    // 1. Build a "deny by default" policy.
    let mut policy = AclPolicy::new("default".into());
    policy.default_access_level = AccessLevel::DenyAll;

    // 2. Grant `alice` read access to any blob.
    let entry = AclEntry::new(
        Subject::new_user("alice".into()),
        Resource::new(ResourceType::Blob, "*".into()),
        vec![Permission::Read],
        true,
    );
    policy.add_entry(entry);
    let policy_id = policy.id.clone();
    acl.add_policy(policy).await.expect("add policy");
    acl.set_default_policy(policy_id).await.expect("set default");

    // 3. Ask: can `alice` read a blob?
    let can_read = acl
        .can(
            &Subject::new_user("alice".into()),
            &Resource::new(ResourceType::Blob, "document.pdf".into()),
            Permission::Read,
        )
        .await;
    println!("alice can read?  {can_read}");
    assert!(can_read);

    // 4. Ask: can `alice` write the same blob?
    let can_write = acl
        .can(
            &Subject::new_user("alice".into()),
            &Resource::new(ResourceType::Blob, "document.pdf".into()),
            Permission::Write,
        )
        .await;
    println!("alice can write? {can_write}");
    assert!(!can_write);

    // 5. Ask: can a stranger read?
    let can_stranger = acl
        .can(
            &Subject::new_user("mallory".into()),
            &Resource::new(ResourceType::Blob, "document.pdf".into()),
            Permission::Read,
        )
        .await;
    println!("mallory can read? {can_stranger}");
    assert!(!can_stranger);

    // 6. Owner of a resource is allowed full access even without an
    //    explicit ACL entry.
    let owned = Resource::new(ResourceType::Blob, "owned.pdf".into())
        .with_owner("alice".into());
    let can_admin = acl
        .can(
            &Subject::new_user("alice".into()),
            &owned,
            Permission::Admin,
        )
        .await;
    println!("alice can admin owned? {can_admin}");
    assert!(can_admin);
}
