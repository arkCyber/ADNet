//! Tiny demo: retry with exponential backoff.
//!
//! Run with: cargo run -p adnet-resilience --example retry_demo

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use adnet_resilience::{retry_with_backoff, RetryPolicy};

#[tokio::main]
async fn main() {
    let attempt = AtomicU32::new(0);
    let result: Result<&'static str, &'static str> = retry_with_backoff(
        || {
            let n = attempt.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                println!("attempt #{n}");
                if n < 3 {
                    Err("not yet")
                } else {
                    Ok("success!")
                }
            }
        },
        RetryPolicy::Transient.to_config(),
    )
    .await;
    println!("final result: {result:?}");
    let _ = Duration::from_secs(2);
}
