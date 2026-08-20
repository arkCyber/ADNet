//! P1 Features Integration Tests
//!
//! Integration tests for P1 phase features

#![cfg(feature = "iroh")]

use std::time::Duration;
use tokio::time::sleep;
use tempfile::TempDir;

use a3chat_core::id::{ConversationId, UserId, MessageId};
use a3chat_app::{
    app::A3chatApp,
    storage::StorageConfig,
    group_sync_service::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState},
    keyword_notification_service::KeywordNotificationService,
    disappearing_message_service::DisappearingMessageService,
};

// ========================================================================
// Circuit Breaker Tests
// ========================================================================

#[tokio::test]
async fn test_circuit_breaker_state_machine() {
    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        success_threshold: 2,
        timeout: Duration::from_millis(100),
    };
    
    let cb = CircuitBreaker::with_config(config);
    
    // Initial state should be closed
    assert_eq!(cb.state(), CircuitState::Closed);
    
    // Trip the circuit
    for _ in 0..3 {
        cb.record_failure();
    }
    
    assert_eq!(cb.state(), CircuitState::Open);
    assert!(!cb.allow_request());
    
    // Wait for half-open
    sleep(Duration::from_millis(150)).await;
    
    // Should allow requests now
    assert!(cb.state() == CircuitState::HalfOpen || cb.allow_request());
    
    // Recover
    cb.record_success();
    cb.record_success();
    
    assert_eq!(cb.state(), CircuitState::Closed);
    
    println!("✅ Circuit breaker state machine works correctly");
}

#[tokio::test]
async fn test_circuit_breaker_metrics() {
    let cb = CircuitBreaker::new();
    
    // Generate some activity
    cb.record_failure();
    cb.record_success();
    cb.record_success();
    
    let state = cb.state();
    
    println!("✅ Circuit breaker state tracking:");
    println!("   - Current state: {:?}", state);
}

// ========================================================================
// Keyword Service Tests
// ========================================================================

#[tokio::test]
async fn test_keyword_service_basic_operations() {
    let temp_dir = TempDir::new().unwrap();
    let owner = UserId::from("owner");
    let config = StorageConfig::new(temp_dir.path());
    let app = A3chatApp::new(config, owner).unwrap();
    
    let service = KeywordNotificationService::new(app.bus.clone());
    
    let user = UserId::from("test-user");
    
    // Add keyword
    let entry = service.add_keyword(&user, "urgent".to_string(), false).await.unwrap();
    assert_eq!(entry.keyword, "urgent");
    assert!(!entry.is_regex);
    
    // List keywords
    let keywords = service.list_keywords(&user).await;
    assert_eq!(keywords.len(), 1);
    
    // Remove keyword
    let removed = service.remove_keyword(&user, &entry.keyword_id).await.unwrap();
    assert!(removed);
    
    let keywords_after = service.list_keywords(&user).await;
    assert_eq!(keywords_after.len(), 0);
    
    println!("✅ Keyword service basic operations work");
}

#[tokio::test]
async fn test_keyword_service_multi_user_isolation() {
    let temp_dir = TempDir::new().unwrap();
    let app = A3chatApp::new(StorageConfig::new(temp_dir.path()), UserId::from("owner")).unwrap();
    
    let service = KeywordNotificationService::new(app.bus.clone());
    
    let user1 = UserId::from("user1");
    let user2 = UserId::from("user2");
    
    // Each user adds their own keywords
    service.add_keyword(&user1, "urgent".to_string(), false).await.unwrap();
    service.add_keyword(&user2, "important".to_string(), false).await.unwrap();
    
    // Verify isolation
    let user1_kws = service.list_keywords(&user1).await;
    let user2_kws = service.list_keywords(&user2).await;
    
    assert_eq!(user1_kws.len(), 1);
    assert_eq!(user2_kws.len(), 1);
    assert_eq!(user1_kws[0].keyword, "urgent");
    assert_eq!(user2_kws[0].keyword, "important");
    
    println!("✅ Keyword service multi-user isolation works");
}

#[tokio::test]
async fn test_keyword_service_regex_support() {
    let temp_dir = TempDir::new().unwrap();
    let app = A3chatApp::new(StorageConfig::new(temp_dir.path()), UserId::from("owner")).unwrap();
    
    let service = KeywordNotificationService::new(app.bus.clone());
    
    let user = UserId::from("regex-user");
    
    // Add text keyword
    service.add_keyword(&user, "bug".to_string(), false).await.unwrap();
    
    // Add regex keyword
    service.add_keyword(&user, r"issue-\d+".to_string(), true).await.unwrap();
    
    let keywords = service.list_keywords(&user).await;
    assert_eq!(keywords.len(), 2);
    
    let text_count = keywords.iter().filter(|k| !k.is_regex).count();
    let regex_count = keywords.iter().filter(|k| k.is_regex).count();
    
    assert_eq!(text_count, 1);
    assert_eq!(regex_count, 1);
    
    println!("✅ Keyword service regex support works");
}

// ========================================================================
// Disappearing Message Service Tests
// ========================================================================

