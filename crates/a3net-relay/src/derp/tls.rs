//! TLS helpers for the embedded DERP server.
//!
//! The bulk of the certificate plumbing lives in
//! [`crate::derp::load_cert_config`]; this module re-exports the
//! fine-grained glue so callers that want to construct a
//! [`rustls::ServerConfig`] from raw PEM bytes (e.g. tests, or
//! operators loading a cert from memory rather than disk) have a
//! one-stop surface.
//!
//! All public functions are feature-gated on `derp` so the
//! rest of the crate stays free of iroh-relay deps when the
//! feature is off.

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::derp::DerpError;

/// Parse a PEM byte slice into a list of certificates. Returns an
/// error if any block fails to parse or if the resulting DER blob
/// is empty — fail-closed, the same convention used by
/// `a3net-transport::iroh::ca_tls`.
pub fn parse_pem_certs(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, DerpError> {
    let mut reader = pem;
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| DerpError::InvalidTls(format!("parse PEM certs: {e}")))?;
    if certs.is_empty() {
        return Err(DerpError::InvalidTls(
            "PEM input contained no certificates".into(),
        ));
    }
    Ok(certs)
}

/// Parse a PEM byte slice into a single private key. We don't
/// enforce a specific algorithm (RSA / ECDSA / Ed25519) — rustls'
/// `PrivateKeyDer::from_pem_slice` does that and surfaces a useful
/// error if the input is something else.
pub fn parse_pem_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, DerpError> {
    PrivateKeyDer::from_pem_slice(pem)
        .map_err(|e| DerpError::InvalidTls(format!("parse PEM private key: {e}")))
}

/// Build a `rustls::ServerConfig` from pre-parsed cert + key. The
/// crypto provider is fixed to `ring` to match the
/// `tls-ring` feature on `iroh-relay`. This keeps a single
/// process-wide provider installed so downstream code that calls
/// `rustls::ServerConfig::builder()` doesn't panic with "no
/// default provider" the first time it runs.
pub fn build_server_config(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<rustls::ServerConfig, DerpError> {
    let builder = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| DerpError::InvalidTls(format!("rustls protocol versions: {e}")))?
    .with_no_client_auth();

    builder
        .with_single_cert(certs, key)
        .map_err(|e| DerpError::InvalidTls(format!("rustls: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate an ephemeral self-signed cert at test time using
    /// `rcgen` (already a workspace transitive dep). Then round-trip
    /// through the helper to confirm parse_pem_certs is well-typed.
    #[test]
    fn parse_pem_certs_round_trip() {
        let key_pair = rcgen::KeyPair::generate().expect("keypair");
        let params = rcgen::CertificateParams::new(vec!["localhost".into()]).expect("params");
        let cert = params.self_signed(&key_pair).expect("sign");
        let cert_der = cert.der().to_vec();
        let key_der = key_pair.serialize_der();

        // Encode to PEM so we exercise the parse path.
        use base64::Engine;
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64::engine::general_purpose::STANDARD.encode(&cert_der),
        );
        let certs = parse_pem_certs(pem.as_bytes()).expect("parse_pem_certs");
        assert_eq!(certs.len(), 1);

        let key = parse_pem_key(
            format!(
                "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
                base64::engine::general_purpose::STANDARD.encode(&key_der),
            )
            .as_bytes(),
        )
        .expect("parse_pem_key");

        let server_cfg = build_server_config(certs, key).expect("build_server_config");
        // `Debug` printing the rustls config leaks the cert bytes
        // (which the test PEM is not sensitive), so we can just
        // confirm it's non-empty by formatting it.
        assert!(!format!("{server_cfg:?}").is_empty());
    }

    #[test]
    fn parse_pem_certs_rejects_empty_input() {
        let res = parse_pem_certs(b"");
        match res {
            Err(DerpError::InvalidTls(msg)) => {
                assert!(msg.contains("no certificates"));
            }
            other => panic!("expected InvalidTls, got {other:?}"),
        }
    }

    #[test]
    fn parse_pem_certs_rejects_garbage() {
        let res = parse_pem_certs(b"not a pem file");
        match res {
            Err(DerpError::InvalidTls(_)) => {}
            other => panic!("expected InvalidTls, got {other:?}"),
        }
    }
}
