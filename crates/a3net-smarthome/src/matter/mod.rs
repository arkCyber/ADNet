//! Matter protocol support.
//!
//! Integrates the `matter-controller` crate to commission and control
//! Matter-standard smart home devices over IPv6. Devices are exposed
//! through the same `HubEvent` broadcast channel as Xiaomi MIoT devices,
//! so automations work identically regardless of the underlying protocol.
//!
//! ## Feature gates
//!
//! - `ble` — enable BLE commissioning (requires Bluetooth hardware).
//!   Without this feature, only IP commissioning (Ethernet/Wi-Fi) is available.
//!
//! ## Usage
//!
//! ```ignore
//! let config = MatterHubConfig::development();
//! let client = MatterClient::new(config).await?;
//! client.ensure_fabric(1).await?;
//!
//! // Commission from a QR code:
//! let node = client.commission("MT:Y.K90...", Some("lamp".into())).await?;
//! let device = node.into_device();
//! hub.register_device(device).await?;
//! ```

pub mod client;
pub mod error;
pub mod types;

pub use client::{MatterClient, MatterHubConfig, MatterNode, MatterAttestationTrust};
pub use error::MatterError;
pub use types::{MatterCluster, MatterClusterSpec};
pub use client::decode_matter_value;
