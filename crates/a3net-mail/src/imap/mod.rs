//! IMAP client.
//!
//! Wraps `async-imap` with three convenience surfaces:
//!
//! - [`ImapSession::connect`] — open a TLS (or STARTTLS, or plaintext)
//!   connection, log in, and hand back a session ready for `SELECT INBOX`.
//! - [`ImapSession::idle_once`] — enter IMAP IDLE, returning when either the
//!   server reports new mail or the safety timeout fires.
//! - [`ImapSession::fetch_new`] — list and fetch all unread messages
//!   in the watched folder, decoding each into our [`crate::mime::Mail`].

use std::net::SocketAddr;
use std::time::Duration;

use async_imap::Session as ImapInnerSession;
use pin_project::pin_project;
use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader, BufStream};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::error::{MailError, Result};
use crate::login_param::{Account, SocketSecurity};

impl std::fmt::Debug for ImapStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImapStream::Tls(_) => write!(f, "ImapStream::Tls"),
            ImapStream::Plain(_) => write!(f, "ImapStream::Plain"),
        }
    }
}

/// Stream type backing the IMAP session. `Tls` is the TLS / STARTTLS
/// path; `Plain` is the plaintext path (loopback / LAN debug only).
///
/// `async-imap` requires its `Session` to wrap a stream that
/// implements `AsyncRead + AsyncWrite + AsyncBufRead + Unpin`. TLS
/// already meets that bound; for plaintext we have to wrap `TcpStream`
/// in a `BufStream` so it gets `AsyncBufRead`.
///
/// The `Tls` arm is boxed to keep the enum small — `TlsStream`
/// carries a sizeable crypto context (≥ 1100 bytes) while `Plain` is
/// < 140 bytes. Boxing also keeps `Drop` for the inner stream
/// predictable (the stream is dropped when the `Box` is dropped).
#[pin_project(project = ImapStreamProj)]
pub enum ImapStream {
    Tls(#[pin] Box<TlsStream<TcpStream>>),
    Plain(#[pin] BufStream<TcpStream>),
}

impl AsyncRead for ImapStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.project() {
            ImapStreamProj::Tls(s) => s.poll_read(cx, buf),
            ImapStreamProj::Plain(s) => s.poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ImapStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.project() {
            ImapStreamProj::Tls(s) => s.poll_write(cx, buf),
            ImapStreamProj::Plain(s) => s.poll_write(cx, buf),
        }
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.project() {
            ImapStreamProj::Tls(s) => s.poll_flush(cx),
            ImapStreamProj::Plain(s) => s.poll_flush(cx),
        }
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.project() {
            ImapStreamProj::Tls(s) => s.poll_shutdown(cx),
            ImapStreamProj::Plain(s) => s.poll_shutdown(cx),
        }
    }
}

impl AsyncBufRead for ImapStream {
    fn poll_fill_buf(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<&[u8]>> {
        match self.project() {
            ImapStreamProj::Tls(s) => s.poll_fill_buf(cx),
            ImapStreamProj::Plain(s) => s.poll_fill_buf(cx),
        }
    }
    fn consume(self: std::pin::Pin<&mut Self>, amt: usize) {
        match self.project() {
            ImapStreamProj::Tls(s) => s.consume(amt),
            ImapStreamProj::Plain(s) => s.consume(amt),
        }
    }
}

pub mod fetch;
pub mod idle;

pub use fetch::{FetchHandle, FetchedMessage};
pub use idle::{IdleEvent, IdleHandle};

/// Maximum time we'll wait for a TCP connect / TLS handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// An open, authenticated IMAP session.
///
/// `Drop` issues `LOGOUT` if the session is still live; if the server
/// is unresponsive we hand the cleanup to a `tokio::spawn` so the
/// runtime doesn't stall the caller.
///
/// ⚠️ `ImapSession` is **not** `Send` or `Sync`. It must stay on the
/// async task that created it. Sharing a session across tasks requires
/// wrapping in `Arc<Mutex<...>>` at the call-site.
pub struct ImapSession {
    inner: Option<ImapInnerSession<BufReader<ImapStream>>>,
    /// Cached server capabilities, probed at login time.
    capabilities: Vec<String>,
    account: Account,
}

impl ImapSession {
    /// Open a TCP connection, upgrade to TLS (or do STARTTLS), read the
    /// greeting, and authenticate.
    pub async fn connect(account: Account) -> Result<Self> {
        let tcp = tcp_connect(&account.imap.server, account.imap.port).await?;
        let stream: ImapStream = match account.imap.security {
            SocketSecurity::Tls => {
                let tls = tls_wrap(tcp, &account.imap.server, account.certificate_checks).await?;
                ImapStream::Tls(Box::new(tls))
            }
            SocketSecurity::Starttls => {
                let tls = starttls_upgrade(tcp, &account.imap.server, &account).await?;
                ImapStream::Tls(Box::new(tls))
            }
            SocketSecurity::Plain => {
                if account.certificate_checks == crate::login_param::CertificateChecks::Strict {
                    return Err(MailError::Config(
                        "plaintext IMAP requires CertificateChecks::AcceptInvalid".into(),
                    ));
                }
                ImapStream::Plain(BufStream::new(tcp))
            }
        };
        let mut buf = BufReader::new(stream);
        skip_greeting(&mut buf).await?;

        // Hand off to async-imap; it owns the stream from here on.
        let client = async_imap::Client::new(buf);
        let login_result = client
            .login(&account.imap.user, &account.imap.password)
            .await;
        let mut session = match login_result {
            Ok(s) => s,
            Err((err, _client)) => {
                return Err(match err {
                    async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_) => {
                        MailError::Auth {
                            user: account.imap.user.clone(),
                            host: account.imap.server.clone(),
                        }
                    }
                    other => MailError::Imap(other),
                });
            }
        };

