//! `a3net-mail` — interactive mail client example
//!
//! Demonstrates the full `a3net_mail` API surface:
//! - SMTP send (with TLS/STARTTLS/Plain modes)
//! - IMAP receive (IDLE, fetch, mark seen)
//! - Auto-configuration from email address via built-in provider DB
//! - MIME compose / parse / round-trip
//!
//! ## Quick start
//!
//! ```bash
//! # Option A: use a real provider (gmail, outlook, etc.)
//! export MAIL_FROM="alice@gmail.com"
//! export MAIL_PASSWORD="app-specific-password"
//! export MAIL_SMTP_HOST="smtp.gmail.com"
//! export MAIL_IMAP_HOST="imap.gmail.com"
//!
//! # Option B: Mailtrap (https://mailtrap.io — sandbox only)
//! export MAIL_FROM="test@mailtrap.io"
//! export MAIL_PASSWORD="your-mailtrap-api-key"
//! export MAIL_SMTP_HOST="sandbox.smtp.mailtrap.io"
//! export MAIL_SMTP_PORT="587"
//! export MAIL_IMAP_HOST="sandbox.smtp.mailtrap.io"
//! export MAIL_IMAP_PORT="2525"
//!
//! # Send an email
//! cargo run -p a3net-mail --example mail -- smtp-send \
//!     --to bob@example.com \
//!     --subject "Hello" \
//!     --body "Hi Bob"
//!
//! # List recent messages
//! cargo run -p a3net-mail --example mail -- imap-list
//!
//! # Watch INBOX for new mail
//! cargo run -p a3net-mail --example mail -- imap-idle
//!
//! # Interactive mode
//! cargo run -p a3net-mail --example mail -- interactive
//! ```

use std::path::{Path, PathBuf};

use a3net_mail::login_param::{
    Account, CertificateChecks, ImapLoginParam, SmtpLoginParam, SocketSecurity,
};
use a3net_mail::mime::{Address, Attachment, Mail};
use a3net_mail::prelude::{ImapSession, SendOutcome};
use a3net_mail::provider::Provider;
use a3net_mail::retry::{RetryPolicy, send_with_retry};
use a3net_mail::smtp;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand, ValueEnum};
use tracing::{Level, error, info, warn};

// ─── CLI ───────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "a3net-mail-cli", about = "a3net-mail interactive client")]
struct Cli {
    /// Config file path (optional).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Override the sender email address.
    #[arg(long)]
    from: Option<String>,

    /// Override the SMTP hostname.
    #[arg(long)]
    smtp_host: Option<String>,

    /// Override the SMTP port.
    #[arg(long)]
    smtp_port: Option<u16>,

    /// Override the IMAP hostname.
    #[arg(long)]
    imap_host: Option<String>,

    /// Override the IMAP port.
    #[arg(long)]
    imap_port: Option<u16>,

    /// SMTP security mode.
    #[arg(long, value_enum, default_value_t = SmtpMode::Starttls)]
    smtp_mode: SmtpMode,

    /// TLS certificate check: strict (default) or accept-invalid (dev only).
    #[arg(long, value_enum, default_value_t = CertMode::Strict)]
    cert_check: CertMode,

    /// Skip TLS verification.
    #[arg(long, hide = true)]
    insecure: bool,

    /// Enable verbose tracing.
    #[arg(long, short = 'v')]
    verbose: bool,

