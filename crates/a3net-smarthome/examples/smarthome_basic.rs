//! Minimal a3net-smarthome example.
//!
//! Builds a `SmartHomeHub` with default config, registers a couple of
//! virtual devices, and adds a small scene. This is the smallest
//! useful program that exercises the public API of the crate.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p a3net-smarthome --example smarthome_basic
//! ```

use a3net_smarthome::{
    scene::{Scene, SceneAction},
    Device, DeviceType, HubConfig, SmartHomeHub,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Default config binds to 127.0.0.1:8781 (REST API), starts mDNS
    //    discovery every 5 minutes, persists state under ./data/smarthome.
    let config = HubConfig::default();
    let hub = SmartHomeHub::new(config.clone());

    // 2. Register two virtual devices via the hub API. In production this
    //    happens automatically via mDNS/MIOT/Matter discovery.
    let mut lamp = Device::new("lamp-1", "Desk Lamp", DeviceType::WiFi);
    lamp.ip_address = Some("192.168.1.20".into());
    let mut sensor = Device::new("motion-1", "Hallway Motion", DeviceType::Zigbee);
    sensor.online = true;
    hub.register_device(lamp).await?;
    hub.register_device(sensor).await?;

    // 3. Add a simple "Evening" scene that turns the lamp on.
    let scene = Scene::new("evening", "Evening")
        .with_room("Living Room")
        .with_icon("evening")
        .add_action(SceneAction::SetProperty {
            device_id: "lamp-1".to_string(),
            siid: 2,
            piid: 1,
            value: serde_json::json!(true),
        });
    hub.add_scene(scene).await?;

    // 4. Inspect the hub. `list_devices()` / `list_scenes()` return the
    //    same snapshots the REST API would expose.
    let devices = hub.list_devices().await;
    let scenes = hub.list_scenes().await;
    println!("Hub config: bind={}, data_dir={}", config.bind_addr, config.data_dir);
    println!("Devices ({}):", devices.len());
    for d in &devices {
        println!("  - {} ({}) online={}", d.device_id, d.name, d.online);
    }
    println!("Scenes ({}):", scenes.len());
    for s in &scenes {
        println!("  - {} ({}) actions={}", s.id, s.name, s.actions.len());
    }

    // 5. We deliberately do NOT call `hub.start()` here — that would
    //    spawn the discovery / automation background tasks and bind
    //    the REST API. The full lifecycle is shown in
    //    `examples/smarthome_app.rs`.
    println!("\n(Did not call hub.start() — see smarthome_app.rs for the full lifecycle.)");

    Ok(())
}
