//! Typed representation of a parsed QR payload.
//!
//! This is the "value" returned by [`crate::scan::check_qr`] and consumed
//! by UIs / IPC adapters. It deliberately covers both the
//! chatmail-compatible subset (so a Delta Chat client could render our
//! QR codes) and A3Net-native payloads (peer tickets, blob tickets,
//! signed peer tickets, relay-payment pledges).

use serde::{Deserialize, Serialize};

#[cfg(feature = "a3net-token")]
use a3net_token::Pledge;

#[cfg(feature = "a3net-types")]
use a3net_types::{BlobTicket, NodeAddrTicket, PeerTicket, SignedPeerTicket};

#[cfg(feature = "pairing")]
use a3net_pairing::wire::PairingInvitation;

/// Conversion from a `DCLOGIN:` payload into an `a3net_mail` config.
///
/// Lives behind the `mail` cargo feature so the QR crate stays usable
/// in environments that don't ship SMTP/IMAP. The conversion is a
/// one-way bridge — there is no `TryFrom<Account>` back into
/// `DcLoginOptions` because `a3net_mail::Account` may carry fields
/// (`display_name`, OAuth2 tokens) that `DCLOGIN` does not model.
///
/// ## Policy
///
/// * **Default security.** `DcLoginSecurity::Default` defers to the
///   provider-database; we collapse that to [`SocketSecurity::Tls`]
///   because `a3net-mail` does not embed a provider-db that knows
///   which port to dial.
/// * **Strict + Plain = reject.** A `DCLOGIN` that asks for plaintext
///   SMTP/IMAP **and** strict certificate checks is a contradiction.
///   The conversion surfaces this as
///   [`crate::error::QrError::InvalidUrl`] so the caller can't
///   accidentally downgrade to insecure.
/// * **Missing host = reject.** We refuse to invent a hostname; if
///   `ih` / `sh` are missing the caller's chatmail client probably
///   meant to talk to a chatmail relay. They get a hard error.
/// * **User mapping.** The DCLOGIN user field is the bare local-part
///   (e.g. `alice`); chatmail rewrites it to the full address. We do
///   the same — `user_for_domain()` builds `<local>@<domain>` when
///   the DCLOGIN address contains an `@`.
#[cfg(feature = "mail")]
#[derive(Debug, Clone)]
pub struct MailAccountFromQr {
    inner: a3net_mail::login_param::Account,
}

#[cfg(feature = "mail")]
impl MailAccountFromQr {
    /// Borrow the produced [`a3net_mail::Account`].
    pub fn account(&self) -> &a3net_mail::login_param::Account {
        &self.inner
    }
    /// Consume the bridge and hand back the inner `Account`.
    pub fn into_account(self) -> a3net_mail::login_param::Account {
        self.inner
    }
}

#[cfg(feature = "mail")]
impl QrPayload {
    /// Convert a `DCLOGIN:` payload into an `a3net_mail::Account`.
    ///
    /// Returns `None` for every variant except [`QrPayload::DcLogin`]
    /// — the conversion is inherently DCLOGIN-specific.
    ///
    /// See [`crate::payload::MailAccountFromQr`] for the full policy
    /// (security mapping, strict-vs-plain refusal, missing-host
    /// rejection, user-rewriting).
    pub fn into_mail_account(&self) -> crate::error::Result<Option<MailAccountFromQr>> {
        let QrPayload::DcLogin { address, options } = self else {
            return Ok(None);
        };
        let account = build_account_from_dclogin(address, options)?;
        Ok(Some(MailAccountFromQr { inner: account }))
    }
}

#[cfg(feature = "mail")]
fn build_account_from_dclogin(
    address: &str,
    options: &DcLoginOptions,
) -> crate::error::Result<a3net_mail::login_param::Account> {
    use a3net_mail::login_param::CertificateChecks;

    // Policy gate: refuse the strict+plain downgrade. `Default`
    // security is intentionally allowed under strict certs because
    // `Default` defers to chatmail's provider-db, which only ever
    // maps to TLS / STARTTLS guarded by a real cert chain — the
    // contradiction only exists when the caller explicitly asked
    // for Plain.
    if matches!(options.imap_security, Some(DcLoginSecurity::Plain))
        && matches!(
            options.certificate_checks,
            Some(DcCertificateChecks::Strict)
        )
    {
        return Err(crate::error::QrError::Malformed {
            scheme: "dclogin",
            reason: "certificate_checks=strict is incompatible with imap_security=Plain".into(),
        });
    }
    if matches!(options.smtp_security, Some(DcLoginSecurity::Plain))
        && matches!(
            options.certificate_checks,
            Some(DcCertificateChecks::Strict)
        )
    {
        return Err(crate::error::QrError::Malformed {
            scheme: "dclogin",
            reason: "certificate_checks=strict is incompatible with smtp_security=Plain".into(),
        });
    }

    let imap_host = options
        .imap_host
        .clone()
        .ok_or_else(|| crate::error::QrError::Malformed {
            scheme: "dclogin",
            reason: "missing imap host (ih=…)".into(),
        })?;
    let smtp_host = options
        .smtp_host
        .clone()
        .ok_or_else(|| crate::error::QrError::Malformed {
            scheme: "dclogin",
            reason: "missing smtp host (sh=…)".into(),
        })?;

    let imap_security = map_security(options.imap_security);
    let smtp_security = map_security(options.smtp_security);

    let certificate_checks = match options.certificate_checks {
        Some(DcCertificateChecks::Strict) | None => CertificateChecks::Strict,
        Some(DcCertificateChecks::AcceptInvalid) => CertificateChecks::AcceptInvalid,
        // DcCertificateChecks::Automatic — chatmail@core uses the
        // provider-db. We do not embed one, so default to Strict.
        Some(DcCertificateChecks::Automatic) => CertificateChecks::Strict,
    };

    let user = map_username(address, options);
    let imap_password = options
        .imap_password
        .clone()
        .unwrap_or_else(|| options.mail_pw.clone());
    let smtp_password = options
        .smtp_password
        .clone()
        .unwrap_or_else(|| options.mail_pw.clone());

    let imap = a3net_mail::login_param::ImapLoginParam {
        server: imap_host,
        port: options.imap_port.unwrap_or(0),
        folder: String::new(),
        security: imap_security,
        user: user.clone(),
        password: imap_password,
    };
    let smtp = a3net_mail::login_param::SmtpLoginParam {
        server: smtp_host,
        port: options.smtp_port.unwrap_or(0),
        security: smtp_security,
        user,
        password: smtp_password,
    };

    a3net_mail::login_param::Account::new(address.to_string(), imap, smtp)
        .map(|mut acct| {
            acct.certificate_checks = certificate_checks;
            acct
        })
        .map_err(|e| crate::error::QrError::Malformed {
            scheme: "dclogin",
            reason: format!("Account::new failed: {e}"),
        })
}