        // Probe capabilities — useful for callers that want to know
        // about IDLE / MOVE / QUOTA. Best-effort.
        let capabilities: Vec<String> = session
            .capabilities()
            .await
            .map(|caps| {
                caps.iter()
                    .map(|c| match c {
                        async_imap::types::Capability::Imap4rev1 => "IMAP4rev1".into(),
                        async_imap::types::Capability::Auth(s) => format!("AUTH={s}"),
                        async_imap::types::Capability::Atom(s) => s.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            inner: Some(session),
            capabilities,
            account,
        })
    }

    /// Select the watched folder (default: `INBOX`) and return its
    /// UIDVALIDITY so callers can persist it.
    pub async fn select_folder(&mut self) -> Result<SelectInfo> {
        let folder = if self.account.imap.folder.is_empty() {
            "INBOX"
        } else {
            &self.account.imap.folder
        };
        let session = self.inner.as_mut().ok_or(MailError::IdleInterrupted)?;
        let sel = session.select(folder).await.map_err(MailError::Imap)?;
        Ok(SelectInfo {
            folder: folder.to_string(),
            uid_validity: sel.uid_validity,
            uid_next: sel.uid_next,
            highest_modseq: sel.highest_modseq,
        })
    }

    /// Mutable access to the underlying `async-imap` session.
    ///
    /// Most callers should prefer the typed methods on `ImapSession`;
    /// this exists for advanced features (FETCH extensions, etc.).
    pub fn raw_mut(&mut self) -> Result<&mut ImapInnerSession<BufReader<ImapStream>>> {
        self.inner.as_mut().ok_or(MailError::IdleInterrupted)
    }

    /// Borrow the `Account` that opened this session.
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// Return the server capabilities reported at login time.
    /// Returns an empty list if the server did not advertise any or if
    /// probing failed.
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    /// Issue `LOGOUT` and consume the session.
    pub async fn logout(mut self) -> Result<()> {
        if let Some(mut s) = self.inner.take() {
            // async-imap's `logout` consumes the session; errors here
            // are network-only and we don't surface them — the server
            // closes the socket anyway.
            let _ = s.logout().await;
        }
        Ok(())
    }

    /// Convenience: connect, select the watched folder, and return a
    /// [`fetch::FetchHandle`] bound to it.
    pub async fn open_inbox(&mut self) -> Result<FetchHandle<'_>> {
        let info = self.select_folder().await?;
        Ok(FetchHandle::new(self, info))
    }
}

impl Drop for ImapSession {
    fn drop(&mut self) {
        if let Some(mut session) = self.inner.take() {
            // Best-effort async logout in the background.
            tokio::spawn(async move {
                let _ = session.logout().await;
            });
        }
    }
}

/// Result of a `SELECT <folder>` command.
#[derive(Debug, Clone)]
pub struct SelectInfo {
    pub folder: String,
    /// IMAP `UIDVALIDITY` — stable across the lifetime of the mailbox.
    /// Persist this so callers can detect a folder that has been
    /// recreated on the server (which invalidates all cached UIDs).
    pub uid_validity: Option<u32>,
    /// IMAP `UIDNEXT` — the UID that will be assigned to the next
    /// message appended to the folder. Useful as a "high water mark"
    /// for incremental sync.
    pub uid_next: Option<u32>,
    /// IMAP `HIGHESTMODSEQ` — only present when the server supports
    /// the CONDSTORE extension (RFC 4551). Drives incremental sync via
    /// `FETCH (CHANGEDSINCE …)`.
    pub highest_modseq: Option<u64>,
}

// ─── Connection helpers ───────────────────────────────────────────────────

async fn tcp_connect(host: &str, port: u16) -> Result<TcpStream> {
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

async fn tls_wrap(
    tcp: TcpStream,
    hostname: &str,
    policy: crate::login_param::CertificateChecks,
) -> Result<TlsStream<TcpStream>> {
    let server_name = ServerName::try_from(hostname.to_owned()).map_err(|e| MailError::Tls {
        host: hostname.to_string(),
        reason: format!("invalid SNI: {e}"),
    })?;

    let connector = build_connector(policy, hostname)?;
    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| MailError::Tls {
            host: hostname.to_string(),
            reason: format!("handshake: {e}"),
        })?;
    Ok(tls)
}

/// Build a [`tokio_rustls::TlsConnector`] per the validation policy.
///
/// The verifier used for [`CertificateChecks::AcceptInvalid`] lives in
/// [`crate::tls_danger`], shared with the SMTP side, so both
/// transports' "insecure mode" behaviour can never drift apart.
fn build_connector(
    policy: crate::login_param::CertificateChecks,
    hostname: &str,
) -> Result<tokio_rustls::TlsConnector> {
    match policy {
        crate::login_param::CertificateChecks::Strict => Ok(tokio_rustls::TlsConnector::from(
            Arc::new(crate::tls_danger::strict_client_config()),
        )),
        crate::login_param::CertificateChecks::AcceptInvalid => {
            tracing::warn!(
                hostname = %hostname,
                "TLS certificate validation disabled for {hostname} — DO NOT USE IN PRODUCTION"
            );
            Ok(tokio_rustls::TlsConnector::from(Arc::new(
                crate::tls_danger::accept_invalid_client_config(),
            )))
        }
    }
}

/// Issue STARTTLS on a plain TCP stream and wrap it in TLS.
async fn starttls_upgrade(
    tcp: TcpStream,
    host: &str,
    account: &Account,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    // Read the greeting.
    let mut buf = BufReader::new(tcp);
    let mut line = String::new();
    buf.read_line(&mut line).await.map_err(MailError::Io)?;
    if !line.starts_with("* OK") && !line.starts_with("* PREAUTH") {
        return Err(MailError::Tls {
            host: host.to_string(),
            reason: format!("unexpected IMAP greeting before STARTTLS: {line:?}"),
        });
    }

    // Send STARTTLS.
    buf.write_all(b"a001 STARTTLS\r\n")
        .await
        .map_err(MailError::Io)?;
    buf.flush().await.map_err(MailError::Io)?;
    line.clear();
    buf.read_line(&mut line).await.map_err(MailError::Io)?;
    if !line.starts_with("a001 OK") {
        return Err(MailError::Tls {
            host: host.to_string(),
            reason: format!("STARTTLS rejected: {line:?}"),
        });
    }

    let tcp = buf.into_inner();
    let sn = ServerName::try_from(host.to_owned()).map_err(|e| MailError::Tls {
        host: host.to_string(),
        reason: format!("invalid SNI: {e}"),
    })?;
    let connector = build_connector(account.certificate_checks, host)?;
    let tls = connector
        .connect(sn, tcp)
        .await
        .map_err(|e| MailError::Tls {
            host: host.to_string(),
            reason: format!("STARTTLS handshake: {e}"),
        })?;
    Ok(tls)
}

/// Read and discard the IMAP greeting (`* OK ...`).
async fn skip_greeting<R>(r: &mut R) -> Result<()>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut line = String::new();
    r.read_line(&mut line).await.map_err(MailError::Io)?;
    if !line.starts_with("* OK") && !line.starts_with("* PREAUTH") {
        return Err(MailError::Tls {
            host: "<greeting>".into(),
            reason: format!("unexpected IMAP greeting: {line:?}"),
        });
    }
    Ok(())
}

/// Decide whether the IMAP port needs implicit TLS or STARTTLS.
///
/// Helper exported so tests can assert the policy decision.
pub fn implicit_tls_for_imap(sec: SocketSecurity) -> bool {
    matches!(sec, SocketSecurity::Tls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_tls_table() {
        assert!(implicit_tls_for_imap(SocketSecurity::Tls));
        assert!(!implicit_tls_for_imap(SocketSecurity::Starttls));
        assert!(!implicit_tls_for_imap(SocketSecurity::Plain));
    }
}
