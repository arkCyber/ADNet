//! Access Control List (ACL) implementation for ADNet.
//!
//! Provides fine-grained permission management with support for:
//! - Subject-based access control (users, roles, groups)
//! - Resource-based permissions (blobs, nodes, channels)
//! - Hierarchical permission inheritance
//! - Time-based access rules
//! - Geo-based restrictions (future)

use chrono::{DateTime, Duration, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::error::{SecurityError, SecurityResult};

/// Represents a permission in the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Read access to a resource
    Read,
    /// Write access to a resource
    Write,
    /// Delete access to a resource
    Delete,
    /// Execute access (for certain operations)
    Execute,
    /// Admin access (manage permissions)
    Admin,
    /// Full access (read + write + delete + execute + admin)
    Full,
    /// Share access (share with others)
    Share,
    /// Invite access (invite others)
    Invite,
}

impl Permission {
    /// Check if this permission includes another permission.
    pub fn includes(&self, other: &Permission) -> bool {
        match (self, other) {
            (Permission::Full, _) => true,
            (Permission::Admin, Permission::Admin) => true,
            (Permission::Admin, Permission::Write) => true,
            (Permission::Admin, Permission::Read) => true,
            (Permission::Admin, Permission::Delete) => true,
            (Permission::Admin, Permission::Execute) => true,
            (Permission::Write, Permission::Write) => true,
            (Permission::Write, Permission::Read) => true,
            (Permission::Read, Permission::Read) => true,
            (Permission::Share, Permission::Share) => true,
            (Permission::Share, Permission::Read) => true,
            (Permission::Invite, Permission::Invite) => true,
            _ => false,
        }
    }

    /// Get all permissions implied by this permission.
    pub fn implied_permissions(&self) -> Vec<Permission> {
        match self {
            Permission::Full => vec![
                Permission::Read,
                Permission::Write,
                Permission::Delete,
                Permission::Execute,
                Permission::Admin,
                Permission::Share,
                Permission::Invite,
            ],
            Permission::Admin => vec![
                Permission::Read,
                Permission::Write,
                Permission::Delete,
                Permission::Execute,
                Permission::Admin,
            ],
            Permission::Write => vec![Permission::Read, Permission::Write],
            Permission::Share => vec![Permission::Read, Permission::Share],
            _ => vec![*self],
        }
    }
}

/// Represents the type of resource being accessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceType {
    Blob,
    Channel,
    Node,
    Network,
    ChannelMessage,
    UserProfile,
    Roster,
    Workspace,
    Relay,
    Gateway,
    System,
}

impl ResourceType {
    /// Get the default permissions for this resource type.
    pub fn default_permissions(&self) -> Vec<Permission> {
        match self {
            ResourceType::Blob => vec![Permission::Read, Permission::Write, Permission::Share],
            ResourceType::Channel => {
                vec![Permission::Read, Permission::Write, Permission::Share, Permission::Invite]
            }
            ResourceType::Node => vec![Permission::Read, Permission::Write],
            ResourceType::Network => vec![Permission::Read],
            ResourceType::ChannelMessage => {
                vec![Permission::Read, Permission::Write, Permission::Delete]
            }
            ResourceType::UserProfile => {
                vec![Permission::Read, Permission::Write, Permission::Delete]
            }
            ResourceType::Roster => vec![Permission::Read, Permission::Write, Permission::Delete],
            ResourceType::Workspace => vec![Permission::Full],
            ResourceType::Relay => vec![Permission::Read],
            ResourceType::Gateway => vec![Permission::Read, Permission::Write],
            ResourceType::System => vec![Permission::Admin],
        }
    }
}

/// Represents a resource in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    pub resource_type: ResourceType,
    pub id: String,
    pub owner: Option<String>,
    pub parent: Option<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Resource {
    /// Create a new resource.
    pub fn new(resource_type: ResourceType, id: String) -> Self {
        Self {
            resource_type,
            id,
            owner: None,
            parent: None,
            metadata: HashMap::new(),
        }
    }

    /// Set the owner of this resource.
    pub fn with_owner(mut self, owner: String) -> Self {
        self.owner = Some(owner);
        self
    }

    /// Set the parent resource.
    pub fn with_parent(mut self, parent: String) -> Self {
        self.parent = Some(parent);
        self
    }
}

