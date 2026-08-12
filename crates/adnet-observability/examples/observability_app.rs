//! Realistic example: a small "service" emits per-topic / per-crate metrics;
//! the exporter is then rendered the same way the gateway's `/metrics`
//! endpoint would.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-observability --example observability_app
//! ```

use adnet_observability::histogram::Histogram;
use adnet_observability::prometheus::PrometheusExporter;
use adnet_observability::registry::Registry;

fn main() {
    let registry = Registry::default();

    // A handful of metrics that a real ADNet service would expose:
    let blob_puts = registry.register_counter(
        "adnet_blob_puts_total",
        "Total blob inserts accepted by the blobstore",
    );
    let blob_gets = registry.register_counter(
        "adnet_blob_gets_total",
        "Total blob fetches served",
    );
    let dlq = registry.register_counter(
        "adnet_blob_dlq_total",
        "Blobs that failed verification and were routed to the dead-letter queue",
    );
    let open_connections = registry.register_gauge(
        "adnet_active_connections",
        "Currently open inbound connections",
    );
    let handshake_latency = Histogram::new(
        "adnet_handshake_seconds",
        "Time from TCP accept to first application frame",
    );

    // Simulate a small workload.
    for i in 0..50 {
        blob_puts.inc();
        if i % 4 == 0 {
            blob_gets.inc();
        }
        if i % 17 == 0 {
            dlq.inc();
        }
        open_connections.set(((i * 13) % 200) as i64);
        let jitter = (i as f64) * 0.003 + 0.001;
        handshake_latency.observe(jitter);
    }

    // Print the snapshot first, then the Prometheus text. The text
    // form is what a Prometheus / VictoriaMetrics / Grafana Agent
    // scraper would consume.
    let exporter = PrometheusExporter::new(&registry);
    let out = exporter.render();
    println!("--- Prometheus text format ---\n{}", out.text());

    // Make sure the snapshot round-trip is sane.
    let snap = registry.snapshot();
    println!("metrics registered: {}", snap.len());
    for m in snap.sorted() {
        println!("  - {} ({})", m.name(), m.kind().as_prometheus_str());
    }
}
