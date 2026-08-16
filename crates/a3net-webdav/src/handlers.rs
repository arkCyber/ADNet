//! Verb dispatcher. The verb-→-handler map is the single audit
//! choke-point: every state-changing verb here produces an
//! audit record via `Nas::put/mkcol/delete/rename/copy`.

use std::collections::BTreeMap;

use a3net_blobstore::{
    AuditContext, Clock, Entry, NamespaceRead, NamespaceWrite, Nas, PathSegments, SystemClock,
};
use a3net_pairing::CapabilitySet;
use a3net_types::ContentHash;
use thiserror::Error;

use crate::acl::{AclDecision, AclMiddleware, CapabilityResolver, StaticCapabilityResolver};
use crate::props::{multistatus_xml, Depth};
use crate::token::{CapabilityToken, TokenVerifier};

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("unauthorised")]
    Unauthorised,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl HttpError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Unauthorised => 401,
            Self::Forbidden(_) => 403,
            Self::NotFound(_) => 404,
            Self::Conflict(_) => 409,
            Self::BadRequest(_) => 400,
            Self::Internal(_) => 500,
        }
    }
}

/// Pagination metadata returned alongside a paginated PROPFIND response.
#[derive(Debug, Clone)]
pub struct PaginationMeta {
    /// Number of items skipped.
    pub offset: usize,
    /// Page size requested.
    pub limit: usize,
    /// Total number of items in the result set.
    pub total: usize,
    /// Whether more items exist beyond this page.
    pub has_more: bool,
}

/// Handler state — shared by every verb. The `clock` is injected
/// for determinism (DAL-A SR-20).
pub struct HandlerState {
    pub nas: Nas,
    pub acl: AclMiddleware<dyn CapabilityResolver>,
    pub static_resolver: Option<StaticCapabilityResolver>,
    pub verifier: TokenVerifier,
    pub clock: Box<dyn Clock>,
}

impl HandlerState {
    pub fn new(
        nas: Nas,
        resolver: std::sync::Arc<dyn CapabilityResolver>,
        verifier: TokenVerifier,
    ) -> Self {
        Self {
            nas,
            acl: AclMiddleware::new(resolver),
            static_resolver: None,
            verifier,
            clock: Box::new(SystemClock),
        }
    }

    pub fn with_static_resolver(mut self, r: StaticCapabilityResolver) -> Self {
        self.static_resolver = Some(r);
        self
    }

    pub fn with_clock(mut self, c: Box<dyn Clock>) -> Self {
        self.clock = c;
        self
    }

    fn verify(&self, header: Option<&str>) -> Result<CapabilityToken, HttpError> {
        let h = header.ok_or(HttpError::Unauthorised)?;
        let token = CapabilityToken::from_header(h)
            .map_err(|_| HttpError::Unauthorised)?;
        self.verifier.verify(&token).map_err(|_| HttpError::Unauthorised)?;
        Ok(token)
    }

    /// Verify the token + check revocation + check expiry + replay
    /// protection + capability for `verb`. Returns the resolved
    /// capability set on success.
    fn verify_and_authorise(
        &self,
        verb: &str,
        header: Option<&str>,
    ) -> Result<a3net_pairing::CapabilitySet, HttpError> {
        let token = self.verify(header)?;
        let resolved = self
            .acl
            .resolver()
            .resolve(&token.capability_id)
            .map_err(|_| HttpError::Unauthorised)?;
        if resolved.revoked {
            return Err(HttpError::Forbidden("credential revoked".into()));
        }
        if resolved.expires_unix_ms < self.clock.unix_ms() {
            return Err(HttpError::Forbidden("credential expired".into()));
        }
        if token.expires_unix_ms < self.clock.unix_ms() {
            return Err(HttpError::Forbidden("token expired".into()));
        }
        // SR-14: replay.
        if let Some(r) = &self.static_resolver {
            if r.is_nonce_seen(&token.capability_id, &token.nonce) {
                return Err(HttpError::Forbidden("replayed nonce".into()));
            }
            r.record_nonce(&token.capability_id, token.nonce);
        }
        let decision = self.acl.authorise(
            Some(&token.capability_id),
            verb,
            &resolved.caps,
        );
        if !matches!(decision, AclDecision::Allow) {
            return Err(forbidden_from(decision));
        }
        Ok(resolved.caps)
    }

