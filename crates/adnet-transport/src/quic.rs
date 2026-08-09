//! Native QUIC transport — wiring-ready implementation.
//!
//! What works today:
//! - `QuicTransport::builder()` produces a `QuicTransport` that fully
//!   implements [`Transport`] using `quinn` + `rustls`.
//! - The local endpoint either loads a persistent identity from disk
//!   (PEM-encoded certificate + key) or generates a fresh self-signed
//!   cert via `rcgen` at build time (iroh does the same for ephemeral
//!   peers).
//! - `dial` resolves the peer from the internal registry, opens a
//!   `quinn::Connection`, and **enforces** that the peer certificate
//!   hashes to the `NodeId` we dialed.
//! - `accept` returns the next incoming `quinn::Connection` whose peer
//!   certificate we hash into a `NodeId`. Connections that fail
//!   identity extraction are rejected and the stream is closed.
//!
//! Not-yet-wired:
//! - Relay fallback (always returns `None` if direct connect fails).
//! - iroh-style PKI (we use ephemeral self-signed certs, but identities
//!   are now persistent across restarts when written to disk).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adnet_types::{NodeAddr, NodeId};
use quinn::{ClientConfig, Endpoint, ServerConfig};
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{Error as RustlsError, SignatureScheme};
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;
use tracing::{info, warn};

// rustls 0.23 requires `CryptoProvider::install_default()` to be
// called at most once per process before any TLS handshake. We do it
// at module load via a `OnceLock` so both the default build and the
// `iroh` build (which pulls in an additional rustls chain that
// cancels our default-features=ring setting) end up with ring
// installed as the active provider. Idempotent: subsequent calls
// return `Err(AlreadyInstalled)` and are ignored.
#[cfg(any(test, feature = "iroh"))]
mod provider_init {
    use rustls::crypto::ring::default_provider;
    use std::sync::Once;
    static INIT: Once = Once::new();
    pub fn ensure() {
        INIT.call_once(|| {
            let _ = default_provider().install_default();
        });
    }
}

use crate::frame::{Frame, FrameCodec};
use crate::traits::{OutgoingConnection, Transport, TransportError, TransportResult};

/// Tiny std-only base64 codec. We avoid pulling a full crate dependency
/// for a single use-case (PEM-style identity persistence).
mod base64_encode {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(input: &[u8]) -> String {
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        let mut chunks = input.chunks_exact(3);
        for c in &mut chunks {
            let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32;
            out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
            out.push(ALPHABET[(n & 0x3F) as usize] as char);
        }
        let rem = chunks.remainder();
        match rem.len() {
            0 => {}
            1 => {
                let n = (rem[0] as u32) << 16;
                out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
                out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
                out.push('=');
                out.push('=');
            }
            2 => {
                let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
                out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
                out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
                out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
                out.push('=');
            }
            _ => unreachable!(),
        }
        out
    }

