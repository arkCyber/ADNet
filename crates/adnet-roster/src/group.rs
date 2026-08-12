//! Contact group definitions.
//!
//! Mirrors the `ContactGroup` struct from the original
//! `Exodus@src-backup/src-tauri/src/microservice/contact_directory_service.rs`
//! (line 931). Groups are flat, named, color-tagged buckets — there is no
//! hierarchy.

use serde::{Deserialize, Serialize};

use crate::error::RosterResult;

/// Maximum length of a hex color string (e.g. `#336699`).
pub const MAX_GROUP_COLOR_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupColor {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Indigo,
    Purple,
    Pink,
    Gray,
    Custom(String),
}

impl GroupColor {
    pub fn as_str(&self) -> &str {
        match self {
            GroupColor::Red => "red",
            GroupColor::Orange => "orange",
            GroupColor::Yellow => "yellow",
            GroupColor::Green => "green",
            GroupColor::Teal => "teal",
            GroupColor::Blue => "blue",
            GroupColor::Indigo => "indigo",
            GroupColor::Purple => "purple",
            GroupColor::Pink => "pink",
            GroupColor::Gray => "gray",
            GroupColor::Custom(s) => s.as_str(),
        }
    }
}

/// A named, color-tagged grouping of contacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactGroup {
    pub group_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub color: String,
    /// Unix seconds.
    pub created_at: u64,
}

impl ContactGroup {
    /// Validate the static fields. Returns `Ok(())` when the group is
    /// well-formed.
    pub fn validate(&self) -> RosterResult<()> {
        if self.group_id.is_empty() {
            return Err(crate::error::RosterError::InvalidParameter {
                parameter: "group_id".to_string(),
                reason: "group_id cannot be empty".to_string(),
            });
        }
        if self.color.len() > MAX_GROUP_COLOR_LEN {
            return Err(crate::error::RosterError::Validation {
                field: "color".to_string(),
                reason: format!("color longer than {}", MAX_GROUP_COLOR_LEN),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_round_trip() {
        for c in [
            GroupColor::Red,
            GroupColor::Orange,
            GroupColor::Yellow,
            GroupColor::Green,
            GroupColor::Teal,
            GroupColor::Blue,
            GroupColor::Indigo,
            GroupColor::Purple,
            GroupColor::Pink,
            GroupColor::Gray,
        ] {
            assert!(!c.as_str().is_empty());
        }
        assert_eq!(
            GroupColor::Custom("#f0f0f0".into()).as_str(),
            "#f0f0f0"
        );
    }

    #[test]
    fn group_validate_ok() {
        let g = ContactGroup {
            group_id: "g1".into(),
            name: "Friends".into(),
            description: "".into(),
            color: "blue".into(),
            created_at: 0,
        };
        assert!(g.validate().is_ok());
    }

    #[test]
    fn group_validate_rejects_empty_id() {
        let g = ContactGroup {
            group_id: "".into(),
            name: "x".into(),
            description: "".into(),
            color: "".into(),
            created_at: 0,
        };
        assert!(g.validate().is_err());
    }
}