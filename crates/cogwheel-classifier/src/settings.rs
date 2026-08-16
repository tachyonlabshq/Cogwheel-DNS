//! User-facing classifier settings.
//!
//! The product exposes exactly two knobs — a mode and a sensitivity — and nothing else. Everything
//! numeric (the actual probability thresholds) is derived from the model's calibration so a user
//! never has to reason about what `0.87` means.

use serde::{Deserialize, Serialize};

use crate::model::Thresholds;

/// How much authority the classifier has over resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ClassifierMode {
    /// Not running. No scoring, no queueing, no CPU cost.
    Off,
    /// Scores and reports, but never blocks. The safe default for a new install.
    #[default]
    Monitor,
    /// Scores and blocks domains at or above the active threshold.
    Protect,
}

impl ClassifierMode {
    /// Stable string form used in the API and in persisted settings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Monitor => "monitor",
            Self::Protect => "protect",
        }
    }
}

/// How readily the classifier acts, expressed as a target false-positive rate rather than a raw
/// score. The concrete threshold comes from the model's calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    /// Most cautious. Calibrated to roughly 0.1% false positives.
    Low,
    /// Default. Calibrated to roughly 0.5% false positives.
    #[default]
    Balanced,
    /// Most aggressive. Calibrated to roughly 2% false positives.
    High,
}

impl Sensitivity {
    /// Stable string form used in the API and in persisted settings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Balanced => "balanced",
            Self::High => "high",
        }
    }

    /// The calibrated probability threshold for this sensitivity.
    pub fn threshold(self, thresholds: Thresholds) -> f32 {
        match self {
            Self::Low => thresholds.low,
            Self::Balanced => thresholds.balanced,
            Self::High => thresholds.high,
        }
    }

    /// The false-positive rate this setting targets, for display in the UI.
    pub fn target_false_positive_rate(self) -> f32 {
        match self {
            Self::Low => 0.001,
            Self::Balanced => 0.005,
            Self::High => 0.02,
        }
    }
}

/// The persisted classifier configuration.
///
/// `#[serde(default)]` on every field is load-bearing: these settings live in the `settings`
/// key-value table as an opaque JSON blob, and a row written by an older build must still parse
/// after an upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClassifierSettings {
    /// Current mode.
    #[serde(default)]
    pub mode: ClassifierMode,
    /// Current sensitivity.
    #[serde(default)]
    pub sensitivity: Sensitivity,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_monitor_and_balanced() {
        let settings = ClassifierSettings::default();
        assert_eq!(settings.mode, ClassifierMode::Monitor);
        assert_eq!(settings.sensitivity, Sensitivity::Balanced);
    }

    #[test]
    fn serialises_to_stable_lowercase_strings() {
        let json = serde_json::to_string(&ClassifierSettings {
            mode: ClassifierMode::Protect,
            sensitivity: Sensitivity::High,
        })
        .expect("serialise");
        assert_eq!(json, r#"{"mode":"protect","sensitivity":"high"}"#);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let settings: ClassifierSettings = serde_json::from_str("{}").expect("parse");
        assert_eq!(settings, ClassifierSettings::default());
    }

    #[test]
    fn partial_blobs_from_older_builds_still_parse() {
        let settings: ClassifierSettings =
            serde_json::from_str(r#"{"mode":"protect"}"#).expect("parse");
        assert_eq!(settings.mode, ClassifierMode::Protect);
        assert_eq!(settings.sensitivity, Sensitivity::Balanced);
    }

    #[test]
    fn sensitivity_selects_the_matching_threshold() {
        let thresholds = Thresholds {
            low: 0.98,
            balanced: 0.91,
            high: 0.75,
        };
        assert_eq!(Sensitivity::Low.threshold(thresholds), 0.98);
        assert_eq!(Sensitivity::Balanced.threshold(thresholds), 0.91);
        assert_eq!(Sensitivity::High.threshold(thresholds), 0.75);
    }

    #[test]
    fn thresholds_are_ordered_from_cautious_to_aggressive() {
        let thresholds = Thresholds {
            low: 0.98,
            balanced: 0.91,
            high: 0.75,
        };
        assert!(
            Sensitivity::Low.threshold(thresholds) > Sensitivity::Balanced.threshold(thresholds)
        );
        assert!(
            Sensitivity::Balanced.threshold(thresholds) > Sensitivity::High.threshold(thresholds)
        );
    }

    #[test]
    fn string_forms_are_stable() {
        assert_eq!(ClassifierMode::Off.as_str(), "off");
        assert_eq!(ClassifierMode::Monitor.as_str(), "monitor");
        assert_eq!(ClassifierMode::Protect.as_str(), "protect");
        assert_eq!(Sensitivity::Low.as_str(), "low");
        assert_eq!(Sensitivity::Balanced.as_str(), "balanced");
        assert_eq!(Sensitivity::High.as_str(), "high");
    }
}
