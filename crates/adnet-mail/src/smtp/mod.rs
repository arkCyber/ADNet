//! SMTP client.
//!
//! Two layers:
//!
//! - [`connect`] — establish TLS (or STARTTLS, or plaintext),
//!   read the greeting, authenticate, hand back a [`Transport`]
//!   ready for [`send`].
//! - [`send`] — issue `MAIL FROM` / `RCPT TO` / `DATA` for
//!   a pre-built [`crate::mime::Mail`].
//!
//! ## `Transport` shape
//!
//! Async-smtp presents [`SmtpTransport<S>`] where `S: AsyncBufRead +
//! AsyncWrite + Unpin`. TLS-protected transport uses
//! `S = BufStream<TlsStream<TcpStream>>`; the plaintext path uses
//! `S = BufStream<TcpStream>`. We wrap both in a [`Transport`] enum so
//! the public API stays uniform.

use std::time::Duration;

use crate::error::{MailError, Result};
use crate::login_param::{Account, SocketSecurity};
use crate::mime::{Address, Mail};
use async_smtp::{EmailAddress, Envelope, SmtpClient, SmtpTransport};
use tokio::io::{AsyncBufReadExt, BufStream};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

pub mod connect;
pub mod send;

pub use connect::TlsPolicy;
pub use send::SendOutcome;

/// Live SMTP transport. Either TLS-protected or plaintext.
///
/// The variants are an internal implementation detail; callers use
/// [`crate::smtp::send`] and [`crate::smtp::send::quit`] which both
/// take `&mut Transport`.
///
/// Both arms are boxed so the enum is small (one pointer each), and so
/// `Option<Transport>` / `Result<Transport>` don't bloat. The `Deref`
/// on `Box<SmtpTransport<...>>` plus the blanket impl below gives us a
/// `&mut dyn SmtpBackend` out of either variant without a manual
/// `match`.
pub enum Transport {
    /// TLS / STARTTLS-protected connection.
    Tls(Box<SmtpTransport<BufStream<TlsStream<TcpStream>>>>),
    /// Unencrypted (loopback / LAN debug only).
    Plain(Box<SmtpTransport<BufStream<TcpStream>>>),
}

impl Transport {
    /// Pick the inner transport by reference.
    pub(crate) fn inner_mut(&mut self) -> &mut dyn SmtpBackend {
        match self {
            Transport::Tls(t) => &mut **t,
            Transport::Plain(t) => &mut **t,
        }
    }
}

/// Type-erased handle so [`Transport::inner_mut`] can return the same
/// shape for both variants. We achieve this by wrapping the
/// `SmtpTransport<...>` in a small newtype that exposes the small set
/// of methods we actually need on `Transport`.
pub(crate) trait SmtpBackend: Send {
    fn send_email<'a>(
        &'a mut self,
        envelope: Envelope,
        body: Vec<u8>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), async_smtp::error::Error>> + Send + 'a>,
    >;
    fn quit<'a>(
        &'a mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), async_smtp::error::Error>> + Send + 'a>,
    >;
}

impl<S> SmtpBackend for SmtpTransport<BufStream<S>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    BufStream<S>: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    fn send_email<'a>(
        &'a mut self,
        envelope: Envelope,
        body: Vec<u8>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), async_smtp::error::Error>> + Send + 'a>,
    > {
        let sendable = async_smtp::SendableEmail::new(envelope, body);
        Box::pin(async move { self.send(sendable).await.map(|_| ()) })
    }
    fn quit<'a>(
        &'a mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), async_smtp::error::Error>> + Send + 'a>,
    > {
        Box::pin(async move { self.quit().await })
    }
}

/// Maximum time we'll wait for the TCP connect.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Open SMTP transport for `account`, authenticated and ready to send.
///
/// Supports three security modes:
/// - [`SocketSecurity::Tls`]      — implicit TLS, port 465.
/// - [`SocketSecurity::Starttls`] — STARTTLS upgrade, port 587.
/// - [`SocketSecurity::Plain`]    — no TLS, port 25. **Insecure**;
///   only safe on a loopback or trusted LAN. Useful for the
///   in-process SMTP mock in `tests/integration.rs`.
pub async fn connect(account: &Account) -> Result<Transport> {
    let tcp = tcp_connect(&account.smtp.server, account.smtp.port).await?;
    match account.smtp.security {
        SocketSecurity::Tls => connect::implicit_tls_transport(tcp, account).await,
        SocketSecurity::Starttls => connect::starttls_transport(tcp, account).await,
        SocketSecurity::Plain => connect::plaintext_transport(tcp, account).await,
    }
}

