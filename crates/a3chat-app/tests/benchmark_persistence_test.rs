//! Tests for benchmark result persistence

#![cfg(feature = "iroh")]

use tempfile::TempDir;

use a3chat_app::group_sync_service::benchmarks::{
    BenchmarkResults, ThroughputBenchmark, LatencyBenchmark,
    LargeBatchBenchmark, ConcurrentBenchmark,
};

#[test]
fn test_benchmark_results_save_and_load() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("benchmark_results.json");
    
    // Create sample results
    let mut results = BenchmarkResults::new();
    results.throughput = Some(ThroughputBenchmark {
        msg_per_sec: 1000.0,
        total_messages: 5000,
        duration_ms: 5000,
    });
    results.latency = Some(LatencyBenchmark {
        avg_ms: 10.5,
        p50_ms: 8.0,
        p95_ms: 25.0,
        p99_ms: 40.0,
        max_ms: 100,
    });
    
    // Save to file
    results.save_to_file(&file_path).expect("Failed to save");
    
    // Verify file exists
    assert!(file_path.exists(), "Benchmark file should exist");
    
    // Load from file
    let loaded = BenchmarkResults::load_from_file(&file_path).expect("Failed to load");
    
    // Verify data
    assert_eq!(results.version, loaded.version);
    assert!(loaded.throughput.is_some());
    assert!(loaded.latency.is_some());
    
    let tp = loaded.throughput.unwrap();
    assert_eq!(tp.msg_per_sec, 1000.0);
    assert_eq!(tp.total_messages, 5000);
    
    println!("✅ Save and load test passed");
}

#[test]
fn test_benchmark_results_append_history() {
    let temp_dir = TempDir::new().unwrap();
    let history_path = temp_dir.path().join("benchmark_history.jsonl");
    
    // Create and append first result
    let mut result1 = BenchmarkResults::new();
    result1.throughput = Some(ThroughputBenchmark {
        msg_per_sec: 1000.0,
        total_messages: 5000,
        duration_ms: 5000,
    });
    result1.append_to_history(&history_path).expect("Failed to append");
    
    // Create and append second result
    let mut result2 = BenchmarkResults::new();
    result2.throughput = Some(ThroughputBenchmark {
        msg_per_sec: 1200.0,
        total_messages: 6000,
        duration_ms: 5000,
    });
    result2.append_to_history(&history_path).expect("Failed to append");
    
    // Load history
    let history = BenchmarkResults::load_history(&history_path).expect("Failed to load history");
    
    // Verify we have 2 entries
    assert_eq!(history.len(), 2, "Should have 2 historical entries");
    
    // Verify first entry
    let tp1 = history[0].throughput.as_ref().unwrap();
    assert_eq!(tp1.msg_per_sec, 1000.0);
    
    // Verify second entry
    let tp2 = history[1].throughput.as_ref().unwrap();
    assert_eq!(tp2.msg_per_sec, 1200.0);
    
    println!("✅ Append history test passed");
}

#[test]
fn test_benchmark_results_with_metadata() {
    let results = BenchmarkResults::new();
    
    // Verify metadata is present
    assert!(!results.timestamp.is_empty(), "Timestamp should be set");
    assert!(!results.version.is_empty(), "Version should be set");
    
    // Git commit may or may not be available
    println!("Timestamp: {}", results.timestamp);
    println!("Version: {}", results.version);
    if let Some(commit) = &results.git_commit {
        println!("Git commit: {}", commit);
    } else {
        println!("Git commit: N/A");
    }
    
    println!("✅ Metadata test passed");
}

#[tokio::test]
async fn test_complete_benchmark_with_persistence() {
    let temp_dir = TempDir::new().unwrap();
    let results_path = temp_dir.path().join("benchmark_results.json");
    let history_path = temp_dir.path().join("benchmark_history.jsonl");
    
    // Run benchmarks and create results
    let mut results = BenchmarkResults::new();
    
    // Throughput benchmark
    let throughput = ThroughputBenchmark {
        msg_per_sec: 1500.0,
        total_messages: 10000,
        duration_ms: 6667,
    };
    results.throughput = Some(throughput);
    
    // Latency benchmark
    let latency = LatencyBenchmark {
        avg_ms: 12.5,
        p50_ms: 10.0,
        p95_ms: 30.0,
        p99_ms: 50.0,
        max_ms: 120,
    };
    results.latency = Some(latency);
    
    // Large batch benchmark
    let large_batch = LargeBatchBenchmark {
        batch_size: 100,
        processing_time_ms: 5000,
        throughput: 2000.0,
    };
    results.large_batch = Some(large_batch);
    
    // Concurrent benchmark
    let concurrent = ConcurrentBenchmark {
        concurrency: 10,
        total_ops: 1000,
        ops_per_sec: 5000.0,
    };
    results.concurrent = Some(concurrent);
    
    // Save results
    results.save_to_file(&results_path).expect("Failed to save results");
    results.append_to_history(&history_path).expect("Failed to append to history");
    
    // Verify files exist
    assert!(results_path.exists());
    assert!(history_path.exists());
    
    // Load and verify
    let loaded = BenchmarkResults::load_from_file(&results_path).expect("Failed to load");
    assert!(loaded.throughput.is_some());
    assert!(loaded.latency.is_some());
    assert!(loaded.large_batch.is_some());
    assert!(loaded.concurrent.is_some());
    
    println!("✅ Complete benchmark persistence test passed");
    println!("📊 Results saved to: {:?}", results_path);
    println!("📈 History saved to: {:?}", history_path);
}

#[test]
fn test_load_empty_history() {
    let temp_dir = TempDir::new().unwrap();
    let history_path = temp_dir.path().join("nonexistent_history.jsonl");
    
    // Load from non-existent file
    let history = BenchmarkResults::load_history(&history_path).expect("Should return empty vec");
    
    assert_eq!(history.len(), 0, "Should have 0 entries for non-existent file");
    
    println!("✅ Empty history test passed");
}
