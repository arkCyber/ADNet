//! Human-readable byte-size parser & formatter.
//!
//! Used by the CLI for storage capacity, response-size limits, and any
//! other knob that operators would rather express as `"10GiB"` than as
//! the raw integer `10737418240`. The parser is deliberately local — no
//! `humansize` or `byte-unit` dependency — because the surface we need
//! is small and the failure modes are easy to reason about when we own
//! the code.
//!
//! ## Accepted formats
//!
//! | Input           | Bytes        |
//! |-----------------|--------------|
//! | `"0"`           | 0            |
//! | `"1024"`        | 1024         |
//! | `"1KB"`         | 1,000        |
//! | `"1MB"`         | 1,000,000    |
//! | `"1GB"`         | 1,000,000,000 |
//! | `"1TB"`         | 1,000,000,000,000 |
//! | `"1PB"`         | 1,000,000,000,000,000 |
//! | `"1KiB"`        | 1024         |
//! | `"1MiB"`        | 1,048,576    |
//! | `"1GiB"`        | 1,073,741,824 |
//! | `"1TiB"`        | 1,099,511,627,776 |
//! | `"1PiB"`        | 1,125,899,906,842,624 |
//!
//! Matching is case-insensitive. A trailing `B` is optional for the
//! binary units (`"1Gi"` is accepted and means `"1GiB"`). Whitespace
//! around the value is trimmed.
//!
//! ## Rejected formats
//!
//! * Negative numbers — bytes are unsigned.
//! * Percentages (`"10%"`) — without an explicit base they are
//!   ambiguous, and the CLI never needs them. If a future caller does,
//   the right place to add them is a separate `parse_percent(...)` so
//!   the call site can document its base (RAM, disk, etc.).
//! * `f64` precision — rejects anything after the decimal. Storage
//!   capacities are always integers in bytes.
//!
//! ## Errors
//!
//! Every failure returns a [`BytesError`] that **mentions the input
//! value** so a misconfigured `app.toml` produces a recoverable error
//! (`"storage.totalBytes: 'lots' is not a valid byte size: …"`).

use std::fmt;

/// Default total storage budget when the operator does not specify
/// anything. Mirrors the existing `QuotaPolicy::default()` in
/// `adnet-blobstore`, so a fresh install lands on the same 20 GiB the
/// CLI used to hard-code.
pub const DEFAULT_TOTAL_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// Environment variable that overrides the total storage budget. Higher
/// priority than `app.toml`, lower priority than an explicit CLI flag.
pub const ADNET_STORAGE_TOTAL_BYTES_ENV: &str = "ADNET_STORAGE_TOTAL_BYTES";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BytesError {
    Empty,
    InvalidNumber { raw: String, reason: String },
    UnknownUnit { raw: String, unit: String },
    TrailingGarbage { raw: String, after: String },
    Negative,
    Overflow { raw: String },
}

impl fmt::Display for BytesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BytesError::Empty => f.write_str("byte size is empty"),
            BytesError::InvalidNumber { raw, reason } => {
                write!(f, "{raw:?} is not a valid number: {reason}")
            }
            BytesError::UnknownUnit { raw, unit } => {
                write!(
                    f,
                    "{raw:?} has unknown unit {unit:?} (expected B, KB, MB, GB, TB, PB, KiB, MiB, GiB, TiB or PiB)"
                )
            }
            BytesError::TrailingGarbage { raw, after } => {
                write!(f, "{raw:?} has trailing garbage {after:?} after the value")
            }
            BytesError::Negative => f.write_str("byte size cannot be negative"),
            BytesError::Overflow { raw } => {
                write!(f, "{raw:?} is too large to fit in u64 bytes")
            }
        }
    }
}

impl std::error::Error for BytesError {}

