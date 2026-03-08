use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LexicalFeatures {
    pub length: usize,
    pub digit_ratio: f32,
    pub hyphen_ratio: f32,
    pub label_depth: usize,
    pub entropy: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ClassifierMode {
    Off,
    Monitor,
    Protect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassifierSettings {
    pub mode: ClassifierMode,
    pub threshold: f32,
}

impl Default for ClassifierSettings {
    fn default() -> Self {
        Self {
            mode: ClassifierMode::Monitor,
            threshold: 0.92,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Classification {
    pub score: f32,
    pub reasons: Vec<String>,
    pub observed_at: DateTime<Utc>,
}

pub fn extract_lexical_features(domain: &str) -> LexicalFeatures {
    let chars: Vec<char> = domain.chars().collect();
    let len = chars.len().max(1);
    let digits = chars.iter().filter(|c| c.is_ascii_digit()).count();
    let hyphens = chars.iter().filter(|c| **c == '-').count();
    let label_depth = domain.split('.').count();

    let mut counts = std::collections::HashMap::new();
    for ch in chars {
        *counts.entry(ch).or_insert(0usize) += 1;
    }

    let entropy = counts.values().fold(0.0f32, |acc, count| {
        let probability = *count as f32 / len as f32;
        acc - (probability * probability.log2())
    });

    LexicalFeatures {
        length: len,
        digit_ratio: digits as f32 / len as f32,
        hyphen_ratio: hyphens as f32 / len as f32,
        label_depth,
        entropy,
    }
}

pub fn classify_domain(domain: &str, settings: &ClassifierSettings) -> Option<Classification> {
    if matches!(settings.mode, ClassifierMode::Off) {
        return None;
    }

    let features = extract_lexical_features(domain);
    let score = ((features.entropy / 5.0) + features.digit_ratio + features.hyphen_ratio).min(1.0);

    Some(Classification {
        score,
        reasons: vec![
            format!("entropy={:.2}", features.entropy),
            format!("digit_ratio={:.2}", features.digit_ratio),
            format!("hyphen_ratio={:.2}", features.hyphen_ratio),
        ],
        observed_at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_entropy_domain_scores_higher() {
        let settings = ClassifierSettings::default();
        let score = classify_domain("a8d9x0-zz.example", &settings)
            .unwrap()
            .score;
        assert!(score > 0.5);
    }
}