#[cfg(feature = "mail")]
fn map_security(s: Option<DcLoginSecurity>) -> a3net_mail::login_param::SocketSecurity {
    use a3net_mail::login_param::SocketSecurity;
    match s {
        Some(DcLoginSecurity::Ssl) => SocketSecurity::Tls,
        Some(DcLoginSecurity::Starttls) => SocketSecurity::Starttls,
        Some(DcLoginSecurity::Plain) => SocketSecurity::Plain,
        // DCLOGIN "default" defers to provider-db; without one we
        // assume implicit TLS, matching the modern best practice.
        Some(DcLoginSecurity::Default) | None => SocketSecurity::Tls,
    }
}

#[cfg(feature = "mail")]
fn map_username(address: &str, options: &DcLoginOptions) -> String {
    // Priority order:
    //   1. explicit `iu=` override (chatmail fills this when the
    //      IMAP user differs from the SMTP user).
    //   2. the full address (`local@domain`) when the QR's address
    //      field actually contains one.
    //   3. otherwise just the address verbatim — the caller's
    //      `Account::new` will catch the missing-`@` case via
    //      `is_valid_address` and surface it as InvalidUrl.
    if let Some(u) = &options.imap_username {
        return u.clone();
    }
    address.to_string()
}

/// The shape of a parsed QR payload.
///
/// Every variant is a pure-data struct that can be cheaply cloned,
/// serialised for IPC, and rendered into a UI.
///
/// ⚠️ Some variants carry credentials or capabilities. The derived
/// `Debug` implementation calls into [`QrPayload::safe_debug`] for
/// the credential-bearing variants, redacting every password and
/// auth-token field. If you need the plaintext, use the explicit
/// [`QrPayload::expose_secrets`] accessor — but only after user
/// consent has been recorded.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QrPayload {
    /// A `mailto:` payload carrying an address, optional subject, and
    /// optional body. Common in chat clients as a "share a contact"
    /// QR.
    Email {
        address: String,
        subject: Option<String>,
        body: Option<String>,
    },

    /// A `MATMSG:` payload (Android "share via QR" format).
    Matmsg {
        address: String,
        subject: Option<String>,
        body: Option<String>,
    },

    /// A `BEGIN:VCARD` payload carrying a contact name + email.
    Vcard { name: String, address: String },

    /// A `SMTP:` payload (RFC 2368 simplified form).
    Smtp { address: String },

    /// A `DCACCOUNT:` payload asking the user to register an account on
    /// the given domain.
    ///
    /// See <https://github.com/deltachat/interface/blob/master/uri-schemes.md#DCACCOUNT>.
    DcAccount { domain: String },

    /// A `DCLOGIN:` payload carrying full SMTP/IMAP credentials.
    ///
    /// See <https://github.com/deltachat/interface/blob/master/uri-schemes.md#DCLOGIN>.
    ///
    /// ⚠️ **Credential-bearing.** `Debug`, `Display`, and the
    /// default `Serialize` impl redact the `mail_pw` /
    /// `imap_password` / `smtp_password` fields. Use the explicit
    /// [`crate::payload::DcLoginOptions::expose`] method only when
    /// you have user consent and need the plaintext.
    DcLogin {
        address: String,
        options: DcLoginOptions,
    },

    /// A `DCBACKUP` payload. `node_addr` is an iroh `NodeAddr` printed
    /// as JSON; the auth token is a string the receiver sends back over
    /// the iroh-net backup channel.
    ///
    /// The current upstream version is 5; higher versions come back as
    /// [`QrPayload::BackupTooNew`].
    ///
    /// ⚠️ **Capability-bearing.** The `auth_token` is redacted in
    /// `Debug` / `Display`; the unredacted value is available via
    /// [`QrPayload::expose_auth_token`].
    DcBackup {
        version: i32,
        node_addr_json: String,
        auth_token: String,
    },

    /// A `DCBACKUP` payload with a version higher than this build
    /// supports.
    BackupTooNew { version: i32 },

    /// An `OPENPGP4FPR:` or `https://i.delta.chat/` payload describing a
    /// SecureJoin invite. We expose the parsed fields; the SecureJoin
    /// state machine lives in higher crates (chatmail core /
    /// a3net-identity).
    OpenPgp4Fpr(OpenPgp4FprFields),

    /// A `socks5://` or `https://t.me/socks?…` proxy descriptor.
    Proxy {
        url: String,
        host: String,
        port: u16,
    },

    /// An `ss://` Shadowsocks proxy URL.
    Shadowsocks {
        url: String,
        host: String,
        port: u16,
    },

    /// Any other URL (HTTP / HTTPS / arbitrary scheme).
    Url { url: String },

    /// Free-form text the scanner couldn't classify.
    Text { text: String },

    // ────────────────────────────────────────────────────────────────────
    // A3Net-native payloads. These are encoded as `a3net-peer://`,
    // `a3net-blob://`, `a3net-signed-peer://`, and `a3net-token://` URLs.
    // ────────────────────────────────────────────────────────────────────
    /// `a3net-peer://…` — share a single node address.
    #[cfg(feature = "a3net-types")]
    #[serde(rename = "a3net_peer")]
    AdnetPeer { ticket: PeerTicket },

    /// `a3net-addr://…` — share a NodeAddr as a printable ticket.
    #[cfg(feature = "a3net-types")]
    #[serde(rename = "a3net_addr")]
    AdnetAddr { ticket: NodeAddrTicket },

    /// `a3net-blob://…` — share a blob (whole or range-restricted).
    #[cfg(feature = "a3net-types")]
    #[serde(rename = "a3net_blob")]
    AdnetBlob { ticket: BlobTicket },

    /// `a3net-signed-peer://…` — peer ticket + signature from a wallet.
    #[cfg(feature = "a3net-types")]
    #[serde(rename = "a3net_signed_peer")]
    AdnetSignedPeer { ticket: SignedPeerTicket },

    /// `a3net-token://…` — relay-payment pledge.
    #[cfg(feature = "a3net-token")]
    #[serde(rename = "a3net_token")]
    AdnetToken { pledge: Pledge },

    /// `a3net-pairing://…` — pairing invitation. Carries the
    /// invitation envelope produced by `a3net-pairing::SignedInvitation`
    /// (wallet-signed issuer node id + capabilities + expiry) wrapped in
    /// `PairingInvitation::Url` so a scanner can extract the URL form
    /// directly. The full decoded envelope lives behind
    /// [`PairingInvitation::decode`]; see `a3net-pairing` for
    /// verification.
    #[cfg(feature = "pairing")]
    #[serde(rename = "a3net_pairing")]
    AdnetPairing { invitation: PairingInvitation },
}