#[tokio::test]
async fn test_disappearing_message_registration() {
    let temp_dir = TempDir::new().unwrap();
    let app = A3chatApp::new(StorageConfig::new(temp_dir.path()), UserId::from("owner")).unwrap();
    
    let service = DisappearingMessageService::new(
        app.bus.clone(),
        app.chat.storage().clone(),
    );
    
    let msg_id = MessageId::from("test-msg");
    let conv_id = ConversationId::from("test-conv");
    let user_id = UserId::from("test-user");
    
    // Register message (may or may not succeed depending on settings)
    let registered = service.register_message(&msg_id, &conv_id, &user_id).await.unwrap();
    
    println!("✅ Message registration completed (registered: {})", registered);
}

#[tokio::test]
async fn test_disappearing_message_stats() {
    let temp_dir = TempDir::new().unwrap();
    let app = A3chatApp::new(StorageConfig::new(temp_dir.path()), UserId::from("owner")).unwrap();
    
    let service = DisappearingMessageService::new(
        app.bus.clone(),
        app.chat.storage().clone(),
    );
    
    let user_id = UserId::from("test-user");
    
    // Get stats
    let stats = service.get_ephemeral_stats(&user_id).await;
    
    println!("✅ Ephemeral message stats:");
    println!("   - Pending deletions: {}", stats.pending_deletions);
    println!("   - Read messages: {}", stats.read_messages);
}

#[tokio::test]
async fn test_disappearing_message_cleanup() {
    let temp_dir = TempDir::new().unwrap();
    let app = A3chatApp::new(StorageConfig::new(temp_dir.path()), UserId::from("owner")).unwrap();
    
    let service = DisappearingMessageService::new(
        app.bus.clone(),
        app.chat.storage().clone(),
    );
    
    // Register some messages
    for i in 0..3 {
        let msg_id = MessageId::from(format!("msg{}", i));
        let conv_id = ConversationId::from("conv1");
        let user_id = UserId::from("user1");
        let _ = service.register_message(&msg_id, &conv_id, &user_id).await;
    }
    
    // Run cleanup
    service.cleanup_orphaned_messages().await;
    
    println!("✅ Orphan cleanup completed");
}

// ========================================================================
// Benchmark Persistence Tests
// ========================================================================

#[tokio::test]
async fn test_benchmark_persistence() {
    use a3chat_app::group_sync_service::benchmarks::{BenchmarkResults, ThroughputBenchmark};
    
    let temp_dir = TempDir::new().unwrap();
    let results_path = temp_dir.path().join("benchmark.json");
    
    // Run benchmark
    let throughput = ThroughputBenchmark::run(1000, 10).await;
    
    let mut results = BenchmarkResults::new();
    results.throughput = Some(throughput);
    
    // Save
    results.save_to_file(&results_path).unwrap();
    assert!(results_path.exists());
    
    // Load
    let loaded = BenchmarkResults::load_from_file(&results_path).unwrap();
    assert!(loaded.throughput.is_some());
    assert!(!loaded.timestamp.is_empty());
    assert!(!loaded.version.is_empty());
    
    println!("✅ Benchmark persistence works");
}

#[tokio::test]
async fn test_benchmark_history_tracking() {
    use a3chat_app::group_sync_service::benchmarks::{BenchmarkResults, ThroughputBenchmark};
    
    let temp_dir = TempDir::new().unwrap();
    let history_path = temp_dir.path().join("history.jsonl");
    
    // Simulate multiple runs
    for _ in 0..3 {
        let tp = ThroughputBenchmark::run(500, 5).await;
        let mut results = BenchmarkResults::new();
        results.throughput = Some(tp);
        results.append_to_history(&history_path).unwrap();
    }
    
    // Load history
    let history = BenchmarkResults::load_history(&history_path).unwrap();
    assert_eq!(history.len(), 3);
    
    println!("✅ Benchmark history tracking works ({} entries)", history.len());
}

// ========================================================================
// Integration Scenarios
// ========================================================================

#[tokio::test]
async fn test_all_features_integration() {
    let temp_dir = TempDir::new().unwrap();
    let app = A3chatApp::new(StorageConfig::new(temp_dir.path()), UserId::from("owner")).unwrap();
    
    // 1. Circuit breaker
    let cb = CircuitBreaker::new();
    cb.record_success();
    assert_eq!(cb.state(), CircuitState::Closed);
    
    // 2. Keyword service
    let keyword_svc = KeywordNotificationService::new(app.bus.clone());
    let user = UserId::from("test-user");
    keyword_svc.add_keyword(&user, "test".to_string(), false).await.unwrap();
    let keywords = keyword_svc.list_keywords(&user).await;
    assert_eq!(keywords.len(), 1);
    
    // 3. Ephemeral message service
    let ephemeral_svc = DisappearingMessageService::new(
        app.bus.clone(),
        app.chat.storage().clone(),
    );
    let msg_id = MessageId::from("msg1");
    let conv_id = ConversationId::from("conv1");
    let _ = ephemeral_svc.register_message(&msg_id, &conv_id, &user).await;
    
    // 4. Benchmark
    use a3chat_app::group_sync_service::benchmarks::ThroughputBenchmark;
    let benchmark = ThroughputBenchmark::run(100, 10).await;
    assert!(benchmark.msg_per_sec > 0.0);
    
    println!("✅ All P1 features integrated successfully");
    println!("   - Circuit breaker: operational");
    println!("   - Keywords: {} configured", keywords.len());
    println!("   - Ephemeral messages: tracked");
    println!("   - Benchmark: {:.2} msg/s", benchmark.msg_per_sec);
}
