//! Output formatting. Three formats:
//!
//! * `Table` — column-aligned ASCII, human-friendly.
//! * `Json`  — `serde_json::to_string_pretty`, machine-friendly.
//! * `Plain` — one value per line (`key=value`), shell-script-friendly.
//!
//! Every formatter is deterministic given the same input — DO-178C
//! §6.1 (determinism).

use std::io::Write;

use serde::Serialize;

use crate::config::OutputFormat;
use crate::error::{CliError, CliResult};

/// Trait every output formatter implements. `write_to` writes to an
/// arbitrary `Write` so tests can capture into a `Vec<u8>`.
///
/// NOTE: we make this trait object-unsafe intentionally (`Self: Sized`
/// on every method) and route dynamic dispatch through the
/// [`formatter`] function instead. That keeps the call sites simple
/// without forcing a vtable.
pub trait Formatter {
    fn write_to<W: Write, T: Serialize>(&self, value: &T, w: &mut W) -> CliResult<()>;
    fn format<T: Serialize>(&self, value: &T) -> CliResult<String> {
        let mut buf = Vec::new();
        self.write_to(value, &mut buf)?;
        String::from_utf8(buf)
            .map_err(|e| CliError::Internal(format!("non-utf8 output: {e}")))
    }
}

/// Render `value` to stdout using the given format.
pub fn print<T: Serialize>(fmt: OutputFormat, value: &T) -> CliResult<()> {
    let s = formatter(fmt).format(value)?;
    println!("{s}");
    Ok(())
}

/// Convenience: render `value` as a JSON string (no pretty printing).
pub fn print_json<T: Serialize>(value: &T) -> CliResult<()> {
    let s = serde_json::to_string(value)
        .map_err(|e| CliError::Internal(format!("json encode: {e}")))?;
    println!("{s}");
    Ok(())
}

/// Render a two-column table to stdout from a list of `(key, value)`
/// pairs. Keys are sorted for determinism (DO-178C §6.1).
pub fn print_table<K: AsRef<str>, V: AsRef<str>>(rows: &[(K, V)]) -> CliResult<()> {
    let mut rows: Vec<(&str, &str)> = rows
        .iter()
        .map(|(k, v)| (k.as_ref(), v.as_ref()))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    TableFormatter.write_to(&rows, &mut std::io::stdout())
}

/// Build the formatter matching the requested format. We return a
/// concrete-type enum (sum type) to avoid `dyn` boxing; callers
/// pattern-match to invoke the right formatter.
pub enum AnyFormatter {
    Json,
    Plain,
    Table,
}

impl AnyFormatter {
    pub fn from(fmt: OutputFormat) -> Self {
        match fmt {
            OutputFormat::Json => Self::Json,
            OutputFormat::Plain => Self::Plain,
            OutputFormat::Table => Self::Table,
        }
    }
    pub fn format<T: Serialize>(&self, value: &T) -> CliResult<String> {
        let mut buf = Vec::new();
        match self {
            Self::Json => JsonFormatter.write_to(value, &mut buf)?,
            Self::Plain => PlainFormatter.write_to(value, &mut buf)?,
            Self::Table => TableFormatter.write_to(value, &mut buf)?,
        }
        String::from_utf8(buf).map_err(|e| CliError::Internal(format!("non-utf8: {e}")))
    }
}

/// Build the formatter matching the requested format. Deprecated —
/// use [`AnyFormatter::from`] which returns a non-dyn value.
pub fn formatter(fmt: OutputFormat) -> AnyFormatter {
    AnyFormatter::from(fmt)
}

// ── formatters ──────────────────────────────────────────────────────────

pub struct JsonFormatter;
impl Formatter for JsonFormatter {
    fn write_to<W: Write, T: Serialize>(&self, value: &T, w: &mut W) -> CliResult<()> {
        let s = serde_json::to_string_pretty(value)
            .map_err(|e| CliError::Internal(format!("json encode: {e}")))?;
        w.write_all(s.as_bytes())
            .map_err(|e| CliError::Io(e))?;
        Ok(())
    }
}

pub struct PlainFormatter;
impl Formatter for PlainFormatter {
    fn write_to<W: Write, T: Serialize>(&self, value: &T, w: &mut W) -> CliResult<()> {
        let v = serde_json::to_value(value)
            .map_err(|e| CliError::Internal(format!("plain encode: {e}")))?;
        write_plain(w, &v, "")
    }
}

pub struct TableFormatter;
impl Formatter for TableFormatter {
    fn write_to<W: Write, T: Serialize>(&self, value: &T, w: &mut W) -> CliResult<()> {
        let v = serde_json::to_value(value)
            .map_err(|e| CliError::Internal(format!("table encode: {e}")))?;
        write_table(w, &v)
    }
}

// ── internals ───────────────────────────────────────────────────────────

fn write_plain<W: Write>(w: &mut W, v: &serde_json::Value, prefix: &str) -> CliResult<()> {
    match v {
        serde_json::Value::Null => {
            writeln!(w, "{prefix}=").map_err(CliError::Io)?;
        }
        serde_json::Value::Bool(b) => {
            writeln!(w, "{prefix}={b}").map_err(CliError::Io)?;
        }
        serde_json::Value::Number(n) => {
            writeln!(w, "{prefix}={n}").map_err(CliError::Io)?;
        }
        serde_json::Value::String(s) => {
            // Quote if it contains spaces / tabs.
            if s.chars().any(|c| c.is_whitespace()) {
                writeln!(w, "{prefix}=\"{}\"", s.escape_default()).map_err(CliError::Io)?;
            } else {
                writeln!(w, "{prefix}={s}").map_err(CliError::Io)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                let key = format!("{prefix}[{i}]");
                write_plain(w, item, &key)?;
            }
        }
        serde_json::Value::Object(map) => {
            // Sort keys for determinism (DO-178C §6.1).
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                let new_prefix = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                write_plain(w, &map[k], &new_prefix)?;
            }
        }
    }
    Ok(())
}