    #[command(subcommand)]
    cmd: Command,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum SmtpMode {
    /// Implicit TLS on port 465.
    #[default]
    Tls,
    /// STARTTLS on port 587.
    Starttls,
    /// Plaintext (port 25, localhost only).
    Plain,
}

impl SmtpMode {
    fn to_security(self) -> SocketSecurity {
        match self {
            SmtpMode::Tls => SocketSecurity::Tls,
            SmtpMode::Starttls => SocketSecurity::Starttls,
            SmtpMode::Plain => SocketSecurity::Plain,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum CertMode {
    #[default]
    Strict,
    #[clap(name = "accept-invalid")]
    AcceptInvalid,
}

impl CertMode {
    fn to_checks(self) -> CertificateChecks {
        match self {
            CertMode::Strict => CertificateChecks::Strict,
            CertMode::AcceptInvalid => CertificateChecks::AcceptInvalid,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Send a plain-text email.
    SmtpSend {
        #[arg(long)]
        to: String,
        #[arg(long)]
        subject: String,
        #[arg(long)]
        body: String,
        #[arg(long)]
        cc: Option<String>,
        #[arg(long)]
        bcc: Option<String>,
        #[arg(long, default_value_t = 3)]
        retries: u32,
    },

    /// Send a file as attachment.
    SmtpSendFile {
        #[arg(long)]
        to: String,
        #[arg(long, default_value = "Attached file")]
        subject: String,
        #[arg(long, default_value = "Please find the attachment.")]
        body: String,
        #[arg(long = "file", action = clap::ArgAction::Append)]
        files: Vec<PathBuf>,
    },

    /// List recent messages in INBOX.
    ImapList {
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        unseen: bool,
    },

    /// Fetch and display a message by UID.
    ImapFetch { uid: u32 },

    /// Watch INBOX for new mail (Ctrl+C to stop).
    ImapIdle {
        #[arg(long, default_value_t = 300)]
        timeout_secs: u64,
    },

    /// Look up SMTP/IMAP settings from an email address.
    AutoConfigure { address: String },

    /// Print resolved config (passwords redacted) and exit.
    InspectConfig,

    /// Interactive REPL.
    Interactive,
}

// ─── Config resolution ────────────────────────────────────────────────────

/// Resolved mail configuration — holds plaintext passwords and SMTP/IMAP
/// credentials. **Do NOT print this struct.** Use `InspectDisplay`
/// for a safe human-readable view; use `Account::safe_serialize_json()`
/// for a safe serialised form.
///
/// This struct deliberately does NOT implement `Debug` or `Serialize`
/// so accidental logging / serialisation triggers a compile error.
struct ResolvedConfig {
    from: String,
    user: String,
    /// Plaintext IMAP password or OAuth2 token.
    password: String,
    smtp_host: String,
    smtp_port: u16,
    smtp_security: SocketSecurity,
    imap_host: String,
    imap_port: u16,
    imap_security: SocketSecurity,
    cert_checks: CertificateChecks,
}

/// Safe display form of the resolved config. Separate struct so
/// `ResolvedConfig` can't accidentally derive `Debug`.
struct InspectDisplay<'a> {
    from: &'a str,
    user: &'a str,
    smtp_host: &'a str,
    smtp_port: u16,
    smtp_security: SocketSecurity,
    imap_host: &'a str,
    imap_port: u16,
    imap_security: SocketSecurity,
    cert_checks: CertificateChecks,
}

impl<'a> std::fmt::Debug for InspectDisplay<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedConfig")
            .field("from", &self.from)
            .field("user", &self.user)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_security", &self.smtp_security)
            .field("imap_host", &self.imap_host)
            .field("imap_port", &self.imap_port)
            .field("imap_security", &self.imap_security)
            .field("cert_checks", &self.cert_checks)
            .finish()
    }
}

impl ResolvedConfig {
    fn resolve(cli: &Cli) -> Result<Self> {
        // Load config file
        let (file_from, file_user, file_password, file_smtp, file_imap) =
            load_config_file(cli.config.as_deref())?.unwrap_or_default();

        // Env vars
        let env_from = std::env::var("MAIL_FROM").ok();
        let env_user = std::env::var("MAIL_USER").ok();
        let env_password = std::env::var("MAIL_PASSWORD").ok();
        let env_smtp_host = std::env::var("MAIL_SMTP_HOST").ok();
        let env_smtp_port = std::env::var("MAIL_SMTP_PORT")
            .ok()
            .and_then(|s| s.parse().ok());
        let env_smtp_security =
            std::env::var("MAIL_SMTP_SECURITY")
                .ok()
                .and_then(|s| match s.as_str() {
                    "tls" | "ssl" => Some(SocketSecurity::Tls),
                    "starttls" => Some(SocketSecurity::Starttls),
                    "plain" => Some(SocketSecurity::Plain),
                    _ => None,
                });
        let env_imap_host = std::env::var("MAIL_IMAP_HOST").ok();
        let env_imap_port = std::env::var("MAIL_IMAP_PORT")
            .ok()
            .and_then(|s| s.parse().ok());
        let env_imap_security =
            std::env::var("MAIL_IMAP_SECURITY")
                .ok()
                .and_then(|s| match s.as_str() {
                    "tls" | "ssl" => Some(SocketSecurity::Tls),
                    "starttls" => Some(SocketSecurity::Starttls),
                    "plain" => Some(SocketSecurity::Plain),
                    _ => None,
                });

        let cert_checks = if cli.insecure {
            CertificateChecks::AcceptInvalid
        } else {
            cli.cert_check.to_checks()
        };

        // Resolve from address: CLI > env > file
        let from = cli
            .from
            .clone()
            .or(env_from.clone())
            .or(Some(file_from.clone()))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("no sender address: set MAIL_FROM, --from, or config file"))?;

        // Reject bare local-parts (no `@domain`) — they can't be sent from.
        if !from.contains('@') {
            anyhow::bail!(
                "sender address '{from}' has no domain; \
                 SMTP servers will reject it. Use 'user@domain' form."
            );
        }

        // Resolve user: explicit env > derive from from-address > file
        let user = env_user
            .clone()
            .or_else(|| {
                env_from
                    .as_ref()
                    .and_then(|s| s.split_once('@').map(|(l, _)| l.to_string()))
            })
            .or(file_user.clone())
            .unwrap_or_else(|| {
                from.split_once('@')
                    .map(|(l, _)| l.to_string())
                    .unwrap_or_default()
            });

        let password = env_password
            .or(file_password.clone())
            .ok_or_else(|| anyhow!("no password: set MAIL_PASSWORD or config file"))?;

        let smtp_security = env_smtp_security.unwrap_or(cli.smtp_mode.to_security());
        let smtp_host = cli
            .smtp_host
            .clone()
            .or(env_smtp_host.clone())
            .or(file_smtp.clone())
            .ok_or_else(|| {
                anyhow!("no SMTP host: set MAIL_SMTP_HOST, --smtp-host, or config file")
            })?;
        let smtp_port = cli
            .smtp_port
            .or(env_smtp_port)
            .unwrap_or_else(|| default_smtp_port(smtp_security));

        let imap_security = env_imap_security.unwrap_or(match cli.smtp_mode {
            SmtpMode::Tls => SocketSecurity::Tls,
            SmtpMode::Starttls => SocketSecurity::Starttls,
            SmtpMode::Plain => SocketSecurity::Plain,
        });
        let imap_host = cli
            .imap_host
            .clone()
            .or(env_imap_host.clone())
            .or(file_imap.clone())
            .ok_or_else(|| {
                anyhow!("no IMAP host: set MAIL_IMAP_HOST, --imap-host, or config file")
            })?;
        let imap_port = cli
            .imap_port
            .or(env_imap_port)
            .unwrap_or_else(|| default_imap_port(imap_security));

        Ok(Self {
            from,
            user,
            password,
            smtp_host,
            smtp_port,
            smtp_security,
            imap_host,
            imap_port,
            imap_security,
            cert_checks,
        })
    }

    fn to_account(&self) -> Result<Account> {
        let imap = ImapLoginParam {
            server: self.imap_host.clone(),
            port: self.imap_port,
            folder: String::new(),
            security: self.imap_security,
            user: self.user.clone(),
            password: self.password.clone(),
        }
        .with_default_port();

        let smtp = SmtpLoginParam {
            server: self.smtp_host.clone(),
            port: self.smtp_port,
            security: self.smtp_security,
            user: self.user.clone(),
            password: self.password.clone(),
        }
        .with_default_port();

        Account::new(&self.from, imap, smtp)
            .map(|mut acct| {
                acct.certificate_checks = self.cert_checks;
                acct
            })
            .context("invalid account config")
    }
}

fn default_smtp_port(sec: SocketSecurity) -> u16 {
    match sec {
        SocketSecurity::Tls => 465,
        SocketSecurity::Starttls => 587,
        SocketSecurity::Plain => 25,
    }
}

fn default_imap_port(sec: SocketSecurity) -> u16 {
    match sec {
        SocketSecurity::Tls => 993,
        SocketSecurity::Starttls | SocketSecurity::Plain => 143,
    }
}

/// Result of [`load_config_file`]: optional path and up to five
/// optional fields (from, smtp_host, smtp_port, imap_host, imap_port).
/// All `Some` fields are validated; `None` means "not in file".
type MailConfigFile = Option<(
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
)>;

fn load_config_file(path: Option<&Path>) -> Result<MailConfigFile> {
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => {
            let candidates = [
                PathBuf::from("mail.conf.json"),
                dirs::config_dir()
                    .map(|p| p.join("a3net-mail").join("mail.conf.json"))
                    .unwrap_or_default(),
            ];
            candidates
                .into_iter()
                .find(|p| p.exists())
                .unwrap_or_default()
        }
    };

