//! Shared validation policy types for IPC services.
//!
//! Both [`crate::blobs_service::BlobsIpcService`] and
//! [`crate::group_chat_service::GroupChatIpcService`] route every
//! inbound record through the same gate. The gate is configured at
//! construction time via these types and applies DO-178C fail-closed
//! semantics by default.
//!
//! The [`Validate`] trait is a re-export of
//! [`adnet_types::Validate`] so every record that already implements
//! `adnet_types::Validate` (the typed group chat / social feed /
//! announcement records) is automatically usable here.

use serde::{Deserialize, Serialize};

pub use adnet_types::Validate;

/// Validation policy applied at every IPC entry point.
///
/// DO-178C requires that every boundary either admits a value with
/// proven invariants or rejects it. The default is
/// [`ValidationPolicy::Strict`] (fail-closed). The other variants are
/// opt-in for migration scenarios.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationPolicy {
    /// Reject any record that fails `validate()` at the IPC boundary.
    /// This is the **default** and the safe choice.
    #[default]
    Strict,
    /// Accept the record but record the failure as a warning. Useful
    /// for canary rollouts where you want to measure how much legacy
    /// traffic would be rejected before flipping the policy.
    Audit,
    /// Accept the record unconditionally (legacy migration path).
    /// Not recommended.
    Lenient,
}

/// Outcome of a single `check()` call. In `Strict` mode the `error`
/// field is set on rejection; in `Audit` mode `warnings` accumulate;
/// in `Lenient` mode everything is empty.
#[derive(Debug, Default, Clone)]
pub struct ValidationOutcome {
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

impl ValidationOutcome {
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}
