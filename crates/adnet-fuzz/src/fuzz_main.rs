// SPDX-License-Identifier: MIT OR Apache-2.0
//
// adnet-fuzz — Fuzzing infrastructure for ADNet.
//
// This crate uses `cargo-fuzz` for coverage-guided fuzzing.
//
// ## Running Fuzz Tests
//
// 1. Install cargo-fuzz:
//    cargo install cargo-fuzz
//
// 2. List fuzz targets:
//    cargo fuzz list
//
// 3. Run a specific fuzz target:
//    cargo fuzz run parse_announcement
//
// 4. Run with corpus:
//    cargo fuzz run parse_announcement fuzz_corpus/
//
// ## Fuzz Targets
//
// - `parse_announcement`: Fuzz Announcement deserialization
// - `parse_cid`: Fuzz CID parsing and validation
// - `parse_node_id`: Fuzz NodeId parsing
// - `parse_dht_message`: Fuzz DHT wire protocol messages
// - `parse_gossip_message`: Fuzz gossip protocol messages

#![no_main]
