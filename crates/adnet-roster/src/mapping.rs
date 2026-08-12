//! Bidirectional `12_digit_id <-> node_id` mapping record.
//!
//! The original codebase kept these as two parallel `HashMap`s. We
//! persist them as one struct per mapping so SQL stores have a single
//! row to work with.

use serde::{Deserialize, Serialize};

/// One row in the digit ↔ node table.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DigitMapping {
    /// Exactly 12 ASCII digits. May carry a longer prefix in future
    /// versions; see [`MAX_DIGIT_LEN`].
    pub digit_id: String,
    pub node_id: String,
    /// Unix seconds — when this mapping was first registered.
    #[serde(default)]
    pub created_at: u64,
}

impl DigitMapping {
    pub fn new(digit_id: impl Into<String>, node_id: impl Into<String>) -> Self {
        Self {
            digit_id: digit_id.into(),
            node_id: node_id.into(),
            created_at: 0,
        }
    }

    pub fn with_created_at(mut self, ts: u64) -> Self {
        self.created_at = ts;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digit::validate_digit_id;

    #[test]
    fn mapping_validates_digit() {
        let m = DigitMapping::new("123456789012", "node-1");
        validate_digit_id(&m.digit_id).unwrap();
    }
}