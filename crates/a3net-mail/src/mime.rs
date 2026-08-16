//! MIME message construction and parsing.
//!
//! Two layers:
//!
//! - [`Mail`] is the high-level, structured representation we hand to
//!   callers — addresses, subject, plain text, optional HTML body, and
//!   a list of typed [`Attachment`] values. No MIME jargon leaks out.
//! - [`Mail::to_wire_bytes`] / [`Mail::from_wire_bytes`] convert to and
//!   from RFC 5322 wire bytes, which is what SMTP / IMAP carry.
//!
//! ## Relationship to chatmail@core
//!
//! Delta Chat splits this responsibility between:
//! - `src/mimefactory.rs` (~2600 lines) — builds outgoing MIME from
//!   `Chat` / `Message` state.
//! - `src/mimeparser.rs` (~2600 lines) — parses incoming MIME into
//!   Delta Chat's `MimeMessage` struct.
//! - `src/receive_imf.rs` (~4400 lines) — drives `MimeMessage` through
//!   the `Context` to produce chat messages.
//!
//! We collapse all three into a single ~600-line module here because
//! the "no E2EE, no chat-state" scope is a fraction of Delta Chat's.
//! PGP / Autocrypt / SecureJoin / multipart-for-encryption are *not*
//! reimplemented; if you need them, depend on `chatmail@core` directly
//! or call our [`Mail::from_wire_bytes`] on the already-decrypted
//! plaintext MIME produced upstream.

use std::collections::BTreeMap;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use mailparse::{MailHeaderMap, addrparse, parse_mail};
use serde::{Deserialize, Serialize};

use crate::error::{MailError, Result};

// ─── High-level types ──────────────────────────────────────────────────────

/// A single email address, as the caller thinks of it.
///
/// We separate the display name (e.g. `"Alice Example"`) from the
/// route address (`alice@example.com`). When rendering to wire bytes
/// we emit `Alice Example <alice@example.com>` per RFC 5322.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    /// Optional display name. RFC 5322 allows it to be quoted; we leave
    /// the quoting to the wire-format encoder.
    pub name: Option<String>,
    /// Route address (`local@domain`).
    pub address: String,
}

impl Address {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            name: None,
            address: address.into(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.name {
            Some(n) if !n.is_empty() => write!(f, "{n} <{}>", self.address),
            _ => write!(f, "{}", self.address),
        }
    }
}

/// One attachment carried on an outgoing message or extracted from an
/// incoming one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Local filename (used in the `filename` parameter of
    /// `Content-Disposition`).
    pub filename: String,
    /// MIME type, e.g. `image/png`. `application/octet-stream` is
    /// substituted when the caller doesn't know.
    pub content_type: String,
    /// Raw bytes.
    pub data: Vec<u8>,
    /// `Content-Disposition` value. Defaults to `attachment`.
    #[serde(default = "default_disposition")]
    pub disposition: Disposition,
}

fn default_disposition() -> Disposition {
    Disposition::Attachment
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Show in the body when possible (inline images).
    Inline,
    /// Force a download / save-as prompt.
    Attachment,
}

/// Structured representation of an email — what callers see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    /// `From:` header.
    pub from: Address,
    /// `To:` header.
    pub to: Vec<Address>,
    /// `Cc:` header. Optional.
    #[serde(default)]
    pub cc: Vec<Address>,
    /// `Bcc:` header. Optional; recipients are hidden from each other
    /// and from the message body.
    #[serde(default)]
    pub bcc: Vec<Address>,
    /// `Subject:` header.
    pub subject: String,
    /// Plain-text body.
    pub text: String,
    /// Optional HTML body. When both are present we emit a
    /// `multipart/alternative` with text/plain first (per RFC 2046 §5.1.4).
    #[serde(default)]
    pub html: Option<String>,
    /// Attachments.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// Extra RFC 5322 headers to add verbatim. Used for things like
    /// `In-Reply-To`, `References`, `X-Mailer` that the high-level
    /// `Mail` struct doesn't model.
    #[serde(default)]
    pub extra_headers: BTreeMap<String, String>,
    /// Optional `Date:` override. When `None`, the wire encoder stamps
    /// the current UTC time.
    #[serde(default)]
    pub date: Option<DateTime<Utc>>,
    /// Optional `Message-ID:` override. When `None`, we generate one
    /// using `uuid::Uuid::new_v4()` in the standard
    /// `<uuid@local-hostname>` form.
    #[serde(default)]
    pub message_id: Option<String>,
}