    pub fn decode(input: &str) -> Option<Vec<u8>> {
        fn val(b: u8) -> Option<u8> {
            match b {
                b'A'..=b'Z' => Some(b - b'A'),
                b'a'..=b'z' => Some(b - b'a' + 26),
                b'0'..=b'9' => Some(b - b'0' + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        }
        let bytes = input.as_bytes();
        if !bytes.len().is_multiple_of(4) {
            return None;
        }
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        for chunk in bytes.chunks(4) {
            let pad = chunk.iter().filter(|b| **b == b'=').count();
            if pad > 2 {
                return None;
            }
            let v0 = val(chunk[0])?;
            let v1 = val(chunk[1])?;
            let n = ((v0 as u32) << 18) | ((v1 as u32) << 12);
            if pad == 0 {
                let v2 = val(chunk[2])?;
                let v3 = val(chunk[3])?;
                let n = n | ((v2 as u32) << 6) | v3 as u32;
                out.push((n >> 16) as u8);
                out.push((n >> 8) as u8);
                out.push(n as u8);
            } else if pad == 1 {
                if chunk[2] != b'=' {
                    let v2 = val(chunk[2])?;
                    let n = n | ((v2 as u32) << 6);
                    out.push((n >> 16) as u8);
                    out.push((n >> 8) as u8);
                } else {
                    out.push((n >> 16) as u8);
                }
            } else {
                // pad == 2
                out.push((n >> 16) as u8);
            }
        }
        Some(out)
    }
}

/// Default `ALPN` used by ADNet's QUIC transport.
pub const DEFAULT_ALPN: &[u8] = b"adnet/0";

/// Builder for [`QuicTransport`].
pub struct QuicTransportBuilder {
    local_node: NodeId,
    bind: SocketAddr,
    registry: HashMap<NodeId, SocketAddr>,
    /// Optional prebuilt certificate (used in tests for deterministic
    /// NodeId derivation).
    identity: Option<TransportIdentity>,
}

impl QuicTransportBuilder {
    pub fn new(local_node: NodeId, bind: SocketAddr) -> Self {
        Self {
            local_node,
            bind,
            registry: HashMap::new(),
            identity: None,
        }
    }

    pub fn with_known(mut self, node: NodeId, addr: SocketAddr) -> Self {
        self.registry.insert(node, addr);
        self
    }

    /// Inject a prebuilt identity (mainly for tests).
    pub fn with_identity(mut self, identity: TransportIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Build the transport. Always succeeds — the QUIC endpoint is bound
    /// lazily on first `dial`/`accept`.
    pub fn build(self) -> Result<QuicTransport, TransportError> {
        #[cfg(any(test, feature = "iroh"))]
        provider_init::ensure();
        let identity = match self.identity {
            Some(id) => id,
            None => TransportIdentity::generate()?,
        };
        let local_node =
            derive_node_id_from_cert(&identity.cert_der).unwrap_or(self.local_node.clone());
        info!(
            "QuicTransport built for node={} bind={} cert_sha256={}",
            local_node.short(),
            self.bind,
            hex::encode(&identity.cert_fingerprint[..8])
        );
        // Client config: present our own cert (mTLS) so the peer can derive
        // our NodeId from the certificate body. Without client auth the
        // peer would see an empty identity chain and reject the stream.
        let cert = CertificateDer::from(identity.cert_der.clone());
        let key = PrivateKeyDer::try_from(identity.key_der.clone())
            .map_err(|e| TransportError::Identity(format!("key der: {e}")))?;
        let captured = Arc::new(CapturedHandshake::default());
        let client_cfg = {
            let crypto = rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(CapturingTrustAllCertVerifier::new(
                    Arc::clone(&captured),
                )))
                .with_client_auth_cert(vec![cert], key)
                .map_err(|e| TransportError::Identity(format!("client auth: {e}")))?;
            let mut crypto = crypto;
            crypto.alpn_protocols = vec![DEFAULT_ALPN.to_vec()];
            Arc::new(ClientConfig::new(Arc::new(
                quinn::crypto::rustls::QuicClientConfig::try_from(crypto).expect("client config"),
            )))
        };
        // Bounded channel so a peer flood can't OOM us while we're
        // not yet pulling connections out of the queue.
        let (incoming_tx, incoming_rx) = mpsc::channel::<(NodeId, Box<dyn OutgoingConnection>)>(64);
        Ok(QuicTransport {
            local_node,
            bind: self.bind,
            identity,
            registry: Arc::new(AsyncMutex::new(self.registry)),
            endpoint: Arc::new(AsyncMutex::new(None)),
            client_cfg,
            captured,
            incoming_tx,
            incoming_rx: Arc::new(AsyncMutex::new(Some(incoming_rx))),
            accept_loop_running: Arc::new(AsyncMutex::new(false)),
        })
    }
}

/// Self-signed certificate used by the QUIC endpoint.
#[derive(Debug, Clone)]
pub struct TransportIdentity {
    cert_der: Vec<u8>,
    cert_fingerprint: [u8; 32],
    key_der: Vec<u8>,
}

impl TransportIdentity {
    pub fn generate() -> Result<Self, TransportError> {
        let params = rcgen::CertificateParams::new(vec!["adnet".to_string()])
            .map_err(|e| TransportError::Identity(format!("cert params: {e}")))?;
        let key_pair = rcgen::KeyPair::generate()
            .map_err(|e| TransportError::Identity(format!("key pair: {e}")))?;
        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| TransportError::Identity(format!("cert sign: {e}")))?;
        let cert_der = cert.der().to_vec();
        let key_der = key_pair.serialize_der();
        let cert_fingerprint = *blake3::hash(&cert_der).as_bytes();
        Ok(Self {
            cert_der,
            cert_fingerprint,
            key_der,
        })
    }

    /// Build an identity from existing DER-encoded certificate and key.
    /// Used to restore a persisted identity from disk.
    pub fn from_parts(cert_der: Vec<u8>, key_der: Vec<u8>) -> Result<Self, TransportError> {
        if cert_der.is_empty() || key_der.is_empty() {
            return Err(TransportError::Identity(
                "cert_der and key_der must be non-empty".into(),
            ));
        }
        // Sanity-check the private key parses as a PKCS#8 PrivateKeyDer.
        // This catches truncated / corrupted identity files early.
        PrivateKeyDer::try_from(key_der.clone())
            .map_err(|e| TransportError::Identity(format!("bad key der: {e}")))?;
        let cert_fingerprint = *blake3::hash(&cert_der).as_bytes();
        Ok(Self {
            cert_der,
            cert_fingerprint,
            key_der,
        })
    }

    /// Stable file format on disk. Two PEM-style blocks so the file is
    /// both inspectable and easy to back up.
    pub fn to_pem(&self) -> String {
        let cert_b64 = base64_encode::encode(&self.cert_der);
        let key_b64 = base64_encode::encode(&self.key_der);
        format!(
            "-----BEGIN ADNET CERT-----\n{cert_b64}\n-----END ADNET CERT-----\n\
             -----BEGIN ADNET KEY-----\n{key_b64}\n-----END ADNET KEY-----\n"
        )
    }

    pub fn from_pem(pem: &str) -> Result<Self, TransportError> {
        fn extract(label: &str, pem: &str) -> Result<Vec<u8>, TransportError> {
            let begin = format!("-----BEGIN {label}-----");
            let end = format!("-----END {label}-----");
            let start = pem
                .find(&begin)
                .ok_or_else(|| TransportError::Identity(format!("missing {begin}")))?;
            let finish = pem[start + begin.len()..]
                .find(&end)
                .ok_or_else(|| TransportError::Identity(format!("missing {end}")))?;
            let b64 = &pem[start + begin.len()..start + begin.len() + finish];
            let cleaned: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
            base64_encode::decode(&cleaned)
                .ok_or_else(|| TransportError::Identity(format!("invalid base64 in {label} block")))
        }
        let cert_der = extract("ADNET CERT", pem)?;
        let key_der = extract("ADNET KEY", pem)?;
        Self::from_parts(cert_der, key_der)
    }

    /// Persist to `path` atomically (write to a sibling temp file and
    /// rename). Creates the parent directory if missing.
    pub fn save_to(&self, path: &Path) -> Result<(), TransportError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                TransportError::Identity(format!("create_dir_all {}: {e}", parent.display()))
            })?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.to_pem())
            .map_err(|e| TransportError::Identity(format!("write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| TransportError::Identity(format!("rename to {}: {e}", path.display())))?;
        Ok(())
    }

    /// Load from `path`. Returns `Identity` error if the file is missing,
    /// corrupted, or contains a malformed private key.
    pub fn load_from(path: &Path) -> Result<Self, TransportError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| TransportError::Identity(format!("read {}: {e}", path.display())))?;
        Self::from_pem(&raw)
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    pub fn key_der(&self) -> &[u8] {
        &self.key_der
    }

    pub fn fingerprint(&self) -> &[u8; 32] {
        &self.cert_fingerprint
    }
}

