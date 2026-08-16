//! # auto-configure example
//!
//! Demonstrates the built-in provider database: given a bare email
//! address, look up the IMAP/SMTP server config.
//!
//! ```bash
//! cargo run -p a3net-mail --example auto_configure alice@gmail.com
//! ```

use a3net_mail::prelude::*;

fn main() {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "alice@gmail.com".into());

    let provider = match Provider::for_address(&addr) {
        Some(p) => p,
        None => {
            eprintln!("no provider known for {addr}");
            std::process::exit(1);
        }
    };

    let (imap, smtp) = auto_configure(&addr).expect("auto_configure");

    println!("Address:  {addr}");
    println!("Provider: {} ({})", provider.display_name, provider.id);
    println!("OAuth2:   {}", if provider.oauth2 { "yes" } else { "no" });
    println!("IMAP:     {}:{}", imap.server, imap.port);
    println!("SMTP:     {}:{}", smtp.server, smtp.port);

    // We can hand the params straight to MailAccountBuilder and connect.
    println!("\nA `MailAccount` is now constructible:");
    println!("    let acct = MailAccount::builder()");
    println!("        .address(\"{addr}\")");
    println!("        .imap_server(\"{}\")", imap.server);
    println!("        .smtp_server(\"{}\")", smtp.server);
    println!("        .credentials(\"alice\", \"<password or oauth-token>\")");
    println!("        .build()?;");
}
