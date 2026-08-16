//! A3Net Chaos CLI
//!
//! Command-line interface for running chaos experiments.

use std::path::PathBuf;
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use futures::executor::block_on;

use a3net_chaos::{
    profiles::{self, ChaosProfile},
    runner::ChaosEngine,
    scenarios::{Scenario, Hypothesis},
    ChaosEventEmitter, ChaosEvent, ExperimentPhase, ExperimentResult,
    TracingEventEmitter,
};

#[derive(Parser)]
#[command(name = "a3net-chaos")]
#[command(about = "A3Net Chaos Engineering - Fault injection framework", long_about = None)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,

    /// Output format
    #[arg(short, long, default_value = "text")]
    format: OutputFormat,

    /// Output file for results
    #[arg(short, long)]
    output: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a predefined chaos profile
    Run {
        /// Profile name to run
        #[arg(value_enum)]
        profile: ChaosProfileName,

        /// Custom scenario file (JSON)
        #[arg(long)]
        scenario_file: Option<PathBuf>,

        /// Skip baseline measurement
        #[arg(long)]
        skip_baseline: bool,

        /// Experiment duration override
        #[arg(long, value_name = "SECONDS")]
        duration: Option<u64>,
    },

    /// List available profiles
    List,

    /// Validate a scenario file
    Validate {
        /// Scenario file to validate
        file: PathBuf,
    },

    /// Generate a sample scenario
    Generate {
        /// Name of scenario to generate
        #[arg(value_enum)]
        scenario: SampleScenario,

        /// Output file
        #[arg(short, short = 'o')]
        output: PathBuf,
    },
}

#[derive(ValueEnum, Clone)]
enum ChaosProfileName {
    NetworkResilience,
    DataConsistency,
    PerformanceDegradation,
    FaultTolerance,
    DisasterRecovery,
}

#[derive(ValueEnum, Clone)]
enum SampleScenario {
    NetworkPartition,
    NodeCrash,
    HighLatency,
    PacketLoss,
    DataCorruption,
    MemoryPressure,
    CascadingFailure,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
    JsonPretty,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set up logging
    if cli.verbose {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .init();
    }

    // Run command
    match cli.command {
        Command::Run { profile, scenario_file, skip_baseline, duration } => {
            run_experiment(profile, scenario_file, skip_baseline, duration, cli.format)?;
        }
        Command::List => {
            list_profiles()?;
        }
        Command::Validate { file } => {
            validate_scenario(&file)?;
        }
        Command::Generate { scenario, output } => {
            generate_scenario(scenario, &output)?;
        }
    }

    Ok(())
}

fn run_experiment(
    profile: ChaosProfileName,
    scenario_file: Option<PathBuf>,
    _skip_baseline: bool,
    duration: Option<u64>,
    format: OutputFormat,
) -> Result<()> {
    let engine = ChaosEngine::new();

    // Get profile scenarios
    let (scenarios, hypotheses) = match profile {
        ChaosProfileName::NetworkResilience => {
            let p = profiles::network_resilience_profile();
            (p.scenarios, p.hypotheses)
        }
        ChaosProfileName::DataConsistency => {
            let p = profiles::data_consistency_profile();
            (p.scenarios, p.hypotheses)
        }
        ChaosProfileName::PerformanceDegradation => {
            let p = profiles::performance_degradation_profile();
            (p.scenarios, p.hypotheses)
        }
        ChaosProfileName::FaultTolerance => {
            let p = profiles::fault_tolerance_profile();
            (p.scenarios, p.hypotheses)
        }
        ChaosProfileName::DisasterRecovery => {
            let p = profiles::disaster_recovery_profile();
            (p.scenarios, p.hypotheses)
        }
    };

    // Run each scenario
    for scenario in scenarios {
        let mut scenario = scenario;
        
        // Apply duration override
        if let Some(d) = duration {
            scenario.baseline_duration = std::time::Duration::from_secs(d / 4);
            scenario.recovery_duration = std::time::Duration::from_secs(d / 4);
        }

        println!("Running scenario: {}", scenario.name);
        
        let result = block_on(engine.run_with_hypotheses(scenario, hypotheses.clone()))?;
        
        print_result(&result, format);
    }

    Ok(())
}

fn list_profiles() -> Result<()> {
    println!("Available Chaos Profiles:\n");
    
    for profile in profiles::predefined_profiles() {
        println!("  {}: {}", profile.name, profile.description);
        println!("    Scenarios: {}", profile.scenarios.len());
        println!("    Hypotheses: {}", profile.hypotheses.len());
        println!("    Tags: {:?}", profile.tags);
        println!();
    }

    Ok(())
}

fn validate_scenario(file: &PathBuf) -> Result<()> {
    let content = std::fs::read_to_string(file)?;
    let scenario: Scenario = serde_json::from_str(&content)?;
    
    println!("Scenario '{}' is valid", scenario.name);
    println!("  Steps: {}", scenario.steps.len());
    println!("  Baseline: {:?}", scenario.baseline_duration);
    println!("  Recovery: {:?}", scenario.recovery_duration);
    
    Ok(())
}

fn generate_scenario(scenario: SampleScenario, output: &PathBuf) -> Result<()> {
    let s: Scenario = match scenario {
        SampleScenario::NetworkPartition => Scenario::network_partition(),
        SampleScenario::NodeCrash => Scenario::node_crash_recovery(),
        SampleScenario::HighLatency => Scenario::high_latency(),
        SampleScenario::PacketLoss => Scenario::packet_loss(),
        SampleScenario::DataCorruption => Scenario::data_corruption(),
        SampleScenario::MemoryPressure => Scenario::memory_pressure(),
        SampleScenario::CascadingFailure => Scenario::cascading_failure(),
    };

    let json = serde_json::to_string_pretty(&s)?;
    std::fs::write(output, json)?;

    println!("Generated scenario '{}' to {:?}", s.name, output);

    Ok(())
}

fn print_result(result: &ExperimentResult, format: OutputFormat) {
    match format {
        OutputFormat::Json | OutputFormat::JsonPretty => {
            let json = if format == OutputFormat::JsonPretty {
                serde_json::to_string_pretty(result)
            } else {
                serde_json::to_string(result)
            };
            println!("{}", json.unwrap());
        }
        OutputFormat::Text => {
            println!("\n=== Experiment Results ===");
            println!("Experiment ID: {}", result.experiment_id);
            println!("Status: {}", if result.success { "PASSED" } else { "FAILED" });
            println!("\nStatistics:");
            println!("  Duration: {} ms", result.stats.duration_ms);
            println!("  Faults injected: {}", result.stats.faults_injected);
            println!("  Baseline measurements: {}", result.stats.baseline_measurements);
            println!("  Experiment measurements: {}", result.stats.experiment_measurements);
            println!("  Hypothesis violations: {}", result.stats.hypothesis_violations);
            println!("  Success rate: {:.2}%", result.stats.success_rate() * 100.0);
            
            println!("\nHypotheses:");
            for hr in &result.hypothesis_results {
                let status = if hr.passed { "PASSED" } else { "FAILED" };
                println!("  [{}] {}", status, hr.hypothesis.description);
            }
        }
    }
}