/// Represents the type of subject that can be granted access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectType {
    User,
    Role,
    Group,
    Node,
    Service,
    Public,
}

impl SubjectType {
    /// Check if this subject type is authenticated.
    pub fn requires_auth(&self) -> bool {
        !matches!(self, SubjectType::Public)
    }
}

/// Represents a subject (who is requesting access).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub subject_type: SubjectType,
    pub id: String,
    pub roles: Vec<String>,
    pub groups: Vec<String>,
    pub node_id: Option<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Subject {
    /// Create a new subject.
    pub fn new_user(id: String) -> Self {
        Self {
            subject_type: SubjectType::User,
            id,
            roles: Vec::new(),
            groups: Vec::new(),
            node_id: None,
            metadata: HashMap::new(),
        }
    }

    /// Create a new node subject.
    pub fn new_node(node_id: String) -> Self {
        Self {
            subject_type: SubjectType::Node,
            id: node_id.clone(),
            roles: Vec::new(),
            groups: Vec::new(),
            node_id: Some(node_id),
            metadata: HashMap::new(),
        }
    }

    /// Create a public (anonymous) subject.
    pub fn public() -> Self {
        Self {
            subject_type: SubjectType::Public,
            id: "public".to_string(),
            roles: Vec::new(),
            groups: Vec::new(),
            node_id: None,
            metadata: HashMap::new(),
        }
    }

    /// Add a role to this subject.
    pub fn with_role(mut self, role: String) -> Self {
        self.roles.push(role);
        self
    }

    /// Add a group to this subject.
    pub fn with_group(mut self, group: String) -> Self {
        self.groups.push(group);
        self
    }
}

/// Access level determines how permissions are evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessLevel {
    /// Deny all access by default
    DenyAll,
    /// Allow only explicitly granted access
    AllowList,
    /// Allow all access by default
    AllowAll,
    /// Custom policy
    Custom,
}

impl Default for AccessLevel {
    fn default() -> Self {
        AccessLevel::AllowList
    }
}

/// Represents a single ACL entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AclEntry {
    pub id: String,
    pub subject: Subject,
    pub resource: Resource,
    pub permissions: Vec<Permission>,
    pub access_level: AccessLevel,
    pub grant: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub conditions: Vec<AccessCondition>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AclEntry {
    /// Create a new ACL entry.
    pub fn new(
        subject: Subject,
        resource: Resource,
        permissions: Vec<Permission>,
        grant: bool,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            subject,
            resource,
            permissions,
            access_level: AccessLevel::AllowList,
            grant,
            expires_at: None,
            conditions: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Set expiration for this entry.
    pub fn expires_at(mut self, expires_at: DateTime<Utc>) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Set expiration duration from now.
    pub fn expires_in(mut self, duration: Duration) -> Self {
        self.expires_at = Some(Utc::now() + duration);
        self
    }

    /// Add a condition to this entry.
    pub fn with_condition(mut self, condition: AccessCondition) -> Self {
        self.conditions.push(condition);
        self
    }

    /// Check if this entry is still valid.
    pub fn is_valid(&self) -> bool {
        if let Some(expires) = self.expires_at {
            if expires < Utc::now() {
                return false;
            }
        }
        self.conditions.iter().all(|c| c.evaluate(&self.subject, &self.resource))
    }

    /// Check if this entry grants the required permission.
    pub fn grants_permission(&self, permission: &Permission) -> bool {
        if !self.is_valid() {
            return false;
        }
        self.grant && self.permissions.iter().any(|p| p.includes(permission))
    }
}

/// Conditions that can be checked for access.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AccessCondition {
    /// Time-based restriction
    TimeRange {
        start_hour: u8,
        end_hour: u8,
        timezone: Option<String>,
    },
    /// IP-based restriction
    IpRange {
        cidr: String,
    },
    /// Device trust level
    MinTrustLevel {
        level: String,
    },
    /// Geographic restriction
    GeoLocation {
        allowed_countries: Vec<String>,
    },
    /// Custom metadata check
    MetadataCheck {
        key: String,
        operator: String,
        value: serde_json::Value,
    },
}

