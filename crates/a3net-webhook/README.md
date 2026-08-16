# `a3net-webhook`

Fan A3Net events out to HTTP endpoints with HMAC-SHA256 signing,
configurable retry, and an optional disk-backed spool.

## What ships in this crate

| Item | Purpose |
|---|---|
| `EventSink` trait | Pluggable delivery surface (HTTP sink ships by default; in-memory / Kafka consumers can implement this trait). |
| `WebhookSink` | Default `EventSink` implementation. POSTs JSON to a configured URL list with HMAC headers, status classification (`2xx` accept / `4xx` permanent reject / `5xx` + network retry), an atomic on-disk spool, and a bounded retry budget. |
| `DroppedHandler` trait | Plug-in observer for deliveries that exhausted their retry budget. Production code wires this into metrics / alerting. |
| `AdnetEvent` enum | Tagged-union event payload (today: `announcement`). New variants add without breaking the wire shape. |
| `EndpointConfig` | Per-endpoint URL + HMAC secret + optional room filter + per-attempt timeout. |
| `DeliveryStats` | Aggregate counters surfaced by `WebhookSink::stats()` (accepted / rejected / failed / dropped). |
| `load_endpoints` / `save_endpoints` / `from_config_file` | File-based config helpers used by `a3net webhook load/save/list` CLI subcommands. |
| `pump` module | Long-running task that bridges a `broadcast::Receiver<Announcement>` (the same shape `a3net_gossip::GossipBus::subscribe` returns) into the sink. |

## Wire format

```
POST /hook HTTP/1.1
Host: receiver.example.com
Content-Type: application/json
X-Adnet-Delivery: <uuid>
X-Adnet-Signature: sha256=<hex>
X-Adnet-Timestamp: <unix-ms>

{ "event": "announcement", "room_id": "lobby", "node_id": "...", … }
```

The signature is `HMAC-SHA256(secret, body)` rendered as hex with a
`sha256=` prefix. Receivers must recompute the HMAC with their
copy of the secret and reject on mismatch.

## Status classification

| Receiver response | Treated as | Action |
|---|---|---|
| `200..299` | Accepted | Drop the spool entry. |
| `400..499` | Rejected | Permanent — log and drop. Do not retry. |
| `500..599` | Failed | Persist to the spool for retry on the next `deliver()`. |
| Network error / timeout | Failed | Same as `5xx`. |

`X-Adnet-Timestamp` lets receivers cap a replay window
(default guidance: 5 minutes).

## Quick start

```rust,no_run
use a3net_webhook::{EndpointConfig, WebhookSink, AdnetEvent, EventSink};
use std::time::Duration;

let cfg = vec![EndpointConfig {
    url: "http://127.0.0.1:8080/hook".into(),
    secret: b"topsecret".to_vec(),
    room_filter: None,
    request_timeout: Duration::from_secs(5),
}];
let sink = WebhookSink::new(cfg);

let event = AdnetEvent::Announcement {
    payload: serde_json::json!({
        "room_id": "lobby",
        "node_id": "abc",
        "title": "hello",
    }),
};
sink.deliver(&event, "delivery-id-1").await?;
```

`deliver()` returns `Err(WebhookError::Transport(_))` when **every**
attempted endpoint failed. A single endpoint that accepts (or rejects
with `4xx`) is enough for `deliver()` to return `Ok`. The error is
intentionally coarse — the per-endpoint outcomes are still surfaced
via `WebhookSink::stats()`.

## Persistent retry spool

```rust,no_run
use a3net_webhook::{EndpointConfig, WebhookSink};
# use std::time::Duration;
# let cfg = vec![EndpointConfig { url: "http://127.0.0.1:8080".into(), secret: vec![], room_filter: None, request_timeout: Duration::from_secs(5) }];
let sink = WebhookSink::with_spool(cfg, "/var/lib/a3net/webhook-spool.jsonl".into())?;
```

The spool is a JSONL file: one row per pending delivery. Two
robustness properties matter:

- **Append-only** for new pushes (one JSONL row per `push`) — O(1)
  per failed delivery, no whole-file rewrite on the hot path.
- **Atomic rebuild** when the spool is drained
  (`tmp` + `rename`) — a crash mid-rewrite leaves the previous file
  untouched.

On restart, `WebhookSink::with_spool` re-reads the JSONL file so a
crash does not lose in-flight events.

## Retry budget

Every push records `attempts = 1`. Every subsequent `deliver()`
re-attempts every spool entry whose endpoint is still configured:

