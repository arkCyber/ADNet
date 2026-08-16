//! MQTT protocol integration for smart home devices.
//!
//! Supports publishing device states and subscribing to control commands
//! via an MQTT broker. This enables integration with third-party smart home
//! ecosystems (e.g., Home Assistant, OpenHAB) that communicate over MQTT.

pub mod client;
pub mod types;

pub use client::{MqttConfig, MqttClient, MqttEvent};
pub use types::MqttTopic;
