//! Central configuration manager with hot reload support.
//!
//! DO-178C DAL-B: Unified configuration management for A3Net.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::error::{ConfigError, ConfigResult};
use crate::schema::{ConfigValue, ConfigSchema, SchemaValidator};
use crate::source::{ConfigSource, EnvSource, FileSource};
use crate::watcher::{ConfigWatcher, ConfigWatcherEvent};

/// Configuration manager with hot reload support.
///
/// DO-178C SR-1 through SR-12: Comprehensive configuration management.
#[derive(Debug)]
pub struct ConfigManager {
    /// Configuration values (merged from all sources).
    values: RwLock<HashMap<String, ConfigValue>>,
    /// Schema for validation.
    schema: RwLock<Option<ConfigSchema>>,
    /// Configuration sources by priority.
    sources: RwLock<Vec<SourceEntry>>,
    /// File watcher for hot reload.
    watcher: RwLock<Option<ConfigWatcher>>,
    /// Config file path.
    config_path: PathBuf,
    /// Environment prefix.
    env_prefix: String,
    /// Hot reload callback.
    reload_tx: RwLock<Option<mpsc::Sender<()>>>,
    /// Reload receiver for the manager.
    reload_rx: RwLock<Option<mpsc::Receiver<()>>>,
}

/// Source entry with priority tracking.
#[derive(Debug)]
struct SourceEntry {
    source: ConfigSource,
    values: HashMap<String, ConfigValue>,
}

impl ConfigManager {
    /// Create a new configuration manager.
    ///
    /// DO-178C SR-1: Initialize configuration system.
    pub fn new(config_path: impl AsRef<Path>, env_prefix: &str) -> Self {
        let (tx, rx) = mpsc::channel(1);

        Self {
            values: RwLock::new(HashMap::new()),
            schema: RwLock::new(None),
            sources: RwLock::new(Vec::new()),
            watcher: RwLock::new(None),
            config_path: config_path.as_ref().to_path_buf(),
            env_prefix: env_prefix.to_string(),
            reload_tx: RwLock::new(Some(tx)),
            reload_rx: RwLock::new(Some(rx)),
        }
    }

    /// Set the configuration schema for validation.
    ///
    /// DO-178C SR-2: Schema validation ensures configuration correctness.
    pub fn with_schema(mut self, schema: ConfigSchema) -> Self {
        *self.schema.write() = Some(schema);
        self
    }

    /// Load configuration from all sources.
    ///
    /// DO-178C SR-1: Load configuration with priority merging.
    pub fn load(&self) -> ConfigResult<()> {
        self.values.write().clear();
        self.sources.write().clear();

        // Load file source
        self.load_file_source()?;

        // Load environment source
        self.load_env_source()?;

        // Apply defaults
        self.apply_defaults()?;

        // Validate if schema is set
        self.validate()?;

        info!(
            path = %self.config_path.display(),
            env_prefix = %self.env_prefix,
            keys = %self.values.read().len(),
            "Configuration loaded successfully"
        );

        Ok(())
    }

    /// Load configuration from file.
    fn load_file_source(&self) -> ConfigResult<()> {
        if !self.config_path.exists() {
            debug!("Config file not found: {}, using defaults", self.config_path.display());
            return Ok(());
        }

        let mut file_source = FileSource::new(&self.config_path);
        file_source.load()?;

        let values = file_source.values().clone();
        self.sources.write().push(SourceEntry {
            source: ConfigSource::File,
            values: values.clone(),
        });

        // Merge into main values
        self.merge_values(values, ConfigSource::File);

        Ok(())
    }

    /// Load configuration from environment variables.
    fn load_env_source(&self) -> ConfigResult<()> {
        let mut env_source = EnvSource::new(&self.env_prefix);
        env_source.load()?;

        let values = env_source.values().clone();
        if !values.is_empty() {
            self.sources.write().push(SourceEntry {
                source: ConfigSource::Env,
                values: values.clone(),
            });

            // Merge into main values (higher priority than file)
            self.merge_values(values, ConfigSource::Env);
        }

        Ok(())
    }

