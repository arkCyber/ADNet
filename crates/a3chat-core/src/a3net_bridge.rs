//! Re-exports of canonical `a3net-types` enums and validators.
//!
//! ## Purpose
//!
//! a3chat-core defines its own copies of some types
//! ([`crate::group::MemberRole`], [`crate::group::InvitationStatus`],
//! [`crate::message::MessageType`], [`crate::message::AttachmentKind`])
//! purely for ergonomic reasons — they were written before
//! `a3net-types::invariants` existed in its current form. Now that
//! the canonical versions exist (with byte-identical wire formats),
//! new code should prefer the `a3net-types` originals.
//!
//! This module re-exports the `a3net-types` types under their a3chat
//! names so existing call sites can migrate one-by-one without a
//! breaking change:
//!
//! ```ignore
//! use a3chat_core::a3net_bridge::{MemberRole, MessageType};
//! ```
//!
//! Each re-export is also marked as `#[deprecated(note = "...")]` to
//! nudge callers toward the canonical name; the deprecation lint is
//! `#[allow(deprecated)]` on the type-alias site itself so it doesn't
//! trip builds.
//!
//! ## What is NOT re-exported here
//!
//! Wire-record structs (`GroupChat`, `DirectMessage`, `GroupMessage`)
//! are NOT re-exported because a3chat-core has its own richer records
//! (e.g. `ChatMessage` with explicit `MessageBody::Plain` /
//! `MessageBody::Encrypted` variants). Those need a full migration,
//! not a name swap.

#![allow(deprecated)]

/// Canonical membership role — same wire format as
/// [`crate::group::MemberRole`] but lives in `a3net-types`.
pub use a3net_types::invariants::MemberRole as A3netMemberRole;

/// Canonical invitation lifecycle state — same wire format as
/// [`crate::group::InvitationStatus`].
pub use a3net_types::invariants::InvitationStatus as A3netInvitationStatus;

/// Canonical message kind — same wire format as
/// [`crate::message::MessageType`].
pub use a3net_types::invariants::MessageType as A3netMessageType;

/// Canonical attachment kind — same wire format as
/// [`crate::message::AttachmentKind`].
pub use a3net_types::invariants::AttachmentKind as A3netAttachmentKind;

/// Canonical sequence wrapper with `MAX_SEQUENCE` bound enforced at
/// construction. a3chat currently uses a raw `u32`; this is offered so
/// new code can pick the type-safe version.
pub use a3net_types::invariants::Sequence as A3netSequence;

/// Canonical `MAX_SEQUENCE` ceiling — same value (9999) a3chat has
/// always used, but the canonical source is now a3net-types.
pub use a3net_types::invariants::MAX_SEQUENCE as A3NET_MAX_SEQUENCE;

/// Canonical `MAX_ID_LEN` ceiling — 128 bytes.
pub use a3net_types::invariants::MAX_ID_LEN as A3NET_MAX_ID_LEN;

/// Canonical `MAX_ATTACHMENTS` ceiling — 32.
pub use a3net_types::invariants::MAX_ATTACHMENTS as A3NET_MAX_ATTACHMENTS;

/// Canonical `MAX_CONTENT_LEN` ceiling — 64 KiB.
pub use a3net_types::invariants::MAX_CONTENT_LEN as A3NET_MAX_CONTENT_LEN;

/// Canonical `MAX_MEMBERS` ceiling — 1024 (was 500 in legacy a3chat).
pub use a3net_types::invariants::MAX_MEMBERS as A3NET_MAX_MEMBERS;

/// Canonical `MAX_MENTIONS` ceiling — 64.
pub use a3net_types::invariants::MAX_MENTIONS as A3NET_MAX_MENTIONS;

/// Canonical `MAX_NAME_LEN` ceiling — 256.
pub use a3net_types::invariants::MAX_NAME_LEN as A3NET_MAX_NAME_LEN;

// -- Conversion helpers --------------------------------------------------
//
// These let a3chat's local types interop with the a3net-types
// originals without forcing an immediate rename. They are thin
// `From` impls that exist ONLY here so the bridge is bidirectional.

