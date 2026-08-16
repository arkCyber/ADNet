//! Configuration schema definitions and validation.
//!
//! DO-178C SR-2: Schema validation ensures configuration correctness.

use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Display;

use crate::error::{ConfigError, ConfigResult};

/// Configuration key path (dot-separated).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConfigKey(String);

impl ConfigKey {
    /// Create a new configuration key from a dot-separated path.
    pub fn new(path: &str) -> Self {
        Self(path.to_string())
    }

    /// Get the key as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Get the key as a string.
    pub fn as_string(&self) -> String {
        self.0.clone()
    }

    /// Parse a key from a parent and child path.
    pub fn join(&self, child: &str) -> Self {
        if self.0.is_empty() {
            Self(child.to_string())
        } else {
            Self(format!("{}.{}", self.0, child))
        }
    }

    /// Get the parent key (everything except the last segment).
    pub fn parent(&self) -> Option<Self> {
        self.0.rsplit_once('.').map(|(p, _)| Self(p.to_string()))
    }

    /// Get the last segment of the key.
    pub fn last_segment(&self) -> &str {
        self.0.rsplit_once('.').map(|(_, l)| l).unwrap_or(&self.0)
    }
}

impl Display for ConfigKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for ConfigKey {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for ConfigKey {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Configuration value type.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ConfigValue {
    /// String value.
    String(String),
    /// Integer value.
    Integer(i64),
    /// Float value.
    Float(f64),
    /// Boolean value.
    Boolean(bool),
    /// Array value.
    Array(Vec<ConfigValue>),
    /// Object value.
    Object(HashMap<String, ConfigValue>),
    /// Null value.
    Null,
}

impl Eq for ConfigValue {}

impl ConfigValue {
    /// Get a string value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ConfigValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// Get an integer value.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            ConfigValue::Integer(i) => Some(*i),
            ConfigValue::Float(f) if f.fract() == 0.0 => Some(*f as i64),
            _ => None,
        }
    }