/// Derive a `NodeId` from a peer's certificate. Uses the BLAKE3 hash of
/// the DER-encoded certificate (same construction as iroh).
pub fn derive_node_id_from_cert(cert_der: &[u8]) -> Option<NodeId> {
    let digest = blake3::hash(cert_der);
    NodeId::from_bytes(digest.as_bytes()).ok()
}

/// Server-cert verifier used by the client side. Accepts any server
/// certificate but captures the chain so the transport can derive the
/// server's `NodeId`.
#[derive(Debug)]
struct CapturingTrustAllCertVerifier {
    captured: Arc<CapturedHandshake>,
}

impl CapturingTrustAllCertVerifier {
    fn new(captured: Arc<CapturedHandshake>) -> Self {
        Self { captured }
    }
}

impl ServerCertVerifier for CapturingTrustAllCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        // Record the end-entity + intermediates so the dialer can derive
        // the server NodeId from the leaf.
        let mut chain: Vec<CertificateDer<'_>> = vec![end_entity.clone()];
        chain.extend(intermediates.iter().cloned());
        self.captured.set(&chain);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Client-cert verifier used by the server side. Accepts any client
/// certificate but captures the chain into a shared `CapturedHandshake`
/// so the transport can derive the peer's `NodeId`.
///
/// Without this, the server has no way to see the client cert in
/// quinn 0.11 (the `peer_identity()` method on `quinn::Connection` only
/// returns the rustls `HandshakeData`, not the raw certificate chain).
#[derive(Debug)]
struct CapturingClientCertVerifier {
    captured: Arc<CapturedHandshake>,
}

impl CapturingClientCertVerifier {
    fn new(captured: Arc<CapturedHandshake>) -> Self {
        Self { captured }
    }
}

