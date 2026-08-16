//! WebRTC round-trip example (Round-1 scaffold).
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-webrtc --example webrtc_roundtrip --features webrtc
//! ```

use a3net_webrtc::noise_dc::{generate_keypair, Role};
use a3net_webrtc::WebRtcResult;

#[tokio::main]
async fn main() -> WebRtcResult<()> {
    let alice_kp = generate_keypair()?;
    let bob_kp = generate_keypair()?;

    // Real wire would call:
    //   let alice_engine = rtc_engine::Engine::build(&cfg).await?;
    //   let offer = alice_engine.create_offer().await?;
    //   ...
    // For Round-1 we exercise the Noise handshake directly (the part that
    // is independent of webrtc-rs).
    let _ = (alice_kp, bob_kp, Role::Initiator);

    println!("WebRTC roundtrip scaffold — implement full SDP exchange in Round-2.");
    Ok(())
}
