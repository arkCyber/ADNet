//! Built-in provider database (chatmail `configure.rs` equivalent).
//!
//! A *provider* is a pair of IMAP + SMTP server configurations that
//! can be looked up by email-domain suffix. We ship a small
//! hard-coded list of well-known providers (the same set Delta Chat
//! documents in its `provider-db`) so a fresh `MailAccount` can be
//! constructed without the user having to type `imap.gmail.com` etc.
//!
//! The list is intentionally small; production deployments would
//! replace [`BUILTIN_PROVIDERS`] with a runtime-loaded JSON file.

use crate::error::{MailError, Result};
use crate::login_param::{CertificateChecks, ImapLoginParam, SmtpLoginParam, SocketSecurity};

/// One provider in the auto-config database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    /// Provider id (e.g. `"gmail"`, `"outlook"`).
    pub id: &'static str,
    /// Email domains this provider serves (e.g. `["gmail.com", "googlemail.com"]`).
    pub domains: &'static [&'static str],
    /// Display name shown to the user.
    pub display_name: &'static str,
    /// IMAP server template.
    pub imap: ServerTemplate,
    /// SMTP server template.
    pub smtp: ServerTemplate,
    /// OAuth2 supported?
    pub oauth2: bool,
}

/// Either a fixed hostname or `imap.{domain}` style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerTemplate {
    /// `host` is literal; `port` and `security` are fixed.
    Fixed {
        host: &'static str,
        port: u16,
        security: SocketSecurity,
    },
    /// Host is constructed as `template.replace("{domain}", domain)` at lookup time.
    Template {
        host_template: &'static str,
        port: u16,
        security: SocketSecurity,
    },
}

impl ServerTemplate {
    /// Materialize a `host` for a given email-domain.
    pub fn host_for(&self, domain: &str) -> String {
        match self {
            ServerTemplate::Fixed { host, .. } => (*host).into(),
            ServerTemplate::Template { host_template, .. } => {
                host_template.replace("{domain}", domain)
            }
        }
    }
}

impl Provider {
    /// Look up a provider by email-domain suffix. Returns `None` if no
    /// provider claims the domain.
    pub fn for_domain(domain: &str) -> Option<&'static Provider> {
        BUILTIN_PROVIDERS
            .iter()
            .find(|p| p.domains.iter().any(|d| d.eq_ignore_ascii_case(domain)))
    }

    /// Convenience: look up by full address (`alice@gmail.com` → `gmail.com`).
    pub fn for_address(addr: &str) -> Option<&'static Provider> {
        let (_, domain) = addr.split_once('@')?;
        Self::for_domain(domain)
    }

    /// Build a filled-in `ImapLoginParam` for `domain` and `user`.
    pub fn imap_for(&self, domain: &str, user: &str) -> ImapLoginParam {
        let (host, port, security) = match self.imap {
            ServerTemplate::Fixed {
                host,
                port,
                security,
            } => (host.into(), port, security),
            ServerTemplate::Template {
                host_template,
                port,
                security,
            } => (host_template.replace("{domain}", domain), port, security),
        };
        ImapLoginParam {
            server: host,
            port,
            folder: String::new(),
            security,
            user: user.into(),
            password: String::new(), // caller fills in
        }
    }

    /// Build a filled-in `SmtpLoginParam` for `domain` and `user`.
    pub fn smtp_for(&self, domain: &str, user: &str) -> SmtpLoginParam {
        let (host, port, security) = match self.smtp {
            ServerTemplate::Fixed {
                host,
                port,
                security,
            } => (host.into(), port, security),
            ServerTemplate::Template {
                host_template,
                port,
                security,
            } => (host_template.replace("{domain}", domain), port, security),
        };
        SmtpLoginParam {
            server: host,
            port,
            security,
            user: user.into(),
            password: String::new(),
        }
    }
}

