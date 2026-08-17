//! DO-178C §6.3 — fail-safe panic catcher for service dispatchers.
//!
//! Every JSON-RPC entry point eventually funnels through a
//! service-level `dispatch` function. Without protection, a single
//! `unwrap` (or `expect`, or arithmetic overflow in debug) inside
//! the service tears down the daemon's worker task — and on
//! `single-threaded` executors takes the whole process with it.
//!
//! [`catch_unwind_internal`] captures any panic that propagates up
//! the call stack and converts it into an
//! [`A3chatError::Internal`]. The error preserves the panic payload
//! (string or `&'static str`) so the operator can correlate the
//! crash with the original RPC method in `notification_bus` audit
//! logs.
//!
//! ## Usage
//!
//! ```ignore
//! pub async fn dispatch(svc: Arc<S>, method: &str, owner: &UserId, params: Value)
//!     -> Result<Value, A3chatError>
//! {
//!     let fut = dispatch_inner(svc, method, owner, params);
//!     panic_safety::catch_unwind_internal(fut, method).await
//! }
//! ```
//!
//! Notes on soundness:
//! - `AssertUnwindSafe` is appropriate because the service Arc is
//!   only used for `&self` calls inside the closure — no mutable
//!   references cross the await boundary.
//! - The panic payload (`Box<dyn Any + Send>`) is consumed via
//!   `downcast_ref` to extract a printable string. If the payload
//!   type is unknown (a non-`String` panic), a generic message
//!   is returned so the operator still gets *some* signal.

use a3chat_core::error::A3chatError;
use futures::FutureExt;
use std::panic::AssertUnwindSafe;

/// Wrap an async closure so a panic is converted to
/// [`A3chatError::Internal`].
pub async fn catch_unwind_internal<F, T>(fut: F, method_label: &str) -> Result<T, A3chatError>
where
    F: std::future::Future<Output = Result<T, A3chatError>>,
{
    let label = method_label.to_string();
    match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(r) => r,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&'static str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic (non-string payload)".to_string());
            Err(A3chatError::Internal(format!(
                "service panicked in {label}: {msg}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn panic_in_future_is_caught() {
        async fn explode() -> Result<(), A3chatError> {
            panic!("kaboom");
        }
        let r = catch_unwind_internal(explode(), "test_method").await;
        let err = r.expect_err("expected Internal error");
        match err {
            A3chatError::Internal(msg) => {
                assert!(msg.contains("kaboom"), "{msg}");
                assert!(msg.contains("test_method"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn ok_passthrough() {
        async fn ok() -> Result<u32, A3chatError> {
            Ok(7)
        }
        let r = catch_unwind_internal(ok(), "test_ok").await.unwrap();
        assert_eq!(r, 7);
    }
}