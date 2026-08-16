//! `a3net-webdav` — WebDAV gateway for the NAS namespace.
//!
//! DO-178C **DAL-A** module. Surfaces `a3net-blobstore::Nas` as a
//! WebDAV server (RFC 4918) over plain TCP, so Finder, Explorer
//! and any third-party WebDAV client can mount the home NAS.
//!
//! ## Verbs implemented (RFC 4918 §9)
//!
//! | Verb      | Used for            | DAL-A SR |
//! |-----------|---------------------|----------|
//! | `OPTIONS` | capability advertise| SR-12 |
//! | `PROPFIND`| list directory      | SR-12, SR-13 |
//! | `GET`     | download file       | SR-12 |
//! | `HEAD`    | existence check     | SR-12 |
//! | `PUT`     | upload file         | SR-12, SR-15, SR-16 |
//! | `MKCOL`   | create directory    | SR-12, SR-15, SR-16 |
//! | `DELETE`  | remove file         | SR-12, SR-15 |
//! | `MOVE`    | rename (server-side)| SR-12, SR-15, SR-17 |
//! | `COPY`    | duplicate file      | SR-12, SR-15 |
//!
//! ## PROPFIND Properties
//!
//! Supports standard DAV: properties:
//! - `resourcetype` (collection for dirs, empty for files)
//! - `getcontentlength` (file size)
//! - `getcontenttype` (MIME type, guessed from extension)
//! - `displayname` (basename)
//! - `getetag` (content hash)
//! - `supportedlock` (no locking support)
//!
//! ## Depth Header
//!
//! PROPFIND supports `Depth` header per RFC 4918:
//! - `0`: only the resource itself
//! - `1`: the resource and immediate children
//! - `infinity`: the resource and all descendants (default)
//!
//! `LOCK` / `UNLOCK` are deliberately **out of scope** for v0.1;
//! a family NAS does not need WEBDAV `LOCK` semantics — the OS
//! uses its own file-locking, and WebDAV's `LOCK` is famously
//! fussy. Returning `405 Not Implemented` for these keeps the
//! audit surface minimal.
//!
//! ## Authentication
//!
//! Every verb is gated by [`AclMiddleware`] which checks the
//! request's `Authorization:` header against a `CredentialResolver`.
//! The default resolver accepts an `a3net-pairing`-issued capability
//! token (signed + nonce'd + ttl'd per SR-14).
//!
//! ## Auditing
//!
//! Every state-changing verb persists a single NDJSON line to
//! `audit.jsonl` **before** the operation returns (SR-15). The
//! audit record is the user's non-repudiable receipt.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod acl;
pub mod handlers;
pub mod props;
pub mod server;
pub mod token;

/// Aerospace (DO-178C DAL-A) certification constants and hooks
/// for `a3net-webdav`. Mirrors `a3net-blobstore::aerospace` —
/// the `aerospace` cargo feature must be on for both crates to
/// keep the certification baseline aligned.
#[cfg(feature = "aerospace")]
pub mod aerospace {
    pub const SAFETY_REVISION: &str = "SC-2026-08-11-A";
    pub const DAL_LEVEL: &str = "A";
    pub const HAZARD_REGISTER_REV: &str = "HR-2026-08-11-A";
    pub const MC_DC_COVERAGE_TARGET: u8 = 100;
    pub const BRANCH_COVERAGE_TARGET: u8 = 100;
    pub const STMT_COVERAGE_TARGET: u8 = 100;
}

pub use acl::{AclDecision, AclMiddleware, CapabilityResolver, ResolvedCapability, StaticCapabilityResolver};
pub use server::{WebdavConfig, WebdavServer, WebdavServerHandle};
pub use token::{CapabilityToken, TokenError, TokenVerifier};
pub use handlers::{HandlerState, HttpError};
pub use props::Depth;