/// Send a single message.
pub async fn send(transport: &mut Transport, message: &Mail) -> Result<SendOutcome> {
    send::send(transport, message).await
}

/// Convenience wrapper: convert our `Address` to an SMTP `EmailAddress`.
pub(crate) fn to_smtp_addr(a: &Address) -> Result<EmailAddress> {
    EmailAddress::new(a.address.clone())
        .map_err(|e| MailError::InvalidAddr(format!("{} ({e})", a.address)))
}

/// Build a single envelope combining `To` + `Cc` + `Bcc`.
pub(crate) fn envelope_from(mail: &Mail) -> Result<Envelope> {
    let from = EmailAddress::new(mail.from.address.clone())
        .map_err(|e| MailError::InvalidAddr(format!("from={}: {e}", mail.from.address)))?;
    let mut recipients: Vec<EmailAddress> = Vec::new();
    for a in mail.to.iter().chain(mail.cc.iter()).chain(mail.bcc.iter()) {
        recipients.push(to_smtp_addr(a)?);
    }
    if recipients.is_empty() {
        return Err(MailError::EmptyRecipients);
    }
    Envelope::new(Some(from), recipients).map_err(|e| MailError::Build(e.to_string()))
}

pub(crate) async fn tcp_connect(host: &str, port: u16) -> Result<TcpStream> {
    use std::net::SocketAddr;
    let addr: SocketAddr = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| MailError::Dns {
            host: host.to_string(),
            source: Box::new(e),
        })?
        .next()
        .ok_or_else(|| MailError::Dns {
            host: host.to_string(),
            source: Box::new(std::io::Error::other("no addresses returned")),
        })?;
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| MailError::Transient(format!("tcp connect to {addr} timed out")))?
        .map_err(MailError::Io)?;
    Ok(tcp)
}

/// Build the inner `SmtpTransport` over a `BufStream`, log in, and
/// return the authenticated transport. Used by both the TLS and
/// plaintext paths so they share the auth code.
pub(crate) async fn build_transport<S>(
    stream: S,
    account: &Account,
) -> Result<SmtpTransport<BufStream<S>>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    BufStream<S>: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    let mut buf = BufStream::new(stream);

    // Drop the SMTP greeting. We use `SmtpClient::without_greeting()`
    // for the TLS path but the plaintext path goes through async-smtp
    // anyway, so read & discard the first line ourselves.
    let mut line = String::new();
    buf.read_line(&mut line).await.map_err(MailError::Io)?;
    if !line.starts_with("220 ") && !line.starts_with("220-") {
        return Err(MailError::Tls {
            host: "<greeting>".into(),
            reason: format!("unexpected SMTP greeting: {line:?}"),
        });
    }

    let client = SmtpClient::new().smtp_utf8(true).without_greeting();
    let mut transport = SmtpTransport::new(client, buf)
        .await
        .map_err(MailError::Smtp)?;

    let creds = async_smtp::authentication::Credentials::new(
        account.smtp.user.clone(),
        account.smtp.password.clone(),
    );
    transport
        .try_login(
            &creds,
            &[
                async_smtp::authentication::Mechanism::Plain,
                async_smtp::authentication::Mechanism::Login,
            ],
        )
        .await
        .map_err(|e| match e {
            async_smtp::error::Error::Permanent(_) | async_smtp::error::Error::Transient(_) => {
                MailError::Auth {
                    user: account.smtp.user.clone(),
                    host: account.smtp.server.clone(),
                }
            }
            other => MailError::Smtp(other),
        })?;
    Ok(transport)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_rejects_empty_recipients() {
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "x",
            "y",
        );
        m.to.clear();
        m.cc.clear();
        m.bcc.clear();
        assert!(matches!(envelope_from(&m), Err(MailError::EmptyRecipients)));
    }

    #[test]
    fn envelope_combines_to_cc_bcc() {
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "x",
            "y",
        );
        m.cc.push(Address::new("carol@example.com"));
        m.bcc.push(Address::new("dave@example.com"));
        let env = envelope_from(&m).unwrap();
        assert_eq!(env.to().len(), 3);
    }
}
