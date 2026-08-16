//! Pre-flight check demo
//!
//! This example demonstrates the pre-flight device check functionality.

use a3net_video::preflight::{run_preflight_checks, run_interactive_preflight, PreFlightChecker};

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║           Pre-Flight Check Demo                            ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    // Option 1: Quick check (returns bool)
    println!("=== Quick Check ===");
    let ready = run_preflight_checks();
    println!();

    if ready {
        println!("✅ Quick check passed!");
    } else {
        println!("❌ Quick check failed!");
    }

    println!();

    // Option 2: Interactive check with user prompts
    println!("=== Interactive Check ===");
    let ready = run_interactive_preflight();
    println!();

    if ready {
        println!("✅ System is ready for video conference!");
    } else {
        println!("❌ System is not ready. Please resolve the issues above.");
    }
}
