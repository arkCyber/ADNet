//! ADNet Smart Home Hub
//!
//! 智能家居设备控制中心 - 集成在 ADNet NAS 服务器中，
//! 支持小米 MIoT 协议、Matter 协议、mDNS 设备发现和自动化引擎。
//!
//! # 快速开始
//!
//! ```rust,no_run
//! use adnet_smarthome::{SmartHomeHub, HubConfig, miot::MiotAuth};
//!
//! #[tokio::main]
//! async fn main() {
//!     let config = HubConfig::default();
//!     let hub = SmartHomeHub::new(config)
//!         .with_miot(MiotAuth {
//!             user_id: "12345".into(),
//!             service_token: "token".into(),
//!             device_id: "device_id".into(),
//!             ssecurity: "ssecurity".into(),
//!         })
//!         .unwrap();
//!
//!     let hub = std::sync::Arc::new(hub);
//!     hub.start().await.unwrap();
//! }
//! ```

pub mod api;
pub mod automation;
pub mod device;
pub mod discovery;
pub mod error;
pub mod hub;
pub mod matter;
pub mod miot;
pub mod registry;

pub use api::{ApiConfig, ApiHandle};
pub use automation::Automation;
pub use device::{Device, DeviceCapability, DeviceState, DeviceType};
pub use error::{Result, SmartHomeError};
pub use hub::{HubConfig, HubEvent, SmartHomeHub};
pub use miot::{MiotAuth, MiotClient, PollQrError};
pub use registry::DeviceRegistry;