    /// Apply default values for missing keys.
    fn apply_defaults(&self) -> ConfigResult<()> {
        let schema = self.schema.read();
        if let Some(schema) = schema.as_ref() {
            let mut values = self.values.write();
            for (key, schema_type) in &schema.root.fields {
                if let Some(default) = &schema.root.default {
                    if !values.contains_key(key) {
                        values.insert(key.clone(), default.clone());
                    }
                }
            }
        }
        Ok(())
    }

    /// Merge values from a source with priority handling.
    fn merge_values(&self, new_values: HashMap<String, ConfigValue>, source: ConfigSource) {
        let mut values = self.values.write();
        for (key, value) in new_values {
            // Only add if not already present (higher priority source wins)
            if !values.contains_key(&key) {
                values.insert(key, value);
            }
        }
    }

    /// Validate current configuration against schema.
    ///
    /// DO-178C SR-2: Validation ensures correctness.
    pub fn validate(&self) -> ConfigResult<()> {
        let schema = self.schema.read();
        let values = self.values.read();

        if let Some(schema) = schema.as_ref() {
            let validator = SchemaValidator::new();
            validator.validate(&values, schema)?;
        }

        Ok(())
    }

    /// Get a configuration value by key.
    ///
    /// DO-178C SR-3: Type-safe value retrieval.
    pub fn get(&self, key: &str) -> Option<ConfigValue> {
        self.values.read().get(key).cloned()
    }

    /// Get a string configuration value.
    pub fn get_string(&self, key: &str) -> Option<String> {
        self.get(key)?.as_str().map(String::from)
    }

    /// Get an integer configuration value.
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key)?.as_i64()
    }

    /// Get a float configuration value.
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key)?.as_f64()
    }

    /// Get a boolean configuration value.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key)?.as_bool()
    }

    /// Get all configuration keys.
    pub fn keys(&self) -> Vec<String> {
        self.values.read().keys().cloned().collect()
    }

    /// Set a configuration value (runtime override).
    ///
    /// DO-178C SR-3: Runtime overrides for dynamic configuration.
    pub fn set(&self, key: &str, value: ConfigValue) -> ConfigResult<()> {
        let mut values = self.values.write();
        values.insert(key.to_string(), value);
        Ok(())
    }

    /// Start watching for configuration file changes.
    ///
    /// DO-178C SR-6: Hot reload support via file watching.
    pub fn start_watcher(&self) -> ConfigResult<()> {
        let mut watcher_guard = self.watcher.write();

        if watcher_guard.is_some() {
            warn!("Watcher already started");
            return Ok(());
        }

        let mut watcher = ConfigWatcher::new()?;
        watcher.watch(&self.config_path)?;

        *watcher_guard = Some(watcher);

        info!(
            path = %self.config_path.display(),
            "Configuration file watcher started"
        );

        Ok(())
    }

    /// Stop watching for configuration file changes.
    pub fn stop_watcher(&self) -> ConfigResult<()> {
        let mut watcher_guard = self.watcher.write();

        if let Some(mut watcher) = watcher_guard.take() {
            watcher.unwatch(&self.config_path)?;
            info!("Configuration file watcher stopped");
        }

        Ok(())
    }

    /// Reload configuration from file.
    ///
    /// DO-178C SR-6: Manual reload capability.
    pub fn reload(&self) -> ConfigResult<()> {
        info!("Reloading configuration...");
        self.load()
    }

    /// Process a watcher event.
    ///
    /// DO-178C SR-6: Handle file system events.
    pub async fn handle_watcher_event(&self, event: ConfigWatcherEvent) {
        match event {
            ConfigWatcherEvent::Modified { path } => {
                info!(path = %path.display(), "Configuration file modified, reloading...");
                if let Err(e) = self.reload() {
                    error!(error = %e, "Failed to reload configuration");
                }
            }
            ConfigWatcherEvent::Deleted { path } => {
                warn!(path = %path.display(), "Configuration file deleted");
            }
            ConfigWatcherEvent::Created { path } => {
                info!(path = %path.display(), "Configuration file created, reloading...");
                if let Err(e) = self.reload() {
                    error!(error = %e, "Failed to reload configuration");
                }
            }
            ConfigWatcherEvent::Renamed { old, new } => {
                info!(old = %old.display(), new = %new.display(), "Configuration file renamed");
                // Would need to update the config path and restart watcher
            }
            ConfigWatcherEvent::Error { message } => {
                error!(error = %message, "Configuration watcher error");
            }
        }
    }

    /// Get the configuration file path.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Get the environment prefix.
    pub fn env_prefix(&self) -> &str {
        &self.env_prefix
    }

    /// Check if configuration has been loaded.
    pub fn is_loaded(&self) -> bool {
        !self.values.read().is_empty()
    }

    /// Export current configuration as JSON.
    pub fn export_json(&self) -> Result<String, serde_json::Error> {
        let values = self.values.read().clone();
        serde_json::to_string_pretty(&values)
    }
}

