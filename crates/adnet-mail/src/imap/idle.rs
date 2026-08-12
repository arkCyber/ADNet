//! IMAP IDLE — the long-poll mechanism that wakes the client on new mail.
//!
//! Mirrors the pattern from `chatmail@core/src/imap/idle.rs` over the
//! public `async-imap::extensions::idle` API:
//!
//! ```text
//! session.idle()          -> Handle<T>   // synchronously prepares IDLE
//! handle.init().await     -> ()          // sends IDLE, waits for +
//! handle.wait_with_timeout(d) -> (Future, StopSource)
//! handle.done().await     -> Session<T>  // issues DONE
//! ```
//!
//! Because `async_imap::Session::idle` *consumes* the session, our
//! [`IdleHandle`] holds it by value, not by borrow. The
//! [`crate::account::MailAccountOnline::wait_for_mail`] path uses
//! [`ImapSession::idle_consuming`] to swap the session in/out cleanly.

use std::time::Duration;

use async_imap::extensions::idle::IdleResponse;
use tokio::io::BufReader;

use crate::error::{MailError, Result};
use crate::imap::{ImapSession, ImapStream};

/// One event reported by the IDLE loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdleEvent {
    /// Server announced new mail (or any unsolicited update).
    NewMail,
    /// We hit the safety timeout without hearing anything — caller
    /// can decide to re-issue IDLE or treat as a connection refresh.
    Timeout,
    /// Caller asked us to stop via [`IdleHandle::interrupt`].
    Interrupted,
}

/// Owned handle to an in-flight IDLE. Holds the IMAP session by value
/// because [`async_imap::Session::idle`] takes `self`.
pub struct IdleHandle {
    handle: async_imap::extensions::idle::Handle<BufReader<ImapStream>>,
    interrupt_flag: bool,
    /// 5 minutes — matches Delta Chat's `IDLE_TIMEOUT`.
    idle_timeout: Duration,
}

impl IdleHandle {
    pub(crate) fn new(handle: async_imap::extensions::idle::Handle<BufReader<ImapStream>>) -> Self {
        Self {
            handle,
            interrupt_flag: false,
            idle_timeout: Duration::from_secs(5 * 60),
        }
    }

    /// Override the IDLE safety timeout (default 5 min).
    pub fn with_timeout(mut self, dur: Duration) -> Self {
        self.idle_timeout = dur;
        self
    }

    /// Ask the IDLE loop to exit on the next iteration.
    pub fn interrupt(&mut self) {
        self.interrupt_flag = true;
    }

    /// Run IDLE until the server reports new mail, the caller interrupts
    /// us, or the safety timeout fires.
    ///
    /// Returns the inner IMAP session (so the caller can keep using
    /// the connection) plus the triggering event.
    pub async fn run(mut self) -> Result<(async_imap::Session<BufReader<ImapStream>>, IdleEvent)> {
        self.handle.init().await.map_err(MailError::Imap)?;

        let (wait_fut, stop_src) = self.handle.wait_with_timeout(self.idle_timeout);

        let interrupt_poll = async move {
            loop {
                if self.interrupt_flag {
                    drop(stop_src);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::pin!(interrupt_poll);

        let resp = tokio::select! {
            r = wait_fut => r,
            _ = &mut interrupt_poll => Ok(IdleResponse::ManualInterrupt),
        }
        .map_err(MailError::Imap)?;

        let event = match resp {
            IdleResponse::NewData(_) => IdleEvent::NewMail,
            IdleResponse::Timeout => IdleEvent::Timeout,
            IdleResponse::ManualInterrupt => IdleEvent::Interrupted,
        };

        let session = self.handle.done().await.map_err(MailError::Imap)?;
        Ok((session, event))
    }
}

impl ImapSession {
    /// Run one IDLE iteration, swapping the inner session in/out so the
    /// caller can keep using the same `ImapSession` afterwards.
    ///
    /// `chatmail@core` exposes this as
    /// `Imap::idle(context, idle_interrupt_receiver, folder)`; we
    /// return the (now-resumed) session so the caller can issue
    /// `SELECT`, `FETCH`, etc. immediately.
    pub async fn idle_once(&mut self) -> Result<IdleEvent> {
        let inner = self.inner.take().ok_or(MailError::IdleInterrupted)?;
        let idle_handle = IdleHandle::new(inner.idle());
        let (session, event) = idle_handle.run().await?;
        self.inner = Some(session);
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_variants_compile() {
        let _ = IdleEvent::NewMail;
        let _ = IdleEvent::Timeout;
        let _ = IdleEvent::Interrupted;
    }
}
