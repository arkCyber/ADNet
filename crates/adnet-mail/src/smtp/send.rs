//! Send a pre-built [`crate::mime::Mail`] over an SMTP transport.

use async_smtp::Envelope;
use async_smtp::response::{Category, Detail};
use serde::{Deserialize, Serialize};

use crate::error::{MailError, Result};
use crate::mime::Mail;
use crate::smtp::{Transport, envelope_from};

/// Outcome of a single `send()` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SendOutcome {
    /// Server returned `250 OK`.
    Sent,
    /// Server returned a permanent failure (`5.x.y`); retrying won't help.
    Permanent { reason: String },
    /// Server returned a transient failure (`4.x.y`); the caller should
    /// requeue with backoff.
    Transient { reason: String },
}

impl SendOutcome {
    pub fn is_sent(&self) -> bool {
        matches!(self, SendOutcome::Sent)
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self, SendOutcome::Transient { .. })
    }
}

pub async fn send(transport: &mut Transport, mail: &Mail) -> Result<SendOutcome> {
    mail.validate()?;
    let envelope = envelope_from(mail)?;
    let body = mail.to_wire_bytes()?;

    let backend = transport.inner_mut();
    let result = backend.send_email(envelope, body).await;

    match result {
        Ok(_) => Ok(SendOutcome::Sent),
        Err(e) => {
            // Mirror Delta Chat's classification logic (smtp.rs):
            // - Permanent (5.x) → SendOutcome::Permanent
            // - Transient  (4.x) → SendOutcome::Transient
            // - 5.5.0 is special-cased: Postfix misconfigures 5.5.0 as
            //   a permanent reply when it should be a transient retry.
            if let async_smtp::error::Error::Permanent(resp) = &e {
                if resp.code.category == Category::MailSystem
                    && resp.code.detail == Detail::Zero
                    && resp.first_word() == Some("5.5.0")
                {
                    return Ok(SendOutcome::Transient {
                        reason: e.to_string(),
                    });
                }
                return Ok(SendOutcome::Permanent {
                    reason: e.to_string(),
                });
            }
            if let async_smtp::error::Error::Transient(_) = &e {
                return Ok(SendOutcome::Transient {
                    reason: e.to_string(),
                });
            }
            // Anything else (network reset, malformed reply) — propagate.
            Err(MailError::Smtp(e))
        }
    }
}

/// Send a raw pre-rendered RFC 5322 message. Useful for advanced callers
/// (e.g. multipart/encrypted already produced upstream).
pub async fn send_raw(
    transport: &mut Transport,
    from: &str,
    recipients: &[String],
    body: &[u8],
) -> Result<SendOutcome> {
    let from = crate::smtp::to_smtp_addr(&crate::mime::Address::new(from))?;
    let mut rcpts = Vec::with_capacity(recipients.len());
    for r in recipients {
        rcpts.push(crate::smtp::to_smtp_addr(&crate::mime::Address::new(r))?);
    }
    let envelope: Envelope = async_smtp::Envelope::new(Some(from), rcpts)
        .map_err(|e| MailError::Build(e.to_string()))?;
    let backend = transport.inner_mut();
    match backend.send_email(envelope, body.to_vec()).await {
        Ok(_) => Ok(SendOutcome::Sent),
        Err(e) => Err(MailError::Smtp(e)),
    }
}

/// Issue SMTP `QUIT` and drop the transport. Errors are non-fatal; the
/// server closes the socket anyway.
pub async fn quit(transport: Transport) {
    let mut transport = transport;
    let _ = transport.inner_mut().quit().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_helpers() {
        assert!(SendOutcome::Sent.is_sent());
        assert!(!SendOutcome::Sent.is_retryable());

        assert!(
            !SendOutcome::Transient {
                reason: "4.7.1".into()
            }
            .is_sent()
        );
        assert!(
            SendOutcome::Transient {
                reason: "4.7.1".into()
            }
            .is_retryable()
        );

        assert!(
            !SendOutcome::Permanent {
                reason: "5.1.1".into()
            }
            .is_retryable()
        );
    }

    #[test]
    fn send_outcome_json_round_trip() {
        // Sent → {"kind":"sent"}
        let j = serde_json::to_string(&SendOutcome::Sent).unwrap();
        assert_eq!(j, r#"{"kind":"sent"}"#);
        let back = serde_json::from_str::<SendOutcome>(&j).unwrap();
        assert_eq!(back, SendOutcome::Sent);

        // Transient → {"kind":"transient","reason":"4.7.1"}
        let t = SendOutcome::Transient {
            reason: "4.7.1".into(),
        };
        let j = serde_json::to_string(&t).unwrap();
        let back = serde_json::from_str::<SendOutcome>(&j).unwrap();
        assert_eq!(back, t);

        // Permanent
        let p = SendOutcome::Permanent {
            reason: "5.1.1".into(),
        };
        let j = serde_json::to_string(&p).unwrap();
        let back = serde_json::from_str::<SendOutcome>(&j).unwrap();
        assert_eq!(back, p);
    }
}
