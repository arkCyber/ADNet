//! Property-based tests for `a3chat-cli` output formatting.

use a3chat_cli::output::{AnyFormatter, Formatter, JsonFormatter, PlainFormatter, TableFormatter};

/// Property: `PlainFormatter` is order-stable — for any object
/// with N keys, the output positions of `key=` are the same across
/// runs (sorted alphabetically).
#[test]
fn plain_is_key_order_stable() {
    let mut v = serde_json::Map::new();
    for c in b'a'..=b'z' {
        v.insert(
            (c as char).to_string(),
            serde_json::Value::String(c.to_string()),
        );
    }
    let input = serde_json::Value::Object(v);
    let p1 = PlainFormatter.format(&input).unwrap();
    let p2 = PlainFormatter.format(&input).unwrap();
    assert_eq!(p1, p2);
    // Verify alphabetical order.
    let mut prev = None;
    for line in p1.lines() {
        if let Some(eq) = line.find('=') {
            let key = &line[..eq];
            if let Some(p) = prev {
                assert!(p < key.to_string(), "key order violated at {key}");
            }
            prev = Some(key.to_string());
        }
    }
}

/// Property: `TableFormatter` always produces either `(empty)` or
/// at least 2 lines for object-shaped inputs (header + separator).
/// Scalar inputs are allowed to render in 1 line.
#[test]
fn table_always_has_header_or_marker() {
    let objects: Vec<serde_json::Value> = vec![
        serde_json::json!([]),
        serde_json::json!([{"a": 1}]),
        serde_json::json!({"a": 1, "b": 2}),
    ];
    for c in objects {
        let s = TableFormatter.format(&c).unwrap();
        assert!(!s.is_empty(), "empty output for {c:?}");
        if s.trim() != "(empty)" {
            assert!(s.lines().count() >= 2, "table too short: {s}");
        }
    }
    let scalars: Vec<serde_json::Value> = vec![
        serde_json::json!("string"),
        serde_json::json!(42),
        serde_json::json!(true),
    ];
    for c in scalars {
        let s = TableFormatter.format(&c).unwrap();
        // Scalars may render in a single line — the only contract is
        // that the output is non-empty and does not crash.
        assert!(!s.is_empty(), "scalar {c:?} produced empty output");
    }
}

/// Property: `JsonFormatter` round-trips — for any `T: Serialize`,
/// the produced string is parseable back to an equivalent value.
#[test]
fn json_roundtrips_for_various_shapes() {
    let cases: Vec<serde_json::Value> = vec![
        serde_json::json!(null),
        serde_json::json!(true),
        serde_json::json!(1),
        serde_json::json!("hi"),
        serde_json::json!([]),
        serde_json::json!({}),
        serde_json::json!({"nested": {"k": [1, 2]}}),
    ];
    for c in cases {
        let s = JsonFormatter.format(&c).unwrap();
        let back: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c);
    }
}

/// Property: `AnyFormatter::format` matches the underlying
/// formatter for every variant.
#[test]
fn any_formatter_matches_underlying() {
    let v = serde_json::json!({"a": [1, 2, 3], "b": "hi"});
    assert_eq!(
        AnyFormatter::from(a3chat_cli::config::OutputFormat::Json)
            .format(&v)
            .unwrap(),
        JsonFormatter.format(&v).unwrap()
    );
    assert_eq!(
        AnyFormatter::from(a3chat_cli::config::OutputFormat::Plain)
            .format(&v)
            .unwrap(),
        PlainFormatter.format(&v).unwrap()
    );
    assert_eq!(
        AnyFormatter::from(a3chat_cli::config::OutputFormat::Table)
            .format(&v)
            .unwrap(),
        TableFormatter.format(&v).unwrap()
    );
}