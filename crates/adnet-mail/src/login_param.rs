//! Login parameters for a single email account.
//!
//! This is a clean-room simplification of
//! `chatmail@core/src/login_param.rs` (Delta Chat). The original is
//! tightly coupled to Delta Chat's `Context` / SQLite / `provider-db`
//! machinery; here we expose a pure-data struct that callers (CLI,
//! IPC adapter, programmatic API) can construct directly.
//!
//! ## Relationship to other crates
//!
//! `login_param::Account` is the only thing the IMAP and SMTP modules
//! need to know in order to dial a server. Higher layers (e.g.
//! `adnet-chatstore`) persist it however they like — JSON file,
//! SQLite row, YAML config, etc.

use serde::{Deserialize, Serialize};

use crate::error::{MailError, Result};

/// TLS posture for a single server connection.
///
/// Mirrors Delta Chat's `provider::Socket` enum but with explicit
/// names that don't depend on the `strum` derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketSecurity {
    /// Plaintext. Only safe on localhost / Tor / trusted LAN.
    Plain,
    /// `STARTTLS` upgrade on the standard submission port (587).
    Starttls,
    /// Implicit TLS on the dedicated port (465 for SMTP, 993 for IMAP).
    /// This is the modern best-practice default.
    #[default]
    Tls,
}

/// Certificate-check policy.
///
/// Delta Chat distinguishes three modes; we collapse them to two
/// because the "accept invalid" branch is rarely useful outside of
/// self-hosted / LAN debug setups, and a separate variant lets us
/// refuse to even compile it in if we ever want a "strict-only" build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificateChecks {
    /// Validate chain against system roots and require hostname match.
    #[default]
    Strict,
    /// Validate chain, but accept expired / self-signed certs.
    /// Useful for homelab servers; should be opt-in by the operator.
    AcceptInvalid,
}

/// IMAP server parameters.
///
/// ⚠️ **Credential-bearing.** `Debug` is **manually implemented** and
/// redacts the `password` field. `Serialize` / `Deserialize` *do*
/// emit the cleartext — that's how we persist accounts on disk — so
/// callers must not route an `Account` through `serde_json::to_string`
/// for logging, telemetry, or IPC without going through
/// [`crate::login_param::Account::safe_serialize_json`] first.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImapLoginParam {
    /// Server hostname, e.g. `imap.gmail.com`.
    pub server: String,

    /// Server port. `0` means "use the default for the security mode"
    /// (`993` for `Tls`, `143` for `Starttls`/`Plain`).
    pub port: u16,

    /// Folder to watch for new mail. Defaults to `INBOX` when empty.
    #[serde(default)]
    pub folder: String,

    /// TLS posture.
    pub security: SocketSecurity,

    /// Username, e.g. `alice@gmail.com` (full address is fine too).
    pub user: String,

    /// Plain password or OAuth2 bearer token.
    ///
    /// The caller is responsible for storing this encrypted at rest.
    /// `adnet-mail` treats it as opaque bytes and never logs them.
    pub password: String,
}

impl std::fmt::Debug for ImapLoginParam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapLoginParam")
            .field("server", &self.server)
            .field("port", &self.port)
            .field("folder", &self.folder)
            .field("security", &self.security)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// SMTP server parameters.
///
/// ⚠️ **Credential-bearing.** See [`ImapLoginParam`] for the `Debug`
/// / `Serialize` policy.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmtpLoginParam {
    pub server: String,
    pub port: u16,
    pub security: SocketSecurity,
    pub user: String,
    pub password: String,
}

impl std::fmt::Debug for SmtpLoginParam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpLoginParam")
            .field("server", &self.server)
            .field("port", &self.port)
            .field("security", &self.security)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// One email account: address + IMAP + SMTP login + TLS policy.
///
/// This is the "pure data" object that callers persist and the SMTP/IMAP
/// modules consume.
///
/// ⚠️ **Credential-bearing.** `Debug` is manually implemented and
/// redacts every password field. Use [`Account::safe_serialize_json`]
/// for any code path that needs a JSON form (logs, telemetry, IPC
/// over channels that aren't end-to-end encrypted). The default
/// `Serialize` impl on `Account` *does* emit cleartext — that's how
/// we persist accounts on disk — so the safe wrapper is the only
/// route to a printable form.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// From-address, e.g. `alice@example.com`.
    pub addr: String,

    /// IMAP configuration (incoming).
    pub imap: ImapLoginParam,

    /// SMTP configuration (outgoing).
    pub smtp: SmtpLoginParam,

    /// TLS validation policy applied to *both* IMAP and SMTP.
    pub certificate_checks: CertificateChecks,

    /// Optional human-readable display name used in outgoing headers.
    /// e.g. `"Alice Example <alice@example.com>"`.
    #[serde(default)]
    pub display_name: Option<String>,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("addr", &self.addr)
            .field("imap", &self.imap)
            .field("smtp", &self.smtp)
            .field("certificate_checks", &self.certificate_checks)
            .field("display_name", &self.display_name)
            .finish()
    }
}

