//! `a3net-security` — Security primitives for A3Net.
//!
//! This crate provides essential security features that are missing from the
//! current A3Net implementation:
//!
//! - **Access Control Lists (ACL)**: Fine-grained permission management
//! - **Session Encryption**: Signal-like double ratchet for E2E encryption
//! - **Intrusion Detection**: Anomaly detection and threat monitoring
//! - **Key Management**: Key rotation and revocation
//! - **Audit Logging**: Compliance-ready security event tracking
//!
//! ## Design Philosophy
//!
//! - Zero-trust security model
//! - Defense in depth with multiple security layers
//! - Compliance-ready with comprehensive audit trails
//! - No unsafe code (`#![forbid(unsafe_code)]`)

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod acl;
pub mod session;
pub mod intrusion;
pub mod key_management;
pub mod audit;
pub mod error;

pub use acl::{
    AccessControl, AccessDecision, AccessLevel, AclConfig, AclEntry, AclPolicy,
    Permission, Resource, ResourceType, Subject, SubjectType,
};
pub use session::{
    Session, SessionConfig, SessionId, SessionManager, SessionState,
    EncryptedMessage, SessionError,
};
pub use intrusion::{
    IntrusionDetector, ThreatLevel, ThreatType, SecurityEvent,
    AnomalyScore, ThreatPattern,
};
pub use key_management::{
    KeyRotationPolicy, KeyStore, RotatingKey, KeyVersion,
    KeyMetadata, KeyRotationError,
};
pub use audit::{
    AuditLog, AuditEvent, AuditEventType, AuditSeverity,
    AuditRecord, AuditLogger,
};
pub use error::{SecurityError, SecurityResult};