    fn audit(&self, capability_id: Option<String>, note: Option<String>) -> AuditContext {
        AuditContext {
            capability_id,
            note,
        }
    }

    pub fn options(&self) -> String {
        // DAL-A advertise the verbs we support.
        "DAV: 1, 2\r\nAllow: OPTIONS, HEAD, GET, PROPFIND, PUT, MKCOL, DELETE, MOVE, COPY\r\n".to_string()
    }

    pub fn handle_get(&self, path: &PathSegments, auth: Option<&str>) -> Result<Vec<u8>, HttpError> {
        self.verify_and_authorise("get", auth)?;
        match self.nas.lookup(path) {
            Some(Entry::File { hash: _, size_bytes: _, .. }) => {
                // Read the full blob via the store.
                self.nas
                    .read_file(path)
                    .map_err(map_ns)
            }
            Some(Entry::Directory { .. }) => Err(HttpError::BadRequest("not a file".into())),
            None => Err(HttpError::NotFound(path.to_string())),
        }
    }

    /// Like [`handle_get`] but serves a sub-range of the file (HTTP Range).
    ///
    /// `range` is inclusive, i.e. `(0, 99)` means bytes 0-99 (first 100 bytes).
    /// Returns `(body, start, end, total_length)` on success.
    pub fn handle_get_range(
        &self,
        path: &PathSegments,
        range: (u64, u64),
        auth: Option<&str>,
    ) -> Result<(Vec<u8>, u64, u64, u64), HttpError> {
        self.verify_and_authorise("get", auth)?;
        let body = self.nas.read_file(path).map_err(map_ns)?;
        let total = body.len() as u64;
        let (start, end) = (range.0.min(total - 1), range.1.min(total - 1));
        if start >= total {
            return Err(HttpError::BadRequest("range not satisfiable".into()));
        }
        let slice = body[(start as usize)..=(end as usize)].to_vec();
        Ok((slice, start, end, total))
    }

    pub fn handle_propfind(
        &self,
        path: &PathSegments,
        auth: Option<&str>,
        offset: Option<usize>,
        limit: Option<usize>,
        depth: Depth,
    ) -> Result<(String, PaginationMeta), HttpError> {
        self.verify_and_authorise("propfind", auth)?;
        let offset = offset.unwrap_or(0);
        let limit = limit.unwrap_or(1000).min(10000);
        let mut items: Vec<(String, Entry)> = Vec::new();
        self.collect(path, path, 0, depth, &mut items);
        let total = items.len();
        let page = items.into_iter().skip(offset).take(limit);
        let refs: Vec<(String, Entry)> = page.collect();
        let meta = PaginationMeta {
            offset,
            limit,
            total,
            has_more: offset + refs.len() < total,
        };
        Ok((multistatus_xml(&refs.iter().map(|(h, e)| (h.clone(), e)).collect::<Vec<_>>()), meta))
    }

    fn collect(
        &self,
        root: &PathSegments,
        current: &PathSegments,
        depth: usize,
        max_depth: Depth,
        out: &mut Vec<(String, Entry)>,
    ) {
        // Check if we've reached the depth limit
        match max_depth {
            Depth::Zero => {
                // Only include the resource itself
                if depth > 0 {
                    return;
                }
            }
            Depth::One => {
                // Include resource and immediate children (depth 0 and 1)
                if depth > 1 {
                    return;
                }
            }
            Depth::Infinity => {
                // No limit (within reasonable bounds)
                if depth > 64 {
                    return;
                }
            }
            Depth::None => {
                // Include nothing
                return;
            }
        }

        let snap = self.nas.snapshot();
        let entry = walk(&snap.root, current);
        if let Some(e) = entry {
            out.push((format_root(root, current), e.clone()));
            if let Entry::Directory { children } = e {
                for name in children.keys() {
                    let mut child_path = current.0.clone();
                    child_path.push(name.clone());
                    self.collect(root, &PathSegments(child_path), depth + 1, max_depth, out);
                }
            }
        }
    }