/// Parameters carried by a `DCLOGIN` payload (Version 1).
///
/// This is the clean-room subset of
/// `chatmail@core::qr::dclogin_scheme::LoginOptions::V1`. We drop the
/// fields that depend on chatmail's `Socket::Automatic` enum (the
/// `default` security level) and translate `EnteredCertificateChecks`
/// values into [`CertificateChecks`] (the same enum `a3net-mail`
/// already exposes).
///
/// ⚠️ **Credential-bearing.** The struct deliberately does **not**
/// derive `Serialize`; the only path to a JSON form is
/// [`DcLoginOptions::safe_serialize`], which redacts every password
/// field. The `Debug` impl is manual and likewise redacts.
#[derive(Clone, PartialEq, Eq)]
pub struct DcLoginOptions {
    /// IMAP password; if absent the [`QrPayload::DcLogin`] falls back to
    /// `mail_pw` and the SMTP password falls back to `""` (matching
    /// upstream).
    pub mail_pw: String,
    pub imap_host: Option<String>,
    pub imap_port: Option<u16>,
    pub imap_username: Option<String>,
    pub imap_password: Option<String>,
    /// One of `ssl` / `starttls` / `plain` / `default`.
    pub imap_security: Option<DcLoginSecurity>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_username: Option<String>,
    pub smtp_password: Option<String>,
    pub smtp_security: Option<DcLoginSecurity>,
    /// One of `0` (automatic) / `1` (strict) / `2` / `3` (accept
    /// invalid certs — chatmail distinguishes two flavours; we collapse
    /// them to one).
    pub certificate_checks: Option<DcCertificateChecks>,
}

impl std::fmt::Debug for DcLoginOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DcLoginOptions")
            .field("mail_pw", &"<redacted>")
            .field("imap_host", &self.imap_host)
            .field("imap_port", &self.imap_port)
            .field("imap_username", &self.imap_username)
            .field(
                "imap_password",
                &self.imap_password.as_ref().map(|_| "<redacted>"),
            )
            .field("imap_security", &self.imap_security)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_username", &self.smtp_username)
            .field(
                "smtp_password",
                &self.smtp_password.as_ref().map(|_| "<redacted>"),
            )
            .field("smtp_security", &self.smtp_security)
            .field("certificate_checks", &self.certificate_checks)
            .finish()
    }
}

impl DcLoginOptions {
    /// Explicitly expose the cleartext passwords. Callers must have
    /// obtained user consent before invoking this. There is no
    /// `try_…` variant: missing credentials stay missing.
    pub fn expose(&self) -> DcLoginOptionsExposed<'_> {
        DcLoginOptionsExposed { inner: self }
    }

    /// Serialise the options with every password field replaced by
    /// the literal string `"<redacted>"`. Suitable for tracing,
    /// telemetry, and IPC over channels that are not end-to-end
    /// encrypted.
    ///
    /// Implementation note: this builds the JSON object directly
    /// rather than routing through `serde_json::to_value(self)`. The
    /// latter would recurse through our `Serialize` impl, which in
    /// turn calls `safe_serialize` — a stack overflow waiting to
    /// happen. Hand-rolling the object also makes the redaction list
    /// grep-able and reviewable.
    pub fn safe_serialize(&self) -> crate::error::Result<serde_json::Value> {
        use serde_json::{Map, Value};
        let mut obj = Map::with_capacity(13);
        obj.insert("mail_pw".into(), Value::String("<redacted>".into()));
        if let Some(v) = &self.imap_host {
            obj.insert("imap_host".into(), Value::String(v.clone()));
        } else {
            obj.insert("imap_host".into(), Value::Null);
        }
        if let Some(v) = &self.imap_port {
            obj.insert("imap_port".into(), Value::Number((*v).into()));
        } else {
            obj.insert("imap_port".into(), Value::Null);
        }
        if let Some(v) = &self.imap_username {
            obj.insert("imap_username".into(), Value::String(v.clone()));
        } else {
            obj.insert("imap_username".into(), Value::Null);
        }
        obj.insert("imap_password".into(), Value::String("<redacted>".into()));
        obj.insert(
            "imap_security".into(),
            self.imap_security
                .map(security_to_json)
                .unwrap_or(Value::Null),
        );
        if let Some(v) = &self.smtp_host {
            obj.insert("smtp_host".into(), Value::String(v.clone()));
        } else {
            obj.insert("smtp_host".into(), Value::Null);
        }
        if let Some(v) = &self.smtp_port {
            obj.insert("smtp_port".into(), Value::Number((*v).into()));
        } else {
            obj.insert("smtp_port".into(), Value::Null);
        }
        if let Some(v) = &self.smtp_username {
            obj.insert("smtp_username".into(), Value::String(v.clone()));
        } else {
            obj.insert("smtp_username".into(), Value::Null);
        }
        obj.insert("smtp_password".into(), Value::String("<redacted>".into()));
        obj.insert(
            "smtp_security".into(),
            self.smtp_security
                .map(security_to_json)
                .unwrap_or(Value::Null),
        );
        obj.insert(
            "certificate_checks".into(),
            self.certificate_checks
                .map(cert_to_json)
                .unwrap_or(Value::Null),
        );
        Ok(Value::Object(obj))
    }
}

fn security_to_json(s: DcLoginSecurity) -> serde_json::Value {
    use serde_json::Value;
    let s = match s {
        DcLoginSecurity::Ssl => "ssl",
        DcLoginSecurity::Starttls => "starttls",
        DcLoginSecurity::Plain => "plain",
        DcLoginSecurity::Default => "default",
    };
    Value::String(s.into())
}

fn cert_to_json(c: DcCertificateChecks) -> serde_json::Value {
    use serde_json::Value;
    let s = match c {
        DcCertificateChecks::Automatic => "automatic",
        DcCertificateChecks::Strict => "strict",
        DcCertificateChecks::AcceptInvalid => "accept_invalid",
    };
    Value::String(s.into())
}

