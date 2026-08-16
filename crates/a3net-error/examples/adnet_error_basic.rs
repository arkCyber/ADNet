//! Tiny example: build an `AdnetErrorReport`, decorate it with details and a
//! cause chain, and emit it through `tracing`.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-error --example a3net_error_basic
//! ```

use a3net_error::{AdnetErrorReport, ErrorKind, Severity};

fn main() {
    // 1. Build a 'NotFound' report and decorate it.
    let report = AdnetErrorReport::new(
        "BLB-001",
        ErrorKind::NotFound,
        Severity::Warn,
        "blob not found",
        "a3net-blobstore",
    )
    .with_correlation("op-42")
    .with_detail("hash", "ab12cd34…")
    .with_detail("size_bytes", 4096_u64);

    // 2. JSON round-trip. The `details` map is a `BTreeMap` so the order
    // is alphabetical on the wire — easier to grep and diff.
    let json = serde_json::to_string_pretty(&report).expect("serialize");
    println!("{json}");

    let back: AdnetErrorReport = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.code, "BLB-001");
    assert_eq!(back.kind, ErrorKind::NotFound);
    assert_eq!(back.severity, Severity::Warn);
    assert_eq!(back.correlation.as_deref(), Some("op-42"));

    // 3. Emit through tracing. The `tracing` macros pin the structured
    // fields (code, kind, crate, cause) so a JSON log shipper can group
    // by them without parsing the message.
    report.emit();
    println!("emitted: ok");
}
