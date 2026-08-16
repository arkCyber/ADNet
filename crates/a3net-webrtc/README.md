# `a3net-webrtc`

WebRTC DataChannel transport for A3Net. Symmetric: native ↔ native and
native ↔ browser. Built on `webrtc-rs` (the Pion port) and `snow` for the
end-to-end Noise_XX handshake.

## Status (Round-1)

| Component | State |
|-----------|-------|
| `WebRtcConfig` (serde, defaults) | ✅ |
| `WebRtcError` + classification | ✅ |
| `noise_dc::run_noise_handshake` (Noise_XX over any byte stream) | ✅ + tests |
| `noise_dc::StaticPub::to_node_id` (BLAKE3 derivation) | ✅ + tests |
| `frame_codec::{encode, try_decode}` | ✅ + tests |
| `rtc_engine::Engine` (webrtc-rs PeerConnection wrapper) | ✅ scaffold |
| `dc_session::DcSession` (DataChannel + Noise + frame loop) | ✅ scaffold |
| `signaling::{InMemorySignaling, SignalingPayload, SignalingChannel}` | ✅ |
| `signaling::PkarrSignaling` | ⏳ Round-2 |
| `a3net-transport::webrtc::WebRtcTransport` (impl `Transport`) | ⏳ Round-2 |
| Browser JS shim (`a3net-ffi/js/`) | ⏳ Round-3 |
| Browser demo HTML | ⏳ Round-3 |

## Features

- `default = []` — only config + error types; no network deps.
- `webrtc = ["dep:webrtc", "dep:snow", "dep:pkarr"]` — full runtime.
- `signaling = ["dep:pkarr", "dep:snow"]` — just signaling helpers + Noise
  handshake, useful when an embedder pins its own `webrtc-rs` version.

## Quickstart

```rust,no_run
use a3net_webrtc::{config::WebRtcConfig, noise_dc, rtc_engine};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = WebRtcConfig::default();
    let engine = rtc_engine::Engine::build(&cfg).await?;
    let offer = engine.create_offer().await?;
    // ... send `offer` over signaling, receive answer, apply ...
    engine.wait_connected(cfg.establish_timeout()).await?;
    Ok(())
}
```

## Identity

`NodeId` for a WebRTC peer is derived as
`hex(BLAKE3(noise_static_pub))[:32]`. This is consistent with the QUIC
transport's `derive_node_id_from_cert` and means a node's identity is
the same regardless of the transport that introduced it.
