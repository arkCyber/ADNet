//! REST API server exposing the [`crate::hub::SmartHomeHub`] over HTTP.
//!
//! Routes:
//! - `GET  /api/devices`                         list all devices
//! - `GET  /api/devices/:id`                      get one device
//! - `POST /api/devices`                          register a device manually
//! - `DELETE /api/devices/:id`                     remove a device
//! - `POST /api/devices/:id/properties/get`       read MIoT properties
//! - `POST /api/devices/:id/properties/set`       write a MIoT property
//! - `POST /api/devices/:id/action`               invoke a MIoT action
//! - `GET  /api/automations`                       list automations
//! - `POST /api/automations`                       create/replace an automation
//! - `DELETE /api/automations/:id`                  remove an automation
//! - `GET  /healthz`                               liveness probe

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::warn;

use crate::hub::SmartHomeHub;

/// Bind configuration for the REST API.
///
/// `auth_token`, when set, requires every request (except `/healthz`)
/// to carry a matching `Authorization: Bearer <token>` header. Leave
/// it `None` only for trusted, loopback-only deployments — this API
/// can control physical devices (locks, cameras) and should not be
/// exposed unauthenticated on any shared or routable interface.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub bind: SocketAddr,
    pub auth_token: Option<String>,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 8781)),
            auth_token: None,
        }
    }
}

/// Constant-time comparison to avoid leaking token length/prefix via
/// response timing.
fn tokens_match(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn is_authorized(cfg: &ApiConfig, req: &Request<Incoming>) -> bool {
    let Some(expected) = &cfg.auth_token else {
        return true;
    };
    match req.headers().get("authorization").and_then(|v| v.to_str().ok()) {
        Some(header) => header
            .strip_prefix("Bearer ")
            .map(|got| tokens_match(got.trim(), expected))
            .unwrap_or(false),
        None => false,
    }
}

/// Handle to a running API server; drop or call [`Self::shutdown`] to stop it.
pub struct ApiHandle {
    bound_addr: SocketAddr,
    shutdown: watch::Sender<bool>,
}

impl ApiHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.bound_addr
    }
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
}

/// Start the HTTP API server bound to `cfg.bind`, serving `hub`.
pub async fn serve(hub: Arc<SmartHomeHub>, cfg: ApiConfig) -> std::io::Result<ApiHandle> {
    if cfg.auth_token.is_none() {
        warn!(
            "smarthome API on {} is running WITHOUT authentication; \
             only safe on a trusted loopback-only interface",
            cfg.bind
        );
    }
    let listener = TcpListener::bind(cfg.bind).await?;
    let bound = listener.local_addr()?;
    let (tx, _rx) = watch::channel(false);
    let mut shutdown_rx = tx.subscribe();
    let cfg = Arc::new(cfg);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => break,
                accept = listener.accept() => {
                    let (stream, _peer) = match accept {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    let hub = Arc::clone(&hub);
                    let cfg = Arc::clone(&cfg);
                    tokio::spawn(async move {
                        let io = TokioIo::new(stream);
                        let svc = service_fn(move |req| {
                            let hub = Arc::clone(&hub);
                            let cfg = Arc::clone(&cfg);
                            async move { dispatch(hub, cfg, req).await }
                        });
                        if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                            warn!("smarthome api connection error: {e}");
                        }
                    });
                }
            }
        }
    });

    Ok(ApiHandle {
        bound_addr: bound,
        shutdown: tx,
    })
}

