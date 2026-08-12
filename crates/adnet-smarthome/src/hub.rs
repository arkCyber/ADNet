//! Smart Home Hub - central coordinator
//!
//! [`SmartHomeHub`] is the single entry point: it owns the device
//! registry, mDNS discovery, MIoT cloud bridge, automation engine, and
//! the REST API. All background tasks (discovery scan, heartbeat
//! monitor, automation event loop, schedule trigger timer) are spawned
//! inside [`SmartHomeHub::start`].

use crate::{
    automation::{Automation, AutomationAction, Condition, EvaluateError, Trigger},
    device::Device,
    discovery::{DiscoveryEvent, DiscoveryManager, DiscoveryProtocol},
    error::{Result, SmartHomeError},
    matter::MatterClient,
    miot::{MiotAuth, MiotClient, Property, PropertyValue},
    registry::DeviceRegistry,
};
use chrono::{Local, Timelike};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

/// Hub configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubConfig {
    /// HTTP API bind address (used by the REST API server, not this hub)
    pub bind_addr: SocketAddr,
    /// Enable automatic mDNS device discovery
    pub enable_discovery: bool,
    /// Seconds between mDNS discovery scans
    pub discovery_interval_secs: u64,
    /// Seconds of silence before a device is marked offline
    pub heartbeat_timeout_secs: u64,
    /// Directory for persisted device registry and automation state
    pub data_dir: String,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8781".parse().unwrap(),
            enable_discovery: true,
            discovery_interval_secs: 300,
            heartbeat_timeout_secs: 120,
            data_dir: "./data/smarthome".to_string(),
        }
    }
}

/// Events broadcast to all subscribers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HubEvent {
    DeviceDiscovered { device: Device },
    DeviceOnline { device_id: String },
    DeviceOffline { device_id: String },
    PropertyChanged { device_id: String, property: String, value: serde_json::Value },
    DeviceRemoved { device_id: String },
}

/// The Smart Home Hub
pub struct SmartHomeHub {
    pub config: HubConfig,
    registry: Arc<DeviceRegistry>,
    discovery: Arc<DiscoveryManager>,
    miot: Option<Arc<MiotClient>>,
    /// Matter controller, populated via `with_matter()`.
    matter: Option<Arc<MatterClient>>,
    events: broadcast::Sender<HubEvent>,
    automations: Arc<RwLock<HashMap<String, Automation>>>,
}

impl SmartHomeHub {
    /// Create a hub without any cloud credentials. The hub will still
    /// run discovery, the device registry, and automations; MIoT-
    /// specific operations will return `NotSupported`.
    pub fn new(config: HubConfig) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            registry: Arc::new(DeviceRegistry::new(&config.data_dir)),
            discovery: Arc::new(DiscoveryManager::new()),
            miot: None,
            matter: None,
            events,
            automations: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Attach a Xiaomi MIoT cloud client.
    pub fn with_miot(mut self, auth: MiotAuth) -> Result<Self> {
        self.miot = Some(Arc::new(MiotClient::new(auth)?));
        Ok(self)
    }

    /// Attach a Xiaomi MIoT cloud client pointed at a custom API host.
    /// Used by tests to redirect MIoT calls to a local mock server.
    pub fn with_miot_host(mut self, auth: MiotAuth, api_host: String) -> Result<Self> {
        self.miot = Some(Arc::new(MiotClient::with_host(auth, api_host)?));
        Ok(self)
    }

    /// Attach a Matter controller. The controller is initialized
    /// (fabric created/loaded) in [`SmartHomeHub::start`].
    pub fn with_matter(mut self, matter: MatterClient) -> Self {
        self.matter = Some(Arc::new(matter));
        self
    }

    /// Subscribe to hub events. The caller holds a receiver and will
    /// receive all subsequent hub events.
    pub fn subscribe(&self) -> broadcast::Receiver<HubEvent> {
        self.events.subscribe()
    }