- on success → drop the entry;
- on `4xx` → drop the entry, do **not** retry (permanent reject);
- on `5xx` / network / timeout → increment `attempts`;
- when `attempts > max_attempts` → drop the entry and invoke the
  registered `DroppedHandler`.

`DEFAULT_MAX_ATTEMPTS = 5`. Use `WebhookSink::with_spool_and_budget`
to override the budget per sink.

```rust,no_run
use std::sync::Arc;
use a3net_webhook::{DroppedHandler, EndpointConfig, WebhookSink};
# use std::time::Duration;
# let cfg = vec![EndpointConfig { url: "http://127.0.0.1:8080".into(), secret: vec![], room_filter: None, request_timeout: Duration::from_secs(5) }];

struct Metrics;
#[async_trait::async_trait]
impl DroppedHandler for Metrics {
    async fn on_dropped(&self, _event: &a3net_webhook::AdnetEvent,
                        ep: &EndpointConfig, id: &str, attempts: u32) {
        prometheus::counter!("webhook_dropped_total",
            "url" => ep.url.clone(), "attempts" => attempts.to_string()).inc();
    }
}

let sink = WebhookSink::with_spool_and_budget(cfg,
    "/var/lib/a3net/webhook-spool.jsonl".into(), 5)?;
sink.set_dropped_handler(Some(Arc::new(Metrics)));
```

## Stats

```rust,no_run
# use a3net_webhook::{EndpointConfig, WebhookSink};
# use std::time::Duration;
# let cfg = vec![EndpointConfig { url: "http://127.0.0.1:8080".into(), secret: vec![], room_filter: None, request_timeout: Duration::from_secs(5) }];
# let sink = WebhookSink::new(cfg);
let s = sink.stats();
println!("accepted={} rejected={} failed={} dropped={}",
    s.accepted, s.rejected, s.failed, s.dropped);
```

## Wiring to the gossip bus

`a3net-webhook` ships a `pump` module that bridges the gossip
event stream to the HTTP sink. Wire it up after
`a3net_gossip::GossipBus::subscribe`:

```rust,ignore
use std::sync::Arc;
use a3net_webhook::{pump, WebhookSink};

let sink = Arc::new(WebhookSink::new(endpoints));
let rx = gossip_bus.subscribe(&room_id);
let handle = pump::run(sink, rx);
// … handle serves until the upstream `gossip_bus` drops; abort
// with `handle.abort()` to stop immediately.
```

The pump:

- forwards every `Announcement` via
  `WebhookSink::deliver_announcement`, which uses the
  announcement's `message_id` as the delivery id (falling back
  to the content hash hex when no message id is set);
- on `RecvError::Lagged(n)`, emits a `warn!` log and continues
  with the next in-flight message (no event loss beyond the
  broadcast buffer's depth);
- on `RecvError::Closed`, exits cleanly with the number of
  events it attempted to deliver.

The `WebhookSink::deliver_announcement` method is also exported
on its own for callers that already have an `Announcement` value
in hand.

## Scope (this PR)

- HTTP only (`http://`). HTTPS is a TODO — the in-tree HTTP/1.1
  client deliberately avoids pulling a TLS stack into the
  default build.
- Response reads are bounded by `MAX_RESPONSE_BYTES = 64 KiB`
  so a malicious receiver cannot pin the sink's memory.
- The retry scheduler is "best-effort on next delivery": every
  `deliver()` drains the spool once. A future PR may wire a
  dedicated background task with exponential backoff
  (`1s, 2s, 4s, …` capped at `MAX_BACKOFF`); the constant is
  already published so callers can build their own schedule.

## Tests

```
cargo test -p a3net-webhook
```

37 tests covering:

- HMAC signature determinism and per-endpoint secret isolation;
- `EndpointConfig` JSON round-trip + `request_timeout` serde default;
- `save_endpoints` / `load_endpoints` happy path + parent-dir creation
  + malformed-JSON error path;
- `from_config_file` with and without a spool path;
- `parse_status` classification (2xx / 4xx / 5xx / garbage);
- end-to-end HMAC verification against a real local TCP listener;
- 2xx / 4xx / 5xx → matching `DeliveryStats` counters;
- room filter skipping non-matching events;
- pump happy path / clean shutdown / lag resilience / end-to-end
  signature verification;
- spool append-only writes + on-restart reload + atomic rebuild;
- `on_dropped` firing after the retry budget is exhausted;
- oversized-response cap (no hang on a streaming attacker).

The `webhook_pump_smoke` example runs the full broadcast → pump →
HTTP → HMAC-verified round trip on a fake gossip channel.