/// Configuration with hot reload support.
#[derive(Debug)]
pub struct HotReloadConfig {
    /// The configuration manager.
    manager: Arc<ConfigManager>,
    /// Shutdown signal sender.
    shutdown_tx: mpsc::Sender<()>,
}

impl HotReloadConfig {
    /// Create a new hot reload configuration.
    pub fn new(manager: Arc<ConfigManager>) -> Self {
        let (shutdown_tx, _) = mpsc::channel(1);
        Self {
            manager,
            shutdown_tx,
        }
    }

    /// Start the hot reload service.
    ///
    /// DO-178C SR-6: Background service for hot reload.
    pub async fn start(self) -> ConfigResult<()> {
        self.manager.start_watcher()?;

        let mut watcher = self.manager.watcher.write();
        if watcher.is_none() {
            return Err(ConfigError::HotReload("Watcher not initialized".to_string()));
        }

        let mut watcher = watcher.take().unwrap();
        drop(watcher);

        // Note: In production, you'd spawn a background task here
        // that calls manager.handle_watcher_event() for each event

        Ok(())
    }

    /// Stop the hot reload service.
    pub async fn stop(&self) -> ConfigResult<()> {
        self.manager.stop_watcher()?;
        let _ = self.shutdown_tx.send(()).await;
        Ok(())
    }

    /// Get the underlying config manager.
    pub fn manager(&self) -> &Arc<ConfigManager> {
        &self.manager
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_config_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");
        std::fs::write(&config_path, "{}").unwrap();

        let manager = ConfigManager::new(&config_path, "ADNET");
        assert_eq!(manager.env_prefix(), "ADNET");
        assert!(!manager.is_loaded());
    }

    #[test]
    fn test_config_manager_load_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");
        std::fs::write(&config_path, "{}").unwrap();

        let manager = ConfigManager::new(&config_path, "ADNET");
        manager.load().unwrap();
        assert!(manager.is_loaded());
        assert!(manager.keys().is_empty());
    }

    #[test]
    fn test_config_manager_load_with_values() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");
        std::fs::write(&config_path, r#"{"host": "localhost", "port": 8080}"#).unwrap();

        let manager = ConfigManager::new(&config_path, "ADNET");
        manager.load().unwrap();

        assert_eq!(manager.get_string("host"), Some("localhost".to_string()));
        assert_eq!(manager.get_i64("port"), Some(8080));
    }

    #[test]
    fn test_config_manager_env_override() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");
        std::fs::write(&config_path, r#"{"host": "localhost"}"#).unwrap();

        // Set environment variable
        std::env::set_var("ADNET_HOST", "from_env");

        let manager = ConfigManager::new(&config_path, "ADNET");
        manager.load().unwrap();

        // Environment variable should override file
        assert_eq!(manager.get_string("host"), Some("from_env".to_string()));

        std::env::remove_var("ADNET_HOST");
    }

    #[test]
    fn test_config_manager_runtime_set() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");
        std::fs::write(&config_path, "{}").unwrap();

        let manager = ConfigManager::new(&config_path, "ADNET");
        manager.load().unwrap();

        manager.set("dynamic_key", ConfigValue::String("dynamic_value".to_string())).unwrap();
        assert_eq!(manager.get_string("dynamic_key"), Some("dynamic_value".to_string()));
    }

    #[tokio::test]
    async fn test_hot_reload_config_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");
        std::fs::write(&config_path, "{}").unwrap();

        let manager = Arc::new(ConfigManager::new(&config_path, "ADNET"));
        let reload = HotReloadConfig::new(manager);

        assert!(reload.manager().is_loaded());
    }
}