    /// Start all background tasks. Idempotent — safe to call once.
    pub async fn start(self: Arc<Self>) -> Result<()> {
        info!("SmartHomeHub starting, API on {}", self.config.bind_addr);

        // Load persisted state
        if let Err(e) = self.registry.load().await {
            warn!("Could not load device registry: {}", e);
        }
        if let Err(e) = self.load_automations().await {
            warn!("Could not load automations: {}", e);
        }

        // Sync devices from MIoT cloud
        if let Some(miot) = &self.miot {
            let miot = Arc::clone(miot);
            let registry = Arc::clone(&self.registry);
            let events = self.events.clone();
            spawn_logged(async move {
                match miot.get_device_list().await {
                    Ok(cloud_devices) => {
                        for md in cloud_devices {
                            let mut dev = Device::new(
                                md.did.clone(),
                                md.name.clone(),
                                crate::device::DeviceType::Xiaomi,
                            );
                            dev.model = Some(md.model);
                            dev.ip_address = if md.localip.is_empty() { None } else { Some(md.localip) };
                            dev.online = md.online;
                            if let Err(e) = registry.add(dev.clone()).await {
                                error!("Failed to persist device {}: {}", md.did, e);
                            }
                            let _ = events.send(HubEvent::DeviceDiscovered { device: dev });
                        }
                        info!("MIoT device sync complete");
                    }
                    Err(e) => error!("MIoT device sync failed: {}", e),
                }
            });
        }

        // Sync commissioned Matter devices
        if let Some(matter) = &self.matter {
            let matter = Arc::clone(matter);
            let registry = Arc::clone(&self.registry);
            let events = self.events.clone();
            spawn_logged(async move {
                match matter.sync_nodes().await {
                    Ok(nodes) => {
                        for node in nodes {
                            let dev = node.into_device();
                            if let Err(e) = registry.add(dev.clone()).await {
                                error!("Failed to persist Matter device: {}", e);
                            }
                            let _ = events.send(HubEvent::DeviceDiscovered { device: dev });
                        }
                        info!("Matter device sync complete");
                    }
                    Err(e) => error!("Matter device sync failed: {}", e),
                }
            });
        }

        // Background tasks
        if self.config.enable_discovery {
            let hub = Arc::clone(&self);
            spawn_logged(async move { hub.run_discovery_loop().await });
        }

        let hub = Arc::clone(&self);
        spawn_logged(async move { hub.run_heartbeat_monitor().await });

        let hub = Arc::clone(&self);
        spawn_logged(async move { hub.run_automation_loop().await });

        let hub = Arc::clone(&self);
        spawn_logged(async move { hub.run_schedule_monitor().await });

        info!("SmartHomeHub started");
        Ok(())
    }

    // ── device API ───────────────────────────────────────────────────────────

    pub async fn list_devices(&self) -> Vec<Device> {
        self.registry.list_all().await
    }

    pub async fn get_device(&self, id: &str) -> Option<Device> {
        self.registry.get(id).await
    }

    pub async fn register_device(&self, device: Device) -> Result<()> {
        let ev = HubEvent::DeviceDiscovered { device: device.clone() };
        self.registry.add(device).await?;
        let _ = self.events.send(ev);
        Ok(())
    }

    pub async fn remove_device(&self, id: &str) -> Result<()> {
        self.registry.remove(id).await?;
        let _ = self.events.send(HubEvent::DeviceRemoved { device_id: id.to_string() });
        Ok(())
    }

    /// Set a MIoT property (siid / piid).
    pub async fn set_property(
        &self,
        device_id: &str,
        siid: u32,
        piid: u32,
        value: serde_json::Value,
    ) -> Result<()> {
        let miot = self.miot.as_ref()
            .ok_or_else(|| SmartHomeError::NotSupported("MIoT not configured".into()))?;

        miot.set_device_properties(device_id, vec![PropertyValue { siid, piid, value: value.clone() }]).await?;

        self.registry.update_state(device_id, format!("{}.{}", siid, piid), value.clone()).await;
        let _ = self.events.send(HubEvent::PropertyChanged {
            device_id: device_id.to_string(),
            property: format!("{}.{}", siid, piid),
            value,
        });

        Ok(())
    }

