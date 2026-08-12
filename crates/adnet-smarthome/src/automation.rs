//! Automation rules engine
//!
//! The rule engine evaluates a [`Trigger`] (device event or schedule)
//! and optional [`Condition`]s before executing a sequence of
//! [`AutomationAction`]s. Conditions are evaluated against the current
//! device registry state; schedule triggers require a HH:MM cron format
//! and are evaluated by the hub's background scheduler task.

use chrono::NaiveTime;
use serde::{Deserialize, Serialize};

/// A single automation rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Automation {
    pub id: String,
    pub name: String,
    /// Whether this rule is currently active.
    pub enabled: bool,
    /// What fires this rule.
    pub trigger: Trigger,
    /// Additional checks that must all pass before actions run.
    pub conditions: Vec<Condition>,
    /// What to do when the rule fires.
    pub actions: Vec<AutomationAction>,
}

/// What causes an automation to fire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Trigger {
    /// Fires when a device property changes to a specific value.
    PropertyChanged {
        device_id: String,
        property: String,
        value: serde_json::Value,
    },
    /// Fires at a wall-clock time. The `cron` field accepts a
    /// `HH:MM` 24-hour string (e.g. `"08:00"`). Matching is checked
    /// once per minute by the hub's schedule monitor.
    Schedule { cron: String },
    /// Fires when a device comes online.
    DeviceOnline { device_id: String },
    /// Fires when a device goes offline.
    DeviceOffline { device_id: String },
    /// Manual trigger only (used for testing or one-shot rules).
    Manual,
}

/// A condition checked at evaluation time (after the trigger fires).
/// All conditions in an automation must evaluate to `true` for the
/// rule's actions to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Condition {
    /// Device property must equal this value.
    PropertyEquals {
        device_id: String,
        property: String,
        value: serde_json::Value,
    },
    /// Current wall-clock time must be within the range (inclusive),
    /// both times given in `HH:MM` / `HH:MM:SS` 24-hour format.
    TimeInRange { start: String, end: String },
}

impl Condition {
    /// Evaluate this condition against the current local time and a
    /// device-state lookup function. Returns `Ok(true)` if the
    /// condition passes, `Ok(false)` if it fails, or an `Err` if the
    /// condition cannot be evaluated (e.g. unknown device).
    pub fn evaluate<F>(
        &self,
        now: &chrono::DateTime<chrono::Local>,
        device_state: F,
    ) -> std::result::Result<bool, EvaluateError>
    where
        F: Fn(&str, &str) -> Option<serde_json::Value>,
    {
        match self {
            Condition::PropertyEquals { device_id, property, value } => {
                let current = device_state(device_id, property)
                    .ok_or_else(|| EvaluateError::DeviceNotFound(device_id.clone()))?;
                Ok(current == *value)
            }
            Condition::TimeInRange { start, end } => {
                let current_time = now.time();
                let start_time = parse_hhmm(start)
                    .map_err(|e| EvaluateError::InvalidTimeFormat(e.to_string()))?;
                let end_time = parse_hhmm(end)
                    .map_err(|e| EvaluateError::InvalidTimeFormat(e.to_string()))?;
                Ok(if start_time <= end_time {
                    current_time >= start_time && current_time <= end_time
                } else {
                    // Overnight range: e.g. 22:00–06:00
                    current_time >= start_time || current_time <= end_time
                })
            }
        }
    }
}

/// Error evaluating a condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvaluateError {
    DeviceNotFound(String),
    InvalidTimeFormat(String),
}

impl std::fmt::Display for EvaluateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceNotFound(id) => write!(f, "device not found: {id}"),
            Self::InvalidTimeFormat(s) => write!(f, "invalid time format: {s}"),
        }
    }
}

impl std::error::Error for EvaluateError {}

