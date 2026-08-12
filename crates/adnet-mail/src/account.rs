//! High-level `MailAccount` — single object holding both IMAP and SMTP
//! transport handles, plus an [`crate::login_param::Account`] config.
//!
//! ```no_run
//! use adnet_mail::prelude::*;
//!
//! # async fn doc() -> Result<()> {
//! let acct = MailAccount::builder()
//!     .address("alice@example.com")
//!     .imap_server("imap.example.com")
//!     .smtp_server("smtp.example.com")
//!     .credentials("alice", "hunter2")
//!     .build()?;
//!
//! let mut a = acct.connect().await?;
//! let mail = Mail::text_only(
//!     Address::new("alice@example.com"),
//!     Address::new("bob@example.com"),
//!     "hi",
//!     "hello",
//! );
//! let mut sent = a.send_message(&mail).await?;
//! assert!(sent.is_sent());
//! let inbox = a.fetch_inbox().await?;
//! for m in inbox { println!("{:?}", m.mail); }
//! a.shutdown().await?;
//! # Ok(()) }
//! ```

use crate::error::{MailError, Result};
use crate::imap::{self, FetchHandle, FetchedMessage, ImapSession, SelectInfo};
use crate::login_param::{Account, CertificateChecks, SmtpLoginParam};
use crate::mime::Mail;
use crate::smtp::{self, SendOutcome, Transport};

/// Builder for [`MailAccount`]. Created via [`MailAccount::builder`].
///
/// ⚠️ `Debug` is **manually implemented** to redact the password.
pub struct MailAccountBuilder {
    addr: String,
    imap_server: String,
    smtp_server: String,
    user: String,
    password: String,
    display_name: Option<String>,
    certs: CertificateChecks,
}

impl Default for MailAccountBuilder {
    fn default() -> Self {
        Self {
            addr: String::new(),
            imap_server: String::new(),
            smtp_server: String::new(),
            user: String::new(),
            password: String::new(),
            display_name: None,
            certs: CertificateChecks::Strict,
        }
    }
}

impl std::fmt::Debug for MailAccountBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailAccountBuilder")
            .field("addr", &self.addr)
            .field("imap_server", &self.imap_server)
            .field("smtp_server", &self.smtp_server)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .field("display_name", &self.display_name)
            .field("certs", &self.certs)
            .finish()
    }
}

impl MailAccountBuilder {
    pub fn address(mut self, addr: impl Into<String>) -> Self {
        self.addr = addr.into();
        self
    }
    pub fn imap_server(mut self, s: impl Into<String>) -> Self {
        self.imap_server = s.into();
        self
    }
    pub fn smtp_server(mut self, s: impl Into<String>) -> Self {
        self.smtp_server = s.into();
        self
    }
    pub fn credentials(mut self, user: impl Into<String>, password: impl Into<String>) -> Self {
        self.user = user.into();
        self.password = password.into();
        self
    }
    pub fn display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }
    pub fn certificate_checks(mut self, c: CertificateChecks) -> Self {
        self.certs = c;
        self
    }

    /// Validate and assemble into a [`MailAccount`].
    pub fn build(self) -> Result<MailAccount> {
        if self.addr.is_empty() {
            return Err(MailError::Config("missing address".into()));
        }
        if !crate::login_param::is_valid_address(&self.addr) {
            return Err(MailError::InvalidAddr(self.addr));
        }
        if self.imap_server.is_empty() || self.smtp_server.is_empty() {
            return Err(MailError::Config(
                "missing imap_server or smtp_server".into(),
            ));
        }
        if self.user.is_empty() {
            return Err(MailError::Config("missing username".into()));
        }

        let imap = crate::login_param::ImapLoginParam {
            server: self.imap_server,
            port: 0,
            folder: String::new(),
            security: crate::login_param::SocketSecurity::Tls,
            user: self.user.clone(),
            password: self.password.clone(),
        }
        .with_default_port();
        let smtp = SmtpLoginParam {
            server: self.smtp_server,
            port: 0,
            security: crate::login_param::SocketSecurity::Starttls,
            user: self.user,
            password: self.password,
        }
        .with_default_port();

        Ok(MailAccount {
            account: Account {
                addr: self.addr,
                imap,
                smtp,
                certificate_checks: self.certs,
                display_name: self.display_name,
            },
        })
    }
}

/// One open email account. Holds the [`crate::login_param::Account`]
/// config so a caller that wants to dial a second transport later
/// (e.g. opening IMAP after sending some queued mail) can borrow it.
pub struct MailAccount {
    pub(crate) account: Account,
}

impl MailAccount {
    pub fn builder() -> MailAccountBuilder {
        MailAccountBuilder::default()
    }

    /// Borrow the underlying [`Account`] config.
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// Open the SMTP transport. Calling twice without `shutdown()`
    /// between is an error (would leak TCP connections).
    ///
    /// IMAP is opened lazily by [`MailAccountOnline::open_inbox`]
    /// because not every caller wants receive-side state (mailing
    /// list outbox, batch sending, etc.).
    pub async fn connect(self) -> Result<MailAccountOnline> {
        let smtp = smtp::connect(&self.account).await?;
        Ok(MailAccountOnline {
            account: self.account,
            smtp,
            inbox: None,
        })
    }
}

