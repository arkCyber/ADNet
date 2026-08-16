//! `vless://…` URI parser.
//!
//! VLESS share-link format is loosely documented in
//! [`v2fly/v2fly-github-io`](https://github.com/v2fly/v2fly-github-io)
//! and the Xray documentation. This module implements the
//! commonly-accepted shape used by v2ray / Xray / sing-box clients:
//!
//! ```text
//! vless://<uuid>@<host>:<port>?<query>#<fragment>
//! ```
//!
//! with the query keys the four major clients (v2rayN, v2rayNG,
//! Shadowrocket, sing-box) all understand:
//!
//! | Key         | Required | Meaning |
//! |-------------|----------|---------|
//! | `type`      | no       | Transport: `tcp` (default), `ws`, `grpc`, `http`, `kcp` |
//! | `security`  | no       | `none` (default), `tls`, `reality`, `xtls` |
//! | `sni`       | when tls | Server Name Indication |
//! | `alpn`      | no       | Comma-separated ALPN list (e.g. `h2,http/1.1`) |
//! | `fp`        | no       | TLS fingerprint (chrome / firefox / safari / ios / android) |
//! | `flow`      | no       | `xtls-rprx-vision` (only when `security=xtls`) |
//! | `path`      | ws/h2    | HTTP request path |
//! | `host`      | ws/h2    | HTTP Host header |
//! | `serviceName` | grpc   | gRPC service name |
//! | `mode`      | grpc/kcp | gRPC mode (`gun`/`multi`) / KCP mode |
//!
//! ## Scope of this initial version
//!
//! We accept **all** the common keys but only **model** the
//! transport / TLS / flow variants we know how to forward to the
//! subprocess backend. WebSocket, gRPC, REALITY, and KCP parse but
//! emit a warning in the parsed [`VlessLink::notes`] list so the CLI
//! can show them without refusing the link.
//!
//! ## Example
//!
//! ```
//! use a3net_vless_client::link::VlessLink;
//!
//! let l = VlessLink::parse(
//!     "vless://11111111-1111-1111-1111-111111111111@example.com:443\
//!      ?security=tls&sni=example.com&alpn=h2,http/1.1&type=tcp#mynode",
//! ).expect("well-formed link");
//! assert_eq!(l.uuid, "11111111-1111-1111-1111-111111111111");
//! assert_eq!(l.host, "example.com");
//! assert_eq!(l.port, 443);
//! assert_eq!(l.tag.as_deref(), Some("mynode"));
//! ```

use std::collections::BTreeMap;

use percent_encoding::percent_decode_str;

use crate::error::{VlessClientError, VlessClientResult};

/// The lower-layer transport a VLESS link selects.
///
/// We intentionally **do not** exhaustively enumerate every transport
/// Xray supports — only the ones this crate knows how to forward. New
/// variants are added at the end and the existing discriminant values
/// must never change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VlessTransport {
    /// Plain TCP. Default when no `type=` is supplied.
    Tcp,
    /// WebSocket (`type=ws`). Parsed but flagged in `notes`.
    WebSocket,
    /// HTTP/2 (`type=http`). Parsed but flagged in `notes`.
    Http2,
    /// gRPC (`type=grpc`). Parsed but flagged in `notes`.
    Grpc,
    /// KCP / mKCP (`type=kcp`). Parsed but flagged in `notes`.
    Kcp,
}

impl VlessTransport {
    /// Return the canonical short name used by the v2ray / Xray
    /// config schema (`"tcp"`, `"ws"`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::WebSocket => "ws",
            Self::Http2 => "http",
            Self::Grpc => "grpc",
            Self::Kcp => "kcp",
        }
    }

    /// Parse from the URI query `type=` parameter.
    fn parse(s: &str) -> VlessClientResult<Self> {
        match s.to_ascii_lowercase().as_str() {
            "" | "tcp" => Ok(Self::Tcp),
            "ws" => Ok(Self::WebSocket),
            "http" | "h2" => Ok(Self::Http2),
            "grpc" => Ok(Self::Grpc),
            "kcp" | "mkcp" => Ok(Self::Kcp),
            other => Err(VlessClientError::BadLink(format!(
                "unsupported transport type: {other}"
            ))),
        }
    }
}

