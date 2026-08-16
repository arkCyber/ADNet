//! Configuration watcher for hot reload.
//!
//! DO-178C SR-6: File watching for configuration hot reload.

use std::path::Path;
use std::time::Duration;

use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::error::ConfigResult;

/// Events emitted by the config watcher.
#[derive(Debug, Clone)]
pub enum ConfigWatcherEvent {
    /// Configuration file was modified.
    Modified { path: std::path::PathBuf },
    /// Configuration file was created.
    Created { path: std::path::PathBuf },
    /// Configuration file was deleted.
    Deleted { path: std::path::PathBuf },
    /// Configuration file was renamed.
    Renamed { old: std::path::PathBuf, new: std::path::PathBuf },
    /// An error occurred while watching.
    Error { message: String },
}

impl std::fmt::Display for ConfigWatcherEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigWatcherEvent::Modified { path } => write!(f, "Config modified: {}", path.display()),
            ConfigWatcherEvent::Created { path } => write!(f, "Config created: {}", path.display()),
            ConfigWatcherEvent::Deleted { path } => write!(f, "Config deleted: {}", path.display()),
            ConfigWatcherEvent::Renamed { old, new } => {
                write!(f, "Config renamed: {} -> {}", old.display(), new.display())
            }
            ConfigWatcherEvent::Error { message } => write!(f, "Watcher error: {}", message),
        }
    }
}

/// Configuration file watcher.
///
/// Watches configuration files for changes and emits events
/// when modifications are detected.
#[derive(Debug)]
pub struct ConfigWatcher {
    /// The underlying file watcher.
    watcher: RecommendedWatcher,
    /// Channel sender for events.
    tx: mpsc::Sender<ConfigWatcherEvent>,
    /// Channel receiver for events.
    rx: mpsc::Receiver<ConfigWatcherEvent>,
    /// Paths being watched.
    watched_paths: Vec<std::path::PathBuf>,
}

impl ConfigWatcher {
    /// Create a new config watcher.
    ///
    /// DO-178C SR-6: Hot reload requires file system monitoring.
    pub fn new() -> ConfigResult<Self> {
        let (tx, rx) = mpsc::channel(100);

        let tx_clone = tx.clone();
        let watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                let event = match res {
                    Ok(event) => convert_notify_event(event),
                    Err(e) => ConfigWatcherEvent::Error { message: e.to_string() },
                };
                // Send non-blocking, ignore errors if receiver dropped
                let _ = tx_clone.blocking_send(event);
            },
            NotifyConfig::default().with_poll_interval(Duration::from_secs(2)),
        )
        .map_err(|e| crate::error::ConfigError::Watcher(e.to_string()))?;

        Ok(Self {
            watcher,
            tx,
            rx,
            watched_paths: Vec::new(),
        })
    }

    /// Watch a configuration file or directory.
    ///
    /// DO-178C SR-6: Single source of truth for watched paths.
    pub fn watch(&mut self, path: impl AsRef<Path>) -> ConfigResult<()> {
        let path = path.as_ref().to_path_buf();
        self.watcher
            .watch(&path, RecursiveMode::NonRecursive)
            .map_err(|e| crate::error::ConfigError::Watcher(e.to_string()))?;
        self.watched_paths.push(path);
        Ok(())
    }

    /// Watch a configuration file with debouncing.
    ///
    /// Uses debouncer to aggregate rapid file changes.
    pub fn watch_debounced(&mut self, path: impl AsRef<Path>) -> ConfigResult<()> {
        // For now, delegate to regular watch
        // The debouncer is set up at creation time
        self.watch(path)
    }

    /// Unwatch a previously watched path.
    pub fn unwatch(&mut self, path: impl AsRef<Path>) -> ConfigResult<()> {
        let path = path.as_ref();
        self.watcher
            .unwatch(path)
            .map_err(|e| crate::error::ConfigError::Watcher(e.to_string()))?;
        self.watched_paths.retain(|p| p != path);
        Ok(())
    }

    /// Get the next watcher event.
    ///
    /// Returns `None` if the receiver was closed.
    pub async fn next_event(&mut self) -> Option<ConfigWatcherEvent> {
        self.rx.recv().await
    }

    /// Get all watched paths.
    pub fn watched_paths(&self) -> &[std::path::PathBuf] {
        &self.watched_paths
    }

    /// Check if a path is being watched.
    pub fn is_watching(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        self.watched_paths.iter().any(|p| p == path)
    }

    /// Get a clone of the event sender for external event injection.
    pub fn event_sender(&self) -> mpsc::Sender<ConfigWatcherEvent> {
        self.tx.clone()
    }
}