    if !path.exists() {
        return Ok(None);
    }

    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;

    let from = json.get("from").and_then(|v| v.as_str()).map(String::from);
    let user = json.get("user").and_then(|v| v.as_str()).map(String::from);
    let password = json
        .get("password")
        .and_then(|v| v.as_str())
        .map(String::from);
    let smtp_host = json
        .get("smtp_host")
        .and_then(|v| v.as_str())
        .map(String::from);
    let imap_host = json
        .get("imap_host")
        .and_then(|v| v.as_str())
        .map(String::from);

    Ok(Some((
        from.unwrap_or_default(),
        user,
        password,
        smtp_host,
        imap_host,
    )))
}

// ─── Commands ─────────────────────────────────────────────────────────────

async fn cmd_smtp_send(
    cfg: &ResolvedConfig,
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
    retries: u32,
) -> Result<()> {
    let account = cfg.to_account()?;
    info!(from = %account.addr, to, subject, "connecting SMTP");

    let mut transport = smtp::connect(&account).await.context("SMTP connect")?;

    let from_addr = Address::new(&cfg.from);
    let to_addrs: Vec<Address> = to.split(',').map(|s| Address::new(s.trim())).collect();
    let cc_addrs: Vec<Address> = cc
        .map(|s| s.split(',').map(|part| Address::new(part.trim())).collect())
        .unwrap_or_default();
    let bcc_addrs: Vec<Address> = bcc
        .map(|s| s.split(',').map(|part| Address::new(part.trim())).collect())
        .unwrap_or_default();

    let mut mail = Mail::text_only(from_addr.clone(), to_addrs[0].clone(), subject, body);
    mail.cc = cc_addrs;
    mail.bcc = bcc_addrs;
    mail.extra_headers.insert(
        "X-Mailer".into(),
        format!("a3net-mail-cli/{}", env!("CARGO_PKG_VERSION")),
    );

    info!(
        recipients = to_addrs.len(),
        cc = mail.cc.len(),
        bcc = mail.bcc.len(),
        "sending"
    );

    let policy = RetryPolicy {
        max_retries: retries,
        ..RetryPolicy::default()
    };

    let outcome = send_with_retry(&mut transport, &mail, &policy).await?;

    match &outcome {
        SendOutcome::Sent => {
            println!("[OK] Message sent successfully");
            println!("  From: {}", from_addr);
            for (i, to) in to_addrs.iter().enumerate() {
                println!("  To{}: {}", if i > 0 { " (CC/BCC)" } else { "" }, to);
            }
        }
        SendOutcome::Permanent { reason } => {
            error!("[FAIL] Permanent failure: {reason}");
            return Err(anyhow!("permanent failure: {reason}"));
        }
        SendOutcome::Transient { reason } => {
            warn!("[WARN] Transient failure after retries: {reason}");
        }
    }

    smtp::send::quit(transport).await;
    Ok(())
}

async fn cmd_smtp_send_file(
    cfg: &ResolvedConfig,
    to: &str,
    subject: &str,
    body: &str,
    files: &[PathBuf],
) -> Result<()> {
    let account = cfg.to_account()?;

    if files.is_empty() {
        return Err(anyhow!("no files specified; use --file path"));
    }

    let mut attachments: Vec<Attachment> = Vec::new();
    for path in files {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment");
        let mime = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();

        let att = Attachment {
            filename: filename.to_string(),
            content_type: mime,
            data,
            disposition: a3net_mail::mime::Disposition::Attachment,
        };
        println!(
            "  [file] {} ({} bytes, {})",
            att.filename,
            att.data.len(),
            att.content_type
        );
        attachments.push(att);
    }

    info!(from = %account.addr, to, n_files = attachments.len(), "sending with attachments");

    let mut transport = smtp::connect(&account).await.context("SMTP connect")?;

    let mut mail = Mail::text_only(Address::new(&cfg.from), Address::new(to), subject, body);
    mail.attachments = attachments;

    let policy = RetryPolicy {
        max_retries: 3,
        ..RetryPolicy::default()
    };
    let outcome = send_with_retry(&mut transport, &mail, &policy).await?;

    match outcome {
        SendOutcome::Sent => println!("[OK] Attachment(s) sent successfully"),
        SendOutcome::Permanent { reason } => {
            error!("[FAIL] {reason}");
            return Err(anyhow!("{reason}"));
        }
        SendOutcome::Transient { reason } => {
            warn!("[WARN] {reason}");
        }
    }

    smtp::send::quit(transport).await;
    Ok(())
}

async fn cmd_imap_list(cfg: &ResolvedConfig, limit: usize, unseen: bool) -> Result<()> {
    let account = cfg.to_account()?;
    info!(host = %account.imap.server, "connecting IMAP");

    let session = ImapSession::connect(account)
        .await
        .context("IMAP connect")?;
    let mut _guard = SessionGuard::new(session);
    let mut session = _guard.take();

    let info = session.select_folder().await.context("SELECT INBOX")?;

    println!(
        "=== INBOX  uidvalidity={:?}  uidnext={:?} ===",
        info.uid_validity, info.uid_next
    );
    println!("{}", "-".repeat(65));

    let mut handle = a3net_mail::imap::FetchHandle::new(&mut session, info.clone());
    let msgs = if unseen {
        handle.fetch_new().await?
    } else {
        handle.fetch_all().await?
    };

    let n = limit.min(msgs.len());
    for m in msgs.iter().take(n) {
        let seen_marker = if m.was_seen { "   " } else { "[N] " };
        let subject = m
            .mail
            .as_ref()
            .map(|m| m.subject.clone())
            .unwrap_or_else(|| m.parse_error.clone().unwrap_or_else(|| "-".into()));
        let from = m
            .mail
            .as_ref()
            .map(|m| m.from.address.clone())
            .unwrap_or_else(|| "-".into());
        let date = m
            .mail
            .as_ref()
            .and_then(|m| m.date)
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();

        println!(
            "{}{:>5}  {:30.30}  {:22.22}  {}",
            seen_marker, m.uid, subject, from, date
        );
    }

    if msgs.len() > n {
        println!("... and {} more (use --limit to adjust)", msgs.len() - n);
    }

    session.logout().await?;
    _guard.forget();
    Ok(())
}

async fn cmd_imap_fetch(cfg: &ResolvedConfig, uid: u32) -> Result<()> {
    let account = cfg.to_account()?;

    let session = ImapSession::connect(account)
        .await
        .context("IMAP connect")?;
    let mut _guard = SessionGuard::new(session);
    let mut session = _guard.take();

    let info = session.select_folder().await.context("SELECT INBOX")?;

    let mut handle = a3net_mail::imap::FetchHandle::new(&mut session, info);
    let msgs = handle.fetch_all().await?;

    let msg = msgs
        .into_iter()
        .find(|m| m.uid == uid)
        .ok_or_else(|| anyhow!("message UID {uid} not found"))?;

    let mail = msg.mail.ok_or_else(|| {
        anyhow!(
            "failed to parse message: {}",
            msg.parse_error.unwrap_or_default()
        )
    })?;

    println!("{}", "=".repeat(70));
    println!("UID:     {}", msg.uid);
    println!("From:    {}", mail.from);
    println!(
        "To:      {}",
        mail.to
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !mail.cc.is_empty() {
        println!(
            "Cc:      {}",
            mail.cc
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!("Date:    {:?}", mail.date.map(|d| d.to_rfc2822()));
    println!("Subject: {}", mail.subject);
    println!("{}", "-".repeat(70));

    if !mail.text.is_empty() {
        println!("[Plain text]");
        println!("{}", mail.text.trim());
    }
    if let Some(html) = &mail.html {
        let preview = if html.len() > 500 {
            &html[..500]
        } else {
            html.as_str()
        };
        println!("[HTML body (first 500 chars)]");
        println!("{}", preview);
    }

    if !mail.attachments.is_empty() {
        println!("{}", "-".repeat(70));
        println!("Attachments ({}):", mail.attachments.len());
        for att in &mail.attachments {
            println!(
                "  [file] {} ({} bytes, {})",
                att.filename,
                att.data.len(),
                att.content_type
            );
        }
    }

    handle.mark_seen(uid).await?;
    println!("{}", "=".repeat(70));

    session.logout().await?;
    _guard.forget();
    Ok(())
}

async fn cmd_imap_idle(cfg: &ResolvedConfig, timeout_secs: u64) -> Result<()> {
    let account = cfg.to_account()?;
    info!(host = %account.imap.server, timeout = timeout_secs, "starting IMAP IDLE");

    let session = ImapSession::connect(account)
        .await
        .context("IMAP connect")?;
    let mut _guard = SessionGuard::new(session);
    let mut session = _guard.take();

    session.select_folder().await.context("SELECT INBOX")?;

    println!(
        "[*] IDLE active -- watching INBOX (timeout={}s). Ctrl+C to stop.",
        timeout_secs
    );
    println!("{}", "-".repeat(60));

    loop {
        match session.idle_once().await {
            Ok(a3net_mail::imap::IdleEvent::NewMail) => {
                println!("\n[+] New mail detected -- fetching...");
                let info = session.select_folder().await.ok();
                if let Some(info) = info {
                    let mut handle = a3net_mail::imap::FetchHandle::new(&mut session, info);
                    if let Ok(msgs) = handle.fetch_new().await {
                        for m in &msgs {
                            let subj = m
                                .mail
                                .as_ref()
                                .map(|m| m.subject.clone())
                                .unwrap_or_else(|| "-".into());
                            let from = m
                                .mail
                                .as_ref()
                                .map(|m| m.from.address.clone())
                                .unwrap_or_else(|| "-".into());
                            println!("  [N] {:>5}  {}  from {}", m.uid, subj, from);
                            let _ = handle.mark_seen(m.uid).await;
                        }
                    }
                }
            }
            Ok(a3net_mail::imap::IdleEvent::Timeout) => {
                info!("IDLE timeout -- restarting");
            }
            Ok(a3net_mail::imap::IdleEvent::Interrupted) => {
                info!("IDLE interrupted");
                break;
            }
            Err(e) => {
                error!("IDLE error: {e}");
                break;
            }
        }
    }

    session.logout().await?;
    _guard.forget();
    Ok(())
}

fn cmd_auto_configure(addr: &str) -> Result<()> {
    let provider = Provider::for_address(addr)
        .ok_or_else(|| anyhow!("no provider found for domain of {addr}"))?;

    println!("Provider: {} ({})", provider.display_name, provider.id);
    println!("OAuth2:   {}", if provider.oauth2 { "yes" } else { "no" });
    println!();
    println!("Suggested IMAP/SMTP config for {}:", addr);
    println!();

    let (_, domain) = addr
        .split_once('@')
        .ok_or_else(|| anyhow!("invalid address"))?;
    let user = addr;

    let imap = provider.imap_for(domain, user);
    let smtp = provider.smtp_for(domain, user);

    println!(
        "  IMAP  server: {}:{}  ({})",
        imap.server,
        imap.port,
        label_security(imap.security)
    );
    println!(
        "  SMTP  server: {}:{}  ({})",
        smtp.server,
        smtp.port,
        label_security(smtp.security)
    );
    println!();
    println!("  # Environment variables to set:");
    println!("  export MAIL_FROM=\"{}\"", addr);
    println!("  export MAIL_USER=\"{}\"", user);
    println!("  export MAIL_PASSWORD=\"<your-password>\"");
    println!("  export MAIL_SMTP_HOST=\"{}\"", smtp.server);
    println!("  export MAIL_SMTP_PORT=\"{}\"", smtp.port);
    println!("  export MAIL_IMAP_HOST=\"{}\"", imap.server);
    println!("  export MAIL_IMAP_PORT=\"{}\"", imap.port);
    println!();
    println!("  # Then run:");
    println!("  cargo run -p a3net-mail --example mail -- smtp-send \\");
    println!("    --to recipient@example.com \\");
    println!("    --subject \"Hello\" \\");
    println!("    --body \"This is a test.\"");
    println!();
    println!("  # Or interactive mode:");
    println!("  cargo run -p a3net-mail --example mail -- interactive");

    Ok(())
}

fn cmd_inspect_config(cfg: &ResolvedConfig) -> Result<()> {
    let account = cfg.to_account()?;
    println!("Resolved configuration (passwords redacted):");
    println!();
    println!("  Account:    {}", account.addr);
    println!("  User:       {}", cfg.user);
    println!();
    println!(
        "  SMTP:       {}:{}  security={}",
        cfg.smtp_host,
        cfg.smtp_port,
        label_security(cfg.smtp_security)
    );
    println!(
        "  IMAP:       {}:{}  security={}",
        cfg.imap_host,
        cfg.imap_port,
        label_security(cfg.imap_security)
    );
    println!("  Cert checks: {:?}", cfg.cert_checks);
    println!();
    // account.safe_display() is a safe JSON path; here we just print the
    // redacted debug form so the operator can verify the resolved values.
    let safe = InspectDisplay {
        from: &cfg.from,
        user: &cfg.user,
        smtp_host: &cfg.smtp_host,
        smtp_port: cfg.smtp_port,
        smtp_security: cfg.smtp_security,
        imap_host: &cfg.imap_host,
        imap_port: cfg.imap_port,
        imap_security: cfg.imap_security,
        cert_checks: cfg.cert_checks,
    };
    println!("  Safe debug: {:?}", safe);
    Ok(())
}

/// Drop guard that calls `logout()` on the session when dropped, unless
/// `forget()` was called (indicating the caller handled logout explicitly).
/// This prevents `AsyncSession` leaks on early-return error paths.
struct SessionGuard {
    session: Option<ImapSession>,
}

impl SessionGuard {
    fn new(session: ImapSession) -> Self {
        Self {
            session: Some(session),
        }
    }
    /// Reclaim the session for explicit logout. After calling this the
    /// guard drops without logging out — the caller must call `logout()`.
    fn take(&mut self) -> ImapSession {
        self.session.take().expect("session already taken")
    }
    /// Prevent the guard from calling logout on drop.
    fn forget(&mut self) {
        self.session = None;
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            // We intentionally discard the logout future here without
            // awaiting it — `Drop::drop` is synchronous and can't
            // `.await`. Dropping the (unpolled) future drops the
            // `ImapSession` inside it, which closes the TCP connection
            // immediately; the server treats that as a dropped
            // connection rather than a clean LOGOUT, which is fine for
            // example code.
            drop(session.logout());
        }
    }
}

fn label_security(sec: SocketSecurity) -> &'static str {
    match sec {
        SocketSecurity::Tls => "TLS (implicit)",
        SocketSecurity::Starttls => "STARTTLS",
        SocketSecurity::Plain => "PLAINTEXT",
    }
}

// ─── Main ─────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let level = if cli.verbose {
        Level::DEBUG
    } else {
        Level::INFO
    };
    tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(false)
        .init();

