// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Network layer benchmarks.
//
// Benchmarks for:
// - DHT routing table operations
// - K-bucket lookups
// - Gossip fan-out
// - Transport latency

pub mod dht;
pub mod gossip;
pub mod transport;
