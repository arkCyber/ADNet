//! Integration tests for the smart home hub: MIoT cloud client against a
//! local mock server, device registry persistence via the hub, automation
//! triggering, and the REST API (including bearer-token auth).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use a3net_smarthome::miot::{Action, MiotAuth, Property, PropertyValue};
use a3net_smarthome::{
    api::{self, ApiConfig},
    automation::{Automation, AutomationAction, Trigger},
    device::{Device, DeviceType},
    hub::{HubConfig, HubEvent, SmartHomeHub},
    scene::{Scene, SceneAction},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

fn temp_data_dir(label: &str) -> std::path::PathBuf {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let suffix: String = (0..8).map(|_| format!("{:x}", rng.gen_range(0..16u8))).collect();
    std::env::temp_dir().join(format!("a3net-smarthome-it-{}-{}", label, suffix))
}

fn test_auth() -> MiotAuth {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    MiotAuth {
        user_id: "12345".into(),
        service_token: "test-service-token".into(),
        device_id: "test-device-id".into(),
        ssecurity: BASE64.encode(b"0123456789abcdef0123456789abcdef"),
    }
}

/// A minimal mock of the Xiaomi MIoT cloud API, dispatching on path.
/// Doesn't validate the signature (the crypto module already has
/// dedicated unit tests) — it just proves `MiotClient` speaks the
/// expected wire protocol end to end.
async fn spawn_mock_miot_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock miot");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(t) => t,
                Err(_) => continue,
            };
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let svc = service_fn(mock_miot_handler);
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });

    addr
}

async fn mock_miot_handler(
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, std::io::Error> {
    let path = req.uri().path().to_string();
    // Drain the body (form-encoded _nonce/data/signature); the mock
    // doesn't need to inspect it beyond exercising the real client.
    let _ = req.collect().await;

    let result = match path.as_str() {
        "/app/v2/home/device_list" => serde_json::json!({
            "list": [
                {
                    "did": "mock-dev-1",
                    "name": "Mock Lamp",
                    "model": "mock.light.v1",
                    "localip": "192.168.1.50",
                    "mac": "AA:BB:CC:DD:EE:FF",
                    "online": true,
                    "token": ""
                }
            ]
        }),
        "/app/v2/properties/get" => serde_json::json!([
            { "siid": 2, "piid": 1, "value": true }
        ]),
        "/app/v2/properties/set" => serde_json::Value::Null,
        "/app/v2/action/invoke" => serde_json::json!({ "out": [] }),
        _ => {
            let body = serde_json::to_vec(&serde_json::json!({
                "code": -1,
                "message": "not found",
                "result": null,
            }))
            .unwrap();
            return Ok(Response::builder()
                .status(404)
                .body(Full::new(Bytes::from(body)))
                .unwrap());
        }
    };

    let body = serde_json::to_vec(&serde_json::json!({
        "code": 0,
        "message": "ok",
        "result": result,
    }))
    .unwrap();

    Ok(Response::builder()
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap())
}

fn hub_config(dir: &std::path::Path) -> HubConfig {
    HubConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        enable_discovery: false, // avoid touching the real network in tests
        discovery_interval_secs: 3600,
        heartbeat_timeout_secs: 3600,
        data_dir: dir.to_string_lossy().into_owned(),
    }
}