    /// Register a hash+size in the manifest without writing bytes.
    /// Only safe when the blob is already present in the store
    /// (e.g. dedup, or a caller that wrote bytes out-of-band via
    /// [`Self::handle_put_body`]). Kept for callers/tests that manage
    /// blob storage themselves; the transport layer (`server.rs`)
    /// always uses [`Self::handle_put_body`] so uploaded bytes are
    /// actually persisted.
    pub fn handle_put(
        &self,
        path: &PathSegments,
        body_hash: ContentHash,
        size: u64,
        auth: Option<&str>,
        user_agent: Option<String>,
    ) -> Result<(), HttpError> {
        let token = self.verify(auth)?;
        self.verify_and_authorise("put", auth)?;
        let ctx = self.audit(
            Some(token.capability_id.clone()),
            user_agent,
        );
        let quota = a3net_blobstore::NoopQuota;
        self.nas
            .put(path, body_hash, size, &ctx, &*self.clock, &quota)
            .map_err(map_ns)?;
        Ok(())
    }

    /// Full PUT: writes `body` into the content-addressed blob store,
    /// then registers the resulting hash+size in the manifest. If the
    /// caller supplied an `X-Content-Hash` header, the computed hash
    /// must match it (SR-15 audit integrity) or the request is
    /// rejected as a bad request rather than silently accepted under
    /// the wrong name.
    pub fn handle_put_body(
        &self,
        path: &PathSegments,
        body: &[u8],
        expected_hash: Option<ContentHash>,
        auth: Option<&str>,
        user_agent: Option<String>,
    ) -> Result<(), HttpError> {
        let token = self.verify(auth)?;
        self.verify_and_authorise("put", auth)?;
        let (hash, size) = self
            .nas
            .write_bytes(body)
            .map_err(map_ns)?;
        if let Some(expected) = expected_hash
            && expected != hash {
                return Err(HttpError::BadRequest(
                    "X-Content-Hash does not match uploaded body".into(),
                ));
            }
        let ctx = self.audit(
            Some(token.capability_id.clone()),
            user_agent,
        );
        let quota = a3net_blobstore::NoopQuota;
        self.nas
            .put(path, hash, size, &ctx, &*self.clock, &quota)
            .map_err(map_ns)?;
        Ok(())
    }

    pub fn handle_mkcol(
        &self,
        path: &PathSegments,
        auth: Option<&str>,
        user_agent: Option<String>,
    ) -> Result<(), HttpError> {
        let token = self.verify(auth)?;
        self.verify_and_authorise("mkcol", auth)?;
        let ctx = self.audit(Some(token.capability_id.clone()), user_agent);
        self.nas
            .mkcol(path, &ctx, &*self.clock)
            .map_err(map_ns)?;
        Ok(())
    }

    pub fn handle_delete(
        &self,
        path: &PathSegments,
        auth: Option<&str>,
        user_agent: Option<String>,
    ) -> Result<(), HttpError> {
        let token = self.verify(auth)?;
        self.verify_and_authorise("delete", auth)?;
        let ctx = self.audit(Some(token.capability_id.clone()), user_agent);
        self.nas
            .delete(path, &ctx, &*self.clock)
            .map_err(|_| HttpError::Internal("delete failed".into()))?;
        Ok(())
    }

    pub fn handle_move(
        &self,
        from: &PathSegments,
        to: &PathSegments,
        overwrite: bool,
        auth: Option<&str>,
        user_agent: Option<String>,
    ) -> Result<(), HttpError> {
        let token = self.verify(auth)?;
        self.verify_and_authorise("move", auth)?;
        let ctx = self.audit(Some(token.capability_id.clone()), user_agent);
        self.nas
            .rename(from, to, overwrite, &ctx, &*self.clock)
            .map_err(map_ns)?;
        Ok(())
    }

    pub fn handle_copy(
        &self,
        from: &PathSegments,
        to: &PathSegments,
        overwrite: bool,
        auth: Option<&str>,
        user_agent: Option<String>,
    ) -> Result<(), HttpError> {
        let token = self.verify(auth)?;
        self.verify_and_authorise("copy", auth)?;
        let ctx = self.audit(Some(token.capability_id.clone()), user_agent);
        self.nas
            .copy(from, to, overwrite, &ctx, &*self.clock)
            .map_err(map_ns)?;
        Ok(())
    }
}

fn forbidden_from(d: AclDecision) -> HttpError {
    match d {
        AclDecision::Unauthenticated => HttpError::Unauthorised,
        AclDecision::Forbidden(s) | AclDecision::Rejected(s) => HttpError::Forbidden(s),
        AclDecision::Allow => HttpError::Internal("logic bug".into()),
    }
}

