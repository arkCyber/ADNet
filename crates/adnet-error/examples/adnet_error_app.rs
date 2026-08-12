//! Realistic example: an internal error enum implements `IntoReport`
//! and we lift it through the boundary in the same way the ADNet RPC
//! layer does. Demonstrates the cause chain, code convention, and the
//! helper that maps `ErrorKind` to HTTP status codes.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-error --example adnet_error_app
//! ```

use std::error::Error;
use std::fmt;

use adnet_error::{ErrorKind, IntoReport, Severity};

/// A stand-in for a real RPC handler's error.
#[derive(Debug)]
enum ServiceError {
    BadTicket(String),
    /// Wrapped from a `std::io::Error` somewhere down the stack.
    Io { context: String, source: std::io::Error },
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadTicket(t) => write!(f, "bad ticket: {t}"),
            Self::Io { context, source } => write!(f, "io error in {context}: {source}"),
        }
    }
}

impl Error for ServiceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl IntoReport for ServiceError {
    fn code(&self) -> &'static str {
        match self {
            Self::BadTicket(_) => "DEMO-001",
            Self::Io { .. } => "DEMO-002",
        }
    }
    fn kind(&self) -> ErrorKind {
        match self {
            Self::BadTicket(_) => ErrorKind::BadRequest,
            Self::Io { .. } => ErrorKind::Internal,
        }
    }
    fn severity(&self) -> Severity {
        match self {
            Self::BadTicket(_) => Severity::Warn,
            Self::Io { .. } => Severity::Error,
        }
    }
}

fn main() {
    // 1. Simulate a wrapped io error chain.
    let inner = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short read");
    let outer = ServiceError::Io {
        context: "blob_store.get".into(),
        source: inner,
    };

    // 2. Lift into a report — the cause chain is walked automatically.
    let report = outer.into_report("adnet-demo");
    println!("code     : {}", report.code);
    println!("kind     : {:?}", report.kind);
    println!("severity : {:?}", report.severity);
    println!("http     : {}", report.kind.http_status());
    println!("message  : {}", report.message);
    println!(
        "cause    : {}",
        report.cause.as_deref().unwrap_or("(none)")
    );

    // 3. The 'transient' hint is how the RPC client decides whether to
    // retry without parsing the message string.
    println!("transient? : {}", report.kind.is_transient());

    // 4. A cleanly-downgraded client error.
    let client_err = ServiceError::BadTicket("not a valid adnet-blob://ticket".into());
    let client_report = client_err.into_report("adnet-demo");
    println!(
        "client -> http {}, transient={}",
        client_report.kind.http_status(),
        client_report.kind.is_transient(),
    );
    client_report.emit();
}