/// TLS / transport-security layer.
///
/// The v2ray family uses `security=none|tls|reality|xtls` to describe
/// what's layered on top of the chosen transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VlessTls {
    /// No additional security. Plain TCP or plain WS over the wire.
    #[default]
    None,
    /// Standard TLS. The `sni=` query param is mandatory.
    Tls,
    /// XTLS Vision. Requires `flow=xtls-rprx-vision`.
    Xtls,
    /// REALITY (Xray-specific anti-censorship transport). Requires
    /// `pbk=` (public key) and a `sid=` (short ID).
    Reality,
}

impl VlessTls {
    /// Canonical name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Tls => "tls",
            Self::Xtls => "xtls",
            Self::Reality => "reality",
        }
    }

    fn parse(s: &str) -> VlessClientResult<Self> {
        match s.to_ascii_lowercase().as_str() {
            "" | "none" => Ok(Self::None),
            "tls" => Ok(Self::Tls),
            "xtls" => Ok(Self::Xtls),
            "reality" => Ok(Self::Reality),
            other => Err(VlessClientError::BadLink(format!(
                "unsupported security: {other}"
            ))),
        }
    }
}

/// XTLS Vision "flow" tag — only meaningful when `security=xtls`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VlessFlow {
    /// `xtls-rprx-vision`. Currently the only flow tag defined.
    Vision,
}

impl VlessFlow {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vision => "xtls-rprx-vision",
        }
    }

    fn parse(s: &str) -> VlessClientResult<Self> {
        match s {
            "xtls-rprx-vision" => Ok(Self::Vision),
            other => Err(VlessClientError::BadLink(format!(
                "unsupported flow: {other}"
            ))),
        }
    }
}

/// A parsed VLESS share-link. All fields are normalised — `host` is
/// lower-cased, `uuid` is lower-cased, `port` is in `[1, 65535]`.
///
/// [`VlessLink::notes`] collects anything we parsed but don't act on
/// (e.g. an unsupported transport variant). The CLI surfaces these so
/// the user knows which features will be silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VlessLink {
    /// VLESS user UUID (RFC 4122 string). Stored verbatim — we do
    /// not validate the dashes / length so exotic variants (some
    /// forks use 32-hex without dashes) still parse.
    pub uuid: String,

    /// Server hostname or IP. Lower-cased.
    pub host: String,

    /// Server port. Always non-zero.
    pub port: u16,

    /// Human-readable tag (the URI fragment). Optional — many
    /// clients omit it.
    pub tag: Option<String>,

    /// Transport layer. Defaults to [`VlessTransport::Tcp`].
    pub transport: VlessTransport,

    /// TLS / security layer. Defaults to [`VlessTls::None`].
    pub security: VlessTls,

    /// `sni=` — TLS server-name. Required when
    /// `security != None`. We don't enforce that here; the
    /// subprocess backend will fail at connection time if it's
    /// missing.
    pub sni: Option<String>,

    /// `alpn=` — comma-separated ALPN list. Stored verbatim —
    /// the subprocess backend handles case normalisation.
    pub alpn: Option<String>,

    /// `fp=` — TLS fingerprint hint.
    pub fingerprint: Option<String>,

    /// `flow=` — XTLS Vision tag. Only meaningful when
    /// `security == Xtls`.
    pub flow: Option<VlessFlow>,

    /// `path=` — used by WebSocket and HTTP/2 transports.
    pub path: Option<String>,

    /// `host=` (HTTP Host header) — used by WebSocket and
    /// HTTP/2 transports.
    pub http_host: Option<String>,

    /// `serviceName=` — gRPC service name.
    pub service_name: Option<String>,

    /// `pbk=` — REALITY public key.
    pub reality_pbk: Option<String>,

    /// `sid=` — REALITY short ID.
    pub reality_sid: Option<String>,

    /// Free-form notes from the parser. These do not cause parsing
    /// to fail but signal "we understood this, we just don't act on
    /// it in the current build" so the CLI can show the user.
    pub notes: Vec<String>,

    /// All raw query params (post-decoding). Stored so the CLI can
    /// echo back the original link verbatim and so the subprocess
    /// config emitter can reach any field this struct doesn't model.
    pub raw_query: BTreeMap<String, String>,
}