fn map_ns(e: a3net_blobstore::NamespaceError) -> HttpError {
    use a3net_blobstore::NamespaceError::*;
    match e {
        Traversal(s) => HttpError::BadRequest(s),
        DepthExceeded { .. } => HttpError::BadRequest("depth exceeded".into()),
        PathTooLong { .. } => HttpError::BadRequest("path too long".into()),
        TooManyChildren { .. } => HttpError::Conflict("too many children".into()),
        NotFound(s) => HttpError::NotFound(s),
        NotADirectory(s) => HttpError::Conflict(s),
        IsADirectory(s) => HttpError::Conflict(s),
        QuotaExhausted { .. } => HttpError::Conflict("quota exhausted".into()),
        ManifestCorrupt(s) => HttpError::Internal(s),
        AuditFailed(s) => HttpError::Internal(s),
        Io(s) => HttpError::Internal(s.to_string()),
        PoisonRecovered => HttpError::Internal("poison recovered".into()),
        Cancelled => HttpError::Internal("cancelled".into()),
        Unimplemented(s) => HttpError::Internal(format!("unimplemented: {}", s)),
        TrashCapacity { max, size } => HttpError::Conflict(format!(
            "trash capacity exceeded: max {max} bytes, trying to add {size} bytes"
        )),
    }
}

fn walk<'a>(entry: &'a Entry, path: &PathSegments) -> Option<&'a Entry> {
    let mut current = entry;
    for seg in &path.0 {
        current = match current {
            Entry::Directory { children } => children.get(seg)?,
            _ => return None,
        };
    }
    Some(current)
}

fn format_root(_root: &PathSegments, current: &PathSegments) -> String {
    if current.0.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", current.0.join("/"))
    }
}

// Suppress unused-import warnings when only some items are used in
// non-default builds.
#[allow(dead_code)]
fn _suppress(c: BTreeMap<String, Entry>, _cs: CapabilitySet) -> BTreeMap<String, Entry> {
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use a3net_blobstore::Nas;
    use a3net_pairing::capability::Capability;
    use crate::acl::{ResolvedCapability, StaticCapabilityResolver};

    struct MockClock(i64);
    impl a3net_blobstore::Clock for MockClock {
        fn unix_ms(&self) -> i64 { self.0 }
    }

    fn state_for(caps: CapabilitySet) -> (HandlerState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let nas = Nas::open(dir.path()).unwrap();
        let r = StaticCapabilityResolver::new();
        r.register(
            "cred-1".to_string(),
            ResolvedCapability {
                caps,
                nonce: [0u8; 32],
                expires_unix_ms: i64::MAX,
                revoked: false,
            },
        );
        let verifier = TokenVerifier::new([3u8; 32]);
        let mut s = HandlerState::new(nas, Arc::new(r), verifier);
        s.static_resolver = Some(StaticCapabilityResolver::new());
        s.clock = Box::new(MockClock(1_700_000_000_000));
        (s, dir)
    }

    fn token_for(state: &HandlerState, verb_cap: Capability, expires: i64) -> String {
        let nonce = [0u8; 32];
        let token = state.verifier.sign("cred-1", nonce, expires);
        state.acl.resolver().resolve("cred-1").unwrap();        let _ = verb_cap;
        token.to_header()
    }

    #[test]
    fn put_with_read_token_rejected() {
        let (s, _dir) = state_for(CapabilitySet::from_names(["files.read"]));
        let path = PathSegments::decode_http("/a.bin").unwrap();
        let h = ContentHash::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let header = token_for(&s, Capability::FILES_WRITE, i64::MAX);
        let err = s
            .handle_put(&path, h, 1, Some(&header), Some("ua".into()))
            .unwrap_err();
        match err {
            HttpError::Forbidden(_) => {}
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }

    #[test]
    fn put_with_write_token_succeeds() {
        let (s, _dir) = state_for(CapabilitySet::from_names(["files.read", "files.write"]));
        let path = PathSegments::decode_http("/a.bin").unwrap();
        let h = ContentHash::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let header = token_for(&s, Capability::FILES_WRITE, i64::MAX);
        s.handle_put(&path, h, 1024, Some(&header), Some("ua".into()))
            .unwrap();
    }
}