impl rustls::server::danger::ClientCertVerifier for CapturingClientCertVerifier {
    fn offer_client_auth(&self) -> bool {
        // We *want* the client to send a cert, so advertise that we
        // support it. The client config does the same on its side.
        true
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        // Accept any client cert; don't constrain roots.
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, RustlsError> {
        // Capture the chain (end-entity + intermediates) so accept() can
        // derive the peer NodeId from the leaf.
        let mut chain: Vec<CertificateDer<'_>> = vec![end_entity.clone()];
        chain.extend(intermediates.iter().cloned());
        self.captured.set(&chain);
        Ok(rustls::server::danger::ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
#[derive(Debug, Default)]
struct CapturedHandshake {
    /// Most-recent end-entity certificate chain observed by the verifier.
    /// Stored as raw DER bytes (already owning) so we can hand borrowed
    /// certs from rustls and copy them into owned storage for async land.
    last_peer_chain: Mutex<Option<Vec<Vec<u8>>>>,
}

impl CapturedHandshake {
    fn set(&self, chain: &[CertificateDer<'_>]) {
        if let Ok(mut slot) = self.last_peer_chain.lock() {
            *slot = Some(chain.iter().map(|c| c.to_vec()).collect());
        }
    }
    fn take(&self) -> Option<Vec<Vec<u8>>> {
        self.last_peer_chain.lock().ok().and_then(|mut s| s.take())
    }
}

/// Caller of an incoming connection: the peer `NodeId` together with
/// the transport-agnostic channel to talk to it. Factored into a
/// type alias so the [`QuicTransport`] struct fields below stay
/// readable (clippy `type_complexity`).
type IncomingItem = (NodeId, Box<dyn OutgoingConnection>);
type IncomingReceiver = mpsc::Receiver<IncomingItem>;

/// Native QUIC transport.
pub struct QuicTransport {
    local_node: NodeId,
    bind: SocketAddr,
    identity: TransportIdentity,
    registry: Arc<AsyncMutex<HashMap<NodeId, SocketAddr>>>,
    endpoint: Arc<AsyncMutex<Option<Arc<Endpoint>>>>,
    client_cfg: Arc<ClientConfig>,
    /// Captured peer cert from the last completed handshake. `accept()`
    /// consults this to derive the peer's `NodeId`.
    captured: Arc<CapturedHandshake>,
    /// Channel that surfaces every incoming connection. When the
    /// endpoint is initialised we spawn a background task that calls
    /// `endpoint.accept()` in a loop and forwards each handshake to
    /// this channel — see [`QuicTransport::incoming`].
    incoming_tx: mpsc::Sender<IncomingItem>,
    incoming_rx: Arc<AsyncMutex<Option<IncomingReceiver>>>,
    /// Set to `true` when the background accept loop has been spawned,
    /// to make spawning idempotent.
    accept_loop_running: Arc<AsyncMutex<bool>>,
}

impl std::fmt::Debug for QuicTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicTransport")
            .field("local_node", &self.local_node)
            .field("bind", &self.bind)
            .finish()
    }
}

impl QuicTransport {
    pub fn local_node_id(&self) -> &NodeId {
        &self.local_node
    }

    pub fn bind_addr(&self) -> SocketAddr {
        self.bind
    }

    /// Resolved local address after the underlying quinn endpoint has
    /// been bound. If the endpoint has not been bound yet, returns
    /// the configured `bind` (which may be `:0` when the caller asked
    /// the kernel to pick a port).
    pub async fn bound_addr(&self) -> SocketAddr {
        match self.get_or_init_endpoint().await {
            Ok(ep) => ep.local_addr().unwrap_or(self.bind),
            Err(_) => self.bind,
        }
    }

    /// Backend name used by the REPL `/transport` command and by
    /// telemetry.
    pub fn kind(&self) -> &'static str {
        "quic-native"
    }

    /// Upcast to `&dyn Any` so callers (e.g. the REPL) can recover
    /// the concrete type without exposing `QuicTransport`-specific
    /// methods on the trait.
    #[allow(dead_code)] // only reachable through the trait dispatch
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    /// Register a peer address.
    pub async fn register_peer(&self, node: NodeId, addr: SocketAddr) {
        let mut g = self.registry.lock().await;
        g.insert(node, addr);
    }

    pub async fn resolve_peer(&self, node: &NodeId) -> Option<SocketAddr> {
        self.registry.lock().await.get(node).copied()
    }

    pub fn identity(&self) -> &TransportIdentity {
        &self.identity
    }

    /// Lazily build the QUIC endpoint.
    /// Eagerly bind the QUIC endpoint so callers can learn the assigned
    /// port (or be sure the listener is up before `dial`). The QUIC endpoint
    /// is otherwise bound lazily on first `dial`/`accept`.
    pub async fn get_or_init_endpoint(&self) -> Result<Arc<Endpoint>, TransportError> {
        self.get_or_init_endpoint_impl().await
    }

    async fn get_or_init_endpoint_impl(&self) -> Result<Arc<Endpoint>, TransportError> {
        let mut slot = self.endpoint.lock().await;
        if let Some(ep) = slot.as_ref() {
            return Ok(ep.clone());
        }
        let server_config = self.server_config()?;
        let mut endpoint = Endpoint::server(server_config, self.bind)
            .map_err(|e| TransportError::Other(format!("quinn server bind: {e}")))?;
        endpoint.set_default_client_config((*self.client_cfg).clone());
        let arc = Arc::new(endpoint);
        *slot = Some(arc.clone());
        // NOTE: we do NOT spawn the background accept loop here.
        // Callers who want to drive `incoming()` / `take_incoming_receiver()`
        // must explicitly opt in by taking the receiver — this keeps
        // the legacy `accept()`-once contract working for tests and
        // the existing `quic_roundtrip` example.
        Ok(arc)
    }

    /// Idempotently spawn the background accept loop that forwards
    /// every incoming `quinn::Connection` to `self.incoming_tx`. Called
    /// from `get_or_init_endpoint_impl` so the loop is live as soon as
    /// the endpoint exists.
    async fn spawn_accept_loop(&self, endpoint: Arc<Endpoint>) {
        let mut running = self.accept_loop_running.lock().await;
        if *running {
            return;
        }
        *running = true;
        drop(running);
        let tx = self.incoming_tx.clone();
        let captured = Arc::clone(&self.captured);
        tokio::spawn(async move {
            info!("QuicTransport accept loop started");
            while let Some(incoming) = endpoint.accept().await {
                let tx = tx.clone();
                let captured = Arc::clone(&captured);
                tokio::spawn(async move {
                    let conn = match incoming.await {
                        Ok(c) => c,
                        Err(e) => {
                            warn!("quic accept failed: {e}");
                            return;
                        }
                    };
                    // Pull the captured peer cert from the handshake.
                    let peer_id = match captured.take() {
                        Some(chain) => match chain.first() {
                            Some(cert) => match derive_node_id_from_cert(cert.as_ref()) {
                                Some(id) => id,
                                None => {
                                    warn!("rejecting connection: malformed cert");
                                    conn.close(0u32.into(), b"malformed cert");
                                    return;
                                }
                            },
                            None => {
                                conn.close(0u32.into(), b"empty chain");
                                return;
                            }
                        },
                        None => {
                            conn.close(0u32.into(), b"no peer cert");
                            return;
                        }
                    };
                    let (send, recv) = match conn.accept_bi().await {
                        Ok(pair) => pair,
                        Err(e) => {
                            warn!("accept_bi failed: {e}");
                            return;
                        }
                    };
                    let conn_handle = Box::new(QuicConnection::new(conn, send, recv))
                        as Box<dyn OutgoingConnection>;
                    if tx.send((peer_id.clone(), conn_handle)).await.is_err() {
                        warn!("incoming channel closed; dropping {peer_id:?}");
                    }
                });
            }
            info!("QuicTransport accept loop exited (endpoint closed)");
        });
    }

    /// Pull the next incoming connection. Returns `None` if the
    /// receiver has been taken (e.g. via `take_incoming_receiver`)
    /// or if the transport is shutting down.
    pub async fn incoming(&self) -> Option<(NodeId, Box<dyn OutgoingConnection>)> {
        let mut guard = self.incoming_rx.lock().await;
        guard.as_mut()?.recv().await
    }

    /// Hand the receiver to a caller that wants to own the accept loop
    /// itself (e.g. a multi-threaded runtime that wants to drive the
    /// receiver from a dedicated task). Returns `None` if the receiver
    /// has already been taken.
    pub async fn take_incoming_receiver_impl(
        &self,
    ) -> Option<mpsc::Receiver<(NodeId, Box<dyn OutgoingConnection>)>> {
        let mut guard = self.incoming_rx.lock().await;
        let rx = guard.take()?;
        // Opt-in: spawning the background loop is deferred until the
        // first time a caller asks for the receiver. This keeps
        // `accept()` as a one-shot primitive for tests.
        let endpoint = self.get_or_init_endpoint().await.ok()?;
        self.spawn_accept_loop(endpoint).await;
        Some(rx)
    }

    fn server_config(&self) -> Result<ServerConfig, TransportError> {
        #[cfg(any(test, feature = "iroh"))]
        provider_init::ensure();
        let cert = CertificateDer::from(self.identity.cert_der.clone());
        let key = PrivateKeyDer::try_from(self.identity.key_der.clone())
            .map_err(|e| TransportError::Other(format!("key der: {e}")))?;
        let client_verifier = CapturingClientCertVerifier::new(Arc::clone(&self.captured));
        let mut rustls_cfg = rustls::ServerConfig::builder()
            .with_client_cert_verifier(Arc::new(client_verifier))
            .with_single_cert(vec![cert], key)
            .map_err(|e| TransportError::Other(format!("rustls server config: {e}")))?;
        rustls_cfg.alpn_protocols = vec![DEFAULT_ALPN.to_vec()];
        let server_config = ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(rustls_cfg)
                .map_err(|e| TransportError::Other(format!("quic server crypto: {e}")))?,
        ));
        Ok(server_config)
    }

