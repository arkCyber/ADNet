# ADNet WebRTC / WebTransport — 4-Week Plan (Aug 12, 2026)

> **Status:** Design proposal. Subject to sign-off before code lands.
> **Scope:** Symmetric WebRTC (native ↔ native, native ↔ browser) + WebTransport (browser-first, native fallback). webrtc-rs for RTC, wtransport for WebTransport. Integrated behind the existing `adnet-transport::Transport` trait.

---

## 0. Goals & non-goals

### Goals

1. **Browser nodes.** A browser running JS can join the ADNet mesh, send/receive frames, gossip, fetch blobs.
2. **Native symmetric.** A pure-Rust ADNet node can speak WebRTC and WebTransport to (a) a browser, (b) another native node behind hostile NATs where direct QUIC fails.
3. **Trait parity.** New backends implement the existing `adnet_transport::Transport` / `OutgoingConnection` / `Frame` triad so node, gossip, blobstore code does not change.
4. **Tests.** Integration tests cover handshake, frame round-trip, end-to-end via DHT/gossip. CI builds and runs.

### Non-goals (this milestone)

- TURN / ICE-only operation without a signaling server. Signaling is mandatory for now (we already have `pkarr` / DERP discovery to anchor it).
- AV / media pipeline. WebRTC is **transport only** (DataChannel / SCTP-encoded streams, media over WebTransport streams). No SFU/MCU.
- WASM build of the full node. The browser side uses the existing `adnet-ffi` JS bindings for the heavy lifting; we don't try to compile the full node to wasm.

---

## 1. Architecture

```
                           signaling  (existing pkarr / DHT)
                                  │
                                  ▼
        ┌──────────────────────────────────────────────────┐
        │ adnet-webrtc        ── webrtc-rs SDP/ICE        │
        │   ├─ rtc_engine     (peer connection lifecycle)  │
        │   ├─ dc_session     (DataChannel framing)        │
        │   └─ noise_dc       (Noise_XX over DC bytes)     │
        ├──────────────────────────────────────────────────┤
        │ adnet-webtransport  ── wtransport (HTTP/3)       │
        │   ├─ wt_server      (binds cert + connect-token) │
        │   └─ wt_client      (browser-fetchable endpoint) │
        ├──────────────────────────────────────────────────┤
        │ adnet-transport::webrtc (new)                    │
        │   impl Transport for WebRtcTransport             │
        │   impl Transport for WebTransportAdapter         │
        └──────────────────────────────────────────────────┘
```

### 1.1 Why two crates

- `adnet-webrtc` holds the **protocol-agnostic** parts (Noise handshake, frame codec, NodeId↔fingerprint binding). It depends only on `webrtc-rs` and `adnet-identity`. It is reusable for non-DCT transports (e.g. raw audio channel if we ever care).
- `adnet-webtransport` holds the **HTTP/3 server + client**. It depends on `wtransport`, `adnet-webrtc` (for shared handshake code) and `adnet-types`.
- `adnet-transport` gets a new `webrtc` and `webtransport` module that wraps the above into the `Transport` trait. Default build stays zero-cost; opt-in via `cargo build --features webrtc,webtransport`.

### 1.2 Handshake

`Noise_XX` over the WebRTC DataChannel ordered+reliable subprotocol, on the same channel we'll carry application frames. Sequence:

1. WebRTC PeerConnection established via SDP exchange through existing signaling (pkarr or DERP relay). Reuses the existing `adnet-relay::derp` SDP-publish channel.
2. Single DataChannel `"adnet/0"` opened (we explicitly pick the channel label to avoid collision with any future use).
3. **Noise XX** 3-message handshake (e, ee, s, es, se, e, ee) using the same `snow` patterns already present in the workspace (used by DHT).
4. After the handshake, the remote static key is bound to the NodeId (first 32 bytes of BLAKE3 static key). From this point, the channel is "authenticated", and the regular `Frame` codec takes over.
5. New streams multiplexed by **stream-id SCTP-over-DC** (one DC per stream, up to N=16 concurrent before recycling). Same wire format as `Frame` — no extra framing.

### 1.3 WebTransport specifics

WebTransport is browser-friendly (Chrome/Firefox/Safari all support it). It gives us:

