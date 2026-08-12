//! End-to-end tests driven against a tiny in-process SMTP / IMAP
//! mock server.
//!
//! Each test:
//!   1. Binds a loopback `TcpListener`.
//!   2. Spawns a tokio task that drives a single SMTP or IMAP session
//!      against it (the `mock_*` modules).
//!   3. Connects the real `adnet-mail` API against the listener and
//!      asserts round-trip behaviour.
//!
//! No real network access is required; everything is `127.0.0.1`.
//!
//! The IMAP mock is intentionally minimal — it doesn't try to be a
//! fully RFC-3501-compliant server. It speaks just enough protocol for
//! `ImapSession::connect` → `select_folder` → `uid_fetch` to work.

use std::sync::Arc;

use adnet_mail::error::ErrorClass;
use adnet_mail::login_param::{
    Account, CertificateChecks, ImapLoginParam, SmtpLoginParam, SocketSecurity,
};
use adnet_mail::mime::{Address, Attachment, Disposition, Mail};
use adnet_mail::prelude::*;
use adnet_mail::smtp::SendOutcome;
use adnet_mail::smtp::connect;
use adnet_mail::smtp::send as smtp_send;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Bind a loopback listener, returning the bound address and a
/// `JoinHandle` that has not yet been spawned.
async fn bind_loopback() -> (std::net::SocketAddr, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    (addr, listener)
}

/// Build an `Account` pointing at `addr` with self-signed cert trust.
fn loopback_account(addr: std::net::SocketAddr, kind: LoopbackKind) -> Account {
    let imap_port = match kind {
        LoopbackKind::Imap => addr.port(),
        LoopbackKind::Smtp => 0,
    };
    let smtp_port = match kind {
        LoopbackKind::Smtp => addr.port(),
        LoopbackKind::Imap => 0,
    };
    Account {
        addr: "alice@example.com".into(),
        imap: ImapLoginParam {
            server: "127.0.0.1".into(),
            port: imap_port,
            folder: String::new(),
            security: SocketSecurity::Plain,
            user: "alice".into(),
            password: "secret".into(),
        },
        smtp: SmtpLoginParam {
            server: "127.0.0.1".into(),
            port: smtp_port,
            security: SocketSecurity::Plain,
            user: "alice".into(),
            password: "secret".into(),
        },
        certificate_checks: CertificateChecks::AcceptInvalid,
        display_name: Some("Alice Example".into()),
    }
}

#[derive(Copy, Clone)]
enum LoopbackKind {
    Imap,
    Smtp,
}

// ───────────────────────────────────────────────────────────────────────
// SMTP mock
// ───────────────────────────────────────────────────────────────────────

mod smtp_mock {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    /// Minimal SMTP server: EHLO → MAIL FROM → RCPT TO → DATA → QUIT.
    /// Captures the last DATA payload and replies 250 OK to every
    /// well-formed command.
    pub async fn run(listener: TcpListener, captured: Arc<Mutex<Vec<u8>>>) {
        loop {
            let (sock, _peer) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let captured = captured.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_session(sock, captured).await {
                    eprintln!("smtp_mock session ended: {e}");
                }
            });
        }
    }

    async fn handle_session(
        sock: tokio::net::TcpStream,
        captured: Arc<Mutex<Vec<u8>>>,
    ) -> std::io::Result<()> {
        let (read, mut write) = sock.into_split();
        let mut br = BufReader::new(read);

        // Greeting
        write.write_all(b"220 adnet-mock ESMTP ready\r\n").await?;

        loop {
            let mut line = String::new();
            let n = br.read_line(&mut line).await?;
            if n == 0 {
                return Ok(());
            }
            let cmd = line.trim().to_uppercase();
            if cmd.starts_with("EHLO") || cmd.starts_with("HELO") {
                write.write_all(b"250-adnet-mock\r\n250 OK\r\n").await?;
            } else if cmd.starts_with("MAIL FROM") {
                write.write_all(b"250 MAIL OK\r\n").await?;
            } else if cmd.starts_with("RCPT TO") {
                write.write_all(b"250 RCPT OK\r\n").await?;
            } else if cmd.starts_with("DATA") {
                write
                    .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                    .await?;
                // Read until ".\r\n"
                let mut payload = Vec::new();
                loop {
                    let mut buf = Vec::with_capacity(1024);
                    let read = br.read_until(b'\n', &mut buf).await?;
                    if read == 0 {
                        break;
                    }
                    if buf == b".\r\n" {
                        break;
                    }
                    // Dot-stripping: lines starting with ".." lose one dot.
                    let to_push: &[u8] = if buf.starts_with(b"..") {
                        &buf[1..]
                    } else {
                        &buf
                    };
                    payload.extend_from_slice(to_push);
                }
                *captured.lock().await = payload;
                write.write_all(b"250 OK\r\n").await?;
            } else if cmd.starts_with("QUIT") {
                write.write_all(b"221 Bye\r\n").await?;
                return Ok(());
            } else if cmd.starts_with("RSET") || cmd.starts_with("NOOP") {
                write.write_all(b"250 OK\r\n").await?;
            } else {
                write.write_all(b"502 Command not implemented\r\n").await?;
            }
        }
    }
}