/// Hard-coded list of well-known providers. Mirrors the spirit of
/// Delta Chat's `provider-db` but kept compact for this crate.
pub static BUILTIN_PROVIDERS: &[Provider] = &[
    Provider {
        id: "gmail",
        domains: &["gmail.com", "googlemail.com"],
        display_name: "Google Mail",
        imap: ServerTemplate::Fixed {
            host: "imap.gmail.com",
            port: 993,
            security: SocketSecurity::Tls,
        },
        smtp: ServerTemplate::Fixed {
            host: "smtp.gmail.com",
            port: 465,
            security: SocketSecurity::Tls,
        },
        oauth2: true,
    },
    Provider {
        id: "outlook",
        domains: &["outlook.com", "hotmail.com", "live.com", "msn.com"],
        display_name: "Microsoft Outlook",
        imap: ServerTemplate::Fixed {
            host: "outlook.office365.com",
            port: 993,
            security: SocketSecurity::Tls,
        },
        smtp: ServerTemplate::Fixed {
            host: "smtp.office365.com",
            port: 587,
            security: SocketSecurity::Starttls,
        },
        oauth2: true,
    },
    Provider {
        id: "yahoo",
        domains: &["yahoo.com", "ymail.com", "rocketmail.com"],
        display_name: "Yahoo Mail",
        imap: ServerTemplate::Fixed {
            host: "imap.mail.yahoo.com",
            port: 993,
            security: SocketSecurity::Tls,
        },
        smtp: ServerTemplate::Fixed {
            host: "smtp.mail.yahoo.com",
            port: 465,
            security: SocketSecurity::Tls,
        },
        oauth2: true,
    },
    Provider {
        id: "fastmail",
        domains: &["fastmail.com", "fastmail.fm", "messagingengine.com"],
        display_name: "Fastmail",
        imap: ServerTemplate::Fixed {
            host: "imap.fastmail.com",
            port: 993,
            security: SocketSecurity::Tls,
        },
        smtp: ServerTemplate::Fixed {
            host: "smtp.fastmail.com",
            port: 465,
            security: SocketSecurity::Tls,
        },
        oauth2: true,
    },
    Provider {
        id: "mailbox_org",
        domains: &["mailbox.org"],
        display_name: "mailbox.org",
        imap: ServerTemplate::Fixed {
            host: "imap.mailbox.org",
            port: 993,
            security: SocketSecurity::Tls,
        },
        smtp: ServerTemplate::Fixed {
            host: "smtp.mailbox.org",
            port: 465,
            security: SocketSecurity::Tls,
        },
        oauth2: false,
    },
    Provider {
        id: "posteo",
        domains: &["posteo.de", "posteo.net", "posteo.io"],
        display_name: "Posteo",
        imap: ServerTemplate::Fixed {
            host: "posteo.de",
            port: 993,
            security: SocketSecurity::Tls,
        },
        smtp: ServerTemplate::Fixed {
            host: "posteo.de",
            port: 465,
            security: SocketSecurity::Tls,
        },
        oauth2: false,
    },
];

/// Auto-configure an `ImapLoginParam` + `SmtpLoginParam` for the given
/// email address by looking up its domain in the provider database.
pub fn auto_configure(addr: &str) -> Result<(ImapLoginParam, SmtpLoginParam)> {
    let (_, domain) = addr
        .split_once('@')
        .ok_or_else(|| MailError::InvalidAddr(addr.into()))?;
    let provider = Provider::for_domain(domain)
        .ok_or_else(|| MailError::Config(format!("no provider known for {domain}")))?;
    Ok((
        provider.imap_for(domain, addr),
        provider.smtp_for(domain, addr),
    ))
}

/// Get a default `CertificateChecks` value, currently always
/// [`CertificateChecks::Strict`].
pub fn default_certificate_checks() -> CertificateChecks {
    CertificateChecks::Strict
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_address_finds_known_provider() {
        let p = Provider::for_address("alice@gmail.com").unwrap();
        assert_eq!(p.id, "gmail");
        let (imap, smtp) = auto_configure("alice@gmail.com").unwrap();
        assert_eq!(imap.server, "imap.gmail.com");
        assert_eq!(smtp.server, "smtp.gmail.com");
    }

    #[test]
    fn unknown_domain_returns_config_error() {
        let err = auto_configure("alice@example.invalid").unwrap_err();
        assert!(matches!(err, MailError::Config(_)));
    }

    #[test]
    fn template_host_substitutes_domain() {
        // Construct a synthetic provider with a templated host.
        let p = Provider {
            id: "test",
            domains: &["example.org"],
            display_name: "Test",
            imap: ServerTemplate::Template {
                host_template: "imap.{domain}",
                port: 143,
                security: SocketSecurity::Starttls,
            },
            smtp: ServerTemplate::Fixed {
                host: "smtp.example.org",
                port: 587,
                security: SocketSecurity::Starttls,
            },
            oauth2: false,
        };
        assert_eq!(p.imap_for("example.org", "u").server, "imap.example.org");
    }

    #[test]
    fn domain_matching_is_case_insensitive() {
        let p = Provider::for_domain("GMAIL.com");
        assert_eq!(p.unwrap().id, "gmail");
    }

    #[test]
    fn for_address_with_no_at_sign_returns_none() {
        assert!(Provider::for_address("not-an-email").is_none());
        assert!(auto_configure("not-an-email").is_err());
    }

    #[test]
    fn for_address_with_unknown_domain_returns_none() {
        assert!(Provider::for_address("alice@example.invalid").is_none());
        let err = auto_configure("alice@example.invalid").unwrap_err();
        assert!(matches!(err, MailError::Config(_)), "got {err:?}");
    }
}