/// Live, connected account.
pub struct MailAccountOnline {
    account: Account,
    smtp: Transport,
    inbox: Option<(ImapSession, SelectInfo)>,
}

impl MailAccountOnline {
    pub fn account(&self) -> &Account {
        &self.account
    }

    pub async fn send_message(&mut self, mail: &Mail) -> Result<SendOutcome> {
        crate::smtp::send(&mut self.smtp, mail).await
    }

    /// Open the IMAP inbox. Idempotent: calling twice with the same
    /// folder is a no-op; calling with a different folder is an error.
    pub async fn open_inbox(&mut self) -> Result<()> {
        let want_folder = if self.account.imap.folder.is_empty() {
            "INBOX".to_string()
        } else {
            self.account.imap.folder.clone()
        };
        if let Some((_, info)) = &self.inbox {
            if info.folder == want_folder {
                // already open; preserve the cached SELECT info.
                return Ok(());
            }
            return Err(MailError::Config(format!(
                "inbox already opened as {:?}; reopen via a new MailAccount",
                info.folder
            )));
        }
        let mut imap = imap::ImapSession::connect(self.account.clone()).await?;
        let info = imap.select_folder().await?;
        self.inbox = Some((imap, info));
        Ok(())
    }

    /// Fetch all `\Unseen` messages in the watched folder and mark
    /// them `\Seen`. Re-uses the IMAP session opened by
    /// [`MailAccountOnline::open_inbox`] — does not re-`SELECT` the
    /// folder on every call.
    pub async fn fetch_inbox(&mut self) -> Result<Vec<FetchedMessage>> {
        let (session, info) = self
            .inbox
            .as_mut()
            .ok_or_else(|| MailError::Config("inbox not opened".into()))?;
        let mut handle = FetchHandle::new(session, info.clone());
        let msgs = handle.fetch_new().await?;
        for m in &msgs {
            if !m.was_seen {
                handle.mark_seen(m.uid).await?;
            }
        }
        Ok(msgs)
    }

    /// Fetch without auto-marking.
    pub async fn peek_inbox(&mut self) -> Result<Vec<FetchedMessage>> {
        let (session, info) = self
            .inbox
            .as_mut()
            .ok_or_else(|| MailError::Config("inbox not opened".into()))?;
        let mut handle = FetchHandle::new(session, info.clone());
        handle.fetch_new().await
    }

    /// Enter IMAP IDLE; returns when the server reports new mail, the
    /// caller interrupts the loop, or the safety timeout fires. The
    /// caller should call `fetch_inbox()` in response to
    /// [`IdleEvent::NewMail`] and re-issue `wait_for_mail` for the
    /// next batch.
    pub async fn wait_for_mail(&mut self) -> Result<crate::imap::IdleEvent> {
        let (session, _) = self
            .inbox
            .as_mut()
            .ok_or_else(|| MailError::Config("inbox not opened".into()))?;
        session.idle_once().await
    }

    /// Quit both transports cleanly.
    pub async fn shutdown(mut self) -> Result<()> {
        if let Some((session, _)) = self.inbox.take() {
            let _ = session.logout().await;
        }
        crate::smtp::send::quit(self.smtp).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_rejects_missing_addr() {
        let r = MailAccount::builder()
            .imap_server("imap.example.com")
            .smtp_server("smtp.example.com")
            .credentials("alice", "pw")
            .build();
        assert!(matches!(r, Err(MailError::Config(_))));
    }

    #[test]
    fn builder_rejects_invalid_addr() {
        let r = MailAccount::builder()
            .address("not-an-email")
            .imap_server("imap.example.com")
            .smtp_server("smtp.example.com")
            .credentials("alice", "pw")
            .build();
        assert!(matches!(r, Err(MailError::InvalidAddr(_))));
    }

    #[test]
    fn builder_defaults_to_tls_imap_starttls_smtp() {
        let acct = MailAccount::builder()
            .address("alice@example.com")
            .imap_server("imap.example.com")
            .smtp_server("smtp.example.com")
            .credentials("alice", "pw")
            .build()
            .unwrap();
        assert_eq!(acct.account.imap.port, 993);
        assert_eq!(acct.account.smtp.port, 587);
        assert_eq!(
            acct.account.imap.security,
            crate::login_param::SocketSecurity::Tls
        );
        assert_eq!(
            acct.account.smtp.security,
            crate::login_param::SocketSecurity::Starttls
        );
    }

    #[test]
    fn builder_debug_redacts_password() {
        let builder = MailAccount::builder()
            .address("alice@example.com")
            .imap_server("imap.example.com")
            .smtp_server("smtp.example.com")
            .credentials("alice", "supersecret-password");
        let s = format!("{builder:?}");
        assert!(
            !s.contains("supersecret-password"),
            "MailAccountBuilder Debug leaked password: {s}"
        );
        assert!(s.contains("<redacted>"));
    }
}