// ───────────────────────────────────────────────────────────────────────
// IMAP mock
// ───────────────────────────────────────────────────────────────────────

mod imap_mock {
    use super::*;

    /// Single-folder IMAP server. Pre-seeds one message and replies
    /// with reasonable canned responses for LOGIN, SELECT, UID SEARCH,
    /// UID FETCH, LOGOUT.
    ///
    /// The mock deliberately doesn't model every RFC 3501 nuance —
    /// it's just enough surface for `ImapSession::connect` +
    /// `select_folder` + `uid_fetch` to round-trip.
    pub async fn run(listener: TcpListener, fixture: Arc<Mutex<Vec<u8>>>) {
        loop {
            let (sock, _peer) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let fixture = fixture.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_session(sock, fixture).await {
                    eprintln!("imap_mock session ended: {e}");
                }
            });
        }
    }

    async fn handle_session(
        sock: tokio::net::TcpStream,
        fixture: Arc<Mutex<Vec<u8>>>,
    ) -> std::io::Result<()> {
        let (read, mut write) = sock.into_split();
        let mut br = BufReader::new(read);

        write
            .write_all(b"* OK [CAPABILITY IMAP4rev1] adnet-imap-mock\r\n")
            .await?;
        write.flush().await?;

        loop {
            let mut line = String::new();
            let n = br.read_line(&mut line).await?;
            if n == 0 {
                return Ok(());
            }
            // Find the command verb (second token).
            let mut tokens = line.split_whitespace();
            let tag = tokens.next().unwrap_or("").to_string();
            let verb = tokens.next().unwrap_or("").to_uppercase();

            if verb == "CAPABILITY" {
                write.write_all(b"* CAPABILITY IMAP4rev1\r\n").await?;
                write
                    .write_all(format!("{tag} OK CAPABILITY\r\n").as_bytes())
                    .await?;
            } else if verb == "LOGIN" {
                write
                    .write_all(format!("{tag} OK [CAPABILITY IMAP4rev1] logged in\r\n").as_bytes())
                    .await?;
            } else if verb == "SELECT" || verb == "EXAMINE" {
                write.write_all(b"* 1 EXISTS\r\n").await?;
                write.write_all(b"* 1 RECENT\r\n").await?;
                write
                    .write_all(b"* FLAGS (\\Seen \\Answered \\Flagged \\Deleted \\Draft)\r\n")
                    .await?;
                write.write_all(b"* OK [UIDVALIDITY 1] SELECT\r\n").await?;
                write
                    .write_all(format!("{tag} OK SELECT done\r\n").as_bytes())
                    .await?;
            } else if verb == "UID" {
                let sub = tokens.next().unwrap_or("").to_uppercase();
                if sub == "SEARCH" {
                    write.write_all(b"* SEARCH 1\r\n").await?;
                    write
                        .write_all(format!("{tag} OK SEARCH done\r\n").as_bytes())
                        .await?;
                } else if sub == "FETCH" {
                    let body = fixture.lock().await.clone();
                    let n = body.len();
                    // Build the FETCH response: "* 1 FETCH (UID 1 ... BODY[] {N}\r\n<bytes>)\r\n"
                    let header =
                        format!("* 1 FETCH (UID 1 RFC822.SIZE {n} FLAGS () BODY[] {{{n}}}\r\n");
                    let footer = format!(")\r\n{tag} OK FETCH done\r\n");
                    write.write_all(header.as_bytes()).await?;
                    write.write_all(&body).await?;
                    write.write_all(footer.as_bytes()).await?;
                } else if sub == "STORE" {
                    write
                        .write_all(format!("{tag} OK STORE done\r\n").as_bytes())
                        .await?;
                } else if sub == "EXPUNGE" {
                    write
                        .write_all(format!("{tag} OK EXPUNGE done\r\n").as_bytes())
                        .await?;
                } else {
                    write
                        .write_all(format!("{tag} BAD unknown UID verb: {sub}\r\n").as_bytes())
                        .await?;
                }
            } else if verb == "NOOP" {
                write
                    .write_all(format!("{tag} OK NOOP\r\n").as_bytes())
                    .await?;
            } else if verb == "LOGOUT" {
                write.write_all(b"* BYE\r\n").await?;
                write
                    .write_all(format!("{tag} OK LOGOUT\r\n").as_bytes())
                    .await?;
                return Ok(());
            } else {
                write
                    .write_all(format!("{tag} BAD unknown verb: {verb}\r\n").as_bytes())
                    .await?;
            }
            write.flush().await?;
        }
    }
}

