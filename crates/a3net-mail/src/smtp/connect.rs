//! TLS / STARTTLS / plaintext upgrade for the SMTP transport.
//!
//! Three paths:
//! - [`implicit_tls_transport`] — wrap a fresh TCP stream in TLS (port 465).
//! - [`starttls_transport`]      — wrap in TLS *after* STARTTLS (port 587).
//! - [`plaintext_transport`]     — no TLS (loopback / LAN debug only).
//!
//! Auth is folded into [`crate::smtp::build_transport`].

use async_smtp::response::{Category, Code, Detail};
use async_smtp::{SmtpClient, SmtpTransport};
use std::sync::Arc;
use tokio::io::BufStream;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::error::{MailError, Result};
use crate::login_param::{Account, CertificateChecks};
use crate::smtp::Transport;

/// Marker type so the function signatures stay uniform regardless of
/// which path produced the stream.
#[derive(Debug, Clone, Copy)]
pub enum TlsPolicy {
    /// Implicit TLS on connect (port 465).
    Implicit,
    /// STARTTLS upgrade.
    StartTls,
    /// No TLS (loopback / LAN debug only).
    Plain,
}

pub(crate) async fn implicit_tls_transport(tcp: TcpStream, account: &Account) -> Result<Transport> {
    let server_name = server_name(&account.smtp.server)?;
    let connector = build_connector(account.certificate_checks);
    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| MailError::Tls {
            host: account.smtp.server.clone(),
            reason: format!("implicit TLS handshake: {e}"),
        })?;
    let transport = crate::smtp::build_transport(tls, account).await?;
    Ok(Transport::Tls(Box::new(transport)))
}

pub(crate) async fn starttls_transport(tcp: TcpStream, account: &Account) -> Result<Transport> {
    let server_name = server_name(&account.smtp.server)?;

    // Wrap the TCP stream in BufStream so async-smtp's BufRead bound is met.
    let tcp = BufStream::new(tcp);

    let client = SmtpClient::new().smtp_utf8(true).without_greeting();
    let transport: SmtpTransport<BufStream<TcpStream>> = SmtpTransport::new(client, tcp)
        .await
        .map_err(MailError::Smtp)?;

    let plain_stream = transport
        .starttls()
        .await
        .map_err(|e| MailError::Tls {
            host: account.smtp.server.clone(),
            reason: format!("STARTTLS upgrade: {e}"),
        })?
        .into_inner();

    let connector = build_connector(account.certificate_checks);
    let tls = connector
        .connect(server_name, plain_stream)
        .await
        .map_err(|e| MailError::Tls {
            host: account.smtp.server.clone(),
            reason: format!("STARTTLS handshake: {e}"),
        })?;
    let transport = crate::smtp::build_transport(tls, account).await?;
    Ok(Transport::Tls(Box::new(transport)))
}

pub(crate) async fn plaintext_transport(tcp: TcpStream, account: &Account) -> Result<Transport> {
    if account.certificate_checks == CertificateChecks::Strict {
        // Strict-without-TLS is a contradiction. Insecure plaintext must
        // be opted into explicitly via CertificateChecks::AcceptInvalid.
        return Err(MailError::Config(
            "plaintext SMTP requires CertificateChecks::AcceptInvalid".into(),
        ));
    }
    let transport = crate::smtp::build_transport(tcp, account).await?;
    Ok(Transport::Plain(Box::new(transport)))
}

fn build_connector(policy: CertificateChecks) -> TlsConnector {
    match policy {
        CertificateChecks::Strict => {
            TlsConnector::from(Arc::new(crate::tls_danger::strict_client_config()))
        }
        CertificateChecks::AcceptInvalid => {
            tracing::warn!("TLS certificate validation disabled — DO NOT USE IN PRODUCTION");
            TlsConnector::from(Arc::new(crate::tls_danger::accept_invalid_client_config()))
        }
    }
}

pub(crate) fn server_name(host: &str) -> Result<ServerName<'static>> {
    ServerName::try_from(host.to_string()).map_err(|e| MailError::Tls {
        host: host.to_string(),
        reason: format!("invalid SNI: {e}"),
    })
}

/// Helper used in tests: pick the right category for an SMTP reply
/// code so the error classification in [`crate::smtp::send`] can be
/// unit-tested without going over the wire.
#[allow(dead_code)]
pub(crate) fn classify_5_5_0(code: &Code) -> bool {
    code.category == Category::MailSystem && code.detail == Detail::Zero
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_smtp::response::{Category, Code, Detail, Severity};

    #[test]
    fn classify_5_5_0_special_case() {
        let code = Code::new(
            Severity::PermanentNegativeCompletion,
            Category::MailSystem,
            Detail::Zero,
        );
        assert!(classify_5_5_0(&code));
        let code = Code::new(
            Severity::PermanentNegativeCompletion,
            Category::MailSystem,
            Detail::One,
        );
        assert!(!classify_5_5_0(&code));
    }
}
