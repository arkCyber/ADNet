// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Main benchmark runner for ADNet.
//
// Usage:
//     cargo bench -p adnet-bench
//     cargo bench -p adnet-bench -- gossip
//     cargo bench -p adnet-bench -- --html

use adnet_bench::{
    bao_large,
    crypto::{hashing, key_derivation, signing},
    network::{dht, gossip, transport},
    storage::{bao, blob},
};

fn main() {
    // Build Criterion with custom config
    let mut criterion = criterion::Criterion::default()
        .sample_size(100)
        .warm_up_time(std::time::Duration::from_secs(3))
        .measurement_time(std::time::Duration::from_secs(10))
        .noise_threshold(0.02);

    // Register benchmarks by group
    eprintln!("Registering DHT benchmarks...");
    dht::register(&mut criterion);

    eprintln!("Registering Gossip benchmarks...");
    gossip::register(&mut criterion);

    eprintln!("Registering Transport benchmarks...");
    transport::register(&mut criterion);

    eprintln!("Registering Blob storage benchmarks...");
    blob::register(&mut criterion);

    eprintln!("Registering BAO benchmarks...");
    bao::register(&mut criterion);

    eprintln!("Registering Hashing benchmarks...");
    hashing::register(&mut criterion);

    eprintln!("Registering Signing benchmarks...");
    signing::register(&mut criterion);

    eprintln!("Registering Key derivation benchmarks...");
    key_derivation::register(&mut criterion);

    eprintln!("Registering Bao large benchmarks (P0 anchor)...");
    bao_large::register(&mut criterion);

    // Run all benchmarks
    criterion.final_summary();
}
