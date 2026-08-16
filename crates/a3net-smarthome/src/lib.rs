//! A3Net Smart Home Hub
//!
//! 智能家居设备控制中心 - 集成在 A3Net NAS 服务器中，
//! 支持小米 MIoT 协议、Matter 协议、HomeKit、MQTT、mDNS 设备发现和自动化引擎。
//!
//! # 功能特性
//!
//! - **MIoT (小米生态)**: 支持 Xiaomi 设备的云端和本地控制
//! - **Matter**: 支持 Matter 标准协议的设备配对和控制
//! - **HomeKit**: 设备到 Apple HomeKit 的映射（存根实现）
//! - **MQTT**: 与第三方智能家居系统（如 Home Assistant）集成
//! - **场景 (Scene)**: 一键触发多个设备的预设状态
//! - **自动化 (Automation)**: 基于事件和时间的自动化规则引擎
//! - **设备发现**: mDNS 自动发现局域网内的智能设备
//!
//! # 快速开始
//!
//! ```rust,no_run
//! use a3net_smarthome::{SmartHomeHub, HubConfig, miot::MiotAuth};
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
//!
//! # REST API 端点
//!
//! - `GET  /api/devices` - 列出所有设备
//! - `GET  /api/devices/:id` - 获取单个设备
//! - `POST /api/devices` - 注册设备
//! - `DELETE /api/devices/:id` - 删除设备
//! - `POST /api/devices/:id/properties/get` - 读取 MIoT 属性
//! - `POST /api/devices/:id/properties/set` - 设置 MIoT 属性
//! - `POST /api/devices/:id/action` - 调用设备动作
//! - `GET  /api/automations` - 列出自动化规则
//! - `POST /api/automations` - 创建自动化规则
//! - `DELETE /api/automations/:id` - 删除自动化规则
//! - `GET  /api/scenes` - 列出场景
//! - `POST /api/scenes` - 创建场景
//! - `POST /api/scenes/activate` - 激活场景
//! - `GET  /api/matter/nodes` - 列出 Matter 节点
//! - `POST /api/matter/commission` - 配对 Matter 设备
//! - `GET  /api/homekit/accessories` - 列出 HomeKit 配件
//! - `GET  /healthz` - 健康检查

pub mod api;
pub mod automation;
pub mod device;
pub mod discovery;
pub mod error;
pub mod homekit;
pub mod hub;
pub mod matter;
pub mod miot;
pub mod mqtt;
pub mod registry;
pub mod scene;

pub use api::{ApiConfig, ApiHandle};
pub use automation::Automation;
pub use device::{Device, DeviceCapability, DeviceState, DeviceType};
pub use error::{Result, SmartHomeError};
pub use homekit::{HomeKitAccessory, HomeKitBridge, HomeKitCategory, HomeKitCharacteristic, HomeKitCharacteristicType, HomeKitConfig, HomeKitFormat};
pub use hub::{HubConfig, HubEvent, SmartHomeHub};
pub use miot::{MiotAuth, MiotClient, PollQrError};
pub use mqtt::{MqttConfig, MqttClient, MqttEvent};
pub use registry::DeviceRegistry;
pub use scene::{Scene, SceneAction, SceneManager};