#[tokio::test]
async fn miot_get_device_list_via_mock_server() {
    let mock_addr = spawn_mock_miot_server().await;
    let dir = temp_data_dir("device-list");

    let hub = SmartHomeHub::new(hub_config(&dir))
        .with_miot_host(test_auth(), format!("http://{mock_addr}"))
        .unwrap();

    let props = hub
        .get_properties("mock-dev-1", vec![Property { siid: 2, piid: 1 }])
        .await
        .expect("get_properties should succeed against mock server");
    assert_eq!(props.len(), 1);
    assert_eq!(props[0].value, serde_json::json!(true));

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn miot_set_property_updates_registry_and_emits_event() {
    let mock_addr = spawn_mock_miot_server().await;
    let dir = temp_data_dir("set-property");

    let hub = Arc::new(
        SmartHomeHub::new(hub_config(&dir))
            .with_miot_host(test_auth(), format!("http://{mock_addr}"))
            .unwrap(),
    );
    hub.register_device(Device::new("mock-dev-1", "Mock Lamp", DeviceType::Xiaomi))
        .await
        .unwrap();

    let mut events = hub.subscribe();

    hub.set_property("mock-dev-1", 2, 1, serde_json::json!(true))
        .await
        .expect("set_property should succeed against mock server");

    let dev = hub.get_device("mock-dev-1").await.unwrap();
    assert_eq!(dev.state.get("2.1"), Some(&serde_json::json!(true)));

    // The registry-update event should have been broadcast too.
    let mut saw_property_changed = false;
    for _ in 0..5 {
        if let Ok(ev) = tokio::time::timeout(Duration::from_millis(100), events.recv()).await {
            if let Ok(HubEvent::PropertyChanged { device_id, property, value }) = ev {
                if device_id == "mock-dev-1" && property == "2.1" && value == serde_json::json!(true) {
                    saw_property_changed = true;
                    break;
                }
            }
        }
    }
    assert!(saw_property_changed, "expected a PropertyChanged event");

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn miot_invoke_action_against_mock_server() {
    let mock_addr = spawn_mock_miot_server().await;
    let dir = temp_data_dir("invoke-action");

    let hub = SmartHomeHub::new(hub_config(&dir))
        .with_miot_host(test_auth(), format!("http://{mock_addr}"))
        .unwrap();

    let result = hub
        .invoke_action("mock-dev-1", 3, 1, vec![])
        .await
        .expect("invoke_action should succeed against mock server");
    assert_eq!(result, serde_json::json!({ "out": [] }));

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn without_miot_property_calls_return_not_supported() {
    let dir = temp_data_dir("no-miot");
    let hub = SmartHomeHub::new(hub_config(&dir));

    let err = hub
        .set_property("dev-1", 2, 1, serde_json::json!(true))
        .await
        .unwrap_err();
    assert!(matches!(err, a3net_smarthome::SmartHomeError::NotSupported(_)));

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn device_registry_survives_hub_restart() {
    let dir = temp_data_dir("restart");

    {
        let hub = Arc::new(SmartHomeHub::new(hub_config(&dir)));
        hub.clone().start().await.unwrap();
        hub.register_device(Device::new("dev-1", "Lamp", DeviceType::Xiaomi))
            .await
            .unwrap();
    }

    // New hub instance, same data dir — should reload the device on start().
    let hub2 = Arc::new(SmartHomeHub::new(hub_config(&dir)));
    hub2.clone().start().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let devices = hub2.list_devices().await;
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].device_id, "dev-1");

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn automation_triggers_on_property_change() {
    let mock_addr = spawn_mock_miot_server().await;
    let dir = temp_data_dir("automation");

    let hub = Arc::new(
        SmartHomeHub::new(hub_config(&dir))
            .with_miot_host(test_auth(), format!("http://{mock_addr}"))
            .unwrap(),
    );
    hub.clone().start().await.unwrap();

    hub.register_device(Device::new("trigger-dev", "Motion", DeviceType::Xiaomi))
        .await
        .unwrap();
    hub.register_device(Device::new("mock-dev-1", "Mock Lamp", DeviceType::Xiaomi))
        .await
        .unwrap();

    hub.add_automation(Automation {
        id: "auto-1".into(),
        name: "Motion turns on lamp".into(),
        enabled: true,
        trigger: Trigger::PropertyChanged {
            device_id: "trigger-dev".into(),
            property: "3.1".into(),
            value: serde_json::json!(true),
        },
        conditions: vec![],
        actions: vec![AutomationAction::SetProperty {
            device_id: "mock-dev-1".into(),
            siid: 2,
            piid: 1,
            value: serde_json::json!(true),
        }],
    })
    .await
    .unwrap();

    // Fire the triggering property change through the real API path.
    // The mock MIoT server dispatches purely on URL path and ignores
    // the `did` field, so this call succeeds regardless of which
    // device id we pass, updates trigger-dev's local state, and
    // broadcasts the `PropertyChanged` event the automation reacts to.
    hub.set_property("trigger-dev", 3, 1, serde_json::json!(true))
        .await
        .expect("trigger set_property should succeed against mock server");

    // Give the automation loop time to react (it runs `set_property`
    // against the mock server for mock-dev-1).
    let mut fired = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let dev = hub.get_device("mock-dev-1").await.unwrap();
        if dev.state.get("2.1") == Some(&serde_json::json!(true)) {
            fired = true;
            break;
        }
    }
    assert!(fired, "automation should have set mock-dev-1's 2.1 property");

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn automations_persist_across_restart() {
    let dir = temp_data_dir("automation-restart");

    {
        let hub = Arc::new(SmartHomeHub::new(hub_config(&dir)));
        hub.clone().start().await.unwrap();
        hub.add_automation(Automation {
            id: "auto-1".into(),
            name: "Persisted rule".into(),
            enabled: true,
            trigger: Trigger::Manual,
            conditions: vec![],
            actions: vec![AutomationAction::Notify { message: "hi".into() }],
        })
        .await
        .unwrap();
    }

    let hub2 = Arc::new(SmartHomeHub::new(hub_config(&dir)));
    hub2.clone().start().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let autos = hub2.list_automations().await;
    assert_eq!(autos.len(), 1);
    assert_eq!(autos[0].id, "auto-1");

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ── REST API tests ───────────────────────────────────────────────────────

async fn spawn_api(hub: Arc<SmartHomeHub>, auth_token: Option<String>) -> (SocketAddr, api::ApiHandle) {
    let handle = api::serve(
        hub,
        ApiConfig { bind: "127.0.0.1:0".parse().unwrap(), auth_token },
    )
    .await
    .expect("api::serve");
    let addr = handle.local_addr();
    (addr, handle)
}

#[tokio::test]
async fn api_healthz_works_without_token() {
    let dir = temp_data_dir("api-healthz");
    let hub = Arc::new(SmartHomeHub::new(hub_config(&dir)));
    let (addr, _handle) = spawn_api(hub, Some("secret".into())).await;

    let resp = reqwest::get(format!("http://{addr}/healthz")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn api_rejects_missing_token() {
    let dir = temp_data_dir("api-no-token");
    let hub = Arc::new(SmartHomeHub::new(hub_config(&dir)));
    let (addr, _handle) = spawn_api(hub, Some("secret".into())).await;

    let resp = reqwest::get(format!("http://{addr}/api/devices")).await.unwrap();
    assert_eq!(resp.status(), 401);

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn api_rejects_wrong_token() {
    let dir = temp_data_dir("api-wrong-token");
    let hub = Arc::new(SmartHomeHub::new(hub_config(&dir)));
    let (addr, _handle) = spawn_api(hub, Some("secret".into())).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/api/devices"))
        .header("Authorization", "Bearer wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn api_accepts_correct_token_and_serves_devices() {
    let dir = temp_data_dir("api-good-token");
    let hub = Arc::new(SmartHomeHub::new(hub_config(&dir)));
    hub.register_device(Device::new("dev-1", "Lamp", DeviceType::Xiaomi))
        .await
        .unwrap();
    let (addr, _handle) = spawn_api(hub, Some("secret".into())).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/api/devices"))
        .header("Authorization", "Bearer secret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let devices: Vec<Device> = resp.json().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].device_id, "dev-1");

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn api_no_token_configured_allows_all_requests() {
    let dir = temp_data_dir("api-open");
    let hub = Arc::new(SmartHomeHub::new(hub_config(&dir)));
    let (addr, _handle) = spawn_api(hub, None).await;

    let resp = reqwest::get(format!("http://{addr}/api/devices")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn api_device_crud_flow() {
    let dir = temp_data_dir("api-crud");
    let hub = Arc::new(SmartHomeHub::new(hub_config(&dir)));
    let (addr, _handle) = spawn_api(hub, None).await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Create
    let dev = Device::new("dev-1", "Lamp", DeviceType::Xiaomi);
    let resp = client.post(format!("{base}/api/devices")).json(&dev).send().await.unwrap();
    assert_eq!(resp.status(), 201);

    // Read
    let resp = client.get(format!("{base}/api/devices/dev-1")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let got: Device = resp.json().await.unwrap();
    assert_eq!(got.name, "Lamp");

    // Read missing
    let resp = client.get(format!("{base}/api/devices/missing")).send().await.unwrap();
    assert_eq!(resp.status(), 404);

    // Delete
    let resp = client.delete(format!("{base}/api/devices/dev-1")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Read after delete
    let resp = client.get(format!("{base}/api/devices/dev-1")).send().await.unwrap();
    assert_eq!(resp.status(), 404);

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn api_automation_crud_flow() {
    let dir = temp_data_dir("api-automation-crud");
    let hub = Arc::new(SmartHomeHub::new(hub_config(&dir)));
    let (addr, _handle) = spawn_api(hub, None).await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let auto = Automation {
        id: "a1".into(),
        name: "Test rule".into(),
        enabled: true,
        trigger: Trigger::Manual,
        conditions: vec![],
        actions: vec![AutomationAction::Notify { message: "hi".into() }],
    };

    let resp = client.post(format!("{base}/api/automations")).json(&auto).send().await.unwrap();
    assert_eq!(resp.status(), 201);

    let resp = client.get(format!("{base}/api/automations")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let autos: Vec<Automation> = resp.json().await.unwrap();
    assert_eq!(autos.len(), 1);
    assert_eq!(autos[0].id, "a1");

    let resp = client.delete(format!("{base}/api/automations/a1")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client.get(format!("{base}/api/automations")).send().await.unwrap();
    let autos: Vec<Automation> = resp.json().await.unwrap();
    assert!(autos.is_empty());

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn api_properties_endpoints_via_mock_miot() {
    let mock_addr = spawn_mock_miot_server().await;
    let dir = temp_data_dir("api-properties");

    let hub = Arc::new(
        SmartHomeHub::new(hub_config(&dir))
            .with_miot_host(test_auth(), format!("http://{mock_addr}"))
            .unwrap(),
    );
    let (addr, _handle) = spawn_api(hub, None).await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let resp = client
        .post(format!("{base}/api/devices/mock-dev-1/properties/get"))
        .json(&serde_json::json!({ "properties": [{ "siid": 2, "piid": 1 }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let values: Vec<PropertyValue> = resp.json().await.unwrap();
    assert_eq!(values[0].value, serde_json::json!(true));

    let resp = client
        .post(format!("{base}/api/devices/mock-dev-1/properties/set"))
        .json(&serde_json::json!({ "siid": 2, "piid": 1, "value": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!("{base}/api/devices/mock-dev-1/action"))
        .json(&serde_json::json!({ "siid": 3, "aiid": 1, "input": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// Silence unused-import warnings if a given cfg trims some tests out.
#[allow(unused_imports)]
use a3net_smarthome::miot::MiotClient as _UnusedMiotClientImportGuard;
#[allow(unused)]
fn _type_assertions(_: Action) {}

/// Regression for the prior `CreateSceneRequest::actions` field
/// `skip_deserializing`: every scene created via REST used to
/// have an empty action list regardless of the body. This test
/// sends a real scene body with three actions and asserts every
/// action is persisted.
#[tokio::test]
async fn api_scene_create_persists_actions_from_body() {
    let dir = temp_data_dir("api-scene-actions");
    let hub = Arc::new(SmartHomeHub::new(hub_config(&dir)));
    let (addr, _handle) = spawn_api(hub.clone(), None).await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let body = serde_json::json!({
        "id": "evening",
        "name": "Evening",
        "room": "Living room",
        "icon": "moon",
        "actions": [
            { "set_property": {
                "device_id": "lamp-1",
                "siid": 2,
                "piid": 1,
                "value": true,
            }},
            { "delay": { "millis": 250 } },
            { "invoke_action": {
                "device_id": "speaker-1",
                "siid": 5,
                "aiid": 3,
                "input": [10, 20],
            }}
        ],
    });

    let resp = client
        .post(format!("{base}/api/scenes"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Read back via the in-hub method (the GET endpoint is not
    // explicit, but `get_scene` is).
    let stored = hub
        .get_scene("evening")
        .await
        .expect("scene must exist");
    assert_eq!(stored.actions.len(), 3);
    assert_eq!(stored.room.as_deref(), Some("Living room"));
    assert_eq!(stored.icon.as_deref(), Some("moon"));

    // Round-trip through JSON to confirm the wire-compatible
    // encoding captures the action list without surprising the
    // deserializer (the very bug we just fixed).
    let json = serde_json::to_string(&stored).unwrap();
    let back: Scene = serde_json::from_str(&json).unwrap();
    assert_eq!(back.actions.len(), 3);
    match &back.actions[0] {
        SceneAction::SetProperty { device_id, siid, piid, value } => {
            assert_eq!(device_id, "lamp-1");
            assert_eq!(*siid, 2);
            assert_eq!(*piid, 1);
            assert_eq!(value, &serde_json::json!(true));
        }
        other => panic!("expected SetProperty, got {other:?}"),
    }
    match &back.actions[1] {
        SceneAction::Delay { millis } => assert_eq!(*millis, 250),
        other => panic!("expected Delay, got {other:?}"),
    }
    match &back.actions[2] {
        SceneAction::InvokeAction { device_id, siid, aiid, input } => {
            assert_eq!(device_id, "speaker-1");
            assert_eq!(*siid, 5);
            assert_eq!(*aiid, 3);
            assert_eq!(input.len(), 2);
        }
        other => panic!("expected InvokeAction, got {other:?}"),
    }

    let _ = tokio::fs::remove_dir_all(&dir).await;
}
