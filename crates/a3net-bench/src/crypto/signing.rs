// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Signing and verification benchmarks.

use a3net_types::{Announcement, CdnContentKind, ContentHash, NodeId};
use criterion::{BenchmarkId, Criterion, Throughput};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};

/// Generate a test announcement.
fn make_announcement() -> Announcement {
    let node_id = NodeId::random();
    Announcement {
        room_id: "bench-room".into(),
        content_hash: ContentHash::from_bytes(b"test-content"),
        node_id: node_id.clone(),
        title: "Test Announcement".into(),
        kind: CdnContentKind::Article,
        size_bytes: 1024,
        mime_type: None,
        source_url: None,
        ticket: None,
        timestamp: chrono::Utc::now(),
        message_id: None,
        ttl_secs: None,
        signer: None,
        signature: None,
    }
}

/// Benchmark Ed25519 signing.
pub fn bench_ed25519_sign(c: &mut Criterion) {
    let mut group = c.benchmark_group("crypto/ed25519_sign");

    // Generate a keypair
    let signing_key = SigningKey::generate(&mut rand::thread_rng());
    let verification_key = signing_key.verifying_key();
    let message = make_announcement();
    let message_bytes = serde_json::to_vec(&message).unwrap();

    group.throughput(Throughput::Bytes(message_bytes.len() as u64));
    group.bench_function("sign", |b| {
        b.iter(|| {
            let _signature: Signature = signing_key.sign(&message_bytes);
        });
    });

    // Pre-generate a signature for verification benchmark
    let signature = signing_key.sign(&message_bytes);

    group.bench_function("verify", |b| {
        b.iter(|| {
            let _ = verification_key.verify(&message_bytes, &signature);
        });
    });

    group.finish();
}

/// Benchmark batch verification.
pub fn bench_ed25519_batch_verify(c: &mut Criterion) {
    let mut group = c.benchmark_group("crypto/ed25519_batch_verify");

    // Generate multiple keypairs and signatures
    let count = 100;
    let message = make_announcement();
    let message_bytes = serde_json::to_vec(&message).unwrap();

    let keys: Vec<SigningKey> = (0..count)
        .map(|_| SigningKey::generate(&mut rand::thread_rng()))
        .collect();
    let signatures: Vec<Signature> = keys
        .iter()
        .map(|sk| sk.sign(&message_bytes))
        .collect();

    group.throughput(Throughput::Elements(count as u64));
    group.bench_function(BenchmarkId::from_parameter(count), |b| {
        b.iter(|| {
            // Batch verification (simulated - actual batch API is more complex)
            for (sk, sig) in keys.iter().zip(signatures.iter()) {
                let _ = sk.verifying_key().verify(&message_bytes, sig);
            }
        });
    });

    group.finish();
}

/// Benchmark announcement signing.
pub fn bench_announcement_sign(c: &mut Criterion) {
    let mut group = c.benchmark_group("crypto/announcement_sign");

    let announcement = make_announcement();
    let signing_key = SigningKey::generate(&mut rand::thread_rng());

    group.bench_function("full_pipeline", |b| {
        b.iter(|| {
            let mut ann = announcement.clone();
            let bytes = serde_json::to_vec(&ann).unwrap();
            let sig: Signature = signing_key.sign(&bytes);
            ann.signature = Some(sig.to_bytes().to_vec());
            ann
        });
    });

    group.finish();
}

/// Register all signing benchmarks.
pub fn register(c: &mut Criterion) {
    bench_ed25519_sign(c);
    bench_ed25519_batch_verify(c);
    bench_announcement_sign(c);
}
