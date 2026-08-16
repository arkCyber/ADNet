//! Real-world example — programmatically build a chaos scenario
//! with custom `FaultConfig` + `Hypothesis` objects, run it through
//! `ChaosEngine::run_with_hypotheses`, and inspect the
//! `ExperimentResult` exactly like a production resilience job
//! would. Mirrors the workflow `chaos_cli` exposes via the
//! `run <profile>` subcommand, but in-process so it can be embedded
//! in a long-running test harness or a release-blocking CI step.
//!
//! Run with:
//!   cargo run -p a3net-chaos --example chaos_app --features full --release

use std::time::Duration;

use a3net_chaos::{
    ChaosEngine, ChaosError, ExperimentPhase, ExperimentResult, Hypothesis,
    scenarios::{HypothesisType, Scenario, ScenarioStep},
    faults::{
        DataFaultType, FaultConfig, FaultParameters, FaultTarget, FaultType,
        NetworkFaultType,
    },
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ChaosError> {
    // ─── Build a custom scenario ─────────────────────────────────
    // We start from the canned `packet_loss` scenario as a base,
    // then tighten the durations so the example runs in seconds
    // rather than minutes. Operators running the same code against
    // a real cluster would keep the defaults (60s baseline +
    // 60s step + 30s recovery) and just swap the engine for one
    // wired into a real `FaultInjector`.
    let scenario = Scenario::packet_loss()
        .with_baseline(Duration::from_secs(1))
        .with_recovery(Duration::from_secs(1))
        .with_description("a3net-chaos example: short packet-loss run")
        .with_tags(vec!["example".into(), "ci-fast".into()]);

    // Override the first step's wait so the engine doesn't block
    // for a full minute.
    let scenario = override_step_wait(scenario, Duration::from_secs(1));

    // ─── Build the hypothesis set ────────────────────────────────
    // We mirror the canned checks but with thresholds that match a
    // fast smoke-test: latency under 1s, error rate under 5%,
    // availability above 95%.
    let hypotheses = vec![
        Hypothesis::new(
            "Latency under 1s during fault",
            HypothesisType::LatencyBound,
            1000.0,
        ),
        Hypothesis::new(
            "Error rate under 5% during fault",
            HypothesisType::ErrorRateBound,
            0.05,
        ),
        Hypothesis::new(
            "Availability above 95% during fault",
            HypothesisType::Availability,
            0.95,
        ),
    ];

    // ─── Run the experiment ──────────────────────────────────────
    let engine = ChaosEngine::new();
    println!(
        "starting experiment for scenario `{}` (steps = {})",
        scenario.name,
        scenario.steps.len()
    );

    let result = engine.run_with_hypotheses(scenario, hypotheses).await?;
    print_result(&result);

    Ok(())
}

fn override_step_wait(mut scenario: Scenario, wait: Duration) -> Scenario {
    for step in &mut scenario.steps {
        step.wait_duration = wait;
    }
    scenario
}

fn print_result(result: &ExperimentResult) {
    println!();
    println!("=== Experiment {} ===", result.experiment_id);
    println!(
        "outcome    : {}",
        if result.success { "PASSED" } else { "FAILED" }
    );
    println!(
        "duration   : {} ms",
        result.stats.duration_ms
    );
    println!(
        "faults     : injected={} recovered={}",
        result.stats.faults_injected, result.stats.faults_recovered
    );
    println!(
        "samples    : baseline={} experiment={}",
        result.stats.baseline_measurements, result.stats.experiment_measurements
    );
    println!(
        "violations : {}",
        result.stats.hypothesis_violations
    );

    println!();
    println!("hypotheses:");
    for hr in &result.hypothesis_results {
        let last = hr
            .measurements
            .last()
            .copied()
            .map(|v| format!("{v}"))
            .unwrap_or_else(|| "n/a".into());
        println!(
            "  [{}] {} (last = {}, samples = {})",
            if hr.passed { "PASS" } else { "FAIL" },
            hr.hypothesis.description,
            last,
            hr.measurements.len()
        );
    }

    // Sanity check the phase enum is wired up.
    let _ = ExperimentPhase::Completed;
    let _ = FaultType::DataFault(DataFaultType::Corruption);
    let _ = FaultType::NetworkFault(NetworkFaultType::PacketLoss);
    let _ = FaultTarget::all_nodes();
    let _ = FaultConfig::new(
        FaultType::DataFault(DataFaultType::Corruption),
        FaultTarget::all_nodes(),
    )
    .with_parameters(FaultParameters::new().with_corruption(5.0));
    let _ = ScenarioStep::wait("post-fault observation", Duration::from_secs(5));
}