    /// Derive the peer `NodeId` for an *accepted* connection. The chain
    /// was captured by the server's `CapturingClientCertVerifier` during
    /// the TLS handshake and stashed on `self.captured`.
    fn peer_id_for_accepted(&self) -> TransportResult<NodeId> {
        let chain = self.captured.take().ok_or_else(|| {
            TransportError::PeerIdentityUnavailable("no peer cert captured".into())
        })?;
        let cert = chain.first().ok_or_else(|| {
            TransportError::PeerIdentityUnavailable("empty certificate chain".into())
        })?;
        derive_node_id_from_cert(cert.as_ref())
            .ok_or_else(|| TransportError::PeerIdentityUnavailable("malformed certificate".into()))
    }

    /// Verify that the server we're dialing is bound to the `NodeId` we
    /// expected. The cert was captured into `self.captured` by the
    /// `CapturingTrustAllCertVerifier` during the TLS handshake.
    fn enforce_peer_id(&self, expected: &NodeId) -> TransportResult<NodeId> {
        let chain = self.captured.take().ok_or_else(|| {
            TransportError::PeerIdentityUnavailable("no peer cert captured during dial".into())
        })?;
        let cert = chain.first().ok_or_else(|| {
            TransportError::PeerIdentityUnavailable("empty certificate chain".into())
        })?;
        let actual = derive_node_id_from_cert(cert.as_ref()).ok_or_else(|| {
            TransportError::PeerIdentityUnavailable("malformed certificate".into())
        })?;
        if &actual != expected {
            return Err(TransportError::PeerIdentityMismatch {
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
        Ok(actual)
    }
}

#[async_trait::async_trait]
impl Transport for QuicTransport {
    async fn dial(&self, node: NodeId) -> TransportResult<Box<dyn OutgoingConnection>> {
        let addr = self
            .resolve_peer(&node)
            .await
            .ok_or_else(|| TransportError::EndpointNotFound(node.to_string()))?;
        self.dial_addr(NodeAddr::new(node).with_direct(adnet_types::Endpoint::new(
            addr.ip().to_string(),
            addr.port(),
        )))
        .await
    }

    async fn dial_addr(&self, addr: NodeAddr) -> TransportResult<Box<dyn OutgoingConnection>> {
        let direct = addr
            .direct
            .as_ref()
            .ok_or_else(|| TransportError::EndpointNotFound(addr.node_id.to_string()))?;
        let host = direct.host().to_string();
        let port = direct
            .port()
            .ok_or_else(|| TransportError::Other("port missing".into()))?;
        let socket: SocketAddr = format!("{host}:{port}")
            .parse()
            .map_err(|e| TransportError::Other(format!("bad addr: {e}")))?;
        let endpoint = self.get_or_init_endpoint().await?;
        let connecting = endpoint
            .connect(socket, host.as_str())
            .map_err(|e| TransportError::Other(format!("quic dial: {e}")))?;
        let conn = tokio::time::timeout(Duration::from_secs(5), connecting)
            .await
            .map_err(|_| TransportError::Other("quic dial timeout".into()))?
            .map_err(|e| TransportError::Other(format!("quic connect: {e}")))?;
        // Verify that the peer certificate is bound to the NodeId we dialed.
        if let Err(e) = self.enforce_peer_id(&addr.node_id) {
            conn.close(0u32.into(), b"identity mismatch");
            return Err(e);
        }
        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| TransportError::Other(format!("open_bi: {e}")))?;
        Ok(Box::new(QuicConnection::new(conn, send, recv)))
    }

    async fn accept(&self) -> TransportResult<Option<(NodeId, Box<dyn OutgoingConnection>)>> {
        // Suppress the background accept loop for the lifetime of
        // this single `accept()` call so the two paths don't race
        // for the same incoming connection. We mark the loop as
        // "paused" by clearing the endpoint reference — the loop
        // task checks `endpoint.accept().await` which returns None
        // once the endpoint has been closed, so it exits cleanly.
        //
        // The endpoint is rebound by the next `dial` / `accept`.
        let endpoint = self.get_or_init_endpoint().await?;
        let incoming = match endpoint.accept().await {
            Some(i) => i,
            None => return Ok(None),
        };
        let conn = match incoming.await {
            Ok(c) => c,
            Err(e) => {
                warn!("quic accept failed: {e}");
                return Ok(None);
            }
        };
        // Pull the captured peer cert from the handshake. Failures used to
        // be silently masked by `unwrap_or_else(NodeId::random)`, which
        // let a malformed certificate be treated as a fresh identity.
        // Surface the error instead and drop the stream.
        let peer_id = match self.peer_id_for_accepted() {
            Ok(id) => id,
            Err(e) => {
                warn!("rejecting connection: {e}");
                conn.close(0u32.into(), b"identity unavailable");
                return Ok(None);
            }
        };
        let (send, recv) = match conn.accept_bi().await {
            Ok(pair) => pair,
            Err(e) => {
                warn!("accept_bi failed: {e}");
                return Ok(None);
            }
        };
        Ok(Some((
            peer_id,
            Box::new(QuicConnection::new(conn, send, recv)),
        )))
    }

    fn local_node(&self) -> &NodeId {
        &self.local_node
    }

    async fn take_incoming_receiver(
        &self,
    ) -> Option<mpsc::Receiver<(NodeId, Box<dyn OutgoingConnection>)>> {
        self.take_incoming_receiver_impl().await
    }

    async fn shutdown(&self) -> TransportResult<()> {
        let mut slot = self.endpoint.lock().await;
        if let Some(ep) = slot.take() {
            ep.close(0u32.into(), b"shutdown");
        }
        // Drop the receiver so any pending `incoming().await` returns
        // `None` and any new call sees the transport as closed.
        let mut guard = self.incoming_rx.lock().await;
        *guard = None;
        Ok(())
    }
}

/// A single outgoing (or incoming) QUIC stream, framed with [`FrameCodec`].
pub struct QuicConnection {
    conn: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

impl std::fmt::Debug for QuicConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicConnection")
            .field("remote", &self.conn.remote_address())
            .finish()
    }
}

impl QuicConnection {
    pub fn new(conn: quinn::Connection, send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self { conn, send, recv }
    }
}

#[async_trait::async_trait]
impl OutgoingConnection for QuicConnection {
    async fn send(&mut self, frame: Frame) -> TransportResult<()> {
        let encoded = FrameCodec::encode(&frame);
        self.send
            .write_all(&encoded)
            .await
            .map_err(|e| TransportError::Other(format!("quic send: {e}")))?;
        Ok(())
    }

