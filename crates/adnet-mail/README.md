# `adnet-mail` — SMTP send + IMAP receive for the ADNet workspace

A focused, self-contained crate for sending and receiving email over
standard SMTP / IMAP, with RFC 5322 MIME construction and parsing.

This crate is a **clean-room simplification of `chatmail@core`** (Delta
Chat 2.58-dev). It copies *only* the email-transport subset — chat
state, OpenPGP / Autocrypt / SecureJoin, webxdc, peer channels, the
`provider-db` machinery, etc. are deliberately **not** included.

## Why

`chatmail@core` is a ~14 000-line crate. ADNet uses only the SMTP /
IMAP / MIME parts, and wants a build without Delta Chat's SQLite /
runtime dependencies. `adnet-mail` is that cut-down version.

| Kept from chatmail@core | Dropped from chatmail@core |
|-------------------------|----------------------------|
| `smtp.rs` / `smtp/connect.rs` | `pgp.rs`, `securejoin.rs`, `aheader.rs` |
| `imap.rs` / `imap/{client,idle,fetch}.rs` | `provider.rs` (we ship a tiny built-in list) |
| `mimefactory.rs` / `mimeparser.rs` | `chat.rs`, `contact.rs`, `context.rs` |
| `login_param.rs` | `peer_channels.rs`, `webxdc/`, `ephemeral/`, `receive_imf.rs` |
| `transport.rs` (Socket enum) | `scheduler.rs` |

If you need end-to-end encryption, depend on `chatmail@core` directly
and feed our `Mail::from_wire_bytes` / `Mail::to_wire_bytes` its
already-decrypted plaintext MIME.

## Crate layout

| Module | Responsibility |
|--------|---------------|
| [`error`] | Typed errors with DO-178C-style recoverability (`UserError` / `Recoverable` / `Fatal`). |
| [`login_param`] | `Account` (server config) + helpers + `is_valid_address`. |
| [`mime`] | `Mail` struct: parse / emit RFC 5322 wire bytes. |
| [`imap`] | IMAP connect / IDLE / fetch. |
| [`smtp`] | SMTP connect / send (Tls / Starttls / Plain). |
| [`provider`] | Built-in auto-config for Gmail / Outlook / Yahoo / Fastmail / mailbox.org / Posteo. |
| [`retry`] | Exponential-backoff retry policy for `Mail::send_message`. |
| [`account`] | `MailAccount` high-level facade (both transports). |

## Quick start

### Send

```rust
use adnet_mail::prelude::*;

# async fn doc() -> Result<()> {
let acct = MailAccount::builder()
    .address("alice@example.com")
    .imap_server("imap.example.com")
    .smtp_server("smtp.example.com")
    .credentials("alice", "hunter2")
    .build()?;

let mut online = acct.connect().await?;
let mail = Mail::text_only(
    Address::new("alice@example.com").with_name("Alice"),
    Address::new("bob@example.com"),
    "Quarterly report",
    "Hi Bob, the Q3 numbers are in.",
);
let outcome = online.send_message(&mail).await?;
assert!(outcome.is_sent());
online.shutdown().await?;
# Ok(()) }
```

### Receive

```rust
# use adnet_mail::prelude::*;
# async fn doc() -> Result<()> {
# let mut online: MailAccountOnline = unimplemented!();
online.open_inbox().await?;
let msgs = online.fetch_inbox().await?;
for m in msgs {
    if let Some(mail) = m.mail {
        println!("{:?} — {:?}", mail.subject, mail.from);
    }
}
# Ok(()) }
```

### Auto-configure

```rust
use adnet_mail::prelude::*;

let (imap, smtp) = auto_configure("alice@gmail.com")?;
assert_eq!(imap.server, "imap.gmail.com");
assert_eq!(smtp.server, "smtp.gmail.com");
```

### Retry on transient failure

```rust
# use adnet_mail::prelude::*;
# async fn doc() -> Result<()> {
# let mut transport: adnet_mail::smtp::Transport = unimplemented!();
# let mail: Mail = unimplemented!();
let outcome = send_with_retry(&mut transport, &mail, &RetryPolicy::default()).await?;
# let _ = outcome;
# Ok(()) }
```

## Examples

| File | What it shows |
|------|---------------|
| `examples/send_and_receive.rs` | Compose a message with text + HTML + inline image + attachment; encode to RFC 5322 bytes; decode back; JSON round-trip. |
| `examples/auto_configure.rs` | Look up IMAP/SMTP servers for a given email address. |

Run with `cargo run -p adnet-mail --example <name>`.

## Tests

```
cargo test -p adnet-mail
```

* 30 unit tests covering MIME round-trips, error classification,
  login-param validation, retry policy, provider lookup, etc.
* 6 integration tests driving a tiny in-process SMTP / IMAP mock
  server (no real network access required).
* 2 doc-tests covering the `MailAccount::builder` and
  `send_with_retry` quick-start snippets.

## Compatibility matrix

| `async-smtp` | `async-imap` | `mailparse` | tested |
|--------------|--------------|-------------|--------|
| 0.10         | 0.11         | 0.15        | yes    |

## Safety

`#![warn(unused_must_use)]`. The crate uses `unsafe` only in the
`pin-project`-generated IMAP stream enum (one `Pin::new_unchecked`
per `poll_*` method, safe because both enum variants are always
`Unpin`-aware).