impl serde::Serialize for DcLoginOptions {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Always redact at the source — any code path that calls
        // `serde_json::to_string(&payload)` on a `QrPayload::DcLogin`
        // (or anything containing one) will go through this method.
        // Callers needing the cleartext must build the JSON by hand
        // after invoking `expose()`.
        let safe = self.safe_serialize().map_err(serde::ser::Error::custom)?;
        safe.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for DcLoginOptions {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Accept both the redacted form produced by our `Serialize`
        // (password fields equal to `"<redacted>"`) and the cleartext
        // form we never expect to see on the wire. Treat the redacted
        // sentinel as "missing" so a round-trip from
        // `serde_json::to_string` back through `serde_json::from_str`
        // doesn't accidentally resurrect a credential marker.
        fn parse_security<E: serde::de::Error>(s: &str) -> Result<DcLoginSecurity, E> {
            match s {
                "ssl" => Ok(DcLoginSecurity::Ssl),
                "starttls" => Ok(DcLoginSecurity::Starttls),
                "plain" => Ok(DcLoginSecurity::Plain),
                "default" => Ok(DcLoginSecurity::Default),
                other => Err(E::custom(format!("unknown security level {other:?}"))),
            }
        }
        fn parse_cert<E: serde::de::Error>(s: &str) -> Result<DcCertificateChecks, E> {
            match s {
                "automatic" => Ok(DcCertificateChecks::Automatic),
                "strict" => Ok(DcCertificateChecks::Strict),
                "accept_invalid" => Ok(DcCertificateChecks::AcceptInvalid),
                other => Err(E::custom(format!("unknown cert check level {other:?}"))),
            }
        }

        #[derive(serde::Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            MailPw,
            ImapHost,
            ImapPort,
            ImapUsername,
            ImapPassword,
            ImapSecurity,
            SmtpHost,
            SmtpPort,
            SmtpUsername,
            SmtpPassword,
            SmtpSecurity,
            CertificateChecks,
        }

        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = DcLoginOptions;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("DCLOGIN options")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<DcLoginOptions, A::Error> {
                let mut out = DcLoginOptions {
                    mail_pw: String::new(),
                    imap_host: None,
                    imap_port: None,
                    imap_username: None,
                    imap_password: None,
                    imap_security: None,
                    smtp_host: None,
                    smtp_port: None,
                    smtp_username: None,
                    smtp_password: None,
                    smtp_security: None,
                    certificate_checks: None,
                };
                while let Some(key) = map.next_key::<Field>()? {
                    match key {
                        Field::MailPw => {
                            let v: Option<String> = map.next_value()?;
                            out.mail_pw = match v {
                                Some(s) if s != "<redacted>" => s,
                                _ => String::new(),
                            };
                        }
                        Field::ImapHost => out.imap_host = map.next_value()?,
                        Field::ImapPort => out.imap_port = map.next_value()?,
                        Field::ImapUsername => out.imap_username = map.next_value()?,
                        Field::ImapPassword => {
                            let v: Option<String> = map.next_value()?;
                            out.imap_password = match v {
                                Some(s) if s != "<redacted>" => Some(s),
                                _ => None,
                            };
                        }
                        Field::ImapSecurity => {
                            if let Some(s) = map.next_value::<Option<String>>()? {
                                out.imap_security = Some(parse_security::<A::Error>(&s)?);
                            }
                        }
                        Field::SmtpHost => out.smtp_host = map.next_value()?,
                        Field::SmtpPort => out.smtp_port = map.next_value()?,
                        Field::SmtpUsername => out.smtp_username = map.next_value()?,
                        Field::SmtpPassword => {
                            let v: Option<String> = map.next_value()?;
                            out.smtp_password = match v {
                                Some(s) if s != "<redacted>" => Some(s),
                                _ => None,
                            };
                        }
                        Field::SmtpSecurity => {
                            if let Some(s) = map.next_value::<Option<String>>()? {
                                out.smtp_security = Some(parse_security::<A::Error>(&s)?);
                            }
                        }
                        Field::CertificateChecks => {
                            if let Some(s) = map.next_value::<Option<String>>()? {
                                out.certificate_checks = Some(parse_cert::<A::Error>(&s)?);
                            }
                        }
                    }
                }
                Ok(out)
            }
        }
        deserializer.deserialize_map(V)
    }
}

/// Borrowed view of [`DcLoginOptions`] with the passwords visible.
/// Construct only via [`DcLoginOptions::expose`]; the `Debug` impl on
/// the wrapper still redacts by default and a `Display` impl is
/// intentionally NOT provided so accidental `println!` doesn't leak.
#[derive(Clone, Copy)]
pub struct DcLoginOptionsExposed<'a> {
    inner: &'a DcLoginOptions,
}

impl<'a> DcLoginOptionsExposed<'a> {
    pub fn mail_pw(&self) -> &'a str {
        &self.inner.mail_pw
    }
    pub fn imap_password(&self) -> Option<&'a str> {
        self.inner.imap_password.as_deref()
    }
    pub fn smtp_password(&self) -> Option<&'a str> {
        self.inner.smtp_password.as_deref()
    }
    pub fn inner(&self) -> &'a DcLoginOptions {
        self.inner
    }
}

impl<'a> std::fmt::Debug for DcLoginOptionsExposed<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DcLoginOptionsExposed")
            .field("mail_pw", &"<redacted-use mail_pw()>")
            .field(
                "imap_password",
                &self.inner.imap_password.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "smtp_password",
                &self.inner.smtp_password.as_ref().map(|_| "<redacted>"),
            )
            .field("options", &self.inner)
            .finish()
    }
}

/// Socket security encoded in a `DCLOGIN` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DcLoginSecurity {
    /// Implicit TLS (`ssl`).
    Ssl,
    /// STARTTLS upgrade (`starttls`).
    Starttls,
    /// Plaintext (`plain`).
    Plain,
    /// Let the client pick (`default`).
    Default,
}

/// Certificate-check policy encoded in a `DCLOGIN` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DcCertificateChecks {
    /// `0` — automatic; defer to provider-db (we keep this as an opt-in
    /// even though `a3net-mail` defaults to strict).
    Automatic,
    /// `1` — strict chain validation.
    Strict,
    /// `2` / `3` — accept invalid certificates (self-signed, expired).
    /// chatmail distinguishes the two; we collapse them.
    AcceptInvalid,
}

/// Group / broadcast metadata extracted from an `OPENPGP4FPR` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpenPgpGroup {
    /// Verifiable group invite (`OPENPGP4FPR:…#a=ADDR&g=NAME&x=ID&…`).
    Group { grpname: String, grpid: String },
    /// Broadcast invite (`…&b=NAME&x=ID&j=INVITENUMBER`).
    Broadcast { name: String, grpid: String },
}

/// Sub-fields parsed from an `OPENPGP4FPR:` or `https://i.delta.chat/#…`
/// payload. Returned by [`crate::chatmail::decode_openpgp`]; the caller
/// wraps it in [`QrPayload::OpenPgp4Fpr`] itself (or extends it with
/// SecureJoin state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPgp4FprFields {
    /// OpenPGP fingerprint, hex (uppercase).
    pub fingerprint: String,
    /// `a=` parameter; URL-decoded.
    pub address: Option<String>,
    /// `n=` parameter; URL-decoded.
    pub name: Option<String>,
    /// `i=` (or `j=` for broadcasts) parameter — the invite number.
    pub invitenumber: Option<String>,
    /// `s=` parameter — the authentication code.
    pub authcode: Option<String>,
    /// Either a group or broadcast invite.
    pub group: Option<OpenPgpGroup>,
}