/// Parse a `HH:MM` or `HH:MM:SS` string into a `NaiveTime`.
fn parse_hhmm(s: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(s, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M:%S"))
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum AutomationAction {
    /// Set a device property
    SetProperty {
        device_id: String,
        siid: u32,
        piid: u32,
        value: serde_json::Value,
    },
    /// Invoke a device action
    InvokeAction {
        device_id: String,
        siid: u32,
        aiid: u32,
        #[serde(default)]
        input: Vec<serde_json::Value>,
    },
    /// Wait for a number of seconds
    Delay { seconds: u64 },
    /// Send a notification (log for now)
    Notify { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;

    #[test]
    fn automation_json_roundtrip() {
        let auto = Automation {
            id: "a1".into(),
            name: "Night light".into(),
            enabled: true,
            trigger: Trigger::PropertyChanged {
                device_id: "dev-1".into(),
                property: "2.1".into(),
                value: serde_json::json!(true),
            },
            conditions: vec![Condition::TimeInRange {
                start: "20:00".into(),
                end: "23:00".into(),
            }],
            actions: vec![
                AutomationAction::SetProperty {
                    device_id: "dev-2".into(),
                    siid: 2,
                    piid: 1,
                    value: serde_json::json!(true),
                },
                AutomationAction::Delay { seconds: 5 },
                AutomationAction::Notify { message: "done".into() },
            ],
        };

        let json = serde_json::to_string(&auto).unwrap();
        let back: Automation = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "a1");
        assert_eq!(back.actions.len(), 3);
        assert!(matches!(back.trigger, Trigger::PropertyChanged { .. }));
    }

    #[test]
    fn trigger_tag_is_snake_case_type_field() {
        let t = Trigger::DeviceOnline { device_id: "dev-1".into() };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["type"], "device_online");
        assert_eq!(json["device_id"], "dev-1");
    }

    #[test]
    fn condition_time_in_range_passes_within_window() {
        let now = Local::now()
            .with_time(NaiveTime::from_hms_opt(14, 30, 0).unwrap())
            .unwrap();
        let cond = Condition::TimeInRange { start: "09:00".into(), end: "17:00".into() };
        assert_eq!(cond.evaluate(&now, |_, _| None).unwrap(), true);
    }

    #[test]
    fn condition_time_in_range_fails_outside_window() {
        let now = Local::now()
            .with_time(NaiveTime::from_hms_opt(22, 0, 0).unwrap())
            .unwrap();
        let cond = Condition::TimeInRange { start: "09:00".into(), end: "17:00".into() };
        assert_eq!(cond.evaluate(&now, |_, _| None).unwrap(), false);
    }

    #[test]
    fn condition_time_in_range_overnight() {
        let now = Local::now()
            .with_time(NaiveTime::from_hms_opt(23, 0, 0).unwrap())
            .unwrap();
        // Overnight range: 22:00 to 06:00
        let cond = Condition::TimeInRange { start: "22:00".into(), end: "06:00".into() };
        assert_eq!(cond.evaluate(&now, |_, _| None).unwrap(), true);
        // 10:00 is outside
        let now2 = Local::now()
            .with_time(NaiveTime::from_hms_opt(10, 0, 0).unwrap())
            .unwrap();
        assert_eq!(cond.evaluate(&now2, |_, _| None).unwrap(), false);
    }

    #[test]
    fn condition_property_equals_passes_when_value_matches() {
        let now = Local::now();
        let cond = Condition::PropertyEquals {
            device_id: "dev-1".into(),
            property: "2.1".into(),
            value: serde_json::json!(true),
        };
        assert_eq!(
            cond.evaluate(&now, |did, prop| {
                if did == "dev-1" && prop == "2.1" {
                    Some(serde_json::json!(true))
                } else {
                    None
                }
            }).unwrap(),
            true
        );
    }

    #[test]
    fn condition_property_equals_fails_when_value_differs() {
        let now = Local::now();
        let cond = Condition::PropertyEquals {
            device_id: "dev-1".into(),
            property: "2.1".into(),
            value: serde_json::json!(true),
        };
        assert_eq!(
            cond.evaluate(&now, |_, _| Some(serde_json::json!(false))).unwrap(),
            false
        );
    }

    #[test]
    fn condition_property_equals_errs_on_missing_device() {
        let now = Local::now();
        let cond = Condition::PropertyEquals {
            device_id: "ghost".into(),
            property: "2.1".into(),
            value: serde_json::json!(true),
        };
        assert!(matches!(
            cond.evaluate(&now, |_, _| None),
            Err(EvaluateError::DeviceNotFound(_))
        ));
    }

    #[test]
    fn condition_invalid_time_format_errors() {
        let now = Local::now();
        let cond = Condition::TimeInRange { start: "bad".into(), end: "17:00".into() };
        assert!(matches!(
            cond.evaluate(&now, |_, _| None),
            Err(EvaluateError::InvalidTimeFormat(_))
        ));
    }
}
