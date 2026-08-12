//! `adnet-invite` — email invitation layer for ADNet device pairing.
//!
//! Builds [`Mail`] objects carrying a signed pairing invitation as a
//! typed attachment (`application/x-adnet-pairing`) and parses incoming
//! messages back into a [`PairingInvitation`].
//!
//! ## Threat model & design decisions
//!
//! 1. **Invitation is signed, not encrypted.** We rely on TLS (SMTP
//!    submission / IMAP) for transport confidentiality, matching the
//!    risk profile of password-reset tokens sent by banks and GitHub.
//!    Adding PGP / S/MIME is the caller's choice; this crate stays
//!    transport-agnostic.
//! 2. **Size limit.** Pairing invitation JSON is typically ~256 bytes.
//!    We hard-reject attachments larger than **32 KiB** to bound memory
//!    during parsing and prevent maliciously large MIME blobs.
//! 3. **Expiry is on the invitation**, not on the email itself. A
//!    received invitation is verified via `SignedInvitation::verify`
//!    which checks the expiry timestamp in the payload. The email can
//!    be re-read from IMAP long after receipt; the expiry check is
//!    always live.
//! 4. **No bounce protection.** We assume SMTP forward-confirmation
//!    or SPF/DKIM validation is done by the caller (or by the MTA).
//!
//! ## Usage
//!
//! ### Sending an invitation
//!
//! ```
//! # use adnet_invite::{InvitationMailer, InvitationContent};
//! # use adnet_pairing::{capability::CapabilitySet, SignedInvitation};
//! # use adnet_mail::mime::{Address, Mail};
//! # use adnet_identity::wallet::Wallet;
//! # use adnet_types::node::NodeId;
//! // 1. Create the invitation (see `adnet-pairing` docs).
//! # let wallet = Wallet::generate();
//! # let node_id = NodeId::from_bytes(&[0xAAu8; 32]).unwrap();
//! let invitation = SignedInvitation::create(
//!     &node_id,
//!     &wallet,
//!     CapabilitySet::from_names(["chat", "files.read"]),
//!     15 * 60,  // 15 min TTL
//!     Some("Join ADNet from my laptop".into()),
//! ).unwrap();
//!
//! // 2. Build the email.
//! let content = InvitationContent {
//!     from: Address::new("alice@example.com").with_name("Alice"),
//!     to: vec![Address::new("bob@example.com")],
//!     subject: "ADNet Pairing Invitation".into(),
//!     body: "Hello Bob, here's my ADNet pairing invitation. \
//!            Scan the attached QR code or visit adnet-pairing:// \
//!            from the ADNet app on your device.".into(),
//! };
//! let mail = InvitationMailer::build_invitation_email(&invitation, &content).unwrap();
//! // 3. Send via SMTP (see `adnet-mail`).
//! // smtp::send(&mail, &smtp_config).await.unwrap();
//! ```
//!
//! ### Receiving an invitation
//!
//! ```
//! # use adnet_mail::mime::Mail;
//! # use adnet_invite::InvitationMailer;
//! # use adnet_pairing::PairingInvitation;
//! # async fn receive_invitation(mail: Mail) -> Result<(), Box<dyn std::error::Error>> {
//! let invitation = InvitationMailer::extract_from_mail(&mail)?;
//! let now = chrono::Utc::now().timestamp();
//! invitation.verify(now)?;
//! match invitation.decode()? {
//!     Some(inv) => {
//!         println!("Invitation from wallet {:?}", inv.payload.issuer_wallet);
//!         println!("Expires at {}", inv.payload.expires_at_unix);
//!     }
//!     None => println!("Could not decode invitation payload"),
//! }
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod error;
pub mod mailer;

pub use error::{InviteError, InviteResult};
pub use mailer::{
    InvitationContent, InvitationMailer, ADNET_PAIRING_FILENAME, ADNET_PAIRING_MIME,
    MAX_INVITATION_SIZE, ADNET_QR_FILENAME, TEXT_CODE_PREFIX,
    create_text_code, parse_text_code, TextCode,
};