impl AccessCondition {
    /// Evaluate this condition.
    pub fn evaluate(&self, subject: &Subject, _resource: &Resource) -> bool {
        match self {
            AccessCondition::TimeRange {
                start_hour,
                end_hour,
                timezone: _,
            } => {
                let hour = Utc::now().naive_utc().hour() as u8;
                if start_hour <= end_hour {
                    hour >= *start_hour && hour <= *end_hour
                } else {
                    hour >= *start_hour || hour <= *end_hour
                }
            }
            AccessCondition::IpRange { cidr: _ } => {
                // Would need actual IP checking - placeholder
                true
            }
            AccessCondition::MinTrustLevel { level: _ } => {
                // Would need trust level checking - placeholder
                true
            }
            AccessCondition::GeoLocation {
                allowed_countries: _,
            } => {
                // Would need geo location - placeholder
                true
            }
            AccessCondition::MetadataCheck { .. } => {
                // Would need metadata checking - placeholder
                true
            }
        }
    }
}

/// ACL policy combining multiple entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AclPolicy {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub default_access_level: AccessLevel,
    pub entries: Vec<AclEntry>,
    pub inherits_from: Vec<String>,
    pub priority: i32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AclPolicy {
    /// Create a new ACL policy.
    pub fn new(name: String) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            description: None,
            default_access_level: AccessLevel::AllowList,
            entries: Vec::new(),
            inherits_from: Vec::new(),
            priority: 0,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    /// Add an entry to this policy.
    pub fn add_entry(&mut self, entry: AclEntry) {
        self.updated_at = Utc::now();
        self.entries.push(entry);
    }

    /// Check access for a subject and resource.
    pub fn check_access(
        &self,
        subject: &Subject,
        resource: &Resource,
        permission: &Permission,
    ) -> bool {
        if !self.enabled {
            return matches!(self.default_access_level, AccessLevel::AllowAll);
        }

        // Find matching entries for this subject and resource
        let matching_entries: Vec<&AclEntry> = self
            .entries
            .iter()
            .filter(|e| {
                self.subject_matches(&e.subject, subject)
                    && self.resource_matches(&e.resource, resource)
                    && e.is_valid()
            })
            .collect();

        // If no matching entries, use default
        if matching_entries.is_empty() {
            return matches!(self.default_access_level, AccessLevel::AllowAll);
        }

        // Evaluate permissions (highest priority wins)
        let mut granted = matches!(self.default_access_level, AccessLevel::AllowAll);

        for entry in matching_entries {
            if entry.grants_permission(permission) {
                return true;
            }
            if entry.grant {
                for p in &entry.permissions {
                    if p.includes(permission) {
                        return true;
                    }
                }
            } else {
                for p in &entry.permissions {
                    if p.includes(permission) {
                        return false;
                    }
                }
            }
        }

        granted
    }

    /// Check if a subject matches the entry's subject criteria.
    fn subject_matches(&self, entry_subject: &Subject, request_subject: &Subject) -> bool {
        // Check direct match
        if entry_subject.subject_type == request_subject.subject_type
            && entry_subject.id == request_subject.id
        {
            return true;
        }

        // Check role match
        for role in &request_subject.roles {
            if entry_subject.roles.contains(role) {
                return true;
            }
        }

        // Check group match
        for group in &request_subject.groups {
            if entry_subject.groups.contains(group) {
                return true;
            }
        }

        // Check public access
        if entry_subject.subject_type == SubjectType::Public {
            return true;
        }

        // Check node match
        if entry_subject.subject_type == SubjectType::Node {
            if let (Some(entry_node), Some(req_node)) =
                (&entry_subject.node_id, &request_subject.node_id)
            {
                if entry_node == req_node {
                    return true;
                }
            }
        }

        false
    }

    /// Check if a resource matches the entry's resource criteria.
    fn resource_matches(&self, entry_resource: &Resource, request_resource: &Resource) -> bool {
        // Check exact match
        if entry_resource.resource_type == request_resource.resource_type
            && entry_resource.id == request_resource.id
        {
            return true;
        }

        // Check type match with wildcard ID
        if entry_resource.resource_type == request_resource.resource_type
            && entry_resource.id == "*"
        {
            return true;
        }

        // Check owner match
        if entry_resource.id == "*"
            && entry_resource.resource_type == request_resource.resource_type
        {
            if let (Some(owner), Some(req_owner)) =
                (&entry_resource.owner, &request_resource.owner)
            {
                if owner == req_owner {
                    return true;
                }
            }
        }

        false
    }
}