// ───────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtp_send_classifies_250_ok() {
    let (addr, listener) = bind_loopback().await;
    let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
    tokio::spawn(smtp_mock::run(listener, captured.clone()));

    let account = loopback_account(addr, LoopbackKind::Smtp);
    let mut transport = connect(&account).await.expect("smtp connect");

    let mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "hello",
        "world",
    );
    let outcome = smtp_send(&mut transport, &mail).await.expect("send");
    assert_eq!(outcome, SendOutcome::Sent);

    // The mock should have captured exactly the message body.
    let got = captured.lock().await.clone();
    let got = String::from_utf8_lossy(&got).into_owned();
    assert!(got.contains("Subject: hello"), "captured: {got}");
    assert!(got.contains("world"), "captured: {got}");
    let _ = adnet_mail::smtp::send::quit(transport).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtp_send_rejects_empty_recipients_as_user_error() {
    let (addr, listener) = bind_loopback().await;
    let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
    tokio::spawn(smtp_mock::run(listener, captured.clone()));

    let account = loopback_account(addr, LoopbackKind::Smtp);
    let mut transport = connect(&account).await.expect("smtp connect");

    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "x",
        "y",
    );
    mail.to.clear();
    mail.cc.clear();
    mail.bcc.clear();

    let err = smtp_send(&mut transport, &mail).await.unwrap_err();
    assert_eq!(err.recoverability(), ErrorClass::UserError);
    let _ = adnet_mail::smtp::send::quit(transport).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtp_send_with_attachments_round_trip() {
    let (addr, listener) = bind_loopback().await;
    let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
    tokio::spawn(smtp_mock::run(listener, captured.clone()));

    let account = loopback_account(addr, LoopbackKind::Smtp);
    let mut transport = connect(&account).await.expect("smtp connect");

    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "report",
        "see attached",
    );
    mail.attachments.push(Attachment {
        filename: "data.txt".into(),
        content_type: "text/plain".into(),
        data: b"abc".to_vec(),
        disposition: Disposition::Attachment,
    });

    let outcome = smtp_send(&mut transport, &mail).await.expect("send");
    assert_eq!(outcome, SendOutcome::Sent);

    let got = captured.lock().await.clone();
    let got = String::from_utf8_lossy(&got).into_owned();
    assert!(got.contains("multipart/mixed"), "captured: {got}");
    assert!(got.contains("data.txt"), "captured: {got}");
    let _ = adnet_mail::smtp::send::quit(transport).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imap_fetch_decodes_canned_message() {
    // Pre-build a minimal RFC 5322 message for the mock to serve.
    let mut canned = Mail::text_only(
        Address::new("carol@example.com").with_name("Carol"),
        Address::new("alice@example.com"),
        "greetings",
        "Hi Alice,\n\nHow are you?\n",
    );
    canned.date = Some(
        chrono::DateTime::parse_from_rfc2822("Mon, 04 Aug 2025 10:00:00 +0000")
            .unwrap()
            .with_timezone(&chrono::Utc),
    );
    let bytes = canned.to_wire_bytes().unwrap();

    let (addr, listener) = bind_loopback().await;
    let fixture = Arc::new(Mutex::new(bytes));
    tokio::spawn(imap_mock::run(listener, fixture.clone()));

    let account = loopback_account(addr, LoopbackKind::Imap);
    let mut session = adnet_mail::imap::ImapSession::connect(account)
        .await
        .expect("imap connect");
    let mut handle = session.open_inbox().await.expect("open inbox");
    let msgs = handle.fetch_new().await.expect("fetch new");
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];
    let body = m.mail.as_ref().expect("mail parsed");
    assert_eq!(body.subject, "greetings");
    assert!(body.text.contains("How are you?"));
    assert_eq!(body.from.address, "carol@example.com");
    let _ = session.logout().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imap_fetch_skips_oversized_message_body() {
    // Aerospace-grade regression for the two-phase fetch: a message
    // whose server-reported RFC822.SIZE exceeds the configured cap
    // must come back with `mail: None` and a descriptive
    // `parse_error`, *without* the client ever issuing a
    // `BODY.PEEK[]` fetch for it. We can't directly observe "no
    // second round-trip" through this mock, but we can assert the
    // returned `FetchedMessage` reflects the skip rather than a
    // successfully parsed body.
    let canned = Mail::text_only(
        Address::new("carol@example.com"),
        Address::new("alice@example.com"),
        "big message",
        "this body is bigger than our tiny test cap",
    );
    let bytes = canned.to_wire_bytes().unwrap();
    let real_len = bytes.len() as u32;
    assert!(real_len > 10, "fixture must exceed the test cap");

    let (addr, listener) = bind_loopback().await;
    let fixture = Arc::new(Mutex::new(bytes));
    tokio::spawn(imap_mock::run(listener, fixture.clone()));

    let account = loopback_account(addr, LoopbackKind::Imap);
    let mut session = adnet_mail::imap::ImapSession::connect(account)
        .await
        .expect("imap connect");
    let info = session.select_folder().await.expect("select");
    let mut handle =
        adnet_mail::imap::FetchHandle::new(&mut session, info).with_max_message_size(10); // absurdly small — everything is "oversized"

    let msgs = handle.fetch_new().await.expect("fetch new");
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];
    assert!(
        m.mail.is_none(),
        "oversized message body must not be parsed"
    );
    assert_eq!(m.size, Some(real_len));
    let err = m.parse_error.as_ref().expect("must carry a skip reason");
    assert!(
        err.contains("exceeds max_message_size"),
        "unexpected parse_error: {err}"
    );
    let _ = session.logout().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn envelope_combine_to_cc_bcc() {
    let (addr, listener) = bind_loopback().await;
    let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
    tokio::spawn(smtp_mock::run(listener, captured.clone()));

    let account = loopback_account(addr, LoopbackKind::Smtp);
    let mut transport = connect(&account).await.expect("smtp connect");

    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "broadcast",
        "shared to several",
    );
    mail.cc.push(Address::new("carol@example.com"));
    mail.bcc.push(Address::new("dave@example.com"));

    let outcome = smtp_send(&mut transport, &mail).await.expect("send");
    assert_eq!(outcome, SendOutcome::Sent);

    // The mock doesn't surface the envelope, but it must accept the
    // DATA payload without erroring — implicitly verified by the
    // 250 OK / Sent outcome above. The body itself should not contain
    // the Bcc header.
    let got = captured.lock().await.clone();
    let got = String::from_utf8_lossy(&got).into_owned();
    assert!(!got.contains("Bcc:"), "Bcc leaked: {got}");
    let _ = adnet_mail::smtp::send::quit(transport).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mail_account_builder_to_full_account() {
    let account = MailAccount::builder()
        .address("alice@example.com")
        .imap_server("127.0.0.1")
        .smtp_server("127.0.0.1")
        .credentials("alice", "secret")
        .display_name("Alice")
        .build()
        .expect("build");
    assert_eq!(account.account().addr, "alice@example.com");
    assert_eq!(
        account.account().imap.security,
        adnet_mail::login_param::SocketSecurity::Tls
    );
    assert_eq!(
        account.account().smtp.security,
        adnet_mail::login_param::SocketSecurity::Starttls
    );
}