fn json_body<T: Serialize>(status: StatusCode, value: &T) -> Response<Full<Bytes>> {
    match serde_json::to_vec(value) {
        Ok(bytes) => Response::builder()
            .status(status)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .header("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
            .header("Access-Control-Allow-Headers", "Authorization, Content-Type")
            .body(Full::new(Bytes::from(bytes)))
            .unwrap_or_else(|_| Response::new(Full::new(Bytes::from_static(b"{}")))),
        Err(_) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Full::new(Bytes::from_static(b"{\"error\":\"serialization failed\"}")))
            .unwrap_or_else(|_| Response::new(Full::new(Bytes::from_static(b"{}")))),
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

fn error_response(status: StatusCode, msg: impl Into<String>) -> Response<Full<Bytes>> {
    json_body(status, &ErrorBody { error: msg.into() })
}

fn cors_preflight_response() -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
        .header("Access-Control-Allow-Headers", "Authorization, Content-Type")
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

/// Maximum request body size: 1 MiB. Prevents memory exhaustion from
/// malicious or accidental huge payloads.
const MAX_BODY_SIZE: usize = 1 << 20;

async fn read_json_body<T: for<'de> Deserialize<'de>>(
    req: Request<Incoming>,
) -> Result<T, Response<Full<Bytes>>> {
    // Collect with a hard size limit.
    let collected = req
        .collect()
        .await
        .map_err(|e| error_response(StatusCode::BAD_REQUEST, format!("read body: {e}")))?;

    let bytes = collected.to_bytes();
    if bytes.len() > MAX_BODY_SIZE {
        return Err(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("body exceeds {} bytes", MAX_BODY_SIZE),
        ));
    }

    serde_json::from_slice(&bytes)
        .map_err(|e| error_response(StatusCode::BAD_REQUEST, format!("invalid json: {e}")))
}

#[derive(Deserialize)]
struct SetPropertyRequest {
    siid: u32,
    piid: u32,
    value: serde_json::Value,
}

#[derive(Deserialize)]
struct GetPropertiesRequest {
    properties: Vec<crate::miot::Property>,
}

#[derive(Deserialize)]
struct InvokeActionRequest {
    siid: u32,
    aiid: u32,
    #[serde(default)]
    input: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct MatterCommissionRequest {
    payload: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Deserialize)]
struct SceneActivateRequest {
    id: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct CreateSceneRequest {
    id: String,
    name: String,
    #[serde(default)]
    room: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    actions: Vec<crate::scene::SceneAction>,
}

async fn dispatch(
    hub: Arc<SmartHomeHub>,
    cfg: Arc<ApiConfig>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, std::io::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let segments: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();

    if path != "/healthz" && !is_authorized(&cfg, &req) {
        return Ok(error_response(StatusCode::UNAUTHORIZED, "missing or invalid bearer token"));
    }

    // CORS preflight — always allowed, no auth required.
    if method == Method::OPTIONS {
        return Ok(cors_preflight_response());
    }

    let resp = match (&method, segments.as_slice()) {
        (&Method::GET, ["healthz"]) => json_body(StatusCode::OK, &serde_json::json!({"status": "ok"})),

        (&Method::GET, ["api", "devices"]) => {
            let devices = hub.list_devices().await;
            json_body(StatusCode::OK, &devices)
        }
        (&Method::POST, ["api", "devices"]) => match read_json_body::<crate::device::Device>(req).await {
            Ok(device) => match hub.register_device(device).await {
                Ok(()) => json_body(StatusCode::CREATED, &serde_json::json!({"status": "registered"})),
                Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            },
            Err(resp) => resp,
        },
        (&Method::GET, ["api", "devices", id]) => match hub.get_device(id).await {
            Some(dev) => json_body(StatusCode::OK, &dev),
            None => error_response(StatusCode::NOT_FOUND, "device not found"),
        },
        (&Method::DELETE, ["api", "devices", id]) => match hub.remove_device(id).await {
            Ok(()) => json_body(StatusCode::OK, &serde_json::json!({"status": "removed"})),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        },
        (&Method::POST, ["api", "devices", id, "properties", "get"]) => {
            let id = id.to_string();
            match read_json_body::<GetPropertiesRequest>(req).await {
                Ok(body) => match hub.get_properties(&id, body.properties).await {
                    Ok(values) => json_body(StatusCode::OK, &values),
                    Err(e) => error_response(StatusCode::BAD_GATEWAY, e.to_string()),
                },
                Err(resp) => resp,
            }
        }
        (&Method::POST, ["api", "devices", id, "properties", "set"]) => {
            let id = id.to_string();
            match read_json_body::<SetPropertyRequest>(req).await {
                Ok(body) => match hub.set_property(&id, body.siid, body.piid, body.value).await {
                    Ok(()) => json_body(StatusCode::OK, &serde_json::json!({"status": "ok"})),
                    Err(e) => error_response(StatusCode::BAD_GATEWAY, e.to_string()),
                },
                Err(resp) => resp,
            }
        }
        (&Method::POST, ["api", "devices", id, "action"]) => {
            let id = id.to_string();
            match read_json_body::<InvokeActionRequest>(req).await {
                Ok(body) => match hub
                    .invoke_action(&id, body.siid, body.aiid, body.input)
                    .await
                {
                    Ok(result) => json_body(StatusCode::OK, &result),
                    Err(e) => error_response(StatusCode::BAD_GATEWAY, e.to_string()),
                },
                Err(resp) => resp,
            }
        }

        (&Method::GET, ["api", "automations"]) => {
            let autos = hub.list_automations().await;
            json_body(StatusCode::OK, &autos)
        }
        (&Method::POST, ["api", "automations"]) => {
            match read_json_body::<crate::automation::Automation>(req).await {
                Ok(auto) => match hub.add_automation(auto).await {
                    Ok(()) => json_body(StatusCode::CREATED, &serde_json::json!({"status": "ok"})),
                    Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
                },
                Err(resp) => resp,
            }
        }
        (&Method::DELETE, ["api", "automations", id]) => match hub.remove_automation(id).await {
            Ok(()) => json_body(StatusCode::OK, &serde_json::json!({"status": "removed"})),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        },

        // ── Matter endpoints ────────────────────────────────────────────────
        (&Method::GET, ["api", "matter", "nodes"]) => {
            // Return list of Matter node IDs
            let nodes: Vec<u64> = hub.list_matter_nodes().await.unwrap_or_default();
            json_body(StatusCode::OK, &nodes)
        }
        (&Method::POST, ["api", "matter", "commission"]) => {
            match read_json_body::<MatterCommissionRequest>(req).await {
                Ok(body) => match hub.matter_commission(&body.payload, body.label).await {
                    Ok(node) => json_body(StatusCode::CREATED, &node),
                    Err(e) => error_response(StatusCode::BAD_GATEWAY, e.to_string()),
                },
                Err(resp) => resp,
            }
        }

        // ── Scene endpoints ───────────────────────────────────────────────
        (&Method::GET, ["api", "scenes"]) => {
            let scenes = hub.list_scenes().await;
            json_body(StatusCode::OK, &scenes)
        }
        (&Method::POST, ["api", "scenes"]) => {
            match read_json_body::<CreateSceneRequest>(req).await {
                Ok(body) => {
                    let scene = crate::scene::Scene::new(&body.id, &body.name);
                    let mut scene = match (body.room, body.icon) {
                        (Some(room), Some(icon)) => scene.with_room(&room).with_icon(&icon),
                        (Some(room), None) => scene.with_room(&room),
                        (None, Some(icon)) => scene.with_icon(&icon),
                        (None, None) => scene,
                    };
                    // The persisted scene must carry every supplied
                    // action; the prior `skip_deserializing` on the
                    // `actions` field silently dropped them, leaving
                    // the REST endpoint a no-op for the action list.
                    for action in body.actions {
                        scene = scene.add_action(action);
                    }
                    match hub.add_scene(scene).await {
                        Ok(()) => json_body(StatusCode::CREATED, &serde_json::json!({"status": "created"})),
                        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
                    }
                }
                Err(resp) => resp,
            }
        }
        (&Method::GET, ["api", "scenes", id]) => {
            match hub.get_scene(id).await {
                Some(scene) => json_body(StatusCode::OK, &scene),
                None => error_response(StatusCode::NOT_FOUND, "scene not found"),
            }
        }
        (&Method::DELETE, ["api", "scenes", id]) => match hub.remove_scene(id).await {
            Ok(()) => json_body(StatusCode::OK, &serde_json::json!({"status": "removed"})),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        },
        (&Method::POST, ["api", "scenes", "activate"]) => {
            match read_json_body::<SceneActivateRequest>(req).await {
                Ok(body) => match hub.activate_scene(&body.id).await {
                    Ok(()) => json_body(StatusCode::OK, &serde_json::json!({"status": "activated"})),
                    Err(e) => error_response(StatusCode::BAD_GATEWAY, e.to_string()),
                },
                Err(resp) => resp,
            }
        }

        // ── HomeKit endpoints ───────────────────────────────────────────────
        (&Method::GET, ["api", "homekit", "accessories"]) => {
            let accessories = hub.list_homekit_accessories().await;
            json_body(StatusCode::OK, &accessories)
        }

        _ => error_response(StatusCode::NOT_FOUND, "not found"),
    };

    Ok(resp)
}
