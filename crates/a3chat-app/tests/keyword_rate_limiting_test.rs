//! Integration tests for keyword notification service with rate limiting

use a3chat_app::keyword_notification_service::{KeywordNotificationService, rate_limiter::RateLimiterConfig};
use a3chat_app::notification_bus::NotificationBus;
use a3chat_core::id::{ConversationId, MessageId, UserId};

#[tokio::test]
async fn test_rate_limiting_prevents_notification_storm() {
    let config = RateLimiterConfig {
        max_notifications: 3,
        window_seconds: 60,
        per_keyword: true,
    };
    
    let bus = NotificationBus::new(100);
    let service = KeywordNotificationService::with_rate_limiter(bus, config);
    
    let user_id = UserId::from("test_user");
    let conv_id = ConversationId::from("test_conv");
    let msg_id = MessageId::from("test_msg");
    let sender_id = UserId::from("sender");
    
    // Add a keyword
    service.add_keyword(&user_id, "urgent".to_string(), false)
        .await
        .unwrap();
    
    // First 3 messages should trigger notifications
    let matches1 = service.check_message(&user_id, &conv_id, &msg_id, &sender_id, "This is urgent!")
        .await
        .unwrap();
    assert_eq!(matches1.len(), 1, "First match should succeed");
    
    let matches2 = service.check_message(&user_id, &conv_id, &msg_id, &sender_id, "Another urgent message")
        .await
        .unwrap();
    assert_eq!(matches2.len(), 1, "Second match should succeed");
    
    let matches3 = service.check_message(&user_id, &conv_id, &msg_id, &sender_id, "Still urgent!")
        .await
        .unwrap();
    assert_eq!(matches3.len(), 1, "Third match should succeed");
    
    // 4th message should be rate limited
    let matches4 = service.check_message(&user_id, &conv_id, &msg_id, &sender_id, "Too many urgent messages")
        .await
        .unwrap();
    assert_eq!(matches4.len(), 0, "Fourth match should be rate limited");
    
    // Check rate limiter stats
    let stats = service.get_rate_limiter_stats();
    assert_eq!(stats.total_limited, 1, "Should have 1 rate limited notification");
    
    println!("✅ Rate limiting test passed");
}

#[tokio::test]
async fn test_per_keyword_rate_limiting() {
    let config = RateLimiterConfig {
        max_notifications: 2,
        window_seconds: 60,
        per_keyword: true, // Different keywords have separate limits
    };
    
    let bus = NotificationBus::new(100);
    let service = KeywordNotificationService::with_rate_limiter(bus, config);
    
    let user_id = UserId::from("test_user");
    let conv_id = ConversationId::from("test_conv");
    let msg_id = MessageId::from("test_msg");
    let sender_id = UserId::from("sender");
    
    // Add two keywords
    service.add_keyword(&user_id, "urgent".to_string(), false)
        .await
        .unwrap();
    service.add_keyword(&user_id, "important".to_string(), false)
        .await
        .unwrap();
    
    // Use up "urgent" quota
    service.check_message(&user_id, &conv_id, &msg_id, &sender_id, "This is urgent!")
        .await
        .unwrap();
    service.check_message(&user_id, &conv_id, &msg_id, &sender_id, "Still urgent!")
        .await
        .unwrap();
    
    // "urgent" should be rate limited
    let matches = service.check_message(&user_id, &conv_id, &msg_id, &sender_id, "Too urgent")
        .await
        .unwrap();
    assert_eq!(matches.len(), 0, "urgent should be rate limited");
    
    // "important" should still work (separate bucket)
    let matches = service.check_message(&user_id, &conv_id, &msg_id, &sender_id, "This is important!")
        .await
        .unwrap();
    assert_eq!(matches.len(), 1, "important should still work");
    
    println!("✅ Per-keyword rate limiting test passed");
}

#[tokio::test]
async fn test_available_quota() {
    let config = RateLimiterConfig {
        max_notifications: 5,
        window_seconds: 60,
        per_keyword: true,
    };
    
    let bus = NotificationBus::new(100);
    let service = KeywordNotificationService::with_rate_limiter(bus, config);
    
    let user_id = UserId::from("test_user");
    let conv_id = ConversationId::from("test_conv");
    let msg_id = MessageId::from("test_msg");
    let sender_id = UserId::from("sender");
    
    service.add_keyword(&user_id, "test".to_string(), false)
        .await
        .unwrap();
    
    // Initial quota should be 5
    let quota = service.get_available_quota(&user_id, Some("test"));
    assert_eq!(quota, 5, "Initial quota should be max");
    
    // Consume 2
    service.check_message(&user_id, &conv_id, &msg_id, &sender_id, "test 1")
        .await
        .unwrap();
    service.check_message(&user_id, &conv_id, &msg_id, &sender_id, "test 2")
        .await
        .unwrap();
    
    // Should have 3 left
    let quota = service.get_available_quota(&user_id, Some("test"));
    assert_eq!(quota, 3, "Should have 3 remaining");
    
    println!("✅ Available quota test passed");
}