impl QrPayload {
    /// Short, stable tag for logs / metrics.
    pub fn tag(&self) -> &'static str {
        match self {
            QrPayload::Email { .. } => "mailto",
            QrPayload::Matmsg { .. } => "matmsg",
            QrPayload::Vcard { .. } => "vcard",
            QrPayload::Smtp { .. } => "smtp",
            QrPayload::DcAccount { .. } => "dcaccount",
            QrPayload::DcLogin { .. } => "dclogin",
            QrPayload::DcBackup { .. } => "dcbackup",
            QrPayload::BackupTooNew { .. } => "backup_too_new",
            QrPayload::OpenPgp4Fpr(_) => "openpgp4fpr",
            QrPayload::Proxy { .. } => "proxy",
            QrPayload::Shadowsocks { .. } => "shadowsocks",
            QrPayload::Url { .. } => "url",
            QrPayload::Text { .. } => "text",
            #[cfg(feature = "a3net-types")]
            QrPayload::AdnetPeer { .. } => "a3net_peer",
            #[cfg(feature = "a3net-types")]
            QrPayload::AdnetAddr { .. } => "a3net_addr",
            #[cfg(feature = "a3net-types")]
            QrPayload::AdnetBlob { .. } => "a3net_blob",
            #[cfg(feature = "a3net-types")]
            QrPayload::AdnetSignedPeer { .. } => "a3net_signed_peer",
            #[cfg(feature = "a3net-token")]
            QrPayload::AdnetToken { .. } => "a3net_token",
            #[cfg(feature = "pairing")]
            QrPayload::AdnetPairing { .. } => "a3net_pairing",
        }
    }

    /// Whether this payload carries a credential / capability that
    /// would leak through `Debug`/`Display`/`Serialize`. Callers
    /// should consult this before logging or telemetry-exporting.
    pub fn carries_secret(&self) -> bool {
        matches!(self, QrPayload::DcLogin { .. } | QrPayload::DcBackup { .. })
    }

    /// Return a redacted `Debug` view of this payload. The
    /// credential-bearing variants print `<redacted>` instead of the
    /// password / auth token; other variants are formatted normally.
    pub fn safe_debug(&self) -> String {
        match self {
            QrPayload::DcLogin { address, options } => {
                format!("DcLogin {{ address: {address:?}, options: {options:?} }}")
            }
            QrPayload::DcBackup {
                version,
                node_addr_json,
                ..
            } => format!(
                "DcBackup {{ version: {version}, node_addr_json: {node_addr_json}, auth_token: <redacted> }}"
            ),
            other => format!("{other:?}"),
        }
    }

    /// Borrow the cleartext passwords of a `DCLOGIN` payload. Returns
    /// `None` if the variant isn't a login payload.
    pub fn expose_secrets(&self) -> Option<DcLoginOptionsExposed<'_>> {
        match self {
            QrPayload::DcLogin { options, .. } => Some(options.expose()),
            _ => None,
        }
    }

    /// Return the cleartext `auth_token` of a `DCBACKUP` payload, if
    /// present. Callers must record user consent before invoking.
    pub fn expose_auth_token(&self) -> Option<&str> {
        match self {
            QrPayload::DcBackup { auth_token, .. } => Some(auth_token.as_str()),
            _ => None,
        }
    }
}

impl serde::Serialize for QrPayload {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Build the JSON object by hand for the credential-bearing
        // variants and delegate to the derived serde for the rest.
        // The hand-built path guarantees the redaction list is
        // grep-able and never recurses through `to_value(self)`.
        match self {
            QrPayload::DcBackup {
                version,
                node_addr_json,
                ..
            } => {
                use serde::ser::SerializeStruct;
                let mut s = serializer.serialize_struct("DcBackup", 4)?;
                s.serialize_field("kind", "dc_backup")?;
                s.serialize_field("version", version)?;
                s.serialize_field("node_addr_json", node_addr_json)?;
                s.serialize_field("auth_token", "<redacted>")?;
                s.end()
            }
            QrPayload::DcLogin { address, options } => {
                use serde::ser::SerializeStruct;
                let mut s = serializer.serialize_struct("DcLogin", 3)?;
                s.serialize_field("kind", "dc_login")?;
                s.serialize_field("address", address)?;
                // `options.safe_serialize()` returns a `Value`; use
                // `serde_json::Value::serialize` so we don't recurse
                // through `DcLoginOptions::serialize` (which calls
                // `safe_serialize` again).
                let safe = options
                    .safe_serialize()
                    .map_err(serde::ser::Error::custom)?;
                s.serialize_field("options", &safe)?;
                s.end()
            }
            other => other.serialize_default(serializer),
        }
    }
}

/// Helper trait so the non-credential-bearing variants can serialise
/// without recursing through our `Serialize` impl. Implemented for
/// every variant by passing the value back through serde's normal
/// derive-driven serialisation path.
trait SerializeDefault {
    fn serialize_default<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error>;
}