- HTTP/3 + QUIC under the hood (we reuse the cert chain; ECH optionally).
- True multiple streams (vs DC's 16 concurrent soft cap).
- Cleaner integration with `fetch`-style APIs in the browser.

The handshake re-uses `adnet-webrtc::noise_dc` verbatim — we treat the WebTransport bidirectional stream as a "Noise channel". The transport binds on a port using `wtransport::Server`, gets a connect URL (`https://host:port/adnet`), and exposes `POST /ticket` to mint a short-lived connect-token. Browsers then do `new WebTransport(url)` and proceed with Noise.

### 1.4 Integration with adnet-node

`adnet-node` constructs a `MultiTransport`:

```rust
let native = Arc::new(QuicTransport::new(cfg.clone()));
let webrtc = Arc::new(WebRtcTransport::new(cfg.clone()));
let webt   = Arc::new(WebTransportAdapter::new(cfg.clone()));

let multi = MultiTransport::new(vec![native, webrtc, webt]);
node.with_transport(multi).run().await?;
```

`MultiTransport` dials all in parallel, takes first success; accepts from all. This is a thin utility in `adnet-transport`, not a hard dep on the node.

---

## 2. Schedule

### Week 1 — Audit, scaffold, protocol primitives

| Day | Deliverable |
|-----|-------------|
| 1–2 | **AUDIT_WEBRTC_WEBTRANSPORT.md** — code review of `adnet-transport::traits`, `frame`, `quic`, `iroh`, `relay_fallback`. Document exactly which trait methods must be implemented, which already-pass `Frame`s work over the new backend. Identify hand-off points for the FFI/JS side. |
| 3   | Add `crates/adnet-webrtc/` and `crates/adnet-webtransport/` to workspace `Cargo.toml`. Stub `lib.rs`, `Cargo.toml` with deps `webrtc-rs = "0.13"`, `wtransport = "0.5"`, `snow = "0.9"`, `tokio`, `adnet-types`, `adnet-identity`. |
| 4   | `adnet-webrtc::noise_dc` — port the existing DHT Noise XX code into a self-contained module. Unit tests for handshake success/failure/mismatch. |
| 5   | `adnet-webrtc::frame_codec` — wire `Frame` ↔ DC bytes. Stream-id multiplexing. Tests for short frames, max-size frames, fragment-reassembly off (we lean on SCTP). |

### Week 2 — WebRTC working slice

| Day | Deliverable |
|-----|-------------|
| 1   | `adnet-webrtc::rtc_engine` — wraps `webrtc::peer_connection::RTCPeerConnection`. SDP offer/answer via callback, ICE candidate trickle, deterministic fingerprint derivation. |
| 2   | `adnet-webrtc::dc_session` — owns the ordered DC, runs the Noise handshake, hands off to `frame_codec`. |
| 3   | `adnet-transport::webrtc::WebRtcTransport` impl `Transport` trait. Dial / Accept / `local_node` / `transport_name = "webrtc"`. Maps `ConnectionType::Relay` (because DC is essentially a TURN-style relayed path). |
| 4   | `WebRtcOutgoing` impl `OutgoingConnection`. Streams `Frame` over DC via the noise channel. Honors `set_priority` (DC priority map). |
| 5   | Integration test: two native nodes, one is "dialer", one is "answerer". Publish offer via a tiny in-memory signaling channel (so the test doesn't need network). Verify 10 round-trip frames including max-size. |

### Week 3 — WebTransport + browser

| Day | Deliverable |
|-----|-------------|
| 1   | `adnet-webtransport::wt_server` — `wtransport::Server` with self-signed cert (rcgen already in deps), `/adnet` endpoint, connect-token middleware. |
| 2   | `adnet-webtransport::wt_client` — connects, performs Noise over the first bidi stream, hands off. |
| 3   | `adnet-transport::webtransport::WebTransportAdapter` impl `Transport`. Same trait surface as WebRTC. |
| 4   | `crates/adnet-ffi/js/` — TypeScript glue: `connectWebRTC(offerSdp, signalingUrl)` and `connectWebTransport(url, connectToken)`. Runs Noise via `libsodium-wrappers` (matches the Rust side's primitives). |
| 5   | Demo HTML page (`examples/browser_demo.html`) — connect, ping, list 5 peers, fetch one blob. |

### Week 4 — Tests, CI, polish

| Day | Deliverable |
|-----|-------------|
| 1   | Property tests (`proptest`) for frame codec, Noise state machine, stream-id wraparound. |
| 2   | Chaos: simulate lossy DC, ordered-then-lossy, reordering. Use `adnet-chaos` profiles. |
| 3   | Bench harness in `adnet-bench` for both backends (handshake latency, frame RTT, throughput with 64KB frames). |
| 4   | CI updates: `cargo build -p adnet-webrtc -p adnet-webtransport --all-features`, `cargo test --workspace --features webrtc,webtransport`, `cargo clippy -- -D warnings`. WASM job for the JS glue compiles via `wasm-pack` smoke. |
| 5   | Audit doc updates: `AUDIT_WEBRTC_WEBTRANSPORT.md`, `AUDIT_WEBRTC_WEBTRANSPORT_ROUND_2.md`. README + SAFETY_CASE delta. |

---

## 3. Risk register

| Risk | Mitigation |
|------|------------|
| `webrtc-rs` API churn. 0.13 in late 2025, may have moved by now. | Pin to 0.13.0 exactly. Adapter layer (`rtc_engine`) isolates the rest of the crate. |
| `wtransport` requires OpenSSL on some platforms. | Already have `rustls` + `quinn` in tree. Use `rustls` build of wtransport if available; otherwise feature-gate `webtransport-rustls`. |
| WebRTC DC is message-oriented up to ~256 KiB. Larger blocks need SCTP fragmentation, but SCTP is internally limited to ~16 KiB. | Cap `max_datagram_size` to 16 KiB; for larger blobs, add `chunked` stream mode (already partly specced in `adnet-blobstore`). |
| TURN not available in CI. | Tests use in-process signaling (no TURN/ICE). Real network test is a stretch goal in Week 4 — only if we already have a TURN instance we can borrow. |
| Browser side cryptography drift. | Pin the JS-side `libsodium-wrappers` to a specific version and emit a known-answer test vector that the Rust side re-validates in CI. |

---

## 4. Open questions

1. **Channel-binding for Noise.** libp2p binds Noise to the TLS exporter. WebRTC DataChannel doesn't expose one cleanly. Do we bind to (a) the SDP fingerprint hash, (b) a fresh DH we run over the DC before Noise, or (c) accept the absence of exporter binding because WebRTC itself authenticates the DTLS session?
   - **Recommendation (c) for v1**, document it. Optional v2: bind to SDP fingerprint hash (already deterministic via BLAKE3 over the offer/answer bytes).

2. **How much WebTransport do we actually need if WebRTC DataChannel works for browsers?**
   - WebTransport is strictly better (multistream, no 16-channel cap, fetch-style). But it's a new dep and another surface to test. If we hit time pressure in Week 3, we ship WebRTC only and document WebTransport as "phase 2".

3. **Should the browser-side ever act as a signaling relay for native nodes?**
   - Useful for low-power devices behind symmetric NAT. But it's a much bigger surface (TURN-equivalent). Punt to a future audit.

---

## 5. Definition of done

- [ ] `cargo build --workspace --features webrtc,webtransport` succeeds on macOS + Linux CI.
- [ ] `cargo test --workspace --features webrtc,webtransport` green; coverage on `noise_dc` ≥ 90% (per `cargo-llvm-cov`).
- [ ] `adnet-bench` produces a CSV with handshake + RTT numbers vs `adnet-transport::quic`.
- [ ] Browser demo connects to a running `adnet-node` and prints `hello, peer <NodeId>`.
- [ ] `AUDIT_WEBRTC_WEBTRANSPORT.md` + round-2 follow-up merged.
- [ ] No new `unsafe_code` (workspace lints forbid it).
- [ ] No new `panic!()` / `unwrap()` in transport hot path.

---

## 6. Crate skeletons (preview)

```text
crates/adnet-webrtc/
├── Cargo.toml        (webrtc-rs, snow, adnet-identity, adnet-types, tokio)
├── README.md
├── src/
│   ├── lib.rs
│   ├── config.rs     (WebRtcConfig — ICE servers, STUN/TURN, fingerprint mode)
│   ├── rtc_engine.rs (webrtc::RTCPeerConnection wrapper)
│   ├── dc_session.rs (DataChannel ↔ Noise ↔ Frame)
│   ├── noise_dc.rs   (Noise XX handshake, NodeId binding)
│   ├── frame_codec.rs (Frame ↔ bytes, stream-id multiplexing)
│   ├── signaling.rs   (offer/answer codec + pkarr/DHT publish hook)
│   └── tests.rs
└── examples/
    ├── webrtc_roundtrip.rs (two in-process endpoints)
    └── webrtc_app.rs

crates/adnet-webtransport/
├── Cargo.toml        (wtransport, rustls, adnet-webrtc, adnet-identity, adnet-types)
├── README.md
├── src/
│   ├── lib.rs
│   ├── config.rs     (WebTransportConfig — bind addr, cert, connect-token TTL)
│   ├── cert.rs       (rcgen self-signed, ACME stub)
│   ├── wt_server.rs  (wtransport::Server + /adnet endpoint)
│   ├── wt_client.rs  (wtransport::Client + Noise handshake)
│   ├── connect_token.rs (HMAC-signed token, ephemeral)
│   ├── tests.rs
└── examples/
    ├── wt_roundtrip.rs
    └── wt_app.rs
```

---

## 7. Audit check-list (for Week 1)

`AUDIT_WEBRTC_WEBTRANSPORT.md` will be created in Week 1 and must cover:

1. `adnet-transport::traits::Transport` — every required method, every default-implemented method, and which backends currently satisfy each.
2. `adnet-transport::frame::Frame` — the exact byte layout, max sizes, and how the new backends must honor them.
3. `adnet-transport::relay_fallback` — how DERP discovery returns connection hints and whether SDP can ride the same path.
4. `adnet-ffi` — where to plug the JS-side glue without breaking the existing C ABI.
5. `adnet-node` — the `MultiTransport` insertion point (currently single transport; refactor to a Vec).
6. Existing `adnet-ffi` examples — confirm `ADNetFfi.kt` doesn't need changes.
