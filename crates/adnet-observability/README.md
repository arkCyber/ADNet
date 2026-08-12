# `adnet-observability`

> Lightweight metrics, histogram, health-check, and Prometheus
> formatter for the ADNet runtime. A no-`prometheus`-dep,
> in-process implementation that the rest of the workspace
> (`adnet-transport`, `adnet-relay`, `adnet-mesh-firewall`,
> …) hooks into without pulling in OpenTelemetry.

## Modules

| module      | purpose                                                       |
|-------------|---------------------------------------------------------------|
| `metrics`   | typed `Counter`, `Gauge`, `Histogram` (lock-free via atomics) |
| `registry`  | process-global `Registry` + `Snapshot` for tests/REPL         |
| `histogram` | HDR-ish buckets (`exponential_buckets`)                       |
| `labels`    | `LabelSet` + `LabelKey` (fixed-shape label contract)          |
| `prometheus`| scrape-format exporter                                        |
| `http`      | optional `/metrics` + `/health` HTTP handler                  |
| `http_health` | HTTP-only health-check serving                             |
| `health`    | `HealthCheck` trait + global registry + `run_checks` runner   |
| `bridge`    | glue to `adnet-transport`'s metrics surface                   |
| `labels`    | shared label vocab                                            |

## Quick start

```rust
use adnet_observability::{Counter, Histogram, Registry};

let registry = Registry::default();
let requests = Counter::new("requests_total");
let latency = Histogram::exponential("request_latency_ms", 10.0, 10);
registry.register("requests_total", requests.clone());
registry.register("request_latency_ms", latency.clone());

requests.inc();
latency.observe(42.0);

let snapshot = registry.snapshot();
println!("{snapshot:#?}");
```

## Testing

```bash
cargo test -p adnet-observability   # 54 tests
```

## License

Same as the workspace root.