    /// Get a float value.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            ConfigValue::Float(f) => Some(*f),
            ConfigValue::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// Get a boolean value.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ConfigValue::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    /// Get an array value.
    pub fn as_array(&self) -> Option<&[ConfigValue]> {
        match self {
            ConfigValue::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Get an object value.
    pub fn as_object(&self) -> Option<&HashMap<String, ConfigValue>> {
        match self {
            ConfigValue::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Check if this is null.
    pub fn is_null(&self) -> bool {
        matches!(self, ConfigValue::Null)
    }
}

/// Schema type for configuration values.
#[derive(Debug, Clone)]
pub enum SchemaType {
    /// String with optional constraints.
    String {
        min_length: Option<usize>,
        max_length: Option<usize>,
        pattern: Option<String>,
    },
    /// Integer with optional range.
    Integer { min: Option<i64>, max: Option<i64> },
    /// Float with optional range.
    Float { min: Option<f64>, max: Option<f64> },
    /// Boolean.
    Boolean,
    /// Array with element schema.
    Array { element: Box<SchemaType> },
    /// Object with field schemas.
    Object { fields: HashMap<String, SchemaType> },
    /// Any type (for optional fields).
    Any,
}

/// Schema definition for a configuration section.
#[derive(Debug, Clone, Default)]
pub struct SchemaDefinition {
    /// Fields in this schema.
    pub fields: HashMap<String, SchemaType>,
    /// Whether this section is required.
    pub required: bool,
    /// Default value if not provided.
    pub default: Option<ConfigValue>,
}

/// Configuration schema for validation.
#[derive(Debug, Clone, Default)]
pub struct ConfigSchema {
    /// Root schema definition.
    pub root: SchemaDefinition,
    /// Named schema definitions for reuse.
    pub definitions: HashMap<String, SchemaDefinition>,
}

impl ConfigSchema {
    /// Create a new empty schema.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a field schema.
    pub fn field(mut self, name: &str, schema: SchemaType) -> Self {
        self.root.fields.insert(name.to_string(), schema);
        self
    }

    /// Add a required field.
    pub fn required_field(mut self, name: &str, schema: SchemaType) -> Self {
        self.root.fields.insert(name.to_string(), schema);
        self
    }

    /// Add an optional field with a default value.
    pub fn optional_field(mut self, name: &str, schema: SchemaType, default: ConfigValue) -> Self {
        self.root.fields.insert(name.to_string(), schema);
        self.root.default = Some(default);
        self
    }
}

/// Schema validator for configuration values.
#[derive(Debug, Clone, Default)]
pub struct SchemaValidator;

impl SchemaValidator {
    /// Create a new schema validator.
    pub fn new() -> Self {
        Self
    }

    /// Validate a configuration value against a schema type.
    pub fn validate_value(&self, value: &ConfigValue, schema: &SchemaType) -> ConfigResult<()> {
        match (value, schema) {
            (ConfigValue::String(s), SchemaType::String { min_length, max_length, pattern }) => {
                if let Some(min) = min_length {
                    if s.len() < *min {
                        return Err(ConfigError::Validation(format!(
                            "string length {} is less than minimum {}",
                            s.len(),
                            min
                        )));
                    }
                }
                if let Some(max) = max_length {
                    if s.len() > *max {
                        return Err(ConfigError::Validation(format!(
                            "string length {} exceeds maximum {}",
                            s.len(),
                            max
                        )));
                    }
                }
                if let Some(pattern) = pattern {
                    if !regex_lite_match(pattern, s) {
                        return Err(ConfigError::Validation(format!(
                            "string does not match pattern '{}'",
                            pattern
                        )));
                    }
                }
                Ok(())
            }
            (ConfigValue::Integer(i), SchemaType::Integer { min, max }) => {
                if let Some(min) = min {
                    if i < min {
                        return Err(ConfigError::Validation(format!(
                            "integer {} is less than minimum {}",
                            i, min
                        )));
                    }
                }
                if let Some(max) = max {
                    if i > max {
                        return Err(ConfigError::Validation(format!(
                            "integer {} exceeds maximum {}",
                            i, max
                        )));
                    }
                }
                Ok(())
            }
            (ConfigValue::Float(f), SchemaType::Float { min, max }) => {
                if let Some(min) = min {
                    if f < min {
                        return Err(ConfigError::Validation(format!(
                            "float {} is less than minimum {}",
                            f, min
                        )));
                    }
                }
                if let Some(max) = max {
                    if f > max {
                        return Err(ConfigError::Validation(format!(
                            "float {} exceeds maximum {}",
                            f, max
                        )));
                    }
                }
                Ok(())
            }
            (ConfigValue::Boolean(_), SchemaType::Boolean) => Ok(()),
            (ConfigValue::Array(arr), SchemaType::Array { element }) => {
                for (i, item) in arr.iter().enumerate() {
                    self.validate_value(item, element)
                        .map_err(|e| ConfigError::Validation(format!("array[{}]: {}", i, e)))?;
                }
                Ok(())
            }
            (ConfigValue::Object(obj), SchemaType::Object { fields }) => {
                for (key, value) in obj {
                    if let Some(schema) = fields.get(key) {
                        self.validate_value(value, schema)
                            .map_err(|e| ConfigError::Validation(format!("{}.{}", key, e)))?;
                    }
                }
                Ok(())
            }
            (ConfigValue::Null, _) => Ok(()), // Null is always valid
            _ => Err(ConfigError::Validation(format!(
                "type mismatch: got {:?}, expected {:?}",
                value, schema
            ))),
        }
    }

    /// Validate a complete configuration against a schema.
    pub fn validate(&self, config: &HashMap<String, ConfigValue>, schema: &ConfigSchema) -> ConfigResult<()> {
        for (key, value) in config {
            if let Some(schema_type) = schema.root.fields.get(key) {
                self.validate_value(value, schema_type)?;
            }
        }
        Ok(())
    }
}

/// Simple regex match for basic patterns (no full regex crate dependency).
fn regex_lite_match(pattern: &str, text: &str) -> bool {
    // Support simple patterns: ^ prefix, $ suffix, .*, .+
    if pattern.starts_with('^') && pattern.ends_with('$') {
        let inner = &pattern[1..pattern.len() - 1];
        if inner == ".*" {
            return true; // Match anything
        }
        if inner.ends_with(".*") {
            return text.starts_with(&inner[..inner.len() - 2]);
        }
        if inner.starts_with(".*") {
            return text.ends_with(&inner[2..]);
        }
        if let Some(star) = inner.find('*') {
            let before = &inner[..star];
            let after = &inner[star + 1..];
            if let Some(pos) = text.find(before) {
                return text[pos + before.len()..].starts_with(after);
            }
        }
        text == inner
    } else {
        text.contains(pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_key_operations() {
        let key = ConfigKey::new("database.host");
        assert_eq!(key.as_str(), "database.host");
        assert_eq!(key.parent(), Some(ConfigKey::new("database")));
        assert_eq!(key.last_segment(), "host");

        let joined = key.join("port");
        assert_eq!(joined.as_str(), "database.host.port");
    }

    #[test]
    fn test_config_value_type_conversion() {
        assert_eq!(ConfigValue::String("123".to_string()).as_i64(), Some(123));
        assert_eq!(ConfigValue::Integer(42).as_str(), None);
        assert_eq!(ConfigValue::Boolean(true).as_bool(), Some(true));
    }

    #[test]
    fn test_schema_validator_string() {
        let validator = SchemaValidator::new();
        let schema = SchemaType::String {
            min_length: Some(3),
            max_length: Some(10),
            pattern: None,
        };

        assert!(validator.validate_value(&ConfigValue::String("hello".to_string()), &schema).is_ok());
        assert!(validator.validate_value(&ConfigValue::String("hi".to_string()), &schema).is_err());
        assert!(validator.validate_value(&ConfigValue::String("a very long string".to_string()), &schema).is_err());
    }

    #[test]
    fn test_schema_validator_integer() {
        let validator = SchemaValidator::new();
        let schema = SchemaType::Integer {
            min: Some(0),
            max: Some(100),
        };

        assert!(validator.validate_value(&ConfigValue::Integer(50), &schema).is_ok());
        assert!(validator.validate_value(&ConfigValue::Integer(-1), &schema).is_err());
        assert!(validator.validate_value(&ConfigValue::Integer(101), &schema).is_err());
    }
}
