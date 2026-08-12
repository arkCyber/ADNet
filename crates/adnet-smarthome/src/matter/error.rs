//! Error types for the Matter protocol bridge.
//!
//! All Matter-specific errors are wrapped into
//! [`crate::SmartHomeError::Matter`] so callers have a single error
//! type to handle.

use crate::error::SmartHomeError;
use thiserror::Error;

/// Errors specific to Matter commissioning and device control.
#[derive(Debug, Error)]
pub enum MatterError {
    #[error("controller error: {0}")]
    Controller(String),

    #[error("device not commissioned: {0}")]
    NotCommissioned(String),

    #[error("interaction model error: {0}")]
    Interaction(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("certificate error: {0}")]
    Certificate(String),

    #[error("trust / attestation error: {0}")]
    Trust(String),

    #[error("setup code (QR / manual pairing code) invalid: {0}")]
    SetupCode(String),

    #[error("commissioning rejected by device: code {0}")]
    CommissioningRejected(u8),

    #[error("NOC / operational credentials rejected: code {0}")]
    NocRejected(u8),

    #[error("ACL would lock out the controller")]
    AclLockOut,

    #[error("group not provisioned: group id {0}")]
    GroupNotProvisioned(u16),

    #[error("fabric already exists")]
    FabricExists,

    #[error("node not found: {0}")]
    NodeNotFound(u64),

    #[error("subscription stream closed")]
    SubscriptionClosed,
}

impl From<matter_controller::Error> for MatterError {
    fn from(e: matter_controller::Error) -> Self {
        use matter_controller::Error as MC;
        match e {
            MC::NotCommissioned(id) => Self::NotCommissioned(id),
            MC::InteractionModel(ie) => Self::Interaction(format!("{:?}", ie)),
            MC::Transport(te) => Self::Transport(te.to_string()),
            MC::Cert(ce) => Self::Certificate(ce.to_string()),
            MC::Trust(te) => Self::Trust(te),
            MC::SetupCode(se) => Self::SetupCode(se),
            MC::CommissioningWindowRejected(code) => Self::CommissioningRejected(code),
            MC::OperationalCredentialsRejected(code) => Self::NocRejected(code),
            MC::AclWouldLockOut => Self::AclLockOut,
            MC::GroupNotProvisioned(id) => Self::GroupNotProvisioned(id),
            MC::ControllerStopped => Self::Controller("controller stopped".into()),
            MC::NoTrust => Self::Trust("no trust anchors configured".into()),
            _ => Self::Controller(e.to_string()),
        }
    }
}

impl From<MatterError> for SmartHomeError {
    fn from(e: MatterError) -> Self {
        SmartHomeError::Protocol(e.to_string())
    }
}