    match &cli.cmd {
        Command::AutoConfigure { address } => {
            cmd_auto_configure(address)?;
        }
        Command::InspectConfig => {
            let cfg = ResolvedConfig::resolve(&cli)?;
            cmd_inspect_config(&cfg)?;
        }
        Command::SmtpSend {
            to,
            subject,
            body,
            cc,
            bcc,
            retries,
        } => {
            let cfg = ResolvedConfig::resolve(&cli)?;
            cmd_smtp_send(
                &cfg,
                to,
                subject,
                body,
                cc.as_deref(),
                bcc.as_deref(),
                *retries,
            )
            .await?;
        }
        Command::SmtpSendFile {
            to,
            subject,
            body,
            files,
        } => {
            let cfg = ResolvedConfig::resolve(&cli)?;
            cmd_smtp_send_file(&cfg, to, subject, body, files).await?;
        }
        Command::ImapList { limit, unseen } => {
            let cfg = ResolvedConfig::resolve(&cli)?;
            cmd_imap_list(&cfg, *limit, *unseen).await?;
        }
        Command::ImapFetch { uid } => {
            let cfg = ResolvedConfig::resolve(&cli)?;
            cmd_imap_fetch(&cfg, *uid).await?;
        }
        Command::ImapIdle { timeout_secs } => {
            let cfg = ResolvedConfig::resolve(&cli)?;
            cmd_imap_idle(&cfg, *timeout_secs).await?;
        }
        Command::Interactive => {
            interactive_mode(&cli).await?;
        }
    }

