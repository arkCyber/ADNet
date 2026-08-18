//! `a3chat schema` — dump the a3chat-core JSON Schema document to
//! stdout. The schema is hand-rolled in `a3chat_core::schema` and
//! serves as the stable wire contract for frontend codegen (TS / Dart /
//! Swift). This subcommand does **not** talk to a daemon — it is a
//! pure offline tool, useful in CI for `a3chat schema > schema.json`
//! piped into quicktype or similar.
//!
//! ## Usage
//!
//! ```text
//! $ a3chat schema                # full document
//! $ a3chat schema --name PublicAccount
//!                                   # single definition by name
//!                                   # (returns a $ref to the def)
//! ```

use crate::error::CliResult;
use clap::Args;

/// Arguments for `a3chat schema`.
#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// Optional definition name (e.g. `PublicAccount`). When set, the
    /// output is `{ "$ref": "#/definitions/<name>" }` rather than the
    /// full document — convenient for inspecting a single shape.
    #[arg(long)]
    pub name: Option<String>,
}

/// Print the a3chat-core JSON Schema document (or one named
/// definition) to stdout. Exits non-zero on serialization failure
/// (which should be impossible given the schema is built
/// programmatically, but the call site is safe).
pub fn run(args: SchemaArgs) -> CliResult<()> {
    let doc = a3chat_core::a3chat_json_schema();
    let out = match args.name.as_deref() {
        None => doc,
        Some(name) => {
            let defs = doc
                .get("definitions")
                .and_then(|v| v.as_object())
                .ok_or_else(|| {
                    crate::error::CliError::Internal(
                        "schema document has no definitions object".into(),
                    )
                })?;
            if !defs.contains_key(name) {
                let known: Vec<&str> =
                    defs.keys().map(String::as_str).collect();
                return Err(crate::error::CliError::Usage(format!(
                    "unknown schema definition {name:?}; known: {known:?}"
                )));
            }
            serde_json::json!({ "$ref": format!("#/definitions/{name}") })
        }
    };
    let pretty = serde_json::to_string_pretty(&out).map_err(|e| {
        crate::error::CliError::Internal(format!(
            "failed to serialize schema: {e}"
        ))
    })?;
    println!("{pretty}");
    Ok(())
}