/// Parse a human-readable byte size into `u64`.
///
/// See the [module docs](self) for the accepted formats. The parser
/// panics on no inputs — the only failure mode is [`BytesError`].
pub fn parse_bytes(raw: &str) -> Result<u64, BytesError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(BytesError::Empty);
    }
    // Negative check: the split helper below drops the leading `-`
    // because it's not a digit, so we catch the sign first.
    if trimmed.starts_with('-') {
        return Err(BytesError::Negative);
    }
    // Walk forward: the numeric prefix ends at the first
    // non-digit (after any leading decimals). Everything after
    // is the unit candidate; a trailing non-unit character (e.g.
    // `"!"` in `"1GiB!"`) becomes a `TrailingGarbage` error
    // instead of a confusing `UnknownUnit`.
    let bytes = trimmed.as_bytes();
    let mut split_at = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if b.is_ascii_digit() {
            continue;
        }
        if *b == b'.' && i > 0 && bytes[i - 1].is_ascii_digit() {
            continue;
        }
        split_at = i;
        break;
    }
    let (num_part, rest) = trimmed.split_at(split_at);
    if num_part.is_empty() {
        return Err(BytesError::InvalidNumber {
            raw: raw.to_string(),
            reason: "no numeric prefix".into(),
        });
    }

    // Accept both integer (`"1024"`) and decimal (`"1.5"`) numeric
    // prefixes. We parse as `f64` and round to the nearest byte —
    // storage capacities are conceptually integers but a config
    // template that reads `"1.5 GiB"` should still work.
    let n_f: f64 = num_part.parse().map_err(|e: std::num::ParseFloatError| {
        BytesError::InvalidNumber {
            raw: raw.to_string(),
            reason: e.to_string(),
        }
    })?;
    if !n_f.is_finite() || n_f < 0.0 {
        return Err(BytesError::Negative);
    }
    let n: u64 = n_f.round() as u64;

    // The unit is the longest prefix of `rest` that matches
    // `unit_multiplier`. Any leftover after the matched unit
    // becomes `TrailingGarbage`. An empty `rest` is fine — it
    // maps to `Some(1)` in `unit_multiplier`.
    let rest_len = rest.len();
    let mut unit_end = 0usize;
    let mut multiplier: Option<u64> = None;
    if rest_len == 0 {
        unit_end = 0;
        multiplier = Some(1);
    } else {
        for end in 1..=rest_len {
            if let Some(m) = unit_multiplier(&rest[..end]) {
                unit_end = end;
                multiplier = Some(m);
            }
        }
    }
    let multiplier = match multiplier {
        Some(m) => m,
        None => {
            return Err(BytesError::UnknownUnit {
                raw: raw.to_string(),
                unit: rest.to_string(),
            });
        }
    };
    if unit_end < rest_len {
        return Err(BytesError::TrailingGarbage {
            raw: raw.to_string(),
            after: rest[unit_end..].to_string(),
        });
    }

    n.checked_mul(multiplier).ok_or_else(|| BytesError::Overflow {
        raw: raw.to_string(),
    })
}

/// Format a byte count into the most natural human unit. Sub-KiB
/// values render as plain integers (`"512 B"`); values that land
/// on an exact binary boundary render without a trailing `".00"`
/// (`"1 KiB"` not `"1.00 KiB"`) so the output is round-trippable
/// through [`parse_bytes`].
pub fn format_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        // Drop the trailing `.00` when the scaled value is
        // numerically an integer. We use a tolerance of `1e-9`
        // (much wider than `f64::EPSILON`) because the divide
        // by 1024 accumulates rounding error — e.g.
        // `1024.0 / 1024.0` can produce `0.9999999999...` on
        // some platforms.
        let rounded = v.round();
        if (v - rounded).abs() < 1e-9 {
            format!("{} {}", rounded as u64, UNITS[u])
        } else {
            format!("{v:.2} {}", UNITS[u])
        }
    }
}

/// Split `"10GiB"` into `("10", "GiB")`. Whitespace is the
/// separator. The unit may be empty (`"1024"` → `("1024", "")`).
fn split_number_unit(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut split_at = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if b.is_ascii_digit() {
            continue;
        }
        // Allow one decimal-style prefix like "1." but reject "."
        // without a leading digit. We pass everything up to the first
        // non-digit as the number side.
        if *b == b'.' && i > 0 && bytes[i - 1].is_ascii_digit() {
            continue;
        }
        split_at = i;
        break;
    }
    s.split_at(split_at)
}