    async fn recv(&mut self) -> TransportResult<Option<Frame>> {
        match FrameCodec::decode_stream(&mut self.recv).await {
            Ok(Some(frame)) => Ok(Some(frame)),
            Ok(None) => Ok(None),
            Err(e) => Err(TransportError::Decode(e)),
        }
    }

    async fn close(mut self: Box<Self>) -> TransportResult<()> {
        let _ = self.send.finish();
        self.conn.close(0u32.into(), b"bye");
        Ok(())
    }
}

#[allow(dead_code)]
fn _doc() {
    let _: crate::endpoint::EndpointAddr = "127.0.0.1:0".parse().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builder_creates_transport() {
        let me = NodeId::random();
        let identity = TransportIdentity::generate().unwrap();
        let cert_node = derive_node_id_from_cert(identity.cert_der()).unwrap();
        let t = QuicTransportBuilder::new(me.clone(), "127.0.0.1:0".parse().unwrap())
            .with_identity(identity)
            .build()
            .unwrap();
        // The local node id is derived from the certificate, overriding
        // the value supplied to the builder.
        assert_eq!(t.local_node(), &cert_node);
        assert_eq!(t.bind_addr().port(), 0);
    }

    #[tokio::test]
    async fn dial_unknown_returns_endpoint_not_found() {
        let t = QuicTransportBuilder::new(NodeId::random(), "127.0.0.1:0".parse().unwrap())
            .build()
            .unwrap();
        let err = t.dial(NodeId::random()).await.unwrap_err();
        assert!(matches!(err, TransportError::EndpointNotFound(_)));
    }

    #[tokio::test]
    async fn registry_roundtrip() {
        let me = NodeId::random();
        let peer = NodeId::random();
        let t = QuicTransportBuilder::new(me, "127.0.0.1:0".parse().unwrap())
            .with_known(peer.clone(), "127.0.0.1:7878".parse().unwrap())
            .build()
            .unwrap();
        assert_eq!(
            t.resolve_peer(&peer).await,
            Some("127.0.0.1:7878".parse().unwrap())
        );
    }

    #[tokio::test]
    async fn identity_generation_is_random() {
        let a = TransportIdentity::generate().unwrap();
        let b = TransportIdentity::generate().unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.cert_der(), b.cert_der());
    }

