//! Scene management for smart home automation.
//!
//! A scene is a predefined collection of device states that can be activated
//! with a single action. Scenes simplify daily routines (e.g., "Good Morning",
//! "Movie Time", "Away Mode") by restoring multiple devices to their desired
//! states simultaneously.

use crate::error::{Result, SmartHomeError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// A scene containing multiple device actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scene {
    /// Unique scene identifier
    pub id: String,
    /// Human-readable scene name
    pub name: String,
    /// Optional room/location
    pub room: Option<String>,
    /// Whether this scene is currently active
    pub active: bool,
    /// Icon or category for UI
    pub icon: Option<String>,
    /// Actions to execute when scene is activated
    pub actions: Vec<SceneAction>,
}

/// A single action within a scene.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneAction {
    /// Set a device property to a value
    SetProperty {
        device_id: String,
        siid: u32,
        piid: u32,
        value: serde_json::Value,
    },
    /// Wait for a specified duration
    Delay {
        millis: u64,
    },
    /// Invoke a MIoT action
    InvokeAction {
        device_id: String,
        siid: u32,
        aiid: u32,
        input: Vec<serde_json::Value>,
    },
}

impl Scene {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            room: None,
            active: false,
            icon: None,
            actions: Vec::new(),
        }
    }

    pub fn with_room(mut self, room: impl Into<String>) -> Self {
        self.room = Some(room.into());
        self
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn add_action(mut self, action: SceneAction) -> Self {
        self.actions.push(action);
        self
    }
}

/// Scene manager for storing and executing scenes.
pub struct SceneManager {
    scenes: Arc<RwLock<HashMap<String, Scene>>>,
    data_dir: String,
}

impl SceneManager {
    pub fn new(data_dir: impl Into<String>) -> Self {
        Self {
            scenes: Arc::new(RwLock::new(HashMap::new())),
            data_dir: data_dir.into(),
        }
    }

    /// Load scenes from disk.
    pub async fn load(&self) -> Result<()> {
        let path = format!("{}/scenes.json", self.data_dir);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                let loaded: Vec<Scene> = serde_json::from_str(&content)
                    .map_err(|e| SmartHomeError::Storage(format!("parse scenes.json: {}", e)))?;
                let mut guard = self.scenes.write().await;
                for scene in loaded {
                    guard.insert(scene.id.clone(), scene);
                }
                info!("Loaded {} scenes from {}", guard.len(), path);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!("No scenes.json found, starting fresh");
                Ok(())
            }
            Err(e) => Err(SmartHomeError::Storage(format!("read scenes.json: {}", e))),
        }
    }

    /// Save scenes to disk.
    pub async fn save(&self) -> Result<()> {
        let guard = self.scenes.read().await;
        let scenes: Vec<&Scene> = guard.values().collect();
        let content = serde_json::to_string_pretty(&scenes)?;
        let path = format!("{}/scenes.json", self.data_dir);
        tokio::fs::create_dir_all(&self.data_dir).await
            .map_err(|e| SmartHomeError::Storage(format!("create data dir: {}", e)))?;
        tokio::fs::write(&path, content).await
            .map_err(|e| SmartHomeError::Storage(format!("write scenes.json: {}", e)))?;
        debug!("Saved {} scenes to {}", scenes.len(), path);
        Ok(())
    }

    /// Add or update a scene.
    pub async fn add(&self, scene: Scene) -> Result<()> {
        let id = scene.id.clone();
        self.scenes.write().await.insert(id.clone(), scene);
        debug!("Scene added/updated: {}", id);
        self.save().await
    }

    /// Get a scene by ID.
    pub async fn get(&self, id: &str) -> Option<Scene> {
        self.scenes.read().await.get(id).cloned()
    }

    /// List all scenes.
    pub async fn list_all(&self) -> Vec<Scene> {
        self.scenes.read().await.values().cloned().collect()
    }

    /// List scenes by room.
    pub async fn list_by_room(&self, room: &str) -> Vec<Scene> {
        self.scenes
            .read()
            .await
            .values()
            .filter(|s| s.room.as_deref() == Some(room))
            .cloned()
            .collect()
    }

    /// Remove a scene.
    pub async fn remove(&self, id: &str) -> Result<()> {
        let removed = self.scenes.write().await.remove(id).is_some();
        if !removed {
            return Err(SmartHomeError::Storage(format!("scene not found: {}", id)));
        }
        debug!("Scene removed: {}", id);
        self.save().await
    }

    /// Set a scene as active (only one scene can be active at a time).
    pub async fn set_active(&self, id: &str) -> Result<()> {
        let mut guard = self.scenes.write().await;
        
        // Deactivate all scenes
        for scene in guard.values_mut() {
            scene.active = false;
        }
        
        // Activate the requested scene
        if let Some(scene) = guard.get_mut(id) {
            scene.active = true;
            info!("Scene activated: {}", id);
        } else {
            return Err(SmartHomeError::Storage(format!("scene not found: {}", id)));
        }
        
        drop(guard);
        self.save().await
    }

    /// Get the currently active scene.
    pub async fn get_active(&self) -> Option<Scene> {
        self.scenes
            .read()
            .await
            .values()
            .find(|s| s.active)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> String {
        std::env::temp_dir()
            .join(format!("adnet-scene-test-{}", uuid_short()))
            .to_string_lossy()
            .into_owned()
    }

    fn uuid_short() -> String {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..8).map(|_| format!("{:x}", rng.gen_range(0..16u8))).collect()
    }

    #[tokio::test]
    async fn add_and_get_scene() {
        let mgr = SceneManager::new(temp_dir());
        let scene = Scene::new("morning", "Good Morning").with_room("Bedroom");
        mgr.add(scene).await.unwrap();

        let found = mgr.get("morning").await.unwrap();
        assert_eq!(found.name, "Good Morning");
        assert_eq!(found.room, Some("Bedroom".to_string()));
    }

    #[tokio::test]
    async fn list_by_room() {
        let mgr = SceneManager::new(temp_dir());
        mgr.add(Scene::new("s1", "Scene 1").with_room("Living Room")).await.unwrap();
        mgr.add(Scene::new("s2", "Scene 2").with_room("Bedroom")).await.unwrap();
        mgr.add(Scene::new("s3", "Scene 3").with_room("Living Room")).await.unwrap();

        let living = mgr.list_by_room("Living Room").await;
        assert_eq!(living.len(), 2);
    }

    #[tokio::test]
    async fn set_active_scene() {
        let mgr = SceneManager::new(temp_dir());
        mgr.add(Scene::new("s1", "Scene 1")).await.unwrap();
        mgr.add(Scene::new("s2", "Scene 2")).await.unwrap();

        mgr.set_active("s1").await.unwrap();
        assert!(mgr.get("s1").await.unwrap().active);
        assert!(!mgr.get("s2").await.unwrap().active);

        mgr.set_active("s2").await.unwrap();
        assert!(!mgr.get("s1").await.unwrap().active);
        assert!(mgr.get("s2").await.unwrap().active);
    }

    #[tokio::test]
    async fn scene_persistence() {
        let dir = temp_dir();
        {
            let mgr = SceneManager::new(dir.clone());
            mgr.add(Scene::new("s1", "Scene 1")).await.unwrap();
        }

        let mgr2 = SceneManager::new(dir.clone());
        mgr2.load().await.unwrap();
        let scenes = mgr2.list_all().await;
        assert_eq!(scenes.len(), 1);
        assert_eq!(scenes[0].id, "s1");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