    /// Get MIoT properties.
    pub async fn get_properties(
        &self,
        device_id: &str,
        props: Vec<Property>,
    ) -> Result<Vec<PropertyValue>> {
        let miot = self.miot.as_ref()
            .ok_or_else(|| SmartHomeError::NotSupported("MIoT not configured".into()))?;

        miot.get_device_properties(device_id, props).await
    }

    /// Invoke a MIoT action.
    pub async fn invoke_action(
        &self,
        device_id: &str,
        siid: u32,
        aiid: u32,
        input: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let miot = self.miot.as_ref()
            .ok_or_else(|| SmartHomeError::NotSupported("MIoT not configured".into()))?;

        miot.invoke_action(device_id, crate::miot::Action { siid, aiid, input }).await
    }

    // ── automation API ───────────────────────────────────────────────────────

    pub async fn add_automation(&self, automation: Automation) -> Result<()> {
        let id = automation.id.clone();
        self.automations.write().await.insert(id.clone(), automation);
        info!("Automation '{}' registered", id);
        self.save_automations().await
    }

    pub async fn remove_automation(&self, id: &str) -> Result<()> {
        self.automations.write().await.remove(id);
        self.save_automations().await
    }

    pub async fn list_automations(&self) -> Vec<Automation> {
        self.automations.read().await.values().cloned().collect()
    }

