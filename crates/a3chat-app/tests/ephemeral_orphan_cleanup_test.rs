//! Tests for orphaned ephemeral message cleanup

use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use a3chat_app::disappearing_message_service::{
    DisappearingMessageService, DisappearingTimer,
};
use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};
use a3chat_app::keyring::E2eKeyring;
use a3chat_core::id::{ConversationId, MessageId, UserId};
use tempfile::TempDir;

fn make_test_storage() -> (ChatStorage, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let config = StorageConfig::new(temp_dir.path());
    let owner = UserId::from("test_owner");
    let keyring = E2eKeyring::new(owner);
    let storage = ChatStorage::new(config, keyring);
    (storage, temp_dir)
}

#[tokio::test]
async fn test_orphan_cleanup_detects_expired_messages() {
    let bus = NotificationBus::new(100);
    let (storage, _temp) = make_test_storage();
    let service = Arc::new(DisappearingMessageService::new(bus, storage));
    
    let user_id = UserId::from("test_user");
    let conv_id = ConversationId::from("test_conv");
    let msg_id = MessageId::from("test_msg");
    
    // Set timer to 5 seconds
    service.set_timer(&user_id, &conv_id, DisappearingTimer::FiveSeconds)
        .await
        .unwrap();
    
    // Register ephemeral message
    service.register_message(&msg_id, &conv_id, &user_id)
        .await
        .unwrap();
    
    // Mark as read (starts timer)
    service.mark_message_read(&user_id, &msg_id, &conv_id)
        .await
        .unwrap();
    
    // Force expire the message
    service.force_expire_message(&user_id, &msg_id).await;
    
    // Run cleanup
    service.cleanup_orphaned_messages().await;
    
    // Verify message was cleaned up
    let status = service.get_message_status(&user_id, &msg_id).await.unwrap();
    assert!(status.is_none(), "Orphaned message should be deleted");
    
    println!("✅ Orphan cleanup detection test passed");
}

#[tokio::test]
async fn test_orphan_cleanup_preserves_valid_messages() {
    let bus = NotificationBus::new(100);
    let (storage, _temp) = make_test_storage();
    let service = Arc::new(DisappearingMessageService::new(bus, storage));
    
    let user_id = UserId::from("test_user");
    let conv_id = ConversationId::from("test_conv");
    let msg_id = MessageId::from("test_msg");
    
    // Set timer to 1 hour
    service.set_timer(&user_id, &conv_id, DisappearingTimer::OneHour)
        .await
        .unwrap();
    
    // Register ephemeral message
    service.register_message(&msg_id, &conv_id, &user_id)
        .await
        .unwrap();
    
    // Mark as read
    service.mark_message_read(&user_id, &msg_id, &conv_id)
        .await
        .unwrap();
    
    // Run cleanup (should not delete - message not expired)
    service.cleanup_orphaned_messages().await;
    
    // Verify message was NOT deleted (still within timer)
    let status = service.get_message_status(&user_id, &msg_id).await.unwrap();
    assert!(status.is_some(), "Valid message should not be deleted");
    
    println!("✅ Valid message preservation test passed");
}

#[tokio::test]
async fn test_orphan_cleanup_handles_multiple_users() {
    let bus = NotificationBus::new(100);
    let (storage, _temp) = make_test_storage();
    let service = Arc::new(DisappearingMessageService::new(bus, storage));
    
    let user1 = UserId::from("user1");
    let user2 = UserId::from("user2");
    let conv_id = ConversationId::from("test_conv");
    let msg1 = MessageId::from("msg1");
    let msg2 = MessageId::from("msg2");
    
    // Setup for user1 - expired message
    service.set_timer(&user1, &conv_id, DisappearingTimer::FiveSeconds)
        .await
        .unwrap();
    service.register_message(&msg1, &conv_id, &user1)
        .await
        .unwrap();
    service.mark_message_read(&user1, &msg1, &conv_id)
        .await
        .unwrap();
    
    // Force expire user1's message
    service.force_expire_message(&user1, &msg1).await;
    
    // Setup for user2 - valid message
    service.set_timer(&user2, &conv_id, DisappearingTimer::OneHour)
        .await
        .unwrap();
    service.register_message(&msg2, &conv_id, &user2)
        .await
        .unwrap();
    service.mark_message_read(&user2, &msg2, &conv_id)
        .await
        .unwrap();
    
    // Run cleanup
    service.cleanup_orphaned_messages().await;
    
    // Verify user1's message deleted, user2's preserved
    let status1 = service.get_message_status(&user1, &msg1).await.unwrap();
    assert!(status1.is_none(), "User1's expired message should be deleted");
    
    let status2 = service.get_message_status(&user2, &msg2).await.unwrap();
    assert!(status2.is_some(), "User2's valid message should be preserved");
    
    println!("✅ Multi-user cleanup test passed");
}

#[tokio::test]
async fn test_get_ephemeral_stats() {
    let bus = NotificationBus::new(100);
    let (storage, _temp) = make_test_storage();
    let service = Arc::new(DisappearingMessageService::new(bus, storage));
    
    let user_id = UserId::from("test_user");
    let conv_id = ConversationId::from("test_conv");
    
    service.set_timer(&user_id, &conv_id, DisappearingTimer::OneHour)
        .await
        .unwrap();
    
    // Register and read 3 messages
    for i in 1..=3 {
        let msg_id = MessageId::from(format!("msg_{}", i));
        service.register_message(&msg_id, &conv_id, &user_id)
            .await
            .unwrap();
        service.mark_message_read(&user_id, &msg_id, &conv_id)
            .await
            .unwrap();
    }
    
    // Register 2 more messages but don't read them
    for i in 4..=5 {
        let msg_id = MessageId::from(format!("msg_{}", i));
        service.register_message(&msg_id, &conv_id, &user_id)
            .await
            .unwrap();
    }
    
    let stats = service.get_ephemeral_stats(&user_id).await;
    
    assert_eq!(stats.total_tracked, 5, "Should track 5 messages");
    assert_eq!(stats.read_messages, 3, "Should have 3 read messages");
    assert_eq!(stats.pending_deletions, 3, "Should have 3 pending deletions");
    
    println!("✅ Stats test passed: {:?}", stats);
}

#[tokio::test]
async fn test_background_worker_with_orphan_cleanup() {
    let bus = NotificationBus::new(100);
    let (storage, _temp) = make_test_storage();
    let service = Arc::new(DisappearingMessageService::new(bus, storage));
    
    // Start background worker
    service.clone().start_background_worker();
    
    let user_id = UserId::from("test_user");
    let conv_id = ConversationId::from("test_conv");
    let msg_id = MessageId::from("test_msg");
    
    service.set_timer(&user_id, &conv_id, DisappearingTimer::FiveSeconds)
        .await
        .unwrap();
    
    service.register_message(&msg_id, &conv_id, &user_id)
        .await
        .unwrap();
    
    service.mark_message_read(&user_id, &msg_id, &conv_id)
        .await
        .unwrap();
    
    // Force expire the message
    service.force_expire_message(&user_id, &msg_id).await;
    
    // Wait a bit for cleanup to run (background worker checks every second)
    sleep(Duration::from_secs(2)).await;
    
    // Verify message was cleaned up by background worker
    let status = service.get_message_status(&user_id, &msg_id).await.unwrap();
    assert!(status.is_none(), "Background worker should clean up expired message");
    
    // Stop worker
    service.stop().await;
    
    println!("✅ Background worker orphan cleanup test passed");
}