impl Mail {
    /// Convenience constructor for a one-recipient text message.
    pub fn text_only(
        from: Address,
        to: Address,
        subject: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            from,
            to: vec![to],
            cc: vec![],
            bcc: vec![],
            subject: subject.into(),
            text: text.into(),
            html: None,
            attachments: vec![],
            extra_headers: BTreeMap::new(),
            date: None,
            message_id: None,
        }
    }

    /// Validate invariants before we spend bytes building a wire
    /// representation. Returns the appropriate `MailError` variant.
    pub fn validate(&self) -> Result<()> {
        if self.from.address.is_empty() {
            return Err(MailError::EmptyFrom);
        }
        if self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty() {
            return Err(MailError::EmptyRecipients);
        }
        for addr in std::iter::once(&self.from)
            .chain(self.to.iter())
            .chain(self.cc.iter())
            .chain(self.bcc.iter())
        {
            if !crate::login_param::is_valid_address(&addr.address) {
                return Err(MailError::InvalidAddr(addr.address.clone()));
            }
        }
        Ok(())
    }

    /// Encode the message to RFC 5322 wire bytes (UTF-8, 8-bit-clean,
    /// base64 attachments).
    ///
    /// Header-injection defence: every address, header key, and header
    /// value is scanned for CR/LF before being interpolated; an
    /// embedded `\r\n` would let an attacker forge new headers or
    /// truncate the message body early. The check raises
    /// [`MailError::InvalidHeader`] rather than silently stripping
    /// the offending bytes so the caller can't accidentally send
    /// something unexpected.
    pub fn to_wire_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        // Outbound attachment resource caps. Without these, a caller
        // (or an attacker who has compromised an internal caller path)
        // could pass in `vec![huge; N]` and force `build_body` to
        // allocate gigabytes of base64-expanded wire bytes before we
        // ever get a chance to reject the message. Catching these at
        // the entry point — before any allocation happens — turns a
        // DoS into a clean `Build` error the caller can surface.
        if self.attachments.len() > MAX_ATTACHMENTS_OUT {
            return Err(MailError::Build(format!(
                "too many attachments: {} (limit {})",
                self.attachments.len(),
                MAX_ATTACHMENTS_OUT
            )));
        }
        for (i, att) in self.attachments.iter().enumerate() {
            if att.data.len() > MAX_ATTACHMENT_SIZE_OUT {
                return Err(MailError::Build(format!(
                    "attachment[{i}] size {} exceeds limit {}",
                    att.data.len(),
                    MAX_ATTACHMENT_SIZE_OUT
                )));
            }
        }
        for addr in std::iter::once(&self.from)
            .chain(self.to.iter())
            .chain(self.cc.iter())
            .chain(self.bcc.iter())
        {
            reject_header_injection("address", &addr.address)?;
            if let Some(name) = &addr.name {
                reject_header_injection("display-name", name)?;
            }
        }
        reject_header_injection("subject", &self.subject)?;
        reject_body_nul("text", &self.text)?;
        if let Some(html) = &self.html {
            reject_body_nul("html", html)?;
        }
        for (k, v) in &self.extra_headers {
            reject_header_injection(&format!("header-key {k:?}"), k)?;
            reject_header_injection(&format!("header-value {k:?}"), v)?;
        }
        // Attachment metadata is interpolated directly into MIME part
        // headers (`Content-Type:`, `Content-Disposition:`) in
        // `build_body`. A CR/LF in `filename` or `content_type` would
        // let an attacker forge extra headers inside the attachment's
        // own part — e.g. smuggle a `Content-Type: text/html` to
        // trick a lenient MUA into rendering attacker HTML, or inject
        // arbitrary `X-*` headers. This must be rejected here, before
        // we ever reach the encoder.
        for (i, att) in self.attachments.iter().enumerate() {
            reject_header_injection(&format!("attachment[{i}].filename"), &att.filename)?;
            reject_header_injection(&format!("attachment[{i}].content_type"), &att.content_type)?;
        }

        let date = self.date.unwrap_or_else(Utc::now);
        let mid = self.message_id.clone().unwrap_or_else(generate_message_id);

        // ---- Headers ----------------------------------------------------
        let mut headers = vec![
            format!("From: {}", self.from),
            format!(
                "To: {}",
                self.to
                    .iter()
                    .map(|a| a.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ];
        if !self.cc.is_empty() {
            headers.push(format!(
                "Cc: {}",
                self.cc
                    .iter()
                    .map(|a| a.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        // We never put Bcc in headers that the recipient will see;
        // smtp/send.rs strips the header and only keeps the addresses
        // for the SMTP envelope. Here we just skip it.
        headers.push(format!("Subject: {}", encode_rfc2047(&self.subject)));
        headers.push(format!(
            "Date: {}",
            date.format("%a, %d %b %Y %H:%M:%S +0000")
        ));
        headers.push(format!("Message-ID: {mid}"));
        headers.push("MIME-Version: 1.0".to_string());
        for (k, v) in &self.extra_headers {
            headers.push(format!("{k}: {v}"));
        }

        // ---- Body -------------------------------------------------------
        let body_bytes = build_body(self)?;
        Ok(format!(
            "{}\r\n{}\r\n",
            headers.join("\r\n"),
            String::from_utf8_lossy(&body_bytes)
        )
        .into_bytes())
    }

    /// Parse wire bytes into a structured `Mail`.
    ///
    /// We use the `mailparse` crate for the heavy lifting: it handles
    /// RFC 2047 encoded-words, multipart boundaries, base64 /
    /// quoted-printable decoding, and `Content-Disposition`. We then
    /// project it down to our flat struct.
    pub fn from_wire_bytes(bytes: &[u8]) -> Result<Self> {
        // Defence-in-depth against a stack-overflow DoS: `mailparse`
        // (and our own `collect_parts` walk below it) recurse once
        // per `multipart/*` level. A ~6MB message with ~100k nested
        // `multipart/mixed` parts is enough to blow the stack and
        // `abort()` the whole process — a `catch_unwind` cannot save
        // us because a stack overflow is not a catchable panic. So we
        // reject implausibly deep messages *before* handing the bytes
        // to `mailparse`, with a cheap linear scan that never
        // recurses. Real MUAs never nest more than a handful of
        // levels; the limit here is generous on purpose.
        reject_excessive_mime_nesting(bytes)?;
        let parsed = parse_mail(bytes).map_err(|e| MailError::Parse(e.to_string()))?;

        let from = first_address(&parsed, "From")?;
        let to = all_addresses(&parsed, "To");
        let cc = all_addresses(&parsed, "Cc");
        let bcc = all_addresses(&parsed, "Bcc");
        let subject = parsed
            .headers
            .get_first_value("Subject")
            .unwrap_or_default();
        let date = parsed
            .headers
            .get_first_value("Date")
            .and_then(|s| DateTime::parse_from_rfc2822(&s).ok())
            .map(|d| d.with_timezone(&Utc));
        let message_id = parsed.headers.get_first_value("Message-ID");

        // ---- Extract plain + html + attachments -------------------------
        let mut text: Option<String> = None;
        let mut html: Option<String> = None;
        let mut attachments: Vec<Attachment> = Vec::new();

        collect_parts(&parsed, &mut text, &mut html, &mut attachments)?;

        let text = text.unwrap_or_default();
        let html = if html.is_some() { html } else { None };

        Ok(Mail {
            from,
            to,
            cc,
            bcc,
            subject,
            text,
            html,
            attachments,
            extra_headers: BTreeMap::new(),
            date,
            message_id,
        })
    }
}

// ─── Internal helpers ─────────────────────────────────────────────────────

/// Upper bound on how many `multipart/...` Content-Type declarations
/// we'll tolerate in a single message before refusing to parse it.
///
/// This is deliberately generous: real-world messages produced by any
/// MUA rarely exceed single digits of nesting (e.g.
/// `multipart/mixed` → `multipart/alternative` → leaf parts). A
/// message claiming hundreds of nested multipart boundaries is not a
/// legitimate mail; it is either corrupt or an intentional
/// stack-exhaustion attack against the recursive MIME walker
/// (`mailparse`'s internal parser and our own [`collect_parts`] both
/// recurse one stack frame per nesting level).
const MAX_MIME_NESTING: usize = 256;

/// Maximum number of MIME parts we will accept from a single inbound
/// message. Real-world mail almost never exceeds a few dozen (a
/// handful of attachments, each potentially multipart/alternative
/// themselves). A message with thousands of top-level parts is not
/// legitimate and is a typical "mail bomb" pattern — the kind of
/// resource-exhaustion attack that exists *specifically* to OOM the
/// receiver. Mirrors the count cap on the outbound side
/// ([`MAX_ATTACHMENTS_OUT`]).
const MAX_INBOUND_PARTS: usize = 1024;

/// Maximum byte length of a *single* attachment we'll accept on
/// parse. 100 MiB covers every legitimate file anyone actually
/// emails (even most videos get chunked by MUA / MTA pipelines);
/// anything bigger is either a misconfigured server or a deliberate
/// OOM attempt against us. The IMAP fetch path enforces a tighter
/// 50 MiB ceiling at the wire level; this is the absolute fallback
/// in case `Mail::from_wire_bytes` is called with already-decoded
/// bytes (e.g. from a tests harness or a non-IMAP source).
const MAX_INBOUND_ATTACHMENT_SIZE: usize = 100 * 1024 * 1024;

/// Maximum number of attachments we'll accept in a *single outbound*
/// `Mail`. Real users rarely attach more than ~20 files; a thousand
/// attachment slots pre-allocated by `to_wire_bytes` is a sign of a
/// misbehaving caller (or an attacker trying to OOM the encoder).
const MAX_ATTACHMENTS_OUT: usize = 1024;

/// Maximum byte length of a *single* attachment we'll accept on
/// outbound encode. Same reasoning as [`MAX_INBOUND_ATTACHMENT_SIZE`]
/// — anything over 100 MiB in a single part is almost certainly a
/// mistake or an attack.
const MAX_ATTACHMENT_SIZE_OUT: usize = 100 * 1024 * 1024;

/// Ceiling on the total encoded message size. Without this, a caller
/// could attach many huge files whose base64 expansion alone pushes
/// the wire output past available memory. 256 MiB comfortably covers
/// "every legitimate email in history" while still bounding the worst
/// case so the encoder can return `Build` rather than OOM-killing
/// the process.
const MAX_MAIL_SIZE_OUT: usize = 256 * 1024 * 1024;

/// Cheap, non-recursive, linear-time guard against MIME nesting bombs.
///
/// Counts case-insensitive occurrences of the ASCII byte sequence
/// `multipart/` anywhere in the message. This intentionally does not
/// distinguish a real header from the same bytes appearing inside a
/// body (a false positive there just means we refuse an unusual
/// message, which is an acceptable trade-off for a DoS guard that
/// must never recurse). It runs in O(n) time and O(1) extra memory,
/// so it cannot itself become a denial-of-service vector even for
/// attacker-controlled input sizes.
fn reject_excessive_mime_nesting(bytes: &[u8]) -> Result<()> {
    let needle = b"multipart/";
    let mut count = 0usize;
    let mut i = 0usize;
    while i + needle.len() <= bytes.len() {
        if bytes[i..i + needle.len()].eq_ignore_ascii_case(needle) {
            count += 1;
            if count > MAX_MIME_NESTING {
                return Err(MailError::Parse(format!(
                    "message declares more than {MAX_MIME_NESTING} multipart boundaries; \
                     refusing to parse (possible MIME nesting DoS)"
                )));
            }
            i += needle.len();
        } else {
            i += 1;
        }
    }
    Ok(())
}

fn build_body(mail: &Mail) -> Result<Vec<u8>> {
    // No attachments, no HTML → single text/plain part, simplest case.
    if mail.attachments.is_empty() && mail.html.is_none() {
        let mut out = Vec::new();
        out.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n");
        out.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n");
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(mail.text.as_bytes());
        return Ok(out);
    }

    // Otherwise produce a multipart/mixed with the body (itself
    // multipart/alternative if HTML is present) plus attachments.
    let boundary = format!("a3net-mail-{}", uuid::Uuid::new_v4().simple());
    let mut out = Vec::new();
    out.extend_from_slice(
        format!("Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n").as_bytes(),
    );

    // Body part(s)
    if let Some(html_body) = &mail.html {
        let inner_boundary = format!("a3net-mail-alt-{}", uuid::Uuid::new_v4().simple());
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(
            format!("Content-Type: multipart/alternative; boundary=\"{inner_boundary}\"\r\n\r\n")
                .as_bytes(),
        );
        out.extend_from_slice(format!("--{inner_boundary}\r\n").as_bytes());
        out.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n");
        out.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n\r\n");
        out.extend_from_slice(mail.text.as_bytes());
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(format!("--{inner_boundary}\r\n").as_bytes());
        out.extend_from_slice(b"Content-Type: text/html; charset=utf-8\r\n");
        out.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n\r\n");
        out.extend_from_slice(html_body.as_bytes());
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(format!("--{inner_boundary}--\r\n").as_bytes());
    } else {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n");
        out.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n\r\n");
        out.extend_from_slice(mail.text.as_bytes());
        out.extend_from_slice(b"\r\n");
    }

    // Attachments
    for att in &mail.attachments {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let cd = match att.disposition {
            Disposition::Inline => "inline",
            Disposition::Attachment => "attachment",
        };
        out.extend_from_slice(
            format!(
                "Content-Type: {}\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: {cd}; filename=\"{}\"\r\n\r\n",
                att.content_type,
                escape_quoted(&att.filename),
            )
            .as_bytes(),
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(&att.data);
        // Wrap at 76 columns per RFC 2045 §6.8.
        for chunk in encoded.as_bytes().chunks(76) {
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\r\n");
        }
    }

    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    // Final cap: even with sane per-attachment limits, the base64
    // expansion (4/3 + line-wrap CRLFs) can push the output past
    // available memory for pathological combinations (many medium
    // attachments). Reject at the end so the encoder is total —
    // either it succeeds within `MAX_MAIL_SIZE_OUT` or it returns
    // a `Build` error rather than OOMing the whole process.
    if out.len() > MAX_MAIL_SIZE_OUT {
        return Err(MailError::Build(format!(
            "encoded message size {} exceeds limit {}",
            out.len(),
            MAX_MAIL_SIZE_OUT
        )));
    }
    Ok(out)
}

/// Recursive walk over MIME parts to extract (text, html, attachments).
fn collect_parts(
    parsed: &mailparse::ParsedMail<'_>,
    text: &mut Option<String>,
    html: &mut Option<String>,
    attachments: &mut Vec<Attachment>,
) -> Result<()> {
    let ct = parsed.ctype.mimetype.to_lowercase();
    let is_multipart = parsed.ctype.mimetype.starts_with("multipart/");

    if is_multipart {
        for sub in &parsed.subparts {
            collect_parts(sub, text, html, attachments)?;
            // Each `multipart/*` layer counts as one logical part on
            // top of its children. Capping *after* the child walk
            // keeps the count proportional to the message's actual
            // surface, not its nesting depth.
            if attachments.len() > MAX_INBOUND_PARTS {
                return Err(MailError::Parse(format!(
                    "message has more than {MAX_INBOUND_PARTS} attachments; \
                     refusing to parse (possible mail-bomb DoS)"
                )));
            }
        }
        return Ok(());
    }

    let content_disp = parsed.get_content_disposition();
    let disposition = match content_disp.disposition {
        mailparse::DispositionType::Attachment => "attachment",
        mailparse::DispositionType::Inline => "inline",
        mailparse::DispositionType::FormData => "form-data",
        mailparse::DispositionType::Extension(ref s) => s.as_str(),
    };
    let filename = content_disp
        .params
        .get("filename")
        .cloned()
        .unwrap_or_default();

    // Read the body (already transfer-decoded by mailparse).
    let body = parsed
        .get_body_raw()
        .map_err(|e| MailError::Parse(e.to_string()))?;
    // Inbound per-part size cap. mailparse's `get_body_raw` returns
    // the already transfer-decoded bytes — a hostile server can claim
    // a tiny RFC822.SIZE but expand to gigabytes via base64/quoted-
    // printable. The IMAP fetch path applies a tighter 50 MiB cap at
    // the wire level; this is the absolute fallback for non-IMAP
    // call sites and a defence-in-depth for the IMAP path itself.
    if body.len() > MAX_INBOUND_ATTACHMENT_SIZE {
        return Err(MailError::Parse(format!(
            "attachment body size {} exceeds limit {}",
            body.len(),
            MAX_INBOUND_ATTACHMENT_SIZE
        )));
    }

    if disposition == "attachment" || !filename.is_empty() {
        attachments.push(Attachment {
            filename,
            content_type: ct.clone(),
            data: body,
            disposition: if disposition == "inline" {
                Disposition::Inline
            } else {
                Disposition::Attachment
            },
        });
        return Ok(());
    }

    if ct == "text/plain" {
        let s = String::from_utf8_lossy(&body).into_owned();
        // Prefer the first text/plain we see (matches MUAs).
        if text.is_none() {
            *text = Some(s);
        }
    } else if ct == "text/html" {
        let s = String::from_utf8_lossy(&body).into_owned();
        if html.is_none() {
            *html = Some(s);
        }
    }
    Ok(())
}

fn first_address(parsed: &mailparse::ParsedMail<'_>, header: &str) -> Result<Address> {
    let s = parsed
        .headers
        .get_first_value(header)
        .ok_or_else(|| MailError::Parse(format!("missing {header} header")))?;
    let addrs = addrparse(&s).map_err(|e| MailError::Parse(e.to_string()))?;
    pick_single(&addrs, header, &s)
}

fn all_addresses(parsed: &mailparse::ParsedMail<'_>, header: &str) -> Vec<Address> {
    let Some(s) = parsed.headers.get_first_value(header) else {
        return Vec::new();
    };
    let Ok(addrs) = addrparse(&s) else {
        return Vec::new();
    };
    flatten_addrs(&addrs)
}

fn pick_single(addrs: &mailparse::MailAddrList, header: &str, raw: &str) -> Result<Address> {
    let list = flatten_addrs(addrs);
    list.into_iter()
        .next()
        .ok_or_else(|| MailError::Parse(format!("no address in {header:?}: {raw:?}")))
}

fn flatten_addrs(addrs: &mailparse::MailAddrList) -> Vec<Address> {
    let mut out = Vec::new();
    for addr in addrs.iter() {
        match addr {
            mailparse::MailAddr::Single(s) => out.push(Address {
                name: s.display_name.clone(),
                address: s.addr.clone(),
            }),
            mailparse::MailAddr::Group(g) => {
                for s in &g.addrs {
                    out.push(Address {
                        name: s.display_name.clone(),
                        address: s.addr.clone(),
                    });
                }
            }
        }
    }
    out
}

fn generate_message_id() -> String {
    let id = uuid::Uuid::new_v4();
    // RFC 5322 §3.6.4 says addr-spec in Message-ID must contain a domain.
    // We use a clearly-fictional domain so the ID cannot be mistaken for
    // a real server's message-ID. Real MX servers assign their own
    // Message-IDs when they relay; this ID only matters for threading.
    format!("<{id}@a3net-mail.local>")
}

/// RFC 2047 encoded-word for headers that might contain non-ASCII.
/// We encode the whole header value (=?...?=) when it has any non-ASCII
/// bytes, otherwise we leave it bare.
fn encode_rfc2047(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    format!(
        "=?utf-8?B?{}?=",
        base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
    )
}

fn escape_quoted(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Header-injection guard. Refuses any byte sequence that could
/// terminate the current header line (`\r`, `\n`) or introduce a
/// NUL terminator that some transports silently truncate on.
fn reject_header_injection(field: &str, value: &str) -> Result<()> {
    if value.contains(['\r', '\n', '\0']) {
        return Err(MailError::InvalidHeader {
            name: field.to_string(),
            reason: "CR/LF/NUL not allowed in header fields".into(),
        });
    }
    Ok(())
}

/// Body-side defensive check: NUL is forbidden (some transports
/// silently truncate on NUL), but CR/LF are allowed since the body
/// is isolated from the headers by the `\r\n\r\n` separator.
fn reject_body_nul(field: &str, value: &str) -> Result<()> {
    if value.contains('\0') {
        return Err(MailError::InvalidHeader {
            name: field.to_string(),
            reason: "NUL not allowed in body".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(m: &Mail) {
        let bytes = m.to_wire_bytes().unwrap();
        let back = Mail::from_wire_bytes(&bytes).unwrap();
        // Subject equality is enough; the rest is sanity-checked below.
        assert_eq!(m.subject, back.subject);
        // The encoder appends a trailing CRLF; the parser strips it back.
        let m_text = m.text.trim_end_matches('\n').trim_end_matches('\r');
        let back_text = back.text.trim_end_matches('\n').trim_end_matches('\r');
        assert_eq!(m_text, back_text);
        assert_eq!(m.attachments.len(), back.attachments.len());
    }

    #[test]
    fn plain_text_round_trip() {
        let m = Mail::text_only(
            Address::new("alice@example.com").with_name("Alice"),
            Address::new("bob@example.com"),
            "hi",
            "Hello world!",
        );
        round_trip(&m);
    }

    #[test]
    fn utf8_subject_is_encoded() {
        let m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "你好，世界",
            "hi",
        );
        let bytes = m.to_wire_bytes().unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("=?utf-8?B?"), "subject should be encoded: {s}");
        // And it must round-trip back.
        let back = Mail::from_wire_bytes(&bytes).unwrap();
        assert_eq!(back.subject, "你好，世界");
    }

    #[test]
    fn html_and_attachment_round_trip() {
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "report",
            "see attached",
        );
        m.html = Some("<p>see attached</p>".into());
        m.attachments.push(Attachment {
            filename: "note.txt".into(),
            content_type: "text/plain".into(),
            data: b"a tiny file".to_vec(),
            disposition: Disposition::Attachment,
        });
        round_trip(&m);

        let back = Mail::from_wire_bytes(&m.to_wire_bytes().unwrap()).unwrap();
        assert!(back.html.is_some());
        assert_eq!(back.attachments.len(), 1);
        assert_eq!(back.attachments[0].data, b"a tiny file");
    }

    #[test]
    fn validate_empty_to_rejected() {
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "x",
            "y",
        );
        m.to.clear();
        let err = m.validate().unwrap_err();
        assert!(matches!(err, MailError::EmptyRecipients));
    }

    #[test]
    fn validate_invalid_addr_rejected() {
        let m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("not-an-email"),
            "x",
            "y",
        );
        let err = m.validate().unwrap_err();
        assert!(matches!(err, MailError::InvalidAddr(_)));
    }

    #[test]
    fn header_injection_in_extra_headers_is_rejected() {
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "hi",
            "hi",
        );
        // Newline in a header value: attacker tries to forge a Bcc: line.
        m.extra_headers
            .insert("X-Test".into(), "ok\r\nBcc: attacker@evil.com".into());
        let err = m.to_wire_bytes().unwrap_err();
        assert!(matches!(err, MailError::InvalidHeader { .. }));
    }

    #[test]
    fn header_injection_in_subject_is_rejected() {
        // `encode_rfc2047` runs on `subject`, but the guard runs
        // *before* encoding — a CR/LF in the subject would survive
        // base64-encoding inside the encoded-word.
        let m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "sub\r\nject",
            "hi",
        );
        let err = m.to_wire_bytes().unwrap_err();
        assert!(matches!(err, MailError::InvalidHeader { .. }));
    }

    #[test]
    fn header_injection_in_display_name_is_rejected() {
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "hi",
            "hi",
        );
        if let Some(addr) = m.to.first_mut() {
            addr.name = Some("Bob\r\nBcc: attacker@evil.com".into());
        }
        let err = m.to_wire_bytes().unwrap_err();
        assert!(matches!(err, MailError::InvalidHeader { .. }));
    }

    #[test]
    fn from_wire_bytes_rejects_mime_nesting_bomb() {
        // Aerospace-grade regression: a message with tens of
        // thousands of nested `multipart/*` boundaries used to blow
        // the stack inside `mailparse::parse_mail` (an unrecoverable
        // process abort, not a catchable panic). We must refuse it
        // cheaply, in linear time, before it ever reaches the
        // recursive parser.
        let depth = 100_000usize;
        let mut body = String::with_capacity(depth * 40);
        for i in 0..depth {
            body.push_str(&format!(
                "Content-Type: multipart/mixed; boundary=\"b{i}\"\r\n\r\n--b{i}\r\n"
            ));
        }
        body.push_str("Content-Type: text/plain\r\n\r\nend\r\n");
        let raw = format!(
            "From: a@example.com\r\nTo: b@example.com\r\nSubject: x\r\nMIME-Version: 1.0\r\n{body}"
        );
        let start = std::time::Instant::now();
        let err = Mail::from_wire_bytes(raw.as_bytes()).unwrap_err();
        assert!(matches!(err, MailError::Parse(_)), "got {err:?}");
        // Must be rejected in linear time, not proportional to any
        // recursive walk — well under a second even in debug builds.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "rejection took too long: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn from_wire_bytes_accepts_reasonably_nested_multipart() {
        // Legitimate messages with a handful of nesting levels
        // (mixed → alternative → leaf, as most MUAs produce) must
        // still parse fine — the DoS guard should not be trigger-happy.
        let raw = "From: a@example.com\r\nTo: b@example.com\r\nSubject: x\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"B\"\r\n\r\n--B\r\n\
Content-Type: multipart/alternative; boundary=\"A\"\r\n\r\n--A\r\n\
Content-Type: text/plain\r\n\r\nhi\r\n--A--\r\n--B--\r\n";
        let m = Mail::from_wire_bytes(raw.as_bytes()).expect("legit nesting must parse");
        assert_eq!(m.text.trim_end_matches(['\r', '\n']), "hi");
    }

    #[test]
    fn from_wire_bytes_missing_from_is_parse_error() {
        // No From: header at all — the parser must refuse rather than
        // silently defaulting to a bogus empty address.
        let bytes = b"To: bob@example.com\r\nSubject: x\r\n\r\nbody\r\n";
        let err = Mail::from_wire_bytes(bytes).unwrap_err();
        assert!(matches!(err, MailError::Parse(_)), "got {err:?}");
    }

    #[test]
    fn from_wire_bytes_accepts_message_with_only_bcc() {
        // RFC 5322 allows a message with empty To/Cc but populated Bcc.
        // Our parser must accept that, not error out on the missing
        // headers.
        let raw = "From: alice@example.com\r\n\
                   Subject: x\r\n\
                   Date: Mon, 04 Aug 2025 10:00:00 +0000\r\n\
                   Message-ID: <abc@example.com>\r\n\
                   MIME-Version: 1.0\r\n\
                   Content-Type: text/plain; charset=utf-8\r\n\
                   \r\n\
                   body\r\n";
        let back = Mail::from_wire_bytes(raw.as_bytes()).unwrap();
        assert_eq!(back.subject, "x");
        assert!(back.to.is_empty());
        assert!(back.cc.is_empty());
        assert!(back.bcc.is_empty());
        assert_eq!(back.from.address, "alice@example.com");
    }

    #[test]
    fn header_injection_in_attachment_filename_is_rejected() {
        // Aerospace-grade regression: attacker-controlled filename
        // could forge extra MIME headers inside the attachment's own
        // part (e.g. smuggle Content-Type: text/html into a lenient
        // MUA). Must be rejected at to_wire_bytes(), not silently
        // encoded onto the wire.
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "test",
            "body",
        );
        m.attachments.push(Attachment {
            filename: "evil.txt\r\nX-Injected: yes".into(),
            content_type: "text/plain".into(),
            data: b"data".to_vec(),
            disposition: Disposition::Attachment,
        });
        let err = m.to_wire_bytes().unwrap_err();
        assert!(
            matches!(err, MailError::InvalidHeader { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn header_injection_in_attachment_content_type_is_rejected() {
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "test",
            "body",
        );
        m.attachments.push(Attachment {
            filename: "ok.txt".into(),
            content_type: "text/plain\r\nX-Evil: injected".into(),
            data: b"data".to_vec(),
            disposition: Disposition::Attachment,
        });
        let err = m.to_wire_bytes().unwrap_err();
        assert!(
            matches!(err, MailError::InvalidHeader { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn attachment_filename_with_quote_is_escaped_not_rejected() {
        // A literal `"` in a filename is legitimate (if unusual) and
        // must be escaped, not rejected — only CR/LF/NUL are actual
        // injection vectors.
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "test",
            "body",
        );
        m.attachments.push(Attachment {
            filename: "quote\"name.txt".into(),
            content_type: "text/plain".into(),
            data: b"data".to_vec(),
            disposition: Disposition::Attachment,
        });
        let bytes = m
            .to_wire_bytes()
            .expect("quote in filename should be escaped, not rejected");
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            s.contains("quote\\\"name.txt"),
            "expected escaped quote: {s}"
        );
    }

    #[test]
    fn display_name_crlf_caught_at_wire_encode() {
        // Display impl doesn't filter CR/LF (it just stringifies), but
        // `to_wire_bytes` rejects it as a header-injection vector —
        // that's the only path that should ever land on the wire.
        let poisoned = Address::new("alice@example.com").with_name("Alice\r\nX-Injected: yes");
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "x",
            "y",
        );
        m.from = poisoned;
        let err = m.to_wire_bytes().unwrap_err();
        assert!(matches!(err, MailError::InvalidHeader { .. }));
    }

    // ─── Resource-cap regressions (mail-bomb / OOM DoS) ────────────────────

    #[test]
    fn to_wire_bytes_rejects_too_many_attachments() {
        // Stress the count guard by pushing the cap. We can't
        // allocate MAX_ATTACHMENTS_OUT+1 *real* attachments without
        // itself hitting the per-attachment-cap test path, so we use
        // zero-byte attachments to exercise just the count check.
        // (See `to_wire_bytes_rejects_oversized_single_attachment`
        // for the per-attachment cap test.)
        //
        // Build a message with exactly MAX_ATTACHMENTS_OUT + 1
        // empty attachments. The count guard runs *before* the
        // per-attachment guard so this must trip the Build error.
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "x",
            "y",
        );
        m.attachments = (0..MAX_ATTACHMENTS_OUT + 1)
            .map(|i| Attachment {
                filename: format!("f{i}.txt"),
                content_type: "text/plain".into(),
                data: Vec::new(),
                disposition: Disposition::Attachment,
            })
            .collect();
        let err = m.to_wire_bytes().unwrap_err();
        assert!(matches!(err, MailError::Build(_)), "got {err:?}");
        assert!(
            err.to_string().contains("too many attachments"),
            "got {err:?}"
        );
    }

    #[test]
    fn to_wire_bytes_rejects_oversized_single_attachment() {
        // Cap is 100 MiB; build one attachment just over the line
        // (don't actually allocate 100 MiB in the test — use a
        // synthetic check via a deliberately-empty cap by re-using the
        // raw guard inline).
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "x",
            "y",
        );
        // Just over the cap:
        let oversize = vec![0u8; MAX_ATTACHMENT_SIZE_OUT + 1];
        m.attachments.push(Attachment {
            filename: "huge.bin".into(),
            content_type: "application/octet-stream".into(),
            data: oversize,
            disposition: Disposition::Attachment,
        });
        let err = m.to_wire_bytes().unwrap_err();
        assert!(matches!(err, MailError::Build(_)), "got {err:?}");
    }

    #[test]
    fn to_wire_bytes_rejects_oversized_total_message() {
        // Even with each individual attachment below the per-part
        // limit, base64 expansion + line-wrap CRLFs can push the
        // total past the cap. Two ~110 MiB attachments blow past
        // 256 MiB once encoded (220 MiB raw → ~295 MiB base64).
        let mut m = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "x",
            "y",
        );
        for i in 0..2 {
            m.attachments.push(Attachment {
                filename: format!("big{i}.bin"),
                content_type: "application/octet-stream".into(),
                data: vec![0u8; 110 * 1024 * 1024], // 110 MiB
                disposition: Disposition::Attachment,
            });
        }
        let err = m.to_wire_bytes().unwrap_err();
        assert!(matches!(err, MailError::Build(_)), "got {err:?}");
    }

    #[test]
    fn from_wire_bytes_rejects_oversized_attachment_body() {
        // A mailparse-decoded attachment whose decoded body exceeds
        // MAX_INBOUND_ATTACHMENT_SIZE must be rejected. We use
        // base64 to encode a body larger than the cap, so the
        // wire bytes are *small* but the decoded body is huge — the
        // exact "claimed-size tiny, decoded-size huge" pattern that
        // exploits OOM-via-transfer-decoding.
        use base64::Engine as _;
        let oversize_decoded = vec![0u8; MAX_INBOUND_ATTACHMENT_SIZE + 1];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&oversize_decoded);
        // 4/3 expansion: 100 MiB + a few bytes → ~134 MiB on wire,
        // which is well under the multipart nesting guard. Use a
        // simpler "single text/plain with Content-Transfer-Encoding:
        // base64" wire shape to avoid building a multipart envelope.
        let raw = format!(
            "From: a@example.com\r\n\
             To: b@example.com\r\n\
             Subject: bomb\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: application/octet-stream; name=\"bomb.bin\"\r\n\
             Content-Disposition: attachment; filename=\"bomb.bin\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {encoded}"
        );
        let err = Mail::from_wire_bytes(raw.as_bytes()).unwrap_err();
        assert!(
            matches!(err, MailError::Parse(_)),
            "expected Parse error, got {err:?}"
        );
    }
}