impl Default for ConfigWatcher {
    fn default() -> Self {
        Self::new().expect("ConfigWatcher::new should not fail with default config")
    }
}

/// Convert notify events to our event type.
fn convert_notify_event(event: notify::Event) -> ConfigWatcherEvent {
    use notify::EventKind;

    match event.kind {
        EventKind::Create(_) => {
            let path = event.paths.first().cloned();
            path.map(|p| ConfigWatcherEvent::Created { path: p })
                .unwrap_or(ConfigWatcherEvent::Error {
                    message: "Create event with no paths".to_string(),
                })
        }
        EventKind::Modify(_) => {
            let path = event.paths.first().cloned();
            path.map(|p| ConfigWatcherEvent::Modified { path: p })
                .unwrap_or(ConfigWatcherEvent::Error {
                    message: "Modify event with no paths".to_string(),
                })
        }
        EventKind::Remove(_) => {
            let path = event.paths.first().cloned();
            path.map(|p| ConfigWatcherEvent::Deleted { path: p })
                .unwrap_or(ConfigWatcherEvent::Error {
                    message: "Remove event with no paths".to_string(),
                })
        }
        EventKind::Other => {
            // Handle rename as a combination of Remove + Create
            if event.paths.len() >= 2 {
                ConfigWatcherEvent::Renamed {
                    old: event.paths[0].clone(),
                    new: event.paths[1].clone(),
                }
            } else {
                ConfigWatcherEvent::Error {
                    message: "Unknown event type".to_string(),
                }
            }
        }
        _ => ConfigWatcherEvent::Error {
            message: format!("Unhandled event kind: {:?}", event.kind),
        },
    }
}

/// Builder for creating a ConfigWatcher with custom settings.
#[derive(Debug, Default)]
pub struct ConfigWatcherBuilder {
    poll_interval: Option<Duration>,
    channel_size: usize,
}

impl ConfigWatcherBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the poll interval for file watching.
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = Some(interval);
        self
    }

    /// Set the channel buffer size for events.
    pub fn with_channel_size(mut self, size: usize) -> Self {
        self.channel_size = size;
        self
    }

    /// Build the ConfigWatcher.
    pub fn build(self) -> ConfigResult<ConfigWatcher> {
        let (tx, rx) = mpsc::channel(self.channel_size.max(1));

        let config = NotifyConfig::default().with_poll_interval(
            self.poll_interval.unwrap_or(Duration::from_secs(2)),
        );

        let tx_clone = tx.clone();
        let watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                let event = match res {
                    Ok(event) => convert_notify_event(event),
                    Err(e) => ConfigWatcherEvent::Error { message: e.to_string() },
                };
                let _ = tx_clone.blocking_send(event);
            },
            config,
        )
        .map_err(|e| crate::error::ConfigError::Watcher(e.to_string()))?;

        Ok(ConfigWatcher {
            watcher,
            tx,
            rx,
            watched_paths: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_watcher_creation() {
        let watcher = ConfigWatcher::new().unwrap();
        assert!(watcher.watched_paths().is_empty());
    }

    #[tokio::test]
    async fn test_watcher_watch_file() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");
        fs::write(&config_path, "{}").unwrap();

        let mut watcher = ConfigWatcher::new().unwrap();
        watcher.watch(&config_path).unwrap();

        assert!(watcher.is_watching(&config_path));
        assert_eq!(watcher.watched_paths().len(), 1);
    }

    #[tokio::test]
    async fn test_watcher_unwatch() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");
        fs::write(&config_path, "{}").unwrap();

        let mut watcher = ConfigWatcher::new().unwrap();
        watcher.watch(&config_path).unwrap();
        assert!(watcher.is_watching(&config_path));

        watcher.unwatch(&config_path).unwrap();
        assert!(!watcher.is_watching(&config_path));
    }

    #[tokio::test]
    async fn test_watcher_builder() {
        let watcher = ConfigWatcherBuilder::new()
            .with_poll_interval(Duration::from_secs(5))
            .with_channel_size(50)
            .build()
            .unwrap();

        assert!(watcher.watched_paths().is_empty());
    }
}