/// Configuration for the ACL system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AclConfig {
    pub default_policy: AccessLevel,
    pub cache_ttl_secs: u64,
    pub max_entries_per_policy: usize,
    pub enable_audit_logging: bool,
    pub cache_enabled: bool,
}

impl Default for AclConfig {
    fn default() -> Self {
        Self {
            default_policy: AccessLevel::AllowList,
            cache_ttl_secs: 300,
            max_entries_per_policy: 10000,
            enable_audit_logging: true,
            cache_enabled: true,
        }
    }
}

/// Result of an access check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessDecision {
    pub granted: bool,
    pub reason: String,
    pub matched_entry: Option<String>,
    pub evaluated_at: DateTime<Utc>,
    pub policy_id: String,
}

impl AccessDecision {
    /// Create a granted decision.
    pub fn granted(policy_id: String, reason: &str, entry_id: Option<String>) -> Self {
        Self {
            granted: true,
            reason: reason.to_string(),
            matched_entry: entry_id,
            evaluated_at: Utc::now(),
            policy_id,
        }
    }

    /// Create a denied decision.
    pub fn denied(policy_id: String, reason: &str) -> Self {
        Self {
            granted: false,
            reason: reason.to_string(),
            matched_entry: None,
            evaluated_at: Utc::now(),
            policy_id,
        }
    }
}

/// Main access control system.
#[derive(Debug)]
pub struct AccessControl {
    policies: Arc<RwLock<HashMap<String, AclPolicy>>>,
    default_policy: Arc<RwLock<Option<String>>>,
    config: AclConfig,
}

impl AccessControl {
    /// Create a new access control system.
    pub fn new(config: AclConfig) -> Self {
        Self {
            policies: Arc::new(RwLock::new(HashMap::new())),
            default_policy: Arc::new(RwLock::new(None)),
            config,
        }
    }

    /// Create with default configuration.
    pub fn default_config() -> Self {
        Self::new(AclConfig::default())
    }

    /// Add a policy.
    pub async fn add_policy(&self, policy: AclPolicy) -> SecurityResult<()> {
        let mut policies = self.policies.write().await;

        if policies.len() >= self.config.max_entries_per_policy {
            return Err(SecurityError::Internal {
                reason: "Maximum policy count reached".to_string(),
            });
        }

        policies.insert(policy.id.clone(), policy);
        Ok(())
    }

    /// Set the default policy.
    pub async fn set_default_policy(&self, policy_id: String) -> SecurityResult<()> {
        let policies = self.policies.read().await;
        if !policies.contains_key(&policy_id) {
            return Err(SecurityError::InvalidConfig {
                reason: format!("Policy {} not found", policy_id),
            });
        }
        drop(policies);

        let mut default = self.default_policy.write().await;
        *default = Some(policy_id);
        Ok(())
    }

    /// Check access for a subject to a resource.
    pub async fn check_access(
        &self,
        subject: &Subject,
        resource: &Resource,
        permission: &Permission,
    ) -> AccessDecision {
        let policy_id = {
            let default = self.default_policy.read().await;
            default.clone()
        };

        let policy_id = match policy_id {
            Some(id) => id,
            None => return AccessDecision::denied("none".to_string(), "No default policy set"),
        };

        let policies = self.policies.read().await;
        let policy = match policies.get(&policy_id) {
            Some(p) => p,
            None => return AccessDecision::denied("none".to_string(), "Default policy not found"),
        };

        // Check ownership
        if let (Some(owner), Some(resource_owner)) = (&resource.owner, &resource.owner) {
            if subject.id == *owner {
                return AccessDecision::granted(
                    policy_id.clone(),
                    "Subject is resource owner",
                    None,
                );
            }
        }

        // Find matching entry
        for entry in &policy.entries {
            if policy.subject_matches(&entry.subject, subject)
                && policy.resource_matches(&entry.resource, resource)
                && entry.is_valid()
            {
                if entry.grants_permission(permission) {
                    return AccessDecision::granted(
                        policy_id.clone(),
                        "Permission granted by ACL entry",
                        Some(entry.id.clone()),
                    );
                }
            }
        }

        // Check default access level
        let granted = matches!(policy.default_access_level, AccessLevel::AllowAll);

        if granted {
            AccessDecision::granted(policy_id.clone(), "Allowed by default access level", None)
        } else {
            AccessDecision::denied(policy_id.clone(), "Denied by default access level")
        }
    }