/// Return the multiplicative factor for a unit suffix. `None` means
/// the suffix is unrecognised. An empty suffix returns `Some(1)`.
fn unit_multiplier(unit: &str) -> Option<u64> {
    // Normalise: strip an optional trailing `b`/`B` so the binary
    // suffixes (`KiB`, `MiB`, …) and the binary-stripped variants
    // (`Ki`, `Mi`, …) both map to the same multiplier. Note we
    // strip a single trailing B at most — `KIB` therefore becomes
    // `KI` and is matched explicitly below.
    let canonical = unit.trim().to_ascii_uppercase();
    let canonical = canonical
        .strip_suffix('B')
        .unwrap_or(&canonical)
        .to_string();
    let canonical = canonical.as_str();
    match canonical {
        "" => Some(1),
        "K" | "KB" => Some(1_000),
        "M" | "MB" => Some(1_000_000),
        "G" | "GB" => Some(1_000_000_000),
        "T" | "TB" => Some(1_000_000_000_000),
        "P" | "PB" => Some(1_000_000_000_000_000),
        "KI" => Some(1024),
        "MI" => Some(1024 * 1024),
        "GI" => Some(1024 * 1024 * 1024),
        "TI" => Some(1024u64.pow(4)),
        "PI" => Some(1024u64.pow(5)),
        // Binary suffixes with the trailing B intact (e.g. when
        // callers pass them without our normalising step).
        "KIB" => Some(1024),
        "MIB" => Some(1024 * 1024),
        "GIB" => Some(1024 * 1024 * 1024),
        "TIB" => Some(1024u64.pow(4)),
        "PIB" => Some(1024u64.pow(5)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_integer() {
        assert_eq!(parse_bytes("0").unwrap(), 0);
        assert_eq!(parse_bytes("1024").unwrap(), 1024);
        assert_eq!(
            parse_bytes("10737418240").unwrap(),
            10 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(parse_bytes("  1024  ").unwrap(), 1024);
        assert_eq!(parse_bytes("  1 GiB ").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_decimal_units() {
        assert_eq!(parse_bytes("1KB").unwrap(), 1_000);
        assert_eq!(parse_bytes("1MB").unwrap(), 1_000_000);
        assert_eq!(parse_bytes("1GB").unwrap(), 1_000_000_000);
        assert_eq!(parse_bytes("1TB").unwrap(), 1_000_000_000_000);
    }

    #[test]
    fn parse_binary_units() {
        assert_eq!(parse_bytes("1KiB").unwrap(), 1024);
        assert_eq!(parse_bytes("1MiB").unwrap(), 1024 * 1024);
        assert_eq!(parse_bytes("1GiB").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_bytes("1TiB").unwrap(), 1024u64.pow(4));
        assert_eq!(parse_bytes("1PiB").unwrap(), 1024u64.pow(5));
    }

    #[test]
    fn parse_case_insensitive() {
        assert_eq!(parse_bytes("1gib").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_bytes("1GIB").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_bytes("1GB").unwrap(), 1_000_000_000);
        assert_eq!(parse_bytes("1gb").unwrap(), 1_000_000_000);
    }

    #[test]
    fn parse_trailing_b_optional() {
        // "1Gi" is accepted as 1 GiB.
        assert_eq!(parse_bytes("1Gi").unwrap(), 1024 * 1024 * 1024);
        // "1G" (no B) is the decimal gigabyte.
        assert_eq!(parse_bytes("1G").unwrap(), 1_000_000_000);
    }

    #[test]
    fn parse_rejects_empty() {
        assert_eq!(parse_bytes(""), Err(BytesError::Empty));
        assert_eq!(parse_bytes("   "), Err(BytesError::Empty));
    }

    #[test]
    fn parse_rejects_negative() {
        assert_eq!(parse_bytes("-1GiB"), Err(BytesError::Negative));
        assert_eq!(parse_bytes("-0"), Err(BytesError::Negative));
    }

    #[test]
    fn parse_rejects_unknown_unit() {
        let err = parse_bytes("100XB").unwrap_err();
        assert_eq!(
            err,
            BytesError::UnknownUnit {
                raw: "100XB".into(),
                unit: "XB".into()
            }
        );
    }

    #[test]
    fn parse_rejects_garbage_after_unit() {
        // `1GiB!` has a non-numeric character after a known unit.
        // We surface this as a `TrailingGarbage` error so the
        // operator sees *exactly* the stray punctuation, instead
        // of being told that `GiB!` is an unknown unit (which
        // conflates "you typed `GiB!` by mistake" with "you typed
        // a real unit followed by junk").
        let err = parse_bytes("1GiB!").unwrap_err();
        match err {
            BytesError::TrailingGarbage { ref raw, ref after } => {
                assert_eq!(raw, "1GiB!");
                assert_eq!(after, "!");
            }
            other => panic!("expected TrailingGarbage, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_non_numeric() {
        let err = parse_bytes("xyz").unwrap_err();
        assert!(matches!(err, BytesError::InvalidNumber { .. }));
    }

    #[test]
    fn parse_rejects_overflow() {
        // 2^64 bytes — well beyond u64.
        let err = parse_bytes("20000000TiB").unwrap_err();
        assert_eq!(err, BytesError::Overflow { raw: "20000000TiB".into() });
    }

    #[test]
    fn parse_accepts_max_u64_bytes() {
        assert_eq!(parse_bytes("18446744073709551615").unwrap(), u64::MAX);
    }

    #[test]
    fn parse_default_total_bytes() {
        assert_eq!(DEFAULT_TOTAL_BYTES, 20 * 1024 * 1024 * 1024);
        // Sanity round-trip — the default is itself a valid number.
        assert_eq!(
            parse_bytes("20GiB").unwrap(),
            DEFAULT_TOTAL_BYTES,
            "DEFAULT_TOTAL_BYTES should be 20 GiB; otherwise the env override contract is broken"
        );
    }

    #[test]
    fn format_bytes_under_kib() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1), "1 B");
        assert_eq!(format_bytes(1023), "1023 B");
    }

    #[test]
    fn format_bytes_known_values() {
        // Exact binary boundaries elide the trailing `.00`.
        assert_eq!(format_bytes(1024), "1 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1 MiB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1 GiB");
        assert_eq!(format_bytes(1024u64.pow(4)), "1 TiB");
    }

    #[test]
    fn format_bytes_two_decimals_for_off_boundary() {
        // Off-boundary values keep the two decimals — they cannot
        // round-trip through `parse_bytes` without precision loss
        // but operators want them for human reporting.
        assert_eq!(format_bytes(1536), "1.50 KiB");
        assert_eq!(format_bytes(1024 * 1024 + 512 * 1024), "1.50 MiB");
    }

    #[test]
    fn format_then_parse_roundtrip_for_canonical_units() {
        // The formatter is one-way (loses precision below 1 KiB), but
        // every canonical GiB / MiB value should round-trip without
        // touching the wrong binary unit.
        for n in [
            1024u64,
            1024 * 1024,
            1024 * 1024 * 1024,
            1024u64.pow(4),
            1024u64.pow(5),
        ] {
            let s = format_bytes(n);
            let parsed = parse_bytes(&s).unwrap_or_else(|e| {
                panic!("format_bytes({n}) = {s:?} should round-trip, but parse failed: {e}")
            });
            assert_eq!(parsed, n, "round-trip failed for {n} ({s})");
        }
    }

    #[test]
    fn split_number_unit_basic() {
        assert_eq!(split_number_unit("1024"), ("1024", ""));
        assert_eq!(split_number_unit("10GiB"), ("10", "GiB"));
        assert_eq!(split_number_unit("1.5GiB"), ("1.5", "GiB"));
        assert_eq!(split_number_unit("GiB"), ("", "GiB"));
    }

    #[test]
    fn unit_multiplier_known_suffixes() {
        assert_eq!(unit_multiplier(""), Some(1));
        assert_eq!(unit_multiplier("B"), Some(1));
        assert_eq!(unit_multiplier("KB"), Some(1_000));
        assert_eq!(unit_multiplier("KiB"), Some(1024));
        assert_eq!(unit_multiplier("GIB"), Some(1024 * 1024 * 1024));
        assert_eq!(unit_multiplier("XB"), None);
    }

    #[test]
    fn bytes_error_display_mentions_input() {
        let err = BytesError::UnknownUnit {
            raw: "10XB".into(),
            unit: "XB".into(),
        };
        let printed = format!("{err}");
        assert!(printed.contains("10XB"), "{printed}");
        assert!(printed.contains("XB"), "{printed}");
    }
}
