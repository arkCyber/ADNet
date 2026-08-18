//! Tauri command error type.
//!
//! Every command returns [`TauriCommandResult<T>`] =
//! `Result<T, TauriCommandError>`. The `TauriCommandError` is
//! `serde::Serialize` so the Tauri runtime can ship it to the
//! frontend as a JSON payload the user can render.
//!
//! ## DO-178C §6.3 — Fail-safe
//!
//! Every error carries an `error_class` (transient / permanent /
//! security / validation) and a `recovery_hint` (a short operator
//! instruction). The UI uses these to decide whether to retry
//! (Transient), show a flash message (Permanent), force re-login
//! (Security), or block the action (Validation) without parsing
//! human-readable strings.

use serde::Serialize;

use a3chat_core::error::A3chatError;

pub type TauriCommandResult<T> = std::result::Result<T, TauriCommandError>;

/// Error classification — mirrors `a3chat_core::error::ErrorClass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Transient transport / daemon availability problem. Operator
    /// should retry, possibly after consulting "doctor".
    Transient,
    /// Permanent failure — request was rejected by the daemon. UI
    /// surfaces the message as a flash and lets the user amend.
    Permanent,
    /// Security / auth failure — UI MUST force re-login.
    Security,
    /// Validation error — the request was malformed before it left
    /// the UI. Field-level errors can be attached via `fields`.
    Validation,
    /// Internal — frontend / backend bug. Show a generic message and
    /// log the error_class for telemetry.
    Internal,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TauriCommandError {
    pub error_class: ErrorClass,
    pub code: String,
    pub message: String,
    pub recovery_hint: String,
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldError>,
}

impl TauriCommandError {
    /// Custom constructor for transient errors.
    pub fn transient(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error_class: ErrorClass::Transient,
            code: code.into(),
            message: message.into(),
            recovery_hint: "retry — if the failure persists, run `a3chat doctor`".into(),
            request_id: None,
            fields: Vec::new(),
        }
    }

    pub fn permanent(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error_class: ErrorClass::Permanent,
            code: code.into(),
            message: message.into(),
            recovery_hint: "amend the request and try again".into(),
            request_id: None,
            fields: Vec::new(),
        }
    }

    pub fn security(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error_class: ErrorClass::Security,
            code: code.into(),
            message: message.into(),
            recovery_hint: "re-login required".into(),
            request_id: None,
            fields: Vec::new(),
        }
    }

    pub fn validation(
        code: impl Into<String>,
        message: impl Into<String>,
        fields: Vec<FieldError>,
    ) -> Self {
        Self {
            error_class: ErrorClass::Validation,
            code: code.into(),
            message: message.into(),
            recovery_hint: "correct the highlighted fields and retry".into(),
            request_id: None,
            fields,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            error_class: ErrorClass::Internal,
            code: "internal".into(),
            message: message.into(),
            recovery_hint: "this is a bug — please report".into(),
            request_id: None,
            fields: Vec::new(),
        }
    }

    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Map a [`A3chatError`] from a3chat-core into a Tauri-friendly
    /// category. The mapping is conservative: transport / network
    /// errors become Transient, RPC errors with retryable flag
    /// become Transient, auth errors become Security, and everything
    /// else becomes Permanent.
    pub fn from_a3chat(err: A3chatError) -> Self {
        use a3chat_core::error::ErrorClass as Core;
        let class = err.error_class();
        let code = format!("{:?}", err)
            .split_whitespace()
            .next()
            .unwrap_or("unknown")
            .to_string();
        let mut out = Self {
            error_class: match class {
                Core::Transient => ErrorClass::Transient,
                Core::Permanent => ErrorClass::Permanent,
                Core::Security => ErrorClass::Security,
                Core::Internal => ErrorClass::Internal,
            },
            code: code.to_snake_case(),
            message: err.to_string(),
            recovery_hint: match &err {
                A3chatError::NetworkError(_) => "check `a3chat doctor` and your network".into(),
                A3chatError::CryptoError(_) => "logged out — re-login required".into(),
                A3chatError::InvalidInput(_) => "amend the request and retry".into(),
                A3chatError::NotFound(_) => "the resource was not found".into(),
                A3chatError::PermissionDenied(_) => "you don't have permission for this".into(),
                A3chatError::StorageError(_) => "local storage is failing — check disk".into(),
                _ => "see the daemon logs".into(),
            },
            request_id: None,
            fields: Vec::new(),
        };
        if let A3chatError::InvalidInput(_) = &err {
            out.fields.push(FieldError {
                field: "<request>".into(),
                message: err.to_string(),
            });
        }
        out
    }
}

impl From<A3chatError> for TauriCommandError {
    fn from(e: A3chatError) -> Self {
        Self::from_a3chat(e)
    }
}

impl std::fmt::Display for TauriCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for TauriCommandError {}

/// Tiny helper for [to_snake_case] — used to normalise `A3chatError`
/// variant names without pulling in `heck`.
trait ToSnakeCase {
    fn to_snake_case(&self) -> String;
}

impl ToSnakeCase for str {
    fn to_snake_case(&self) -> String {
        let mut out = String::with_capacity(self.len() + 4);
        for (i, c) in self.chars().enumerate() {
            if c.is_ascii_uppercase() {
                if i > 0 {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_error_shape() {
        let e = TauriCommandError::transient("net", "boom");
        assert_eq!(e.error_class, ErrorClass::Transient);
        assert!(e.recovery_hint.contains("retry"));
    }

    #[test]
    fn security_error_force_relogin() {
        let e = TauriCommandError::security("auth", "expired");
        assert_eq!(e.error_class, ErrorClass::Security);
        assert!(e.recovery_hint.contains("re-login"));
    }

    #[test]
    fn validation_carries_fields() {
        let e = TauriCommandError::validation(
            "invalid_input",
            "bad request",
            vec![FieldError {
                field: "body".into(),
                message: "empty".into(),
            }],
        );
        assert_eq!(e.error_class, ErrorClass::Validation);
        assert_eq!(e.fields.len(), 1);
    }

    #[test]
    fn from_a3chat_neterror_is_transient() {
        let e = TauriCommandError::from_a3chat(A3chatError::NetworkError("conn refused".into()));
        assert_eq!(e.error_class, ErrorClass::Transient);
    }

    #[test]
    fn from_a3chat_invalidinput_is_permanent() {
        let e = TauriCommandError::from_a3chat(A3chatError::InvalidInput("bad".into()));
        assert_eq!(e.error_class, ErrorClass::Permanent);
    }

    #[test]
    fn serde_round_trip() {
        let e = TauriCommandError::validation(
            "invalid_input",
            "bad",
            vec![FieldError {
                field: "x".into(),
                message: "y".into(),
            }],
        )
        .with_request_id("req-1");
        let json = serde_json::to_string(&e).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["error_class"], "validation");
        assert_eq!(v["request_id"], "req-1");
        assert_eq!(v["fields"][0]["field"], "x");
    }

    #[test]
    fn snake_case_helper() {
        assert_eq!("NotFound".to_snake_case(), "not_found");
        assert_eq!("HTTP".to_snake_case(), "h_t_t_p");
        assert_eq!("already_lower".to_snake_case(), "already_lower");
    }
}