impl Account {
    /// Construct an `Account` from its parts, applying default ports if
    /// the caller left them at `0`.
    pub fn new(
        addr: impl Into<String>,
        imap: ImapLoginParam,
        smtp: SmtpLoginParam,
    ) -> Result<Self> {
        let addr = addr.into();
        if !is_valid_address(&addr) {
            return Err(MailError::InvalidAddr(addr));
        }
        Ok(Self {
            addr,
            imap: imap.with_default_port(),
            smtp: smtp.with_default_port(),
            certificate_checks: CertificateChecks::default(),
            display_name: None,
        })
    }

    /// Build from a JSON blob — handy for IPC payloads and CLI config.
    ///
    /// Validates the address plus the IMAP/SMTP server hostnames after
    /// deserialising; if any field is missing or empty the call returns
    /// [`MailError::Config`]. This catches the common mistake of feeding
    /// a half-filled JSON template into the runtime.
    pub fn from_json(s: &str) -> Result<Self> {
        let acct: Account = serde_json::from_str(s)?;
        if !is_valid_address(&acct.addr) {
            return Err(MailError::InvalidAddr(acct.addr));
        }
        if acct.imap.server.is_empty() {
            return Err(MailError::Config("imap.server is empty".into()));
        }
        if acct.smtp.server.is_empty() {
            return Err(MailError::Config("smtp.server is empty".into()));
        }
        if acct.imap.user.is_empty() {
            return Err(MailError::Config("imap.user is empty".into()));
        }
        if acct.smtp.user.is_empty() {
            return Err(MailError::Config("smtp.user is empty".into()));
        }
        Ok(acct)
    }

    /// Build a JSON form with every password field replaced by
    /// `"<redacted>"`. Suitable for tracing, telemetry, and IPC over
    /// channels that aren't end-to-end encrypted.
    ///
    /// This is the only safe way to print an `Account` as JSON.
    /// `serde_json::to_string(&account)` will emit the cleartext.
    pub fn safe_serialize_json(&self) -> Result<serde_json::Value> {
        let mut value = serde_json::to_value(self)?;
        if let Some(obj) = value.as_object_mut() {
            if let Some(imap) = obj.get_mut("imap").and_then(|v| v.as_object_mut()) {
                imap.insert(
                    "password".into(),
                    serde_json::Value::String("<redacted>".into()),
                );
            }
            if let Some(smtp) = obj.get_mut("smtp").and_then(|v| v.as_object_mut()) {
                smtp.insert(
                    "password".into(),
                    serde_json::Value::String("<redacted>".into()),
                );
            }
        }
        Ok(value)
    }

    /// Pretty `Display` that hides the password (`***`) — safe for logs.
    pub fn safe_display(&self) -> String {
        format!(
            "{} imap://{}@{}:{}/{} smtp://{}@{}:{} cert={}",
            self.addr,
            self.imap.user,
            self.imap.server,
            effective_imap_port(&self.imap),
            if self.imap.folder.is_empty() {
                "INBOX"
            } else {
                &self.imap.folder
            },
            self.smtp.user,
            self.smtp.server,
            effective_smtp_port(&self.smtp),
            self.certificate_checks_label(),
        )
    }

    fn certificate_checks_label(&self) -> &'static str {
        match self.certificate_checks {
            CertificateChecks::Strict => "strict",
            CertificateChecks::AcceptInvalid => "accept-invalid",
        }
    }
}

impl ImapLoginParam {
    /// Apply the standard port for the chosen security mode if `port == 0`.
    pub fn with_default_port(mut self) -> Self {
        if self.port == 0 {
            self.port = match self.security {
                SocketSecurity::Tls => 993,
                SocketSecurity::Starttls | SocketSecurity::Plain => 143,
            };
        }
        self
    }
}

impl SmtpLoginParam {
    pub fn with_default_port(mut self) -> Self {
        if self.port == 0 {
            self.port = match self.security {
                SocketSecurity::Tls => 465,
                SocketSecurity::Starttls => 587,
                SocketSecurity::Plain => 25,
            };
        }
        self
    }
}

