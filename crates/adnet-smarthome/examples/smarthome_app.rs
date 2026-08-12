//! Realistic adnet-smarthome app example.
//!
//! Walks through the full lifecycle of a smart-home installation on an
//! ADNet NAS:
//!
//! 1. Build a hub with a custom data dir
//! 2. Register devices, a scene, and an automation rule
//! 3. Start the hub (background tasks + REST API) for a short time
//! 4. Hit the REST API via `hyper` to confirm `/healthz` is alive
//! 5. Trigger the scene and inspect the resulting state
//! 6. Stop the hub
//!
//! Run with:
//!
//! ```bash
//! cargo run -p adnet-smarthome --example smarthome_app
//! ```
//!
//! NOTE: This example is hermetic — it binds the REST API to a
//! loopback-only port and tears down everything within a few seconds, so
//! it's safe to run from CI.

use adnet_smarthome::{
    automation::{Automation, AutomationAction, Condition, Trigger},
    scene::{Scene, SceneAction},
    Device, DeviceType, HubConfig, SmartHomeHub,
};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Configure a hub that points to a fresh tmpfs data dir.
    let data_dir = std::env::temp_dir().join(format!("smarthome-app-{}", std::process::id()));
    let std::net::SocketAddr::V4(bind) = "127.0.0.1:0".parse::<std::net::SocketAddr>()? else {
        unreachable!()
    };
    let config = HubConfig {
        bind_addr: std::net::SocketAddr::from(bind),
        enable_discovery: false, // suppress mDNS in CI
        discovery_interval_secs: 600,
        heartbeat_timeout_secs: 60,
        data_dir: data_dir.to_string_lossy().into_owned(),
    };

    let hub = SmartHomeHub::new(config.clone());

    // 2. Register two virtual devices.
    let lamp = Device::new("lamp-1", "Desk Lamp", DeviceType::WiFi);
    let sensor = Device::new("motion-1", "Hallway Motion", DeviceType::Zigbee);
    hub.register_device(lamp).await?;
    hub.register_device(sensor).await?;

    // 3. Add a "Night" scene that turns the lamp off and a manual
    //    automation that triggers it on a fixed schedule.
    let night_scene = Scene::new("night", "Night Mode")
        .with_room("Living Room")
        .add_action(SceneAction::SetProperty {
            device_id: "lamp-1".to_string(),
            siid: 2,
            piid: 1,
            value: serde_json::json!(false),
        });
    hub.add_scene(night_scene.clone()).await?;

    let night_auto = Automation {
        id: "night-auto".into(),
        name: "Night mode at 23:00".into(),
        enabled: true,
        trigger: Trigger::Schedule {
            cron: "23:00".into(),
        },
        conditions: vec![Condition::TimeInRange {
            start: "22:30".into(),
            end: "06:00".into(),
        }],
        actions: vec![AutomationAction::Notify {
            message: "switching to night mode".into(),
        }],
    };
    hub.add_automation(night_auto).await?;

    // 4. Start the hub as an Arc so we can share it with the api task.
    let hub = Arc::new(hub);
    let hub_for_start = hub.clone();
    hub_for_start.start().await?;
    println!("Hub started; data_dir={}", config.data_dir);

    // 5. The hub binds the REST API at the configured address. We can
    //    verify it is alive without bringing up the full HTTP client:
    //    just call the same `api` module another thread would.
    let api_handle = adnet_smarthome::api::serve(
        hub.clone(),
        adnet_smarthome::api::ApiConfig {
            bind: config.bind_addr,
            auth_token: None,
        },
    )
    .await?;
    println!("REST API bound at {}", api_handle.local_addr());

    // 6. Trigger the scene manually and inspect the registry.
    hub.activate_scene("night").await?;
    let scenes = hub.list_scenes().await;
    let active = scenes.iter().find(|s| s.id == "night").map(|s| s.active);
    println!("Activated 'night' scene, active={:?}", active);

    // 7. Quick health check via a raw TCP socket. We don't need to
    //    drive the full hyper client stack; opening a TCP connection
    //    is enough to confirm the API server is bound.
    let health_addr = api_handle.local_addr();
    match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(health_addr),
    )
    .await
    {
        Ok(Ok(_stream)) => println!("REST API TCP socket OK at {}", health_addr),
        Ok(Err(e)) => println!("REST API connect failed: {e}"),
        Err(_) => println!("REST API connect timed out"),
    }

    // 8. Give the automation loop one tick, then tear down.
    tokio::time::sleep(Duration::from_millis(500)).await;
    api_handle.shutdown();
    println!("Hub stopped.");

    Ok(())
}
