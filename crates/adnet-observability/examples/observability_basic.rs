//! Tiny example: stand up a `Registry`, register a Counter + Gauge + Histogram,
//! exercise them, and render the Prometheus text output.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-observability --example observability_basic
//! ```

use adnet_observability::histogram::Histogram;
use adnet_observability::prometheus::PrometheusExporter;
use adnet_observability::registry::Registry;

fn main() {
    let registry = Registry::default();

    // 1. Counter — monotonic request count.
    let requests = registry.register_counter(
        "adnet_demo_requests_total",
        "Total demo requests received",
    );
    requests.inc();
    requests.inc_by(2);

    // 2. Gauge — a value that goes up and down (e.g. active connections).
    let active = registry.register_gauge(
        "adnet_demo_active_connections",
        "Number of in-flight demo connections",
    );
    active.set(7);
    active.dec();

    // 3. Histogram — observed latency. Uses the default bucket layout
    //    (1ms, 10ms, 100ms, 500ms, 1s, 5s, 10s, +Inf).
    let latency = Histogram::new("adnet_demo_latency_seconds", "request latency");
    latency.observe(0.05);
    latency.observe(0.5);
    latency.observe(2.0);

    // 4. Render the Prometheus text format.
    let exporter = PrometheusExporter::new(&registry);
    let out = exporter.render();
    println!("{}", out.text());
    println!("content-type: {}", out.content_type);

    // 5. The metrics also implement `Metric` directly so the registry
    //    can iterate over them.
    let snap = registry.snapshot();
    println!("\n{} metrics registered", snap.len());
    for m in snap.sorted() {
        println!("  - {} ({})", m.name(), m.kind().as_prometheus_str());
    }
}