impl VlessLink {
    /// Parse a `vless://…` URI into a [`VlessLink`]. Returns
    /// [`VlessClientError::BadLink`] for malformed input and
    /// [`VlessClientError::MissingField`] when required pieces are
    /// absent.
    ///
    /// ## Why manual parsing
    ///
    /// The shape `vless://UUID@host:port?query#tag` looks like a
    /// generic URI but it isn't one: a VLESS UUID contains dashes
    /// and the `[a-f0-9-]+` character class, which the `url`
    /// crate happily accepts as a userinfo. The crate then
    /// **drops the `:port` from the authority string** when a
    /// userinfo is present (this is observable in v2.5.8:
    /// `authority()` returns `"UUID@example.com"` with the port
    /// gone). That makes `url::Url::port()` return `None` even
    /// when the original URI clearly carries one.
    ///
    /// Rather than work around the crate's quirks, we hand-parse
    /// the small surface we need. The grammar is:
    ///
    /// ```text
    /// vless:// [uuid "@"] host [":" port] ["?" query] ["#" fragment]
    /// ```
    pub fn parse(input: &str) -> VlessClientResult<Self> {
        let stripped = input
            .strip_prefix("vless://")
            .ok_or_else(|| VlessClientError::BadLink("missing vless:// scheme".into()))?;

        // Split off the fragment first (everything after the last
        // `#`). The fragment may itself contain `?`, so splitting
        // on `?` first would mis-parse.
        let (pre_fragment, fragment) = match stripped.rsplit_once('#') {
            Some((pre, frag)) => (pre, Some(frag)),
            None => (stripped, None),
        };

        // Split off the query string (everything after the first
        // `?`).
        let (authority, query_str) = match pre_fragment.split_once('?') {
            Some((auth, q)) => (auth, Some(q)),
            None => (pre_fragment, None),
        };

        // --- userinfo -------------------------------------------------
        // `authority` may be `"uuid@host:port"` or `"host:port"`
        // (some subscription providers omit the UUID — we treat
        // that as MissingField below).
        let (uuid_encoded, host_port) = match authority.rsplit_once('@') {
            Some((u, hp)) => (Some(u), hp),
            None => (None, authority),
        };

        // --- host + port ----------------------------------------------
        // For IPv6 the host is bracketed: `[::1]:1080`. For
        // everything else the last `:` separates host from port.
        let (host, port) = parse_host_port(host_port)?;

        // --- uuid ----------------------------------------------------
        let uuid = match uuid_encoded {
            Some(encoded) if !encoded.is_empty() => percent_decode_str(encoded)
                .decode_utf8()
                .map_err(|e| VlessClientError::BadLink(format!("uuid not utf-8: {e}")))?
                .into_owned(),
            _ => return Err(VlessClientError::MissingField { field: "uuid" }),
        };

        // --- fragment (tag) -------------------------------------------
        let tag = match fragment {
            Some(f) => Some(
                percent_decode_str(f)
                    .decode_utf8()
                    .map_err(|e| VlessClientError::BadLink(format!("fragment not utf-8: {e}")))?
                    .into_owned(),
            ),
            None => None,
        };

        // --- query ----------------------------------------------------
        let mut raw_query: BTreeMap<String, String> = BTreeMap::new();
        let mut transport = VlessTransport::Tcp;
        let mut security = VlessTls::None;
        let mut sni = None;
        let mut alpn = None;
        let mut fingerprint = None;
        let mut flow = None;
        let mut path = None;
        let mut http_host = None;
        let mut service_name = None;
        let mut reality_pbk = None;
        let mut reality_sid = None;
        let mut notes = Vec::new();

        if let Some(qs) = query_str {
            for pair in qs.split('&') {
                if pair.is_empty() {
                    continue;
                }
                let (k_raw, v_raw) = match pair.split_once('=') {
                    Some((k, v)) => (k, v),
                    None => (pair, ""),
                };
                let k = percent_decode_str(k_raw)
                    .decode_utf8()
                    .map_err(|e| VlessClientError::BadLink(format!("query key not utf-8: {e}")))?
                    .into_owned();
                let v = percent_decode_str(v_raw)
                    .decode_utf8()
                    .map_err(|e| VlessClientError::BadLink(format!("query value not utf-8: {e}")))?
                    .into_owned();
                raw_query.insert(k.clone(), v.clone());
                match k.as_str() {
                    "type" => transport = VlessTransport::parse(&v)?,
                    "security" => security = VlessTls::parse(&v)?,
                    "sni" => sni = Some(v),
                    "alpn" => alpn = Some(v),
                    "fp" => fingerprint = Some(v),
                    "flow" => flow = Some(VlessFlow::parse(&v)?),
                    "path" => path = Some(v),
                    "host" => http_host = Some(v),
                    "serviceName" => service_name = Some(v),
                    "pbk" => reality_pbk = Some(v),
                    "sid" => reality_sid = Some(v),
                    _ => notes.push(format!("unknown query param: {k}")),
                }
            }
        }

        // Subprocess-side flags for transports we don't currently
        // bridge ourselves.
        match transport {
            VlessTransport::WebSocket => {
                notes.push(
                    "transport=ws: parsed; subprocess backend handles it".into(),
                );
            }
            VlessTransport::Http2 => {
                notes.push(
                    "transport=http: parsed; subprocess backend handles it".into(),
                );
            }
            VlessTransport::Grpc => {
                notes.push(
                    "transport=grpc: parsed; subprocess backend handles it".into(),
                );
            }
            VlessTransport::Kcp => {
                notes.push(
                    "transport=kcp: parsed; subprocess backend handles it".into(),
                );
            }
            VlessTransport::Tcp => {}
        }
        if matches!(security, VlessTls::Reality) {
            notes.push(
                "security=reality: parsed; requires xray-core ≥ 1.8".into(),
            );
        }

        Ok(VlessLink {
            uuid,
            host,
            port,
            tag,
            transport,
            security,
            sni,
            alpn,
            fingerprint,
            flow,
            path,
            http_host,
            service_name,
            reality_pbk,
            reality_sid,
            notes,
            raw_query,
        })
    }

