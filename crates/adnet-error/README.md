# ADNet Unified Error Model

> Strategy: P1-2 of the diff-network-platform roadmap.

## Why this crate exists

Every ADNet crate defines its own `thiserror::Error` enum.
That's fine for **internal** ergonomics, but at the
**boundary** (RPC, FFI, CLI exit, HTTP gateway) we want a
stable, machine-readable shape that operators can group,
count, and trace. This is the role of `adnet-error`.

## The shape

```rust
pub struct AdnetErrorReport {
    pub code: String,                  // "BLB-001", "DNS-014"
    pub kind: ErrorKind,               // NotFound / Timeout / Internal / ...
    pub severity: Severity,            // Info / Warn / Error / Fatal
    pub message: String,               // human-readable
    pub source_loc: Option<String>,    // file:line, with `source-loc` feature
    pub correlation: Option<String>,   // request-id / op-id
    pub cause: Option<String>,         // chain of inner messages
    pub details: BTreeMap<String, Value>,
}
```

The shape mirrors **Hessian 2** error envelopes (Dubbo /
Motan). `code` is the stable operator-readable identifier;
`kind` is the coarse bucket for retry-policy; `message`
is the human text; `details` is the free-form protocol
context.

## Code conventions

| Crate prefix | Crate |
|--------------|-------|
| `TYP-`       | `adnet-types` |
| `BLS-`       | `adnet-blobstore` (planned) |
| `NS-`        | `adnet-namespace` (planned) |
| `RPC-`       | `adnet-rpc` (planned) |
| `DNS-`       | `adnet-dns-server` (planned) |

Each variant gets a fresh 3-digit suffix. Don't renumber —
operators depend on code stability.

## Adopting it in a new crate

```rust
use adnet_error::{IntoReport, ErrorKind, Severity};

#[derive(Debug, thiserror::Error)]
pub enum MyError {
    #[error("bad ticket: {0}")]
    BadTicket(String),
}

impl IntoReport for MyError {
    fn code(&self) -> &'static str {
        match self {
            Self::BadTicket(_) => "MYC-001",
        }
    }
    fn kind(&self) -> ErrorKind {
        ErrorKind::BadRequest
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
}

// At the boundary:
let report = my_err.into_report("my-crate");
report.emit();   // tracing + metrics counter
```

## Observability

`AdnetErrorReport::emit()` does two things:

1. **Tracing** — emits at the severity the report carries.
   The structured fields (`code`, `kind`, `crate`) are
   attached so JSON log shippers can group by them.
2. **Counter** — bumps `adnet_error_total` with labels
   `(code, kind, crate, severity)`. Enabled by the
   `metrics` feature in `adnet-error` so the default
   dependency graph stays acyclic.

## Features

| Feature | Default | Purpose |
|---------|---------|---------|
| `default` | on | tracing emit only, no metrics |
| `source-loc` | off | capture file:line of the call site |
| `metrics` | off | wire the `adnet_error_total` counter |

## Migration

Existing crates can adopt `adnet-error` incrementally; the
plan is to start at the boundary (RPC, FFI, CLI) and work
inward. The crate does not pull in `adnet-observability`
by default, so adopting it in a leaf crate (`adnet-types`)
adds no transitive dependency.

## Tests

`cargo test -p adnet-error` runs 13 hermetic tests covering
the public API. With `--features metrics` the suite grows
to 16 tests: the 3 extra tests exercise the
counter-registration path and confirm that repeated
`emit()` calls are idempotent (they hit the `OnceLock`
that wraps the global registry).

| Test                                   | Default | `metrics` |
|----------------------------------------|---------|-----------|
| `kind_codes_are_stable`                |   ✓     |     ✓     |
| `transient_kinds`                      |   ✓     |     ✓     |
| `http_status_mapping`                  |   ✓     |     ✓     |
| `severity_str`                         |   ✓     |     ✓     |
| `report_round_trips_via_json`          |   ✓     |     ✓     |
| `details_deterministic_in_json`        |   ✓     |     ✓     |
| `detail_serialization_accepts_arbitrary_types` | ✓  |     ✓     |
| `cause_chain_collected`                |   ✓     |     ✓     |
| `cause_chain_depth_bounded`            |   ✓     |     ✓     |
| `emit_does_not_panic`                  |   ✓     |     ✓     |
| `source_loc_populated_by_default`      |   ✓     |     ✓     |
| `into_report_default_uses_display_and_walks_cause` | ✓ |     ✓     |
| `into_report_carries_cause_chain`      |   ✓     |     ✓     |
| `emit_bumps_counter_visible_via_read_counter` |   |     ✓     |
| `read_counter_returns_zero_for_unknown_labels` |   |     ✓     |
| `emit_is_idempotent_under_repeated_calls` |    |     ✓     |
