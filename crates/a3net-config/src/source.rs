//! Configuration sources (file, environment variables, CLI overrides).
//!
//! DO-178C SR-1: Multiple configuration sources with priority.

use std::collections::HashMap;
use std::path::Path;

use crate::error::{ConfigError, ConfigResult};
use crate::schema::ConfigValue;

/// Configuration source with priority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Highest priority: command-line arguments.
    Cli,
    /// Environment variables (prefixed).
    Env,
    /// Configuration file (JSON, TOML, etc.).
    File,
    /// Default values.
    Default,
}

impl ConfigSource {
    /// Get the priority of this source (higher = more important).
    pub fn priority(&self) -> u8 {
        match self {
            ConfigSource::Cli => 100,
            ConfigSource::Env => 50,
            ConfigSource::File => 25,
            ConfigSource::Default => 0,
        }
    }
}

/// File-based configuration source.
#[derive(Debug, Clone)]
pub struct FileSource {
    /// Path to the configuration file.
    path: std::path::PathBuf,
    /// Loaded configuration values.
    values: HashMap<String, ConfigValue>,
}

impl FileSource {
    /// Create a new file source from a path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            values: HashMap::new(),
        }
    }

    /// Load configuration from the file.
    pub fn load(&mut self) -> ConfigResult<()> {
        let content = std::fs::read_to_string(&self.path)
            .map_err(|e| ConfigError::FileNotFound(e.to_string()))?;

        self.values = parse_config_content(&content)?;
        Ok(())
    }

    /// Get the file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get all loaded values.
    pub fn values(&self) -> &HashMap<String, ConfigValue> {
        &self.values
    }

    /// Get a specific value.
    pub fn get(&self, key: &str) -> Option<&ConfigValue> {
        self.values.get(key)
    }
}

/// Environment variable configuration source.
#[derive(Debug, Clone)]
pub struct EnvSource {
    /// Environment variable prefix.
    prefix: String,
    /// Separator between prefix and key.
    separator: String,
    /// Loaded values.
    values: HashMap<String, ConfigValue>,
}

impl EnvSource {
    /// Create a new environment source with a prefix.
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_uppercase(),
            separator: "_".to_string(),
            values: HashMap::new(),
        }
    }

    /// Set a custom separator.
    pub fn with_separator(mut self, separator: &str) -> Self {
        self.separator = separator.to_string();
        self
    }

    /// Load configuration from environment variables.
    pub fn load(&mut self) -> ConfigResult<()> {
        self.values.clear();

        for (key, value) in std::env::vars() {
            if let Some(config_key) = self.parse_env_key(&key) {
                let config_value = self.parse_env_value(&value)?;
                self.values.insert(config_key, config_value);
            }
        }

        Ok(())
    }

    /// Parse an environment variable key into a config key.
    fn parse_env_key(&self, env_key: &str) -> Option<String> {
        if !env_key.starts_with(&self.prefix) {
            return None;
        }

        let suffix = &env_key[self.prefix.len()..];
        if suffix.is_empty() || !suffix.starts_with(&self.separator) {
            return None;
        }

        let key = &suffix[self.separator.len()..];
        Some(key.to_lowercase().replace("__", "."))
    }

    /// Parse an environment variable value into a config value.
    fn parse_env_value(&self, value: &str) -> ConfigResult<ConfigValue> {
        // Try to parse as different types
        if value.is_empty() {
            return Ok(ConfigValue::Null);
        }

        // Boolean
        if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") {
            return Ok(ConfigValue::Boolean(true));
        }
        if value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("no") {
            return Ok(ConfigValue::Boolean(false));
        }

        // Integer
        if let Ok(i) = value.parse::<i64>() {
            return Ok(ConfigValue::Integer(i));
        }

        // Float
        if let Ok(f) = value.parse::<f64>() {
            return Ok(ConfigValue::Float(f));
        }

        // String (default)
        Ok(ConfigValue::String(value.to_string()))
    }

    /// Get all loaded values.
    pub fn values(&self) -> &HashMap<String, ConfigValue> {
        &self.values
    }

    /// Get a specific value.
    pub fn get(&self, key: &str) -> Option<&ConfigValue> {
        self.values.get(key)
    }
}

