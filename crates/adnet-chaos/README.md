# ADNet Chaos Engineering Framework

A comprehensive fault injection framework for testing the resilience of ADNet distributed systems.

## Overview

This framework provides tools for injecting various faults into ADNet to test system resilience, measure degradation under stress, and validate recovery mechanisms.

## Features

- **Fault Types**: Network, Node, Data, and System level faults
- **Scenarios**: Predefined fault injection sequences
- **Hypotheses**: Testable assertions about system behavior
- **Profiles**: Ready-to-use experiment configurations
- **Observability**: Integration with tracing and metrics

## Installation

```bash
cargo add adnet-chaos
```

## Quick Start

```rust
use adnet_chaos::{ChaosEngine, Scenario};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = ChaosEngine::new();
    
    // Run a predefined scenario
    let result = engine.run(Scenario::network_partition()).await?;
    
    println!("Experiment {} - {}", result.experiment_id, if result.success { "PASSED" } else { "FAILED" });
    
    Ok(())
}
```

## Fault Types

### Network Faults
- `PacketLoss`: Simulate packet loss (percentage 0-100)
- `Latency`: Add network latency (milliseconds)
- `Partition`: Isolate nodes from the network
- `BandwidthThrottle`: Limit bandwidth (percentage)
- `DnsFailure`: Simulate DNS resolution failures
- `ConnectionTimeout`: Simulate connection timeouts

### Node Faults
- `Crash`: Instant node termination
- `Suspend`: Pause node for duration
- `CpuStress`: Apply CPU stress (percentage)
- `MemoryStress`: Apply memory pressure (percentage)
- `ProcessKill`: Kill a process
- `Restart`: Restart with delay

### Data Faults
- `Corruption`: Corrupt data (percentage)
- `Loss`: Simulate data loss
- `Duplication`: Inject duplicate data
- `Replay`: Replay old data
- `Inconsistency`: Inject inconsistent state

## Predefined Scenarios

```rust
use adnet_chaos::Scenario;

// Network resilience
Scenario::network_partition()
Scenario::high_latency()
Scenario::packet_loss()

// Node resilience  
Scenario::node_crash_recovery()
Scenario::memory_pressure()

// Data integrity
Scenario::data_corruption()

// Cascading failures
Scenario::cascading_failure()
```

## Hypothesis Testing

Define testable hypotheses to verify system behavior:

```rust
use adnet_chaos::{ChaosEngine, Scenario, Hypothesis};

let engine = ChaosEngine::new();

let hypotheses = vec![
    Hypothesis::availability_at_least_99(),
    Hypothesis::latency_below_500ms(),
    Hypothesis::error_rate_below_1_percent(),
];

let result = engine.run_with_hypotheses(scenario, hypotheses).await?;
```

## CLI Usage

```bash
# List available profiles
cargo run -p adnet-chaos --example chaos_cli -- list

# Run a chaos profile
cargo run -p adnet-chaos --example chaos_cli -- run network-resilience

# Generate a sample scenario
cargo run -p adnet-chaos --example chaos_cli -- generate network-partition -o scenario.json

# Validate a scenario file
cargo run -p adnet-chaos --example chaos_cli -- validate scenario.json
```

## Experiment Phases

1. **Baseline**: Measure system behavior under normal conditions
2. **FaultInjection**: Inject faults and observe behavior
3. **Observation**: Collect post-fault data
4. **Recovery**: Wait for system recovery
5. **Completed/Failed**: Final state

## Integrating with Your Application

```rust
use adnet_chaos::{ChaosEngine, ChaosEventEmitter, ChaosEvent};

struct MyEventEmitter {
    // Your metrics collector
}

#[async_trait::async_trait]
impl ChaosEventEmitter for MyEventEmitter {
    async fn emit(&self, event: ChaosEvent) {
        match event {
            ChaosEvent::HypothesisViolated { hypothesis, measurement, threshold } => {
                // Alert your monitoring system
                println!("ALERT: {} violated ({} > {})", hypothesis, measurement, threshold);
            }
            _ => {}
        }
    }
}

let emitter = MyEventEmitter { /* ... */ };
let engine = ChaosEngine::with_emitter(emitter);
```

## Feature Flags

```toml
[dependencies]
adnet-chaos = { version = "0.1", features = ["full"] }

# Or specific features
adnet-chaos = { version = "0.1", features = ["network", "node"] }
```

Available features:
- `network`: Network fault injection
- `node`: Node fault injection
- `data`: Data fault injection
- `full`: All features
- `simulator`: Integration with adnet-simulator

## License

MIT OR Apache-2.0
