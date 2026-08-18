//! Test utilities for integration tests.

use chrono::Utc;

/// Create a test chat message.
pub fn create_chat_message(
    sender: &str,
    content: &str,
    conversation_id: &str,
) -> a3net_types::group_chat::Message {
    a3net_types::group_chat::Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.to_string(),
        sender_id: sender.to_string(),
        content: content.to_string(),
        timestamp: Utc::now(),
        sequence: 0,
        reply_to: None,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    }
}

/// Wait for a condition with timeout.
pub async fn wait_for_condition<F>(mut check: F, timeout_ms: u64) -> bool
where
    F: FnMut() -> bool,
{
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_millis(timeout_ms);

    while start.elapsed() < timeout {
        if check() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    check()
}