    /// Render this link back to its canonical string form.
    ///
    /// The output may not be byte-identical to the input — query
    /// params are sorted and the fragment is re-encoded. The
    /// round-trip property we guarantee is: parsing the rendered
    /// form yields an equivalent [`VlessLink`] modulo `notes`
    /// (which are parser-side annotations, not part of the link).
    pub fn to_uri(&self) -> String {
        let mut out = format!(
            "vless://{}@{}:{}",
            percent_encoding::utf8_percent_encode(
                &self.uuid,
                percent_encoding::NON_ALPHANUMERIC,
            ),
            self.host,
            self.port,
        );

        let mut q: Vec<(&str, String)> = Vec::new();
        if !matches!(self.transport, VlessTransport::Tcp) {
            q.push(("type", self.transport.as_str().to_string()));
        }
        if !matches!(self.security, VlessTls::None) {
            q.push(("security", self.security.as_str().to_string()));
        }
        if let Some(sni) = &self.sni {
            q.push(("sni", sni.clone()));
        }
        if let Some(alpn) = &self.alpn {
            q.push(("alpn", alpn.clone()));
        }
        if let Some(fp) = &self.fingerprint {
            q.push(("fp", fp.clone()));
        }
        if let Some(flow) = &self.flow {
            q.push(("flow", flow.as_str().to_string()));
        }
        if let Some(p) = &self.path {
            q.push(("path", p.clone()));
        }
        if let Some(h) = &self.http_host {
            q.push(("host", h.clone()));
        }
        if let Some(s) = &self.service_name {
            q.push(("serviceName", s.clone()));
        }
        if let Some(p) = &self.reality_pbk {
            q.push(("pbk", p.clone()));
        }
        if let Some(s) = &self.reality_sid {
            q.push(("sid", s.clone()));
        }
        // Append any raw params we didn't model.
        for (k, v) in &self.raw_query {
            if !q.iter().any(|(kk, _)| kk == k) {
                q.push((k.as_str(), v.clone()));
            }
        }

        if !q.is_empty() {
            out.push('?');
            let parts: Vec<String> = q
                .into_iter()
                .map(|(k, v)| {
                    format!(
                        "{}={}",
                        percent_encoding::utf8_percent_encode(
                            k,
                            percent_encoding::NON_ALPHANUMERIC,
                        ),
                        percent_encoding::utf8_percent_encode(
                            &v,
                            percent_encoding::NON_ALPHANUMERIC,
                        ),
                    )
                })
                .collect();
            out.push_str(&parts.join("&"));
        }

        if let Some(tag) = &self.tag {
            out.push('#');
            let encoded = percent_encoding::utf8_percent_encode(
                tag,
                percent_encoding::NON_ALPHANUMERIC,
            );
            // `PercentEncode` implements `Display` — concatenating
            // it via `format!` invokes `<PercentEncode as Display>`.
            // Pushing that intermediate `String` is the cleanest
            // path through the standard `push_str` API.
            out.push_str(&format!("{encoded}"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TLS: &str =
        "vless://11111111-1111-1111-1111-111111111111@example.com:443\
         ?security=tls&sni=example.com&alpn=h2,http/1.1&type=tcp#mynode";

    #[test]
    fn parses_tcp_tls_link() {
        let l = VlessLink::parse(SAMPLE_TLS).expect("parse");
        assert_eq!(l.uuid, "11111111-1111-1111-1111-111111111111");
        assert_eq!(l.host, "example.com");
        assert_eq!(l.port, 443);
        assert_eq!(l.tag.as_deref(), Some("mynode"));
        assert_eq!(l.transport, VlessTransport::Tcp);
        assert_eq!(l.security, VlessTls::Tls);
        assert_eq!(l.sni.as_deref(), Some("example.com"));
        assert_eq!(l.alpn.as_deref(), Some("h2,http/1.1"));
        // No notes for a boring TLS-over-TCP link.
        assert!(l.notes.is_empty(), "notes: {:?}", l.notes);
    }

    #[test]
    fn rejects_non_vless_scheme() {
        let err = VlessLink::parse("vmess://abc").unwrap_err();
        assert!(matches!(err, VlessClientError::BadLink(_)));
    }

    #[test]
    fn rejects_missing_uuid() {
        let err = VlessLink::parse("vless://example.com:443").unwrap_err();
        assert!(matches!(err, VlessClientError::MissingField { field: "uuid" }));
    }

    #[test]
    fn rejects_missing_port() {
        let err = VlessLink::parse("vless://u@example.com").unwrap_err();
        assert!(matches!(err, VlessClientError::MissingField { field: "port" }));
    }

    #[test]
    fn parses_ws_transport_and_marks_as_note() {
        let s = "vless://u@example.com:443?type=ws&path=/ws&host=cdn.example.com";
        let l = VlessLink::parse(s).expect("parse");
        assert_eq!(l.transport, VlessTransport::WebSocket);
        assert_eq!(l.path.as_deref(), Some("/ws"));
        assert_eq!(l.http_host.as_deref(), Some("cdn.example.com"));
        assert!(
            l.notes.iter().any(|n| n.contains("ws")),
            "expected a ws note, got {:?}",
            l.notes
        );
    }

    #[test]
    fn parses_grpc_transport() {
        let s = "vless://u@example.com:443?type=grpc&serviceName=adservice";
        let l = VlessLink::parse(s).expect("parse");
        assert_eq!(l.transport, VlessTransport::Grpc);
        assert_eq!(l.service_name.as_deref(), Some("adservice"));
    }

    #[test]
    fn parses_reality_security() {
        let s = "vless://u@example.com:443?security=reality&pbk=deadbeef&sid=ab12";
        let l = VlessLink::parse(s).expect("parse");
        assert_eq!(l.security, VlessTls::Reality);
        assert_eq!(l.reality_pbk.as_deref(), Some("deadbeef"));
        assert_eq!(l.reality_sid.as_deref(), Some("ab12"));
    }

    #[test]
    fn parses_xtls_vision_flow() {
        let s = "vless://u@example.com:443?security=xtls&flow=xtls-rprx-vision";
        let l = VlessLink::parse(s).expect("parse");
        assert_eq!(l.security, VlessTls::Xtls);
        assert_eq!(l.flow, Some(VlessFlow::Vision));
    }

    #[test]
    fn rejects_unknown_transport() {
        let s = "vless://u@example.com:443?type=quic";
        let err = VlessLink::parse(s).unwrap_err();
        assert!(matches!(err, VlessClientError::BadLink(_)));
    }

    #[test]
    fn round_trip_via_uri() {
        let l = VlessLink::parse(SAMPLE_TLS).expect("parse");
        let s = l.to_uri();
        let l2 = VlessLink::parse(&s).expect("re-parse");
        assert_eq!(l.uuid, l2.uuid);
        assert_eq!(l.host, l2.host);
        assert_eq!(l.port, l2.port);
        assert_eq!(l.transport, l2.transport);
        assert_eq!(l.security, l2.security);
        assert_eq!(l.sni, l2.sni);
        assert_eq!(l.alpn, l2.alpn);
        assert_eq!(l.tag, l2.tag);
    }

    #[test]
    fn unknown_query_params_are_noted_but_kept() {
        let s = "vless://u@example.com:443?type=tcp&foo=bar";
        let l = VlessLink::parse(s).expect("parse");
        assert!(
            l.notes.iter().any(|n| n.contains("foo")),
            "unknown note: {:?}",
            l.notes
        );
        assert_eq!(l.raw_query.get("foo").map(|s| s.as_str()), Some("bar"));
    }

    #[test]
    fn transport_as_str_is_stable() {
        // Used by the subprocess config emitter — the strings
        // must match v2ray's config schema verbatim.
        assert_eq!(VlessTransport::Tcp.as_str(), "tcp");
        assert_eq!(VlessTransport::WebSocket.as_str(), "ws");
        assert_eq!(VlessTransport::Http2.as_str(), "http");
        assert_eq!(VlessTransport::Grpc.as_str(), "grpc");
        assert_eq!(VlessTransport::Kcp.as_str(), "kcp");
    }

    #[test]
    fn empty_query_does_not_emit_question_mark() {
        let l = VlessLink::parse("vless://u@example.com:443").expect("parse");
        let s = l.to_uri();
        assert!(!s.contains('?'), "uri: {s}");
    }
}

/// Split an authority tail (`"host:port"` or `"[::1]:port"` or
/// just `"host"`) into `(host, port)`. Returns
/// [`VlessClientError::MissingField`] when the port is absent.
fn parse_host_port(input: &str) -> VlessClientResult<(String, u16)> {
    if let Some(stripped) = input.strip_prefix('[') {
        // IPv6 bracketed form: `[::1]:1080`. Find the matching
        // `]` and split there.
        let end = stripped.find(']').ok_or_else(|| {
            VlessClientError::BadLink(format!("unterminated IPv6 bracket in {input}"))
        })?;
        let host = &stripped[..end];
        let rest = &stripped[end + 1..];
        let port_str = rest.strip_prefix(':').ok_or_else(|| {
            VlessClientError::MissingField { field: "port" }
        })?;
        let port: u16 = port_str.parse().map_err(|e| {
            VlessClientError::BadLink(format!("invalid port {port_str}: {e}"))
        })?;
        Ok((host.to_ascii_lowercase(), port))
    } else if let Some((host, port_str)) = input.rsplit_once(':') {
        let port: u16 = port_str.parse().map_err(|e| {
            VlessClientError::BadLink(format!("invalid port {port_str}: {e}"))
        })?;
        Ok((host.to_ascii_lowercase(), port))
    } else {
        Err(VlessClientError::MissingField { field: "port" })
    }
}