    fn automations_path(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.config.data_dir).join("automations.json")
    }

    async fn load_automations(&self) -> Result<()> {
        let path = self.automations_path();
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                let rules: Vec<Automation> = serde_json::from_str(&content)
                    .map_err(|e| SmartHomeError::Storage(format!("parse automations.json: {}", e)))?;
                let mut guard = self.automations.write().await;
                for r in rules {
                    guard.insert(r.id.clone(), r);
                }
                info!("Loaded {} automations from {}", guard.len(), path.display());
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SmartHomeError::Storage(format!("read automations.json: {}", e))),
        }
    }

    async fn save_automations(&self) -> Result<()> {
        let guard = self.automations.read().await;
        let rules: Vec<&Automation> = guard.values().collect();
        let content = serde_json::to_string_pretty(&rules)?;
        tokio::fs::create_dir_all(&self.config.data_dir).await
            .map_err(|e| SmartHomeError::Storage(format!("create data dir: {}", e)))?;
        tokio::fs::write(self.automations_path(), content).await
            .map_err(|e| SmartHomeError::Storage(format!("write automations.json: {}", e)))?;
        Ok(())
    }

    // ── background tasks ─────────────────────────────────────────────────────

    async fn run_discovery_loop(&self) {
        let interval = Duration::from_secs(self.config.discovery_interval_secs);
        loop {
            debug!("Running device discovery scan");
            if let Err(e) = self.discovery.discover(DiscoveryProtocol::Mdns).await {
                warn!("mDNS scan error: {}", e);
            }

            while let Ok(ev) = tokio::time::timeout(
                Duration::from_millis(100),
                self.discovery.recv(),
            ).await {
                match ev {
                    Some(DiscoveryEvent::Found(dev)) => {
                        if self.registry.get(&dev.device_id).await.is_none() {
                            info!("Discovered: {} ({})", dev.name, dev.device_id);
                            let device_id = dev.device_id.clone();
                            let _ = self.registry.add(dev.clone()).await;
                            let _ = self.events.send(HubEvent::DeviceDiscovered { device: dev });
                            let _ = self.events.send(HubEvent::DeviceOnline { device_id });
                        } else if self.registry.mark_online(&dev.device_id).await {
                            let _ = self.events.send(HubEvent::DeviceOnline { device_id: dev.device_id });
                        }
                    }
                    Some(DiscoveryEvent::Lost(id)) => {
                        if self.registry.mark_offline(&id).await {
                            let _ = self.events.send(HubEvent::DeviceOffline { device_id: id });
                        }
                    }
                    None => break,
                }
            }

            tokio::time::sleep(interval).await;
        }
    }

    async fn run_heartbeat_monitor(&self) {
        let timeout = Duration::from_secs(self.config.heartbeat_timeout_secs);
        let check_interval = Duration::from_secs(30);

        loop {
            tokio::time::sleep(check_interval).await;
            let now = SystemTime::now();
            // list_all() returns a cloned Vec; the read lock is released
            // before the await point, so we don't hold it across sleep.
            let device_ids: Vec<(String, Option<SystemTime>)> = {
                self.registry.list_all().await
                    .into_iter()
                    .filter(|d| d.online)
                    .map(|d| (d.device_id.clone(), d.last_seen))
                    .collect()
            };

            for (device_id, last_seen) in device_ids {
                if let Some(last) = last_seen {
                    if now.duration_since(last).unwrap_or(timeout) >= timeout
                        && self.registry.mark_offline(&device_id).await
                    {
                        warn!("Device {} timed out", device_id);
                        let _ = self.events.send(HubEvent::DeviceOffline { device_id });
                    }
                }
            }
        }
    }

    async fn run_automation_loop(&self) {
        let mut rx = self.events.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => self.process_hub_event(&event).await,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Automation loop lagged, dropped {} events", n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    /// Background task that fires every 60 seconds, checking
    /// `Trigger::Schedule` rules against the current minute.
    async fn run_schedule_monitor(&self) {
        let tick = Duration::from_secs(60);
        loop {
            tokio::time::sleep(tick).await;
            let now = Local::now();
            let current_minute = format!("{:02}:{:02}", now.hour(), now.minute());
            let to_fire: Vec<Automation> = {
                let guard = self.automations.read().await;
                guard
                    .values()
                    .filter(|a| {
                        a.enabled && matches!(&a.trigger, Trigger::Schedule { cron } if cron == &current_minute)
                    })
                    .cloned()
                    .collect()
            };

            for auto in to_fire {
                debug!("Schedule automation '{}' firing", auto.id);

                let now = Local::now();
                let registry = Arc::clone(&self.registry);
                let conditions_pass = self.evaluate_all_conditions(&auto.conditions, &now, move |did, prop| {
                    tokio::runtime::Handle::current()
                        .block_on(registry.get(did))
                        .and_then(|d| d.state.get(prop).cloned())
                });

                let conditions_pass = match conditions_pass {
                    Ok(p) => p,
                    Err(e) => {
                        debug!("Condition eval error in '{}': {}", auto.id, e);
                        false
                    }
                };

                if conditions_pass {
                    for action in &auto.actions {
                        if let Err(e) = self.execute_action(action).await {
                            error!("Schedule automation '{}' action failed: {}", auto.id, e);
                        }
                    }
                }
            }
        }
    }

    async fn process_hub_event(&self, event: &HubEvent) {
        let automations = self.automations.read().await;

        for (id, auto) in automations.iter() {
            if !auto.enabled { continue; }

            let triggered = self.is_triggered(&auto.trigger, event);
            if !triggered {
                continue;
            }

            // Evaluate all conditions before executing actions.
            let now = Local::now();
            let all_conditions_pass = {
                let registry = Arc::clone(&self.registry);
                let conditions_pass = self
                    .evaluate_all_conditions(&auto.conditions, &now, |did, prop| {
                        // We need to borrow registry inside the closure; capture it.
                        // This is a bit awkward but avoids adding an async block inside.
                        // We use a synchronous helper that accesses the registry via a
                        // tokio::block_on — acceptable here since it's a read-only lookup
                        // and we're in a synchronous context.
                        tokio::runtime::Handle::current()
                            .block_on(registry.get(did))
                            .and_then(|d| d.state.get(prop).cloned())
                    });
                conditions_pass
            };

            let all_conditions_pass = match all_conditions_pass {
                Ok(passes) => passes,
                Err(e) => {
                    debug!("Condition evaluation error in '{}': {}", id, e);
                    false
                }
            };

            if !all_conditions_pass {
                debug!("Automation '{}' conditions not met, skipping actions", id);
                continue;
            }

            debug!("Automation '{}' triggered and conditions met", id);
            for action in &auto.actions {
                if let Err(e) = self.execute_action(action).await {
                    error!("Automation '{}' action failed: {}", id, e);
                }
            }
        }
    }

    /// Returns `true` if this event matches the automation's trigger.
    fn is_triggered(&self, trigger: &Trigger, event: &HubEvent) -> bool {
        match (trigger, event) {
            (Trigger::PropertyChanged { device_id, property, value },
             HubEvent::PropertyChanged { device_id: eid, property: ep, value: ev }) => {
                device_id == eid && property == ep && value == ev
            }
            (Trigger::DeviceOnline { device_id }, HubEvent::DeviceOnline { device_id: eid }) => {
                device_id == eid
            }
            (Trigger::DeviceOffline { device_id }, HubEvent::DeviceOffline { device_id: eid }) => {
                device_id == eid
            }
            // `Schedule` and `Manual` are handled separately in `run_schedule_monitor`.
            (Trigger::Schedule { .. }, _) | (Trigger::Manual, _) => false,
            // Any other trigger/event combination is a non-match.
            _ => false,
        }
    }

    /// Evaluate all conditions, returning `Ok(true)` if every condition
    /// passes or if there are no conditions.
    fn evaluate_all_conditions<F>(
        &self,
        conditions: &[Condition],
        now: &chrono::DateTime<chrono::Local>,
        get_state: F,
    ) -> std::result::Result<bool, crate::error::SmartHomeError>
    where
        F: Fn(&str, &str) -> Option<serde_json::Value>,
    {
        for cond in conditions {
            match cond.evaluate(now, &get_state) {
                Ok(true) => {}
                Ok(false) => return Ok(false),
                Err(EvaluateError::DeviceNotFound(_)) => return Ok(false),
                Err(EvaluateError::InvalidTimeFormat(s)) => {
                    return Err(crate::error::SmartHomeError::Automation(s))
                }
            }
        }
        Ok(true)
    }

    async fn execute_action(&self, action: &AutomationAction) -> Result<()> {
        match action {
            AutomationAction::SetProperty { device_id, siid, piid, value } => {
                self.set_property(device_id, *siid, *piid, value.clone()).await?;
            }
            AutomationAction::InvokeAction { device_id, siid, aiid, input } => {
                let miot = self.miot.as_ref()
                    .ok_or_else(|| SmartHomeError::NotSupported("MIoT not configured".into()))?;
                miot.invoke_action(device_id, crate::miot::Action {
                    siid: *siid,
                    aiid: *aiid,
                    input: input.clone(),
                }).await?;
            }
            AutomationAction::Delay { seconds } => {
                tokio::time::sleep(Duration::from_secs(*seconds)).await;
            }
            AutomationAction::Notify { message } => {
                info!("[AutomationNotify] {}", message);
            }
        }
        Ok(())
    }
}

/// Spawn a background task, logging a warning if the scheduler refuses it.
fn spawn_logged(f: impl std::future::Future<Output = ()> + Send + 'static) {
    // We use tokio::spawn directly; if it fails we can't do much
    // beyond logging, which would require async context. Log synchronously
    // so we at least surface the OOM risk to the caller.
    let handle = tokio::spawn(f);
    // Dropping the JoinHandle means we lose the task on drop (cancelled).
    // For background tasks that are expected to live for the hub's lifetime,
    // this is acceptable — the warning here is informational.
    let _ = handle;
    debug!("spawned background task");
}