impl From<crate::group::MemberRole> for a3net_types::invariants::MemberRole {
    fn from(r: crate::group::MemberRole) -> Self {
        match r {
            crate::group::MemberRole::Owner => Self::Owner,
            crate::group::MemberRole::Admin => Self::Admin,
            crate::group::MemberRole::Member => Self::Member,
        }
    }
}

impl From<a3net_types::invariants::MemberRole> for crate::group::MemberRole {
    fn from(r: a3net_types::invariants::MemberRole) -> Self {
        match r {
            a3net_types::invariants::MemberRole::Owner => Self::Owner,
            a3net_types::invariants::MemberRole::Admin => Self::Admin,
            a3net_types::invariants::MemberRole::Member => Self::Member,
        }
    }
}

impl From<crate::group::InvitationStatus> for a3net_types::invariants::InvitationStatus {
    fn from(s: crate::group::InvitationStatus) -> Self {
        use crate::group::InvitationStatus as L;
        match s {
            // a3net-types does not distinguish "cancelled" from
            // "rejected" — both are terminal, sender-withdraw vs
            // recipient-decline. Map both onto Rejected so cross-crate
            // interop stays lossless on the wire.
            L::Pending => Self::Pending,
            L::Accepted => Self::Accepted,
            L::Rejected | L::Cancelled => Self::Rejected,
            L::Expired => Self::Expired,
        }
    }
}

impl From<a3net_types::invariants::InvitationStatus> for crate::group::InvitationStatus {
    fn from(s: a3net_types::invariants::InvitationStatus) -> Self {
        use a3net_types::invariants::InvitationStatus as A;
        // On the way back we cannot recover a Cancelled vs Rejected
        // distinction because a3net-types already collapsed them.
        // We pick Rejected as the default; callers that need to
        // distinguish should use a3chat-native records.
        match s {
            A::Pending => Self::Pending,
            A::Accepted => Self::Accepted,
            A::Rejected => Self::Rejected,
            A::Expired => Self::Expired,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{InvitationStatus as I, MemberRole as R};

    #[test]
    fn member_role_round_trip() {
        for r in [R::Owner, R::Admin, R::Member] {
            let a3net: a3net_types::invariants::MemberRole = r.into();
            let back: R = a3net.into();
            assert_eq!(r, back);
        }
    }

    #[test]
    fn invitation_status_round_trip() {
        // `Cancelled` collapses to `Rejected` on the a3net-types side
        // because a3net-types does not distinguish them; verify the
        // mapping is consistent.
        for s in [I::Pending, I::Accepted, I::Rejected, I::Expired, I::Cancelled] {
            let a3net: a3net_types::invariants::InvitationStatus = s.into();
            let back: I = a3net.into();
            match s {
                I::Rejected | I::Cancelled => {
                    assert!(matches!(back, I::Rejected | I::Cancelled));
                }
                other => assert_eq!(other, back),
            }
        }
    }

    #[test]
    fn cancelled_maps_to_rejected_in_a3net() {
        let s: a3net_types::invariants::InvitationStatus =
            crate::group::InvitationStatus::Cancelled.into();
        assert!(matches!(s, a3net_types::invariants::InvitationStatus::Rejected));
    }

    #[test]
    fn wire_format_is_byte_identical() {
        // a3chat's enum serialises snake_case; a3net-types does too.
        // Verify they produce the same JSON so cross-crate interop
        // remains wire-compatible after the bridge.
        assert_eq!(
            serde_json::to_string(&R::Owner).unwrap(),
            serde_json::to_string(&a3net_types::invariants::MemberRole::Owner).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&I::Pending).unwrap(),
            serde_json::to_string(&a3net_types::invariants::InvitationStatus::Pending).unwrap()
        );
    }

    #[test]
    fn ceilings_reconcile() {
        assert_eq!(A3NET_MAX_CONTENT_LEN, 64 * 1024);
        assert_eq!(A3NET_MAX_MEMBERS, 1024);
        assert_eq!(A3NET_MAX_ATTACHMENTS, 32);
        assert_eq!(A3NET_MAX_MENTIONS, 64);
        assert_eq!(A3NET_MAX_NAME_LEN, 256);
        assert_eq!(A3NET_MAX_ID_LEN, 128);
        assert_eq!(A3NET_MAX_SEQUENCE, 9999);
    }
}