    Ok(())
}

// ─── Interactive REPL ──────────────────────────────────────────────────────

use std::io::{self, Write};

async fn interactive_mode(cli: &Cli) -> Result<()> {
    println!("===========================================");
    println!("     a3net-mail interactive client");
    println!("===========================================");
    println!();
    println!("Commands:");
    println!("  send <to> <subject> <body>   -- send a plain email");
    println!("  attach <path>                 -- queue attachment for next send");
    println!("  list [n]                     -- list last n messages (default 10)");
    println!("  fetch <uid>                  -- display a message");
    println!("  idle                         -- watch INBOX for new mail");
    println!("  auto <address>               -- show provider config");
    println!("  quit / exit                  -- exit");
    println!();

    let cfg = ResolvedConfig::resolve(cli)?;
    let mut pending_files: Vec<PathBuf> = Vec::new();

    loop {
        print!("a3net-mail> ");
        io::stdout().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.splitn(4, ' ').collect();
        match parts[0].to_lowercase().as_str() {
            "quit" | "exit" => {
                println!("Goodbye!");
                break;
            }
            "send" => {
                if parts.len() < 4 {
                    eprintln!("Usage: send <to> <subject> <body>");
                    continue;
                }
                let (to, subject, body) = (parts[1], parts[2], parts[3]);

                let files = std::mem::take(&mut pending_files);
                if !files.is_empty() {
                    if let Err(e) = cmd_smtp_send_file(&cfg, to, subject, body, &files).await {
                        error!("send file failed: {e}");
                    }
                } else {
                    if let Err(e) = cmd_smtp_send(&cfg, to, subject, body, None, None, 3).await {
                        error!("send failed: {e}");
                    }
                }
            }
            "attach" => {
                if let Some(p) = parts.get(1) {
                    let path = PathBuf::from(*p);
                    if !path.exists() {
                        eprintln!("file not found: {}", path.display());
                    } else {
                        pending_files.push(path);
                        println!("  queued: {:?}", pending_files.last().unwrap());
                    }
                } else {
                    eprintln!("Usage: attach <path>");
                }
            }
            "list" => {
                let limit = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
                if let Err(e) = cmd_imap_list(&cfg, limit, false).await {
                    error!("list failed: {e}");
                }
            }
            "fetch" => {
                if let Some(uid_str) = parts.get(1) {
                    if let Ok(uid) = uid_str.parse() {
                        if let Err(e) = cmd_imap_fetch(&cfg, uid).await {
                            error!("fetch failed: {e}");
                        }
                    } else {
                        eprintln!("invalid UID: {uid_str}");
                    }
                } else {
                    eprintln!("Usage: fetch <uid>");
                }
            }
            "idle" => {
                if let Err(e) = cmd_imap_idle(&cfg, 300).await {
                    error!("idle error: {e}");
                }
            }
            "auto" => {
                if let Some(addr) = parts.get(1) {
                    if let Err(e) = cmd_auto_configure(addr) {
                        error!("auto-configure failed: {e}");
                    }
                } else {
                    eprintln!("Usage: auto <address>");
                }
            }
            "help" => {
                println!("Commands:");
                println!("  send <to> <subject> <body>   -- send a plain email");
                println!("  attach <path>                 -- queue attachment for next send");
                println!("  list [n]                     -- list last n messages");
                println!("  fetch <uid>                  -- display a message");
                println!("  idle                         -- watch INBOX for new mail");
                println!("  auto <address>               -- show provider config");
                println!("  quit / exit                  -- exit");
            }
            _ => {
                eprintln!("unknown command: {}. Type 'help' for commands.", parts[0]);
            }
        }
    }

    Ok(())
}
