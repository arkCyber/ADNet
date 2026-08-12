//! Shared "accept any TLS certificate" verifier for
//! [`crate::login_param::CertificateChecks::AcceptInvalid`].
//!
//! Aerospace-grade maintainability note: this used to be duplicated
//! verbatim between `smtp/connect.rs` and `imap/mod.rs`. Two
//! hand-copied implementations of a security-critical verifier is a
//! latent inconsistency bug waiting to happen — a future change to
//! one copy (e.g. adding a pinned-cert allowlist) could silently fail
//! to apply to the other, leaving one transport more permissive than
//! intended. There must be exactly one implementation.
//!
//! ⚠️ **Danger**: this verifier accepts *any* certificate, expired,
//! self-signed, or for the wrong hostname. It exists only to support
//! [`crate::login_param::CertificateChecks::AcceptInvalid`], an
//! explicit opt-in for homelab / self-hosted servers. It must never
//! be reachable when [`crate::login_param::CertificateChecks::Strict`]
//! is selected (both call sites enforce this before constructing a
//! [`tokio_rustls::TlsConnector`] from this verifier).

use std::sync::Arc;

use tokio_rustls::rustls;

/// A [`rustls::client::danger::ServerCertVerifier`] that accepts every
/// certificate presented by the server. Only reachable via
/// [`crate::login_param::CertificateChecks::AcceptInvalid`].
#[derive(Debug)]
pub(crate) struct AcceptAnythingVerifier;

impl rustls::client::danger::ServerCertVerifier for AcceptAnythingVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme;
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

/// Build a [`rustls::ClientConfig`] that trusts the system root store
/// (webpki bundled roots). Shared by both the SMTP and IMAP strict
/// paths so the trust anchor set never drifts between them.
pub(crate) fn strict_client_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// Build a [`rustls::ClientConfig`] that accepts any server
/// certificate. Callers must have already confirmed the operator
/// opted into [`crate::login_param::CertificateChecks::AcceptInvalid`]
/// and must log a warning at the call site (kept there so each
/// transport's log carries its own `hostname` context).
pub(crate) fn accept_invalid_client_config() -> rustls::ClientConfig {
    rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnythingVerifier))
        .with_no_client_auth()
}