// Manual serialisation of every non-credential variant. Keeping this
// in lock-step with the enum is the cost of avoiding the recursion bug
// (`serde_json::to_value(self)` would call our own `Serialize` impl);
// the round-trip test in `tests/round_trip.rs` asserts the format is
// stable.
impl SerializeDefault for QrPayload {
    fn serialize_default<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        // Manual serialisation of every non-credential variant. Keeping
        // this in lock-step with the enum is the cost of avoiding the
        // recursion bug; the test suite asserts the format is stable.
        use serde::ser::SerializeStruct;
        match self {
            QrPayload::Email {
                address,
                subject,
                body,
            } => {
                let mut s = serializer.serialize_struct("Email", 4)?;
                s.serialize_field("kind", "email")?;
                s.serialize_field("address", address)?;
                s.serialize_field("subject", subject)?;
                s.serialize_field("body", body)?;
                s.end()
            }
            QrPayload::Matmsg {
                address,
                subject,
                body,
            } => {
                let mut s = serializer.serialize_struct("Matmsg", 4)?;
                s.serialize_field("kind", "matmsg")?;
                s.serialize_field("address", address)?;
                s.serialize_field("subject", subject)?;
                s.serialize_field("body", body)?;
                s.end()
            }
            QrPayload::Vcard { name, address } => {
                let mut s = serializer.serialize_struct("Vcard", 3)?;
                s.serialize_field("kind", "vcard")?;
                s.serialize_field("name", name)?;
                s.serialize_field("address", address)?;
                s.end()
            }
            QrPayload::Smtp { address } => {
                let mut s = serializer.serialize_struct("Smtp", 2)?;
                s.serialize_field("kind", "smtp")?;
                s.serialize_field("address", address)?;
                s.end()
            }
            QrPayload::DcAccount { domain } => {
                let mut s = serializer.serialize_struct("DcAccount", 2)?;
                s.serialize_field("kind", "dc_account")?;
                s.serialize_field("domain", domain)?;
                s.end()
            }
            QrPayload::BackupTooNew { version } => {
                let mut s = serializer.serialize_struct("BackupTooNew", 2)?;
                s.serialize_field("kind", "backup_too_new")?;
                s.serialize_field("version", version)?;
                s.end()
            }
            QrPayload::OpenPgp4Fpr(fields) => fields.serialize(serializer),
            QrPayload::Proxy { url, host, port } => {
                let mut s = serializer.serialize_struct("Proxy", 4)?;
                s.serialize_field("kind", "proxy")?;
                s.serialize_field("url", url)?;
                s.serialize_field("host", host)?;
                s.serialize_field("port", port)?;
                s.end()
            }
            QrPayload::Shadowsocks { url, host, port } => {
                let mut s = serializer.serialize_struct("Shadowsocks", 4)?;
                s.serialize_field("kind", "shadowsocks")?;
                s.serialize_field("url", url)?;
                s.serialize_field("host", host)?;
                s.serialize_field("port", port)?;
                s.end()
            }
            QrPayload::Url { url } => {
                let mut s = serializer.serialize_struct("Url", 2)?;
                s.serialize_field("kind", "url")?;
                s.serialize_field("url", url)?;
                s.end()
            }
            QrPayload::Text { text } => {
                let mut s = serializer.serialize_struct("Text", 2)?;
                s.serialize_field("kind", "text")?;
                s.serialize_field("text", text)?;
                s.end()
            }
            #[cfg(feature = "a3net-types")]
            QrPayload::AdnetPeer { ticket } => {
                let mut s = serializer.serialize_struct("AdnetPeer", 2)?;
                s.serialize_field("kind", "a3net_peer")?;
                s.serialize_field("ticket", ticket)?;
                s.end()
            }
            #[cfg(feature = "a3net-types")]
            QrPayload::AdnetAddr { ticket } => {
                let mut s = serializer.serialize_struct("AdnetAddr", 2)?;
                s.serialize_field("kind", "a3net_addr")?;
                s.serialize_field("ticket", ticket)?;
                s.end()
            }
            #[cfg(feature = "a3net-types")]
            QrPayload::AdnetBlob { ticket } => {
                let mut s = serializer.serialize_struct("AdnetBlob", 2)?;
                s.serialize_field("kind", "a3net_blob")?;
                s.serialize_field("ticket", ticket)?;
                s.end()
            }
            #[cfg(feature = "a3net-types")]
            QrPayload::AdnetSignedPeer { ticket } => {
                let mut s = serializer.serialize_struct("AdnetSignedPeer", 2)?;
                s.serialize_field("kind", "a3net_signed_peer")?;
                s.serialize_field("ticket", ticket)?;
                s.end()
            }
            #[cfg(feature = "a3net-token")]
            QrPayload::AdnetToken { pledge } => {
                let mut s = serializer.serialize_struct("AdnetToken", 2)?;
                s.serialize_field("kind", "a3net_token")?;
                s.serialize_field("pledge", pledge)?;
                s.end()
            }
            #[cfg(feature = "pairing")]
            QrPayload::AdnetPairing { invitation } => {
                let mut s = serializer.serialize_struct("AdnetPairing", 2)?;
                s.serialize_field("kind", "a3net_pairing")?;
                s.serialize_field("invitation", invitation)?;
                s.end()
            }
            QrPayload::DcLogin { .. } | QrPayload::DcBackup { .. } => {
                unreachable!("handled by the parent serialize impl")
            }
        }
    }
}

impl std::fmt::Display for QrPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Routing `Display` through `safe_debug` ensures any future
        // tracing macro that prefers Display (e.g. `tracing::info!(?payload)`)
        // can't accidentally leak credentials. The redactable variants
        // print `<redacted>`; everything else prints the same as Debug.
        f.write_str(&self.safe_debug())
    }
}