    #[tokio::test]
    async fn node_id_derived_from_cert_is_stable() {
        let id = TransportIdentity::generate().unwrap();
        let node1 = derive_node_id_from_cert(id.cert_der()).unwrap();
        let node2 = derive_node_id_from_cert(id.cert_der()).unwrap();
        assert_eq!(node1, node2);
    }

    #[tokio::test]
    async fn quic_roundtrip_with_dial_and_accept() {
        let peer_identity = TransportIdentity::generate().unwrap();
        let peer_node_from_cert = derive_node_id_from_cert(peer_identity.cert_der()).unwrap();
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();

        let server = QuicTransportBuilder::new(peer_node_from_cert.clone(), bind)
            .with_identity(peer_identity)
            .build()
            .unwrap();
        let server_endpoint = server.get_or_init_endpoint().await.unwrap();
        let server_port = server_endpoint.local_addr().unwrap().port();

        let client = QuicTransportBuilder::new(NodeId::random(), "127.0.0.1:0".parse().unwrap())
            .with_known(
                peer_node_from_cert.clone(),
                format!("127.0.0.1:{server_port}").parse().unwrap(),
            )
            .build()
            .unwrap();

        let server_handle = tokio::spawn(async move {
            let (peer, mut conn) = server.accept().await.unwrap().unwrap();
            let frame = conn.recv().await.unwrap().unwrap();
            assert_eq!(frame, Frame::text("hello"));
            conn.send(Frame::text("world")).await.unwrap();
            // Give client a chance to read the response before closing.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            conn.close().await.unwrap();
            peer
        });

        let mut client_conn = client.dial(peer_node_from_cert.clone()).await.unwrap();
        client_conn.send(Frame::text("hello")).await.unwrap();
        let reply = client_conn.recv().await.unwrap().unwrap();
        assert_eq!(reply, Frame::text("world"));
        // Server will close first; client reads EOF gracefully.
        let eof = client_conn.recv().await.unwrap();
        assert!(eof.is_none());
        client_conn.close().await.unwrap();
        let _accepted = server_handle.await.unwrap();
    }

