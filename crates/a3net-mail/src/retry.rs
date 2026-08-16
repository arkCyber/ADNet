//! Send-with-retry helper.
//!
//! Mirrors the pattern Delta Chat uses in `smtp.rs`: the caller builds
//! a [`Mail`], the helper sends it, and on a transient failure
//! (`SendOutcome::Transient` or a recoverable [`crate::error::MailError`])
//! it waits with exponential backoff and re-queues. Permanent failures
//! (`5.x.y` other than `5.5.0`) and user errors return immediately.
//!
//! ```no_run
//! use a3net_mail::prelude::*;
//!
//! # async fn doc() -> Result<()> {
//! # let mut transport = unimplemented!();
//! let mail = Mail::text_only(
//!     Address::new("alice@example.com"),
//!     Address::new("bob@example.com"),
//!     "hi",
//!     "hello",
//! );
//! let policy = RetryPolicy::default();
//! let outcome = send_with_retry(&mut transport, &mail, &policy).await?;
//! # let _ = outcome;
//! # Ok(()) }
//! ```

use std::time::Duration;

use crate::error::{ErrorClass, MailError, Result};
use crate::mime::Mail;
use crate::smtp::{SendOutcome, Transport, send as smtp_send};

/// Exponential-backoff retry policy.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of attempts (≥ 1). Total attempts = 1 initial + `max_retries`.
    pub max_retries: u32,
    /// First delay between attempts.
    pub initial_delay: Duration,
    /// Maximum delay between attempts (cap on the exponential growth).
    pub max_delay: Duration,
    /// Multiplier applied to `initial_delay` after each failure. 2.0 = double.
    pub multiplier: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(60 * 5),
            multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    /// Build a "no retries" policy — useful for tests.
    pub fn no_retries() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Build an aggressive retry policy for interactive use.
    pub fn aggressive() -> Self {
        Self {
            max_retries: 10,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            multiplier: 2.0,
        }
    }

    /// Compute the delay for the n-th retry (0-indexed).
    pub fn delay_for(&self, retry_index: u32) -> Duration {
        let mut d = self.initial_delay.as_secs_f64();
        for _ in 0..retry_index {
            d *= self.multiplier;
            if d > self.max_delay.as_secs_f64() {
                d = self.max_delay.as_secs_f64();
                break;
            }
        }
        Duration::from_secs_f64(d)
    }
}

/// Send `mail` over `transport`, retrying transient failures with
/// exponential backoff per `policy`.
///
/// Returns:
/// - `Ok(SendOutcome::Sent)` if a retry eventually succeeded.
/// - `Ok(SendOutcome::Permanent { reason })` if the server permanently
///   rejected the message and we exhausted retries.
/// - `Ok(SendOutcome::Transient { reason })` if we exhausted retries and
///   the last failure was still transient (the caller should re-queue
///   with a longer delay, or escalate to a dead-letter queue).
/// - `Err(MailError)` if a non-retryable error occurred (auth failure,
///   TLS failure, etc.).
pub async fn send_with_retry(
    transport: &mut Transport,
    mail: &Mail,
    policy: &RetryPolicy,
) -> Result<SendOutcome> {
    let mut last: Option<SendOutcome> = None;

    for attempt in 0..=policy.max_retries {
        if attempt > 0 {
            let delay = policy.delay_for(attempt - 1);
            tracing::info!(
                "retry attempt {attempt}/{} after {delay:?}",
                policy.max_retries
            );
            tokio::time::sleep(delay).await;
        }

        match smtp_send(transport, mail).await {
            Ok(SendOutcome::Sent) => return Ok(SendOutcome::Sent),
            Ok(outcome @ SendOutcome::Permanent { .. }) => {
                return Ok(outcome);
            }
            Ok(outcome @ SendOutcome::Transient { .. }) => {
                last = Some(outcome);
                // continue retrying
            }
            Err(e) if e.recoverability() == ErrorClass::Recoverable => {
                last = Some(SendOutcome::Transient {
                    reason: e.to_string(),
                });
                // continue retrying
            }
            Err(e) => {
                // Auth / TLS / Config / Internal — don't retry.
                return Err(e);
            }
        }
    }

    Ok(last.unwrap_or(SendOutcome::Transient {
        reason: "retry loop exited without any outcome".into(),
    }))
}

/// Like [`send_with_retry`] but never returns an `Err` — fatal errors
/// are surfaced as `SendOutcome::Permanent`. Useful for background
/// senders that don't have a `Result`-shaped return channel.
pub async fn send_with_retry_infallible(
    transport: &mut Transport,
    mail: &Mail,
    policy: &RetryPolicy,
) -> SendOutcome {
    match send_with_retry(transport, mail, policy).await {
        Ok(o) => o,
        Err(e) => SendOutcome::Permanent {
            reason: e.to_string(),
        },
    }
}

/// Translate a `MailError` into the most permissive [`SendOutcome`]
/// classification we can derive.
pub fn classify_error(err: &MailError) -> SendOutcome {
    match err.recoverability() {
        ErrorClass::UserError => SendOutcome::Permanent {
            reason: err.to_string(),
        },
        ErrorClass::Recoverable => SendOutcome::Transient {
            reason: err.to_string(),
        },
        ErrorClass::Fatal => SendOutcome::Permanent {
            reason: err.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_grows_then_caps() {
        let p = RetryPolicy {
            max_retries: 5,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(8),
            multiplier: 2.0,
        };
        assert_eq!(p.delay_for(0), Duration::from_secs(1));
        assert_eq!(p.delay_for(1), Duration::from_secs(2));
        assert_eq!(p.delay_for(2), Duration::from_secs(4));
        assert_eq!(p.delay_for(3), Duration::from_secs(8));
        assert_eq!(p.delay_for(4), Duration::from_secs(8)); // capped
    }

    #[test]
    fn classify_error_maps_to_outcome() {
        let user = MailError::EmptyRecipients;
        assert!(matches!(
            classify_error(&user),
            SendOutcome::Permanent { .. }
        ));

        let retry = MailError::Transient("4.7.1".into());
        assert!(matches!(
            classify_error(&retry),
            SendOutcome::Transient { .. }
        ));

        let auth = MailError::Auth {
            user: "u".into(),
            host: "h".into(),
        };
        assert!(matches!(
            classify_error(&auth),
            SendOutcome::Permanent { .. }
        ));
    }
}