    /// Check if subject can perform action (convenience method).
    pub async fn can(
        &self,
        subject: &Subject,
        resource: &Resource,
        permission: Permission,
    ) -> bool {
        self.check_access(subject, resource, &permission)
            .await
            .granted
    }

    /// Get all policies.
    pub async fn list_policies(&self) -> Vec<AclPolicy> {
        let policies = self.policies.read().await;
        policies.values().cloned().collect()
    }

    /// Remove a policy.
    pub async fn remove_policy(&self, policy_id: &str) -> SecurityResult<()> {
        let mut policies = self.policies.write().await;
        policies.remove(policy_id);

        let mut default = self.default_policy.write().await;
        if *default == Some(policy_id.to_string()) {
            *default = None;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_basic_access_control() {
        let acl = AccessControl::default_config();

        // Create a policy
        let mut policy = AclPolicy::new("test-policy".to_string());
        policy.default_access_level = AccessLevel::DenyAll;

        // Add an entry granting read access
        let entry = AclEntry::new(
            Subject::new_user("alice".to_string()),
            Resource::new(ResourceType::Blob, "*".to_string()),
            vec![Permission::Read],
            true,
        );
        policy.add_entry(entry);

        acl.add_policy(policy.clone()).await.unwrap();
        acl.set_default_policy(policy.id.clone()).await.unwrap();

        // Alice should be able to read blobs
        let can_read = acl
            .can(
                &Subject::new_user("alice".to_string()),
                &Resource::new(ResourceType::Blob, "blob-123".to_string()),
                Permission::Read,
            )
            .await;
        assert!(can_read);

        // Alice should not be able to write blobs (only read)
        let can_write = acl
            .can(
                &Subject::new_user("alice".to_string()),
                &Resource::new(ResourceType::Blob, "blob-123".to_string()),
                Permission::Write,
            )
            .await;
        assert!(!can_write);

        // Bob should not have any access
        let can_read_bob = acl
            .can(
                &Subject::new_user("bob".to_string()),
                &Resource::new(ResourceType::Blob, "blob-123".to_string()),
                Permission::Read,
            )
            .await;
        assert!(!can_read_bob);
    }

    #[tokio::test]
    async fn test_owner_access() {
        let acl = AccessControl::default_config();

        let mut policy = AclPolicy::new("test-policy".to_string());
        policy.default_access_level = AccessLevel::DenyAll;
        acl.add_policy(policy.clone()).await.unwrap();
        acl.set_default_policy(policy.id.clone()).await.unwrap();

        // Owner should have full access
        let resource = Resource::new(ResourceType::Blob, "blob-123".to_string())
            .with_owner("alice".to_string());

        let can_read = acl
            .can(
                &Subject::new_user("alice".to_string()),
                &resource,
                Permission::Read,
            )
            .await;
        assert!(can_read);

        let can_admin = acl
            .can(
                &Subject::new_user("alice".to_string()),
                &resource,
                Permission::Admin,
            )
            .await;
        assert!(can_admin);
    }

    #[tokio::test]
    async fn test_role_based_access() {
        let acl = AccessControl::default_config();

        let mut policy = AclPolicy::new("test-policy".to_string());
        policy.default_access_level = AccessLevel::DenyAll;

        // Admin role can do anything
        let admin_entry = AclEntry::new(
            Subject::new_user("admin".to_string()).with_role("admin".to_string()),
            Resource::new(ResourceType::System, "*".to_string()),
            vec![Permission::Full],
            true,
        );
        policy.add_entry(admin_entry);

        acl.add_policy(policy.clone()).await.unwrap();
        acl.set_default_policy(policy.id.clone()).await.unwrap();

        // User with admin role should have access
        let subject = Subject::new_user("charlie".to_string()).with_role("admin".to_string());
        let can_admin = acl
            .can(&subject, &Resource::new(ResourceType::System, "anything".to_string()), Permission::Admin)
            .await;
        assert!(can_admin);
    }

    #[test]
    fn test_permission_includes() {
        assert!(Permission::Full.includes(&Permission::Read));
        assert!(Permission::Full.includes(&Permission::Write));
        assert!(Permission::Full.includes(&Permission::Admin));
        assert!(Permission::Admin.includes(&Permission::Write));
        assert!(Permission::Write.includes(&Permission::Read));
        assert!(!Permission::Read.includes(&Permission::Write));
    }
}