fn effective_imap_port(p: &ImapLoginParam) -> u16 {
    if p.port != 0 {
        p.port
    } else {
        match p.security {
            SocketSecurity::Tls => 993,
            SocketSecurity::Starttls | SocketSecurity::Plain => 143,
        }
    }
}

fn effective_smtp_port(p: &SmtpLoginParam) -> u16 {
    if p.port != 0 {
        p.port
    } else {
        match p.security {
            SocketSecurity::Tls => 465,
            SocketSecurity::Starttls => 587,
            SocketSecurity::Plain => 25,
        }
    }
}

/// Syntactic RFC 5321 mailbox check.
///
/// We intentionally keep this cheap and best-effort. Real validation is
/// the remote server's job; we just want to refuse obvious garbage
/// before paying for a TLS handshake.
pub fn is_valid_address(s: &str) -> bool {
    if s.is_empty() || s.len() > 254 {
        return false;
    }
    // Defence-in-depth: reject control characters (CR/LF/NUL/etc.)
    // outright. `async_smtp::EmailAddress` happens to reject CRLF
    // today, but `Mail::validate()` must not rely on a downstream
    // crate's internal behaviour to catch SMTP/header injection —
    // an attacker-controlled address string (e.g. from a config file
    // or IPC payload) must be rejected here, at the boundary, so the
    // guarantee holds regardless of which transport eventually
    // consumes it.
    if s.chars().any(|c| c.is_control()) {
        return false;
    }
    // Must contain exactly one `@` after a non-empty local part, and the
    // domain part must contain at least one `.`.
    let mut parts = s.splitn(2, '@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    let trailing = parts.next(); // must be None

    !(local.is_empty()
        || domain.is_empty()
        || trailing.is_some()
        || domain.starts_with('.')
        || domain.starts_with('@')
        || domain.ends_with('.')
        || domain.ends_with('@')
        || !domain.contains('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imap() -> ImapLoginParam {
        ImapLoginParam {
            server: "imap.example.com".into(),
            port: 0,
            folder: String::new(),
            security: SocketSecurity::Tls,
            user: "alice".into(),
            password: "secret".into(),
        }
    }

    fn smtp() -> SmtpLoginParam {
        SmtpLoginParam {
            server: "smtp.example.com".into(),
            port: 0,
            security: SocketSecurity::Starttls,
            user: "alice".into(),
            password: "secret".into(),
        }
    }

    #[test]
    fn default_ports_fill_in() {
        let acct = Account::new("alice@example.com", imap(), smtp()).unwrap();
        assert_eq!(acct.imap.port, 993); // Tls
        assert_eq!(acct.smtp.port, 587); // Starttls
    }

    #[test]
    fn invalid_addr_rejected() {
        let err = Account::new("not-an-email", imap(), smtp()).unwrap_err();
        assert_eq!(err.recoverability(), crate::error::ErrorClass::UserError);
        assert!(matches!(err, MailError::InvalidAddr(_)));
    }

    #[test]
    fn valid_addresses_accepted() {
        for ok in [
            "alice@example.com",
            "alice@x.io",
            "a.b+c@sub.example.org",
            "x@x.y", // two-segment TLD (think co.uk) is still valid; we just need a dot
        ] {
            assert!(is_valid_address(ok), "should be valid: {ok}");
        }
    }

    #[test]
    fn invalid_addresses_rejected() {
        for bad in [
            "",
            "no-at-sign",
            "@no-local.com",
            "no-domain@",
            "no-dot@localhost",
            "trailing-dot@x.",
            "leading-dot@.x.com",
            // 255-char local part
            &format!("{}@x.com", "a".repeat(255)),
        ] {
            assert!(!is_valid_address(bad), "should be invalid: {bad}");
        }
    }

    #[test]
    fn control_chars_in_address_rejected() {
        // Aerospace-grade regression: `Mail::validate()` must catch
        // SMTP/header-injection attempts at the address-syntax layer,
        // not rely on `async_smtp::EmailAddress` (a downstream crate)
        // to be the only line of defence.
        for bad in [
            "bob@example.com\r\nRCPT TO:<attacker@evil.com>",
            "bob@example.com\nBcc: attacker@evil.com",
            "bob@example.com\r",
            "bo\0b@example.com",
            "bob@exa\tmple.com",
        ] {
            assert!(
                !is_valid_address(bad),
                "should reject control chars: {bad:?}"
            );
        }
    }

    #[test]
    fn double_at_sign_is_invalid() {
        // "two@@signs.com" — splitn(2, '@') gives domain="@signs.com"
        // which lacks a '.', so it's caught by the dot requirement.
        assert!(!is_valid_address("two@@signs.com"));
    }

    #[test]
    fn json_round_trip() {
        let acct = Account::new("alice@example.com", imap(), smtp()).unwrap();
        let j = serde_json::to_string(&acct).unwrap();
        let back = Account::from_json(&j).unwrap();
        assert_eq!(acct, back);
    }

    #[test]
    fn safe_display_hides_password() {
        let acct = Account::new("alice@example.com", imap(), smtp()).unwrap();
        let s = acct.safe_display();
        assert!(
            !s.contains("secret"),
            "password leaked into safe_display: {s}"
        );
        assert!(s.contains("alice@example.com"));
        assert!(s.contains("imap.example.com"));
    }

    #[test]
    fn debug_redacts_passwords_everywhere() {
        let acct = Account::new("alice@example.com", imap(), smtp()).unwrap();
        // Manual `Debug` impls on ImapLoginParam / SmtpLoginParam /
        // Account must all hide the password.
        for label in ["Account", "ImapLoginParam", "SmtpLoginParam"] {
            let s = format!("{label:?}");
            assert!(!s.contains("secret"), "label {label:?} leaked secret: {s}");
        }
        let s = format!("{acct:?}");
        assert!(!s.contains("secret"), "Account Debug leaked password: {s}");
        assert!(s.contains("<redacted>"));
    }

    #[test]
    fn safe_serialize_redacts_passwords() {
        let acct = Account::new("alice@example.com", imap(), smtp()).unwrap();
        let s = acct.safe_serialize_json().unwrap().to_string();
        assert!(!s.contains("secret"), "safe_serialize leaked pw: {s}");
        assert!(s.contains("<redacted>"));
        // Default Serialize still emits cleartext — that's how we
        // persist accounts. We assert the contrast so a refactor
        // doesn't accidentally remove the safe wrapper.
        let raw = serde_json::to_string(&acct).unwrap();
        assert!(raw.contains("secret"));
    }

    #[test]
    fn safe_serialize_returns_value_object() {
        let acct = Account::new("alice@example.com", imap(), smtp()).unwrap();
        let v = acct.safe_serialize_json().unwrap();
        assert!(v.is_object());
        let imap = v.get("imap").and_then(|x| x.as_object()).unwrap();
        let smtp = v.get("smtp").and_then(|x| x.as_object()).unwrap();
        assert_eq!(imap.get("password").unwrap(), "<redacted>");
        assert_eq!(smtp.get("password").unwrap(), "<redacted>");
        // Non-secret fields survive untouched.
        assert_eq!(imap.get("user").unwrap(), "alice");
        assert_eq!(imap.get("server").unwrap(), "imap.example.com");
    }

    #[test]
    fn from_json_rejects_missing_imap_server() {
        let mut acct = Account::new("alice@example.com", imap(), smtp()).unwrap();
        acct.imap.server = String::new();
        let j = serde_json::to_string(&acct).unwrap();
        let err = Account::from_json(&j).unwrap_err();
        assert!(
            matches!(err, MailError::Config(ref s) if s.contains("imap.server")),
            "got {err:?}"
        );
    }

    #[test]
    fn from_json_rejects_missing_imap_user() {
        let mut acct = Account::new("alice@example.com", imap(), smtp()).unwrap();
        acct.imap.user = String::new();
        let j = serde_json::to_string(&acct).unwrap();
        let err = Account::from_json(&j).unwrap_err();
        assert!(
            matches!(err, MailError::Config(ref s) if s.contains("imap.user")),
            "got {err:?}"
        );
    }

    #[test]
    fn from_json_rejects_missing_smtp_user() {
        let mut acct = Account::new("alice@example.com", imap(), smtp()).unwrap();
        acct.smtp.user = String::new();
        let j = serde_json::to_string(&acct).unwrap();
        let err = Account::from_json(&j).unwrap_err();
        assert!(
            matches!(err, MailError::Config(ref s) if s.contains("smtp.user")),
            "got {err:?}"
        );
    }

    #[test]
    fn from_json_rejects_blank_address() {
        let mut acct = Account::new("alice@example.com", imap(), smtp()).unwrap();
        acct.addr = String::new();
        let j = serde_json::to_string(&acct).unwrap();
        let err = Account::from_json(&j).unwrap_err();
        assert!(matches!(err, MailError::InvalidAddr(_)), "got {err:?}");
    }

    #[test]
    fn from_json_rejects_garbage() {
        let err = Account::from_json("not json at all").unwrap_err();
        // serde error path; we don't pin the variant, but it must be
        // an error and must not panic.
        assert!(err.recoverability() != crate::error::ErrorClass::Recoverable);
    }
}