impl std::fmt::Debug for QrPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.carries_secret() {
            // Force every credential-bearing variant through the safe
            // formatter; the rest print via the default derived Debug.
            f.write_str(&self.safe_debug())
        } else {
            match self {
                QrPayload::Email {
                    address,
                    subject,
                    body,
                } => f
                    .debug_struct("Email")
                    .field("address", address)
                    .field("subject", subject)
                    .field("body", body)
                    .finish(),
                QrPayload::Matmsg {
                    address,
                    subject,
                    body,
                } => f
                    .debug_struct("Matmsg")
                    .field("address", address)
                    .field("subject", subject)
                    .field("body", body)
                    .finish(),
                QrPayload::Vcard { name, address } => f
                    .debug_struct("Vcard")
                    .field("name", name)
                    .field("address", address)
                    .finish(),
                QrPayload::Smtp { address } => {
                    f.debug_struct("Smtp").field("address", address).finish()
                }
                QrPayload::DcAccount { domain } => {
                    f.debug_struct("DcAccount").field("domain", domain).finish()
                }
                QrPayload::BackupTooNew { version } => f
                    .debug_struct("BackupTooNew")
                    .field("version", version)
                    .finish(),
                QrPayload::OpenPgp4Fpr(fields) => f
                    .debug_struct("OpenPgp4Fpr")
                    .field("fingerprint", &fields.fingerprint)
                    .field("address", &fields.address)
                    .field("name", &fields.name)
                    .field("invitenumber", &fields.invitenumber)
                    .field("authcode", &fields.authcode)
                    .field("group", &fields.group)
                    .finish(),
                QrPayload::Proxy { url, host, port } => f
                    .debug_struct("Proxy")
                    .field("url", url)
                    .field("host", host)
                    .field("port", port)
                    .finish(),
                QrPayload::Shadowsocks { url, host, port } => f
                    .debug_struct("Shadowsocks")
                    .field("url", url)
                    .field("host", host)
                    .field("port", port)
                    .finish(),
                QrPayload::Url { url } => f.debug_struct("Url").field("url", url).finish(),
                QrPayload::Text { text } => f.debug_struct("Text").field("text", text).finish(),
                #[cfg(feature = "a3net-types")]
                QrPayload::AdnetPeer { ticket } => {
                    f.debug_struct("AdnetPeer").field("ticket", ticket).finish()
                }
                #[cfg(feature = "a3net-types")]
                QrPayload::AdnetAddr { ticket } => {
                    f.debug_struct("AdnetAddr").field("ticket", ticket).finish()
                }
                #[cfg(feature = "a3net-types")]
                QrPayload::AdnetBlob { ticket } => {
                    f.debug_struct("AdnetBlob").field("ticket", ticket).finish()
                }
                #[cfg(feature = "a3net-types")]
                QrPayload::AdnetSignedPeer { ticket } => f
                    .debug_struct("AdnetSignedPeer")
                    .field("ticket", ticket)
                    .finish(),
                #[cfg(feature = "a3net-token")]
                QrPayload::AdnetToken { pledge } => f
                    .debug_struct("AdnetToken")
                    .field("pledge", pledge)
                    .finish(),
                #[cfg(feature = "pairing")]
                QrPayload::AdnetPairing { invitation } => {
                    // PairingInvitation already redacts in Debug; we just
                    // route through it so logs see the same fields the
                    // pairing crate exposes.
                    f.debug_struct("AdnetPairing")
                        .field("invitation", invitation)
                        .finish()
                }
                QrPayload::DcLogin { .. } | QrPayload::DcBackup { .. } => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "mail")]
    fn full_dclogin() -> DcLoginOptions {
        DcLoginOptions {
            mail_pw: "supersecret".into(),
            imap_host: Some("imap.example.com".into()),
            imap_port: Some(993),
            imap_username: Some("alice".into()),
            imap_password: Some("imap-pw".into()),
            imap_security: Some(DcLoginSecurity::Ssl),
            smtp_host: Some("smtp.example.com".into()),
            smtp_port: Some(465),
            smtp_username: Some("alice".into()),
            smtp_password: Some("smtp-pw".into()),
            smtp_security: Some(DcLoginSecurity::Ssl),
            certificate_checks: Some(DcCertificateChecks::Strict),
        }
    }

    #[cfg(feature = "mail")]
    #[test]
    fn dclogin_to_account_happy_path() {
        let payload = QrPayload::DcLogin {
            address: "alice@example.com".into(),
            options: full_dclogin(),
        };
        let bridge = payload.into_mail_account().unwrap().unwrap();
        let acct = bridge.account();
        assert_eq!(acct.addr, "alice@example.com");
        assert_eq!(acct.imap.server, "imap.example.com");
        assert_eq!(acct.imap.port, 993);
        assert_eq!(acct.imap.user, "alice");
        assert_eq!(acct.imap.password, "imap-pw");
        assert_eq!(acct.smtp.server, "smtp.example.com");
        assert_eq!(acct.smtp.port, 465);
        assert_eq!(acct.smtp.password, "smtp-pw");
        assert!(matches!(
            acct.certificate_checks,
            a3net_mail::login_param::CertificateChecks::Strict
        ));
    }

    #[cfg(feature = "mail")]
    #[test]
    fn dclogin_to_account_falls_back_to_mail_pw() {
        let mut opts = full_dclogin();
        opts.imap_password = None;
        opts.smtp_password = None;
        let payload = QrPayload::DcLogin {
            address: "alice@example.com".into(),
            options: opts,
        };
        let acct = payload.into_mail_account().unwrap().unwrap().into_account();
        assert_eq!(acct.imap.password, "supersecret");
        assert_eq!(acct.smtp.password, "supersecret");
    }

    #[cfg(feature = "mail")]
    #[test]
    fn dclogin_to_account_rejects_plain_with_strict_certs() {
        let mut opts = full_dclogin();
        opts.imap_security = Some(DcLoginSecurity::Plain);
        // certificate_checks stays Strict
        let payload = QrPayload::DcLogin {
            address: "alice@example.com".into(),
            options: opts,
        };
        let err = payload.into_mail_account().unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::QrError::Malformed {
                    scheme: "dclogin",
                    ..
                }
            ),
            "got: {err:?}"
        );
        assert!(
            format!("{err}").contains("strict"),
            "error must explain strict/plain mismatch: {err}"
        );
    }

    #[cfg(feature = "mail")]
    #[test]
    fn dclogin_to_account_rejects_missing_imap_host() {
        let mut opts = full_dclogin();
        opts.imap_host = None;
        let payload = QrPayload::DcLogin {
            address: "alice@example.com".into(),
            options: opts,
        };
        let err = payload.into_mail_account().unwrap_err();
        assert!(matches!(
            err,
            crate::error::QrError::Malformed {
                scheme: "dclogin",
                ..
            }
        ));
        assert!(format!("{err}").contains("imap"));
    }

    #[cfg(feature = "mail")]
    #[test]
    fn dclogin_to_account_rejects_missing_smtp_host() {
        let mut opts = full_dclogin();
        opts.smtp_host = None;
        let payload = QrPayload::DcLogin {
            address: "alice@example.com".into(),
            options: opts,
        };
        let err = payload.into_mail_account().unwrap_err();
        assert!(matches!(
            err,
            crate::error::QrError::Malformed {
                scheme: "dclogin",
                ..
            }
        ));
        assert!(format!("{err}").contains("smtp"));
    }

    #[cfg(feature = "mail")]
    #[test]
    fn dclogin_to_account_returns_none_for_non_login_payload() {
        let payload = QrPayload::Text { text: "hi".into() };
        assert!(payload.into_mail_account().unwrap().is_none());
        let payload = QrPayload::Email {
            address: "a@b.c".into(),
            subject: None,
            body: None,
        };
        assert!(payload.into_mail_account().unwrap().is_none());
    }

    #[cfg(feature = "mail")]
    #[test]
    fn dclogin_to_account_accept_invalid_certs_kept() {
        let mut opts = full_dclogin();
        opts.certificate_checks = Some(DcCertificateChecks::AcceptInvalid);
        opts.imap_security = Some(DcLoginSecurity::Ssl);
        let payload = QrPayload::DcLogin {
            address: "alice@example.com".into(),
            options: opts,
        };
        let acct = payload.into_mail_account().unwrap().unwrap().into_account();
        assert!(matches!(
            acct.certificate_checks,
            a3net_mail::login_param::CertificateChecks::AcceptInvalid
        ));
    }

    #[cfg(feature = "mail")]
    #[test]
    fn dclogin_to_account_default_security_maps_to_tls() {
        // DCLOGIN's "default" security defers to provider-db;
        // we don't have one, so we assume implicit TLS.
        let mut opts = full_dclogin();
        opts.imap_security = Some(DcLoginSecurity::Default);
        opts.smtp_security = Some(DcLoginSecurity::Default);
        opts.certificate_checks = Some(DcCertificateChecks::Strict);
        let payload = QrPayload::DcLogin {
            address: "alice@example.com".into(),
            options: opts,
        };
        let acct = payload.into_mail_account().unwrap().unwrap().into_account();
        assert!(matches!(
            acct.imap.security,
            a3net_mail::login_param::SocketSecurity::Tls
        ));
        assert!(matches!(
            acct.smtp.security,
            a3net_mail::login_param::SocketSecurity::Tls
        ));
    }

    fn debug_redacts_secrets() {
        let payload = QrPayload::DcLogin {
            address: "alice@example.com".into(),
            options: DcLoginOptions {
                mail_pw: "supersecret".into(),
                imap_password: Some("imap-pw".into()),
                smtp_password: Some("smtp-pw".into()),
                imap_host: Some("imap.example.com".into()),
                imap_port: Some(993),
                imap_username: None,
                imap_security: Some(DcLoginSecurity::Ssl),
                smtp_host: None,
                smtp_port: None,
                smtp_username: None,
                smtp_security: None,
                certificate_checks: Some(DcCertificateChecks::Strict),
            },
        };
        let s = format!("{payload:?}");
        assert!(!s.contains("supersecret"), "Debug leaked password: {s}");
        assert!(!s.contains("imap-pw"), "Debug leaked imap pw: {s}");
        assert!(!s.contains("smtp-pw"), "Debug leaked smtp pw: {s}");
        assert!(s.contains("<redacted>"));
    }

    #[test]
    fn debug_redacts_dclogin() {
        debug_redacts_secrets();
    }

    #[test]
    fn debug_redacts_dcbackup() {
        let payload = QrPayload::DcBackup {
            version: 5,
            node_addr_json: "{}".into(),
            auth_token: "auth-token-secret".into(),
        };
        let s = format!("{payload:?}");
        assert!(!s.contains("auth-token-secret"), "Debug leaked token: {s}");
        assert!(s.contains("<redacted>"));
    }

    #[test]
    fn expose_returns_passwords() {
        let payload = QrPayload::DcLogin {
            address: "alice@example.com".into(),
            options: DcLoginOptions {
                mail_pw: "supersecret".into(),
                imap_password: None,
                smtp_password: None,
                imap_host: None,
                imap_port: None,
                imap_username: None,
                imap_security: None,
                smtp_host: None,
                smtp_port: None,
                smtp_username: None,
                smtp_security: None,
                certificate_checks: None,
            },
        };
        let exposed = payload.expose_secrets().unwrap();
        assert_eq!(exposed.mail_pw(), "supersecret");
    }

    #[test]
    fn safe_serialize_redacts_passwords() {
        let opts = DcLoginOptions {
            mail_pw: "supersecret".into(),
            imap_password: Some("imap-pw".into()),
            smtp_password: Some("smtp-pw".into()),
            imap_host: Some("imap.example.com".into()),
            imap_port: Some(993),
            imap_username: None,
            imap_security: Some(DcLoginSecurity::Ssl),
            smtp_host: None,
            smtp_port: None,
            smtp_username: None,
            smtp_security: None,
            certificate_checks: Some(DcCertificateChecks::Strict),
        };
        let inner = opts.safe_serialize().unwrap();
        let s = inner.to_string();
        assert!(!s.contains("supersecret"), "safe_serialize leaked pw: {s}");
        assert!(!s.contains("imap-pw"));
        assert!(!s.contains("smtp-pw"));
        assert!(s.contains("<redacted>"));
    }

    #[test]
    fn serialize_redacts_via_qrpayload() {
        // End-to-end: serde_json::to_string on a QrPayload containing
        // a DcLogin variant must redact the password. This is the
        // regression test for the original gap — before the manual
        // `Serialize` impl, this assertion failed because serde went
        // straight through the derived `Serialize` on `DcLoginOptions`.
        let payload = QrPayload::DcLogin {
            address: "alice@example.com".into(),
            options: DcLoginOptions {
                mail_pw: "supersecret".into(),
                imap_password: Some("imap-pw".into()),
                smtp_password: Some("smtp-pw".into()),
                imap_host: Some("imap.example.com".into()),
                imap_port: Some(993),
                imap_username: None,
                imap_security: Some(DcLoginSecurity::Ssl),
                smtp_host: None,
                smtp_port: None,
                smtp_username: None,
                smtp_security: None,
                certificate_checks: Some(DcCertificateChecks::Strict),
            },
        };
        let s = serde_json::to_string(&payload).unwrap();
        assert!(
            !s.contains("supersecret"),
            "QrPayload Serialize leaked pw: {s}"
        );
        assert!(!s.contains("imap-pw"));
        assert!(!s.contains("smtp-pw"));
        assert!(s.contains("<redacted>"));
    }

    #[test]
    fn serialize_redacts_dcbackup_auth_token() {
        let payload = QrPayload::DcBackup {
            version: 5,
            node_addr_json: "{}".into(),
            auth_token: "auth-token-secret".into(),
        };
        let s = serde_json::to_string(&payload).unwrap();
        assert!(
            !s.contains("auth-token-secret"),
            "QrPayload Serialize leaked auth_token: {s}"
        );
        assert!(s.contains("<redacted>"));
    }

    #[test]
    fn expose_secrets_for_non_login_returns_none() {
        let payload = QrPayload::Text { text: "hi".into() };
        assert!(payload.expose_secrets().is_none());
        assert!(payload.expose_auth_token().is_none());
        assert!(!payload.carries_secret());
    }

    /// Property-style check: every printable representation of a
    /// `QrPayload::DcLogin` / `QrPayload::DcBackup` must NOT contain
    /// the cleartext values, regardless of which formatting trait the
    /// caller reaches for. This guards against a future `Display`
    /// implementation or a new derived `Debug` accidentally re-adding
    /// the leak paths we just closed.
    #[test]
    fn no_credential_leak_in_any_format() {
        let login = QrPayload::DcLogin {
            address: "alice@example.com".into(),
            options: DcLoginOptions {
                mail_pw: "SECRET-MAIN-PW".into(),
                imap_password: Some("SECRET-IMAP-PW".into()),
                smtp_password: Some("SECRET-SMTP-PW".into()),
                imap_host: Some("imap.example.com".into()),
                imap_port: Some(993),
                imap_username: Some("alice".into()),
                imap_security: Some(DcLoginSecurity::Ssl),
                smtp_host: Some("smtp.example.com".into()),
                smtp_port: Some(465),
                smtp_username: Some("alice".into()),
                smtp_security: Some(DcLoginSecurity::Ssl),
                certificate_checks: Some(DcCertificateChecks::Strict),
            },
        };
        let backup = QrPayload::DcBackup {
            version: 5,
            node_addr_json: r#"{"node_id":"abc"}"#.into(),
            auth_token: "SECRET-BACKUP-TOKEN".into(),
        };

        for (label, formatted) in [
            ("Debug", format!("{login:?}")),
            ("Display", format!("{login}")),
            ("serde_json", serde_json::to_string(&login).unwrap()),
            ("safe_debug", login.safe_debug()),
            (
                "options safe_serialize",
                login
                    .expose_secrets()
                    .unwrap()
                    .inner()
                    .safe_serialize()
                    .unwrap()
                    .to_string(),
            ),
        ] {
            for secret in ["SECRET-MAIN-PW", "SECRET-IMAP-PW", "SECRET-SMTP-PW"] {
                assert!(
                    !formatted.contains(secret),
                    "{label} leaked {secret}: {formatted}"
                );
            }
        }
        for (label, formatted) in [
            ("Debug", format!("{backup:?}")),
            ("Display", format!("{backup}")),
            ("serde_json", serde_json::to_string(&backup).unwrap()),
            ("safe_debug", backup.safe_debug()),
        ] {
            assert!(
                !formatted.contains("SECRET-BACKUP-TOKEN"),
                "{label} leaked backup token: {formatted}"
            );
        }
    }
}