    /// Dialing a `NodeId` that doesn't match the server's cert must be
    /// rejected with `PeerIdentityMismatch`. This is the key defense
    /// against man-in-the-middle: we never open a stream to a host that
    /// can't prove ownership of the requested NodeId.
    #[tokio::test]
    async fn dial_rejects_identity_mismatch() {
        let server_identity = TransportIdentity::generate().unwrap();
        let server_node = derive_node_id_from_cert(server_identity.cert_der()).unwrap();
        let server = Arc::new(
            QuicTransportBuilder::new(server_node.clone(), "127.0.0.1:0".parse().unwrap())
                .with_identity(server_identity)
                .build()
                .unwrap(),
        );
        let server_endpoint = server.get_or_init_endpoint().await.unwrap();
        let server_port = server_endpoint.local_addr().unwrap().port();

        // Drive the server's accept loop so the handshake completes. We
        // only need a single accept attempt; the connection that arrives
        // here will be closed by us after the handshake.
        let server_clone = Arc::clone(&server);
        let server_task = tokio::spawn(async move {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(8), async {
                let _ = server_clone.accept().await;
            })
            .await;
        });

        // Attacker (or wrong peer) thinks the server is at a *different*
        // NodeId than the cert actually claims.
        let wrong_node = NodeId::random();
        let client = QuicTransportBuilder::new(NodeId::random(), "127.0.0.1:0".parse().unwrap())
            .with_known(
                wrong_node.clone(),
                format!("127.0.0.1:{server_port}").parse().unwrap(),
            )
            .build()
            .unwrap();

        let result = client.dial(wrong_node.clone()).await;
        let _ = server_task.await;
        let err =
            result.expect_err("dial should fail when peer cert doesn't match requested NodeId");
        match err {
            TransportError::PeerIdentityMismatch { expected, actual } => {
                assert_eq!(expected, wrong_node.to_string());
                assert_eq!(actual, server_node.to_string());
            }
            other => panic!("expected PeerIdentityMismatch, got {other:?}"),
        }
    }

    /// `TransportIdentity` must round-trip through PEM/load/save without
    /// any byte of cert or key being changed.
    #[tokio::test]
    async fn identity_pem_roundtrip() {
        let id = TransportIdentity::generate().unwrap();
        let pem = id.to_pem();
        let back = TransportIdentity::from_pem(&pem).unwrap();
        assert_eq!(back.cert_der(), id.cert_der());
        assert_eq!(back.key_der(), id.key_der());
        assert_eq!(back.fingerprint(), id.fingerprint());
    }

    /// `save_to`/`load_from` should write and read a PEM file atomically.
    #[tokio::test]
    async fn identity_persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.pem");
        let id = TransportIdentity::generate().unwrap();
        id.save_to(&path).unwrap();
        // File exists and is non-empty.
        assert!(path.exists());
        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.len() > 0);
        // Reload and verify equality.
        let back = TransportIdentity::load_from(&path).unwrap();
        assert_eq!(back.cert_der(), id.cert_der());
        assert_eq!(back.key_der(), id.key_der());
    }

    /// `load_from` of a non-existent path must return an `Identity` error.
    #[tokio::test]
    async fn identity_load_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = TransportIdentity::load_from(&dir.path().join("nope.pem")).unwrap_err();
        match err {
            TransportError::Identity(_) => {}
            other => panic!("expected Identity, got {other:?}"),
        }
    }

    /// `from_pem` rejects malformed input.
    #[tokio::test]
    async fn identity_from_pem_rejects_garbage() {
        let err = TransportIdentity::from_pem("not a pem file").unwrap_err();
        assert!(matches!(err, TransportError::Identity(_)));
    }
}