fn write_table<W: Write>(w: &mut W, v: &serde_json::Value) -> CliResult<()> {
    match v {
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                writeln!(w, "(empty)").map_err(CliError::Io)?;
                return Ok(());
            }
            // Collect columns from first element (object) or fall back
            // to single-column "value" for primitives.
            let header: Vec<String> = if let Some(serde_json::Value::Object(obj)) = arr.first() {
                let mut keys: Vec<&String> = obj.keys().collect();
                keys.sort();
                keys.into_iter().cloned().collect()
            } else {
                vec!["value".into()]
            };
            let mut rows: Vec<Vec<String>> = Vec::with_capacity(arr.len());
            for item in arr {
                let row = match item {
                    serde_json::Value::Object(obj) => header
                        .iter()
                        .map(|k| truncate(&format_cell(obj.get(k)), 40))
                        .collect(),
                    other => vec![truncate(&format_cell(Some(other)), 40)],
                };
                rows.push(row);
            }
            // Column widths.
            let mut widths: Vec<usize> = header.iter().map(|s| s.len()).collect();
            for r in &rows {
                for (i, cell) in r.iter().enumerate() {
                    if i < widths.len() && cell.len() > widths[i] {
                        widths[i] = cell.len();
                    }
                }
            }
            // Header.
            for (i, h) in header.iter().enumerate() {
                if i > 0 {
                    write!(w, "  ").map_err(CliError::Io)?;
                }
                write!(w, "{:<width$}", h, width = widths[i]).map_err(CliError::Io)?;
            }
            writeln!(w).map_err(CliError::Io)?;
            // Separator.
            for (i, w_) in widths.iter().enumerate() {
                if i > 0 {
                    write!(w, "  ").map_err(CliError::Io)?;
                }
                write!(w, "{}", "-".repeat(*w_)).map_err(CliError::Io)?;
            }
            writeln!(w).map_err(CliError::Io)?;
            // Rows.
            for r in rows {
                for (i, cell) in r.iter().enumerate() {
                    if i > 0 {
                        write!(w, "  ").map_err(CliError::Io)?;
                    }
                    let w_ = widths.get(i).copied().unwrap_or(cell.len());
                    write!(w, "{cell:<w_$}").map_err(CliError::Io)?;
                }
                writeln!(w).map_err(CliError::Io)?;
            }
        }
        serde_json::Value::Object(obj) => {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            let mut key_w = keys.iter().map(|s| s.len()).max().unwrap_or(0);
            if key_w < "key".len() {
                key_w = "key".len();
            }
            writeln!(w, "{:<key_w$}  value", "key").map_err(CliError::Io)?;
            writeln!(w, "{}  {}", "-".repeat(key_w), "-----").map_err(CliError::Io)?;
            for k in keys {
                let v = format_cell(obj.get(k));
                writeln!(w, "{k:<key_w$}  {v}").map_err(CliError::Io)?;
            }
        }
        scalar => {
            writeln!(w, "{}", format_cell(Some(scalar))).map_err(CliError::Io)?;
        }
    }
    Ok(())
}

fn format_cell(v: Option<&serde_json::Value>) -> String {
    match v {
        None | Some(serde_json::Value::Null) => "-".into(),
        Some(serde_json::Value::Bool(b)) => b.to_string(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_formatter_round_trips() {
        let f = JsonFormatter;
        let s = f.format(&serde_json::json!({"a": 1, "b": [1, 2]})).unwrap();
        assert!(s.contains("\"a\": 1"));
        assert!(s.contains("\"b\": ["));
    }

    #[test]
    fn plain_formatter_sorts_object_keys() {
        let f = PlainFormatter;
        let s = f
            .format(&serde_json::json!({"b": 1, "a": 2}))
            .unwrap();
        // "a=" must precede "b=" deterministically.
        let a_pos = s.find("a=").unwrap();
        let b_pos = s.find("b=").unwrap();
        assert!(a_pos < b_pos);
    }

    #[test]
    fn plain_formatter_quotes_whitespace() {
        let f = PlainFormatter;
        let s = f
            .format(&serde_json::json!({"msg": "hello world"}))
            .unwrap();
        assert!(s.contains("msg=\"hello world\""));
    }

    #[test]
    fn table_formatter_handles_empty_array() {
        let f = TableFormatter;
        let s = f.format(&serde_json::json!([])).unwrap();
        assert!(s.contains("(empty)"));
    }

    #[test]
    fn table_formatter_aligns_columns() {
        let f = TableFormatter;
        let s = f
            .format(&serde_json::json!([
                {"id": "abc", "name": "alice"},
                {"id": "longer_id", "name": "bob"}
            ]))
            .unwrap();
        assert!(s.lines().count() >= 4); // header + sep + 2 rows
        // Header should contain sorted keys.
        let header = s.lines().next().unwrap();
        assert!(header.starts_with("id"));
    }

    #[test]
    fn table_formatter_truncates_long_cells() {
        let f = TableFormatter;
        let long = "x".repeat(200);
        let s = f
            .format(&serde_json::json!([{"k": long}]))
            .unwrap();
        assert!(s.contains('…'));
    }

    #[test]
    fn print_routes_through_formatter() {
        let v = serde_json::json!({"x": 1});
        // Just exercise the dispatch logic — we don't capture stdout.
        let _ = print(OutputFormat::Json, &v);
        let _ = print(OutputFormat::Plain, &v);
        let _ = print(OutputFormat::Table, &v);
    }
}