/// Parse configuration content (JSON).
fn parse_config_content(content: &str) -> ConfigResult<HashMap<String, ConfigValue>> {
    let json: serde_json::Value = serde_json::from_str(content)?;
    parse_json_value(&json)
}

/// Recursively parse JSON values into ConfigValues.
fn parse_json_value(value: &serde_json::Value) -> ConfigResult<HashMap<String, ConfigValue>> {
    let mut map = HashMap::new();

    if let serde_json::Value::Object(obj) = value {
        for (key, val) in obj {
            map.insert(key.clone(), json_to_config_value(val)?);
        }
    }

    Ok(map)
}

/// Convert a JSON value to a ConfigValue.
fn json_to_config_value(value: &serde_json::Value) -> ConfigResult<ConfigValue> {
    match value {
        serde_json::Value::Null => Ok(ConfigValue::Null),
        serde_json::Value::Bool(b) => Ok(ConfigValue::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(ConfigValue::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(ConfigValue::Float(f))
            } else {
                Ok(ConfigValue::Float(n.as_f64().unwrap_or(0.0)))
            }
        }
        serde_json::Value::String(s) => Ok(ConfigValue::String(s.clone())),
        serde_json::Value::Array(arr) => {
            let values: Result<Vec<_>, _> = arr.iter().map(|v| json_single_value(v)).collect();
            Ok(ConfigValue::Array(values?))
        }
        serde_json::Value::Object(obj) => {
            let mut map = HashMap::new();
            for (key, val) in obj {
                map.insert(key.clone(), json_single_value(val)?);
            }
            Ok(ConfigValue::Object(map))
        }
    }
}

/// Convert a JSON value to a single ConfigValue (non-recursive).
fn json_single_value(value: &serde_json::Value) -> ConfigResult<ConfigValue> {
    json_to_config_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_source_parsing() {
        // Set up test environment
        std::env::set_var("ADNET_DATABASE_HOST", "localhost");
        std::env::set_var("ADNET_DATABASE_PORT", "5432");
        std::env::set_var("ADNET_LOG_LEVEL", "debug");

        let mut source = EnvSource::new("ADNET");
        source.load().unwrap();

        assert_eq!(
            source.get("database.host").unwrap().as_str(),
            Some("localhost")
        );
        assert_eq!(
            source.get("database.port").unwrap().as_i64(),
            Some(5432)
        );
        assert_eq!(
            source.get("log.level").unwrap().as_str(),
            Some("debug")
        );

        // Clean up
        std::env::remove_var("ADNET_DATABASE_HOST");
        std::env::remove_var("ADNET_DATABASE_PORT");
        std::env::remove_var("ADNET_LOG_LEVEL");
    }

    #[test]
    fn test_env_source_boolean_parsing() {
        std::env::set_var("ADNET_FEATURE_ENABLED", "true");
        std::env::set_var("ADNET_FEATURE_DISABLED", "false");

        let mut source = EnvSource::new("ADNET");
        source.load().unwrap();

        assert_eq!(source.get("feature.enabled").unwrap().as_bool(), Some(true));
        assert_eq!(source.get("feature.disabled").unwrap().as_bool(), Some(false));

        std::env::remove_var("ADNET_FEATURE_ENABLED");
        std::env::remove_var("ADNET_FEATURE_DISABLED");
    }

    #[test]
    fn test_config_source_priority() {
        assert!(ConfigSource::Cli.priority() > ConfigSource::Env.priority());
        assert!(ConfigSource::Env.priority() > ConfigSource::File.priority());
        assert!(ConfigSource::File.priority() > ConfigSource::Default.priority());
    }
}
