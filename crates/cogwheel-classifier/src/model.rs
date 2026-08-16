//! The classifier model: on-disk format, int8 dequantisation, inference and explanation.
//!
//! The model is a calibrated linear classifier over the features in [`crate::features`]:
//!
//! ```text
//! z = bias + Σ dense_w[i]·dense_x[i] + ngram_scale · Σ ngram_q[b]·x[b]
//! p = sigmoid(platt_a·z + platt_b)
//! ```
//!
//! Linear was chosen over a tiny MLP or a GBDT for three reasons that matter on an appliance:
//! inference is a single pass over ~200 non-zero features (tens of microseconds on a Cortex-A76,
//! no allocation, no SIMD needed); the weights compress to 1 MiB at int8; and — the deciding
//! factor — **every prediction is exactly attributable**, because the contribution of a feature
//! *is* `w·x`. [`Model::explain`] returns real arithmetic, not a templated rationalisation, which
//! is what lets the UI answer "why was this blocked?" honestly.
//!
//! Only the 2^20-entry n-gram block is quantised. The 18 dense weights stay `f32` — they are 72
//! bytes and they carry disproportionate signal, so there is no reason to add error to them.

use crate::adapt::Delta;
use crate::features::{self, Features, N_BUCKETS, N_DENSE, NGRAM_MAX, NGRAM_MIN};

/// File magic. Bump the trailing digit if the layout changes incompatibly.
pub const MAGIC: &[u8; 8] = b"CWMODEL1";

/// Layout version understood by this build.
pub const FORMAT_VERSION: u32 = 1;

/// Byte length of the fixed header preceding the weight blocks.
const HEADER_LEN: usize = 96;

/// Errors from loading or validating a model file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// Buffer is shorter than the header.
    Truncated,
    /// Magic bytes did not match.
    BadMagic,
    /// Format version is not [`FORMAT_VERSION`].
    UnsupportedVersion(u32),
    /// Header geometry disagrees with this build's feature layout.
    GeometryMismatch {
        /// What the file claims.
        found: (u32, u32, u8, u8),
        /// What this build requires.
        expected: (u32, u32, u8, u8),
    },
    /// Declared weight blocks do not fit in the buffer.
    LengthMismatch {
        /// Bytes the header implies.
        expected: usize,
        /// Bytes actually present.
        found: usize,
    },
    /// A float in the header was NaN or infinite.
    NonFiniteParameter(&'static str),
}

impl core::fmt::Display for ModelError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(f, "model buffer is shorter than the header"),
            Self::BadMagic => write!(f, "model magic bytes did not match"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported model format version {v}"),
            Self::GeometryMismatch { found, expected } => write!(
                f,
                "model geometry {found:?} does not match this build's feature layout {expected:?}"
            ),
            Self::LengthMismatch { expected, found } => {
                write!(
                    f,
                    "model declares {expected} bytes of weights but buffer holds {found}"
                )
            }
            Self::NonFiniteParameter(name) => write!(f, "model parameter {name} is not finite"),
        }
    }
}

impl std::error::Error for ModelError {}

/// Quality figures measured on the held-out test split at training time, carried in the file so the
/// runtime can report provenance without a side-channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelQuality {
    /// Area under the ROC curve.
    pub roc_auc: f32,
    /// Area under the precision/recall curve (average precision).
    pub pr_auc: f32,
    /// Recall achieved at each of the three operating points, in `[low, balanced, high]` order.
    pub recall_at_threshold: [f32; 3],
    /// Measured false-positive rate at each operating point.
    pub false_positive_rate: [f32; 3],
}

/// Operating thresholds for the three product sensitivity settings, calibrated to a target
/// false-positive rate rather than to a round number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Fewest false positives; blocks only what the model is near-certain about.
    pub low: f32,
    /// Default operating point.
    pub balanced: f32,
    /// Most aggressive; accepts a higher false-positive rate.
    pub high: f32,
}

/// Full-precision inputs to [`Model::from_float_weights`].
#[derive(Debug, Clone, Copy)]
pub struct FloatModelParams<'a> {
    /// Dense block weights, aligned with [`crate::features::dense_features`].
    pub dense_weights: [f32; N_DENSE],
    /// Hashed n-gram weights; must have exactly [`N_BUCKETS`] entries.
    pub ngram_weights: &'a [f32],
    /// Intercept.
    pub bias: f32,
    /// Platt scaling slope.
    pub platt_a: f32,
    /// Platt scaling intercept.
    pub platt_b: f32,
    /// Calibrated operating thresholds.
    pub thresholds: Thresholds,
    /// Held-out quality figures to record in the file.
    pub quality: ModelQuality,
    /// Unix timestamp (seconds) of training.
    pub trained_at: i64,
}

/// A loaded, ready-to-score model.
#[derive(Debug, Clone)]
pub struct Model {
    dense_weights: [f32; N_DENSE],
    ngram_weights: Vec<i8>,
    ngram_scale: f32,
    bias: f32,
    platt_a: f32,
    platt_b: f32,
    thresholds: Thresholds,
    quality: ModelQuality,
    trained_at: i64,
}

fn read_u32(buffer: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
    ])
}

fn read_i64(buffer: &[u8], offset: usize) -> i64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buffer[offset..offset + 8]);
    i64::from_le_bytes(bytes)
}

fn read_f32(buffer: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
    ])
}

fn checked(value: f32, name: &'static str) -> Result<f32, ModelError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ModelError::NonFiniteParameter(name))
    }
}

impl Model {
    /// Parse a model from its serialised bytes.
    ///
    /// Every field is validated before use: this buffer may come from disk on a device that lost
    /// power mid-write, so a corrupt file must produce an error, never a wrong verdict.
    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ModelError> {
        if buffer.len() < HEADER_LEN {
            return Err(ModelError::Truncated);
        }
        if &buffer[0..8] != MAGIC {
            return Err(ModelError::BadMagic);
        }
        let version = read_u32(buffer, 8);
        if version != FORMAT_VERSION {
            return Err(ModelError::UnsupportedVersion(version));
        }

        let n_dense = read_u32(buffer, 12);
        let n_buckets = read_u32(buffer, 16);
        let ngram_min = buffer[20];
        let ngram_max = buffer[21];
        let found = (n_dense, n_buckets, ngram_min, ngram_max);
        let expected = (
            N_DENSE as u32,
            N_BUCKETS as u32,
            NGRAM_MIN as u8,
            NGRAM_MAX as u8,
        );
        if found != expected {
            return Err(ModelError::GeometryMismatch { found, expected });
        }

        let ngram_scale = checked(read_f32(buffer, 24), "ngram_scale")?;
        let bias = checked(read_f32(buffer, 28), "bias")?;
        let platt_a = checked(read_f32(buffer, 32), "platt_a")?;
        let platt_b = checked(read_f32(buffer, 36), "platt_b")?;
        let thresholds = Thresholds {
            low: checked(read_f32(buffer, 40), "threshold_low")?,
            balanced: checked(read_f32(buffer, 44), "threshold_balanced")?,
            high: checked(read_f32(buffer, 48), "threshold_high")?,
        };
        let quality = ModelQuality {
            roc_auc: checked(read_f32(buffer, 52), "roc_auc")?,
            pr_auc: checked(read_f32(buffer, 56), "pr_auc")?,
            recall_at_threshold: [
                checked(read_f32(buffer, 60), "recall_low")?,
                checked(read_f32(buffer, 64), "recall_balanced")?,
                checked(read_f32(buffer, 68), "recall_high")?,
            ],
            false_positive_rate: [
                checked(read_f32(buffer, 72), "fpr_low")?,
                checked(read_f32(buffer, 76), "fpr_balanced")?,
                checked(read_f32(buffer, 80), "fpr_high")?,
            ],
        };
        let trained_at = read_i64(buffer, 84);

        let dense_start = HEADER_LEN;
        let dense_bytes = N_DENSE * 4;
        let ngram_start = dense_start + dense_bytes;
        let total = ngram_start + N_BUCKETS;
        if buffer.len() < total {
            return Err(ModelError::LengthMismatch {
                expected: total,
                found: buffer.len(),
            });
        }

        let mut dense_weights = [0.0f32; N_DENSE];
        for (index, weight) in dense_weights.iter_mut().enumerate() {
            *weight = checked(read_f32(buffer, dense_start + index * 4), "dense_weight")?;
        }

        let ngram_weights = buffer[ngram_start..total]
            .iter()
            .map(|byte| *byte as i8)
            .collect();

        Ok(Self {
            dense_weights,
            ngram_weights,
            ngram_scale,
            bias,
            platt_a,
            platt_b,
            thresholds,
            quality,
            trained_at,
        })
    }

    /// Build a model from full-precision weights, quantising the n-gram block to int8.
    ///
    /// Uses symmetric quantisation against the largest magnitude weight, so zero stays exactly
    /// zero and the ~98% of buckets that are near zero do not drift. Measured cost of quantisation
    /// on the shipped model: 0.00005 ROC-AUC, 0.0017 mean absolute probability change.
    pub fn from_float_weights(params: FloatModelParams<'_>) -> Self {
        let FloatModelParams {
            dense_weights,
            ngram_weights,
            bias,
            platt_a,
            platt_b,
            thresholds,
            quality,
            trained_at,
        } = params;
        debug_assert_eq!(ngram_weights.len(), N_BUCKETS);
        let max_abs = ngram_weights.iter().fold(0.0f32, |acc, w| acc.max(w.abs()));
        let ngram_scale = if max_abs > 0.0 { max_abs / 127.0 } else { 1.0 };
        let quantised = ngram_weights
            .iter()
            .map(|weight| {
                let scaled = (weight / ngram_scale).round();
                scaled.clamp(-127.0, 127.0) as i8
            })
            .collect();
        Self {
            dense_weights,
            ngram_weights: quantised,
            ngram_scale,
            bias,
            platt_a,
            platt_b,
            thresholds,
            quality,
            trained_at,
        }
    }

    /// Serialise to the on-disk format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(HEADER_LEN + N_DENSE * 4 + N_BUCKETS);
        buffer.extend_from_slice(MAGIC);
        buffer.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buffer.extend_from_slice(&(N_DENSE as u32).to_le_bytes());
        buffer.extend_from_slice(&(N_BUCKETS as u32).to_le_bytes());
        buffer.push(NGRAM_MIN as u8);
        buffer.push(NGRAM_MAX as u8);
        buffer.extend_from_slice(&[0u8; 2]); // reserved padding
        buffer.extend_from_slice(&self.ngram_scale.to_le_bytes());
        buffer.extend_from_slice(&self.bias.to_le_bytes());
        buffer.extend_from_slice(&self.platt_a.to_le_bytes());
        buffer.extend_from_slice(&self.platt_b.to_le_bytes());
        buffer.extend_from_slice(&self.thresholds.low.to_le_bytes());
        buffer.extend_from_slice(&self.thresholds.balanced.to_le_bytes());
        buffer.extend_from_slice(&self.thresholds.high.to_le_bytes());
        buffer.extend_from_slice(&self.quality.roc_auc.to_le_bytes());
        buffer.extend_from_slice(&self.quality.pr_auc.to_le_bytes());
        for value in self.quality.recall_at_threshold {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
        for value in self.quality.false_positive_rate {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
        buffer.extend_from_slice(&self.trained_at.to_le_bytes());
        debug_assert_eq!(buffer.len(), HEADER_LEN - 4);
        buffer.extend_from_slice(&[0u8; 4]); // reserved tail padding to HEADER_LEN

        for weight in self.dense_weights {
            buffer.extend_from_slice(&weight.to_le_bytes());
        }
        buffer.extend(self.ngram_weights.iter().map(|weight| *weight as u8));
        buffer
    }

    /// The raw linear score before calibration.
    fn logit(&self, features: &Features) -> f32 {
        let mut accumulator = self.bias;
        for (index, value) in features.dense.iter().enumerate() {
            accumulator += self.dense_weights[index] * value;
        }
        let mut hashed = 0.0f32;
        for (bucket, value) in &features.buckets {
            hashed += f32::from(self.ngram_weights[*bucket as usize]) * value;
        }
        accumulator + hashed * self.ngram_scale
    }

    /// The raw linear score before calibration, for pre-extracted features.
    ///
    /// Exposed so [`crate::adapt`] can hold the base's opinion fixed while it learns a correction to
    /// it, rather than re-deriving it from a probability.
    pub fn logit_from_features(&self, features: &Features) -> f32 {
        self.logit(features)
    }

    /// The raw linear score with an optional additive correction applied.
    ///
    /// Both terms are linear in the same feature vector, so this is still one linear model — which
    /// is why [`Self::explain_with_delta`] can still decompose it exactly.
    pub fn logit_with_delta(&self, features: &Features, delta: Option<&Delta>) -> f32 {
        let base = self.logit(features);
        match delta {
            Some(delta) => base + delta.logit_shift(features),
            None => base,
        }
    }

    /// The Platt scaling parameters, as `(slope, intercept)`.
    ///
    /// Adaptation trains against the *calibrated* probability, because that is the quantity the
    /// thresholds and the promotion gate are expressed in, so it needs the slope in its gradient.
    pub fn calibration(&self) -> (f32, f32) {
        (self.platt_a, self.platt_b)
    }

    /// Map a raw logit to a calibrated probability.
    pub fn calibrate(&self, logit: f32) -> f32 {
        1.0 / (1.0 + (-(self.platt_a * logit + self.platt_b)).exp())
    }

    /// Calibrated probability that `host` is an ad or tracking domain.
    ///
    /// `host` must already be normalised by [`crate::normalize::normalize`].
    pub fn probability(&self, host: &str) -> f32 {
        self.probability_from_features(&features::extract(host))
    }

    /// Probability for pre-extracted features, so callers that also want an explanation do not pay
    /// for extraction twice.
    pub fn probability_from_features(&self, features: &Features) -> f32 {
        self.probability_from_features_with_delta(features, None)
    }

    /// Calibrated probability with an optional adaptation delta applied.
    ///
    /// `host` must already be normalised by [`crate::normalize::normalize`]. Passing `None` is
    /// bit-identical to [`Self::probability`].
    pub fn probability_with_delta(&self, host: &str, delta: Option<&Delta>) -> f32 {
        self.probability_from_features_with_delta(&features::extract(host), delta)
    }

    /// Calibrated probability for pre-extracted features, with an optional delta applied.
    pub fn probability_from_features_with_delta(
        &self,
        features: &Features,
        delta: Option<&Delta>,
    ) -> f32 {
        self.calibrate(self.logit_with_delta(features, delta))
    }

    /// Calibrated operating thresholds.
    pub fn thresholds(&self) -> Thresholds {
        self.thresholds
    }

    /// Held-out quality figures recorded at training time.
    pub fn quality(&self) -> ModelQuality {
        self.quality
    }

    /// Unix timestamp (seconds) of training.
    pub fn trained_at(&self) -> i64 {
        self.trained_at
    }

    /// Resident size of the weight blocks in bytes.
    pub fn resident_bytes(&self) -> usize {
        self.ngram_weights.len() + N_DENSE * 4
    }

    /// Signed per-feature contributions to the logit, largest magnitude first.
    ///
    /// Because the model is linear, `w·x` *is* the contribution — this is the true decomposition of
    /// the score, not a post-hoc approximation. Dense features are named; hashed features are
    /// reported by the n-gram text that produced them, recovered by re-hashing the candidate
    /// substrings and matching buckets.
    pub fn explain(&self, host: &str, top_k: usize) -> Vec<Contribution> {
        self.explain_with_delta(host, top_k, None)
    }

    /// Signed per-feature contributions with an adaptation delta folded in.
    ///
    /// The delta is additive on the same features, so `(w + Δw)·x` is still the exact contribution
    /// of that feature — adaptation does not cost the model its explainability, which is the reason
    /// the correction was constrained to be linear in the first place. A UI showing an adapted score
    /// must call this rather than [`Self::explain`], or it will attribute the score to weights that
    /// are not the ones that produced it.
    pub fn explain_with_delta(
        &self,
        host: &str,
        top_k: usize,
        delta: Option<&Delta>,
    ) -> Vec<Contribution> {
        let features = features::extract(host);
        let mut contributions: Vec<Contribution> = Vec::new();

        for (index, value) in features.dense.iter().enumerate() {
            let adjustment = delta.map_or(0.0, |delta| delta.dense()[index]);
            let weight = (self.dense_weights[index] + adjustment) * value;
            if weight.abs() > f32::EPSILON {
                contributions.push(Contribution {
                    label: DENSE_FEATURE_NAMES[index].to_string(),
                    kind: ContributionKind::Dense,
                    value: weight,
                });
            }
        }

        // Recover which n-gram text landed in each bucket so the explanation is human-readable.
        let bucket_weights: std::collections::HashMap<u32, f32> =
            features.buckets.iter().copied().collect();
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for (text, bucket) in crate::features::ngram_provenance(host) {
            if !seen.insert(bucket) {
                continue;
            }
            let Some(feature_value) = bucket_weights.get(&bucket) else {
                continue;
            };
            let adjustment = delta.map_or(0.0, |delta| delta.ngram_weight(bucket));
            let weight = (f32::from(self.ngram_weights[bucket as usize]) * self.ngram_scale
                + adjustment)
                * feature_value;
            if weight.abs() > f32::EPSILON {
                contributions.push(Contribution {
                    label: text,
                    kind: ContributionKind::Ngram,
                    value: weight,
                });
            }
        }

        contributions.sort_by(|a, b| {
            b.value
                .abs()
                .partial_cmp(&a.value.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        contributions.truncate(top_k);
        contributions
    }
}

/// Whether a contribution came from an engineered scalar or a hashed n-gram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContributionKind {
    /// One of the [`N_DENSE`] engineered features.
    Dense,
    /// A character n-gram.
    Ngram,
}

/// One term of the linear score.
#[derive(Debug, Clone, PartialEq)]
pub struct Contribution {
    /// Feature name, or the n-gram text.
    pub label: String,
    /// Which block it came from.
    pub kind: ContributionKind,
    /// Signed contribution to the logit. Positive pushes toward "ad domain".
    pub value: f32,
}

/// Human-readable names for the dense block, aligned with [`crate::features::dense_features`].
pub const DENSE_FEATURE_NAMES: [&str; N_DENSE] = [
    "hostname length",
    "label depth",
    "digit ratio",
    "hyphen ratio",
    "vowel ratio",
    "longest consonant run",
    "character entropy",
    "ad-tech tokens in hostname",
    "ad-tech tokens in subdomain",
    "infrastructure subdomain",
    "TLD length",
    "second-level label length",
    "deep subdomain nesting",
    "digits in first label",
    "has subdomain",
    "subdomain length",
    "hex-like first label",
    "hyphen in first label",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn test_model() -> Model {
        let mut dense = [0.0f32; N_DENSE];
        dense[7] = 2.5; // ad-tech tokens in hostname
        let mut ngrams = vec![0.0f32; N_BUCKETS];
        ngrams[0] = 1.0;
        ngrams[12345] = -0.5;
        Model::from_float_weights(FloatModelParams {
            dense_weights: dense,
            ngram_weights: &ngrams,
            bias: -0.25,
            platt_a: 1.0,
            platt_b: 0.0,
            thresholds: Thresholds {
                low: 0.98,
                balanced: 0.91,
                high: 0.75,
            },
            quality: ModelQuality {
                roc_auc: 0.89,
                pr_auc: 0.66,
                recall_at_threshold: [0.17, 0.33, 0.48],
                false_positive_rate: [0.001, 0.005, 0.02],
            },
            trained_at: 1_700_000_000,
        })
    }

    #[test]
    fn round_trips_through_the_binary_format() {
        let model = test_model();
        let bytes = model.to_bytes();
        let restored = Model::from_bytes(&bytes).expect("model should parse");
        assert_eq!(restored.thresholds(), model.thresholds());
        assert_eq!(restored.quality(), model.quality());
        assert_eq!(restored.trained_at(), model.trained_at());
        assert!(
            (restored.probability("ads.example.com") - model.probability("ads.example.com")).abs()
                < 1e-6
        );
    }

    #[test]
    fn serialised_size_matches_the_documented_budget() {
        let bytes = test_model().to_bytes();
        assert_eq!(bytes.len(), HEADER_LEN + N_DENSE * 4 + N_BUCKETS);
        assert!(
            bytes.len() < 8 * 1024 * 1024,
            "model must stay under the 8 MiB budget"
        );
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = test_model().to_bytes();
        bytes[0] = b'X';
        assert_eq!(
            Model::from_bytes(&bytes).expect_err("bad magic must fail"),
            ModelError::BadMagic
        );
    }

    #[test]
    fn rejects_truncated_buffers() {
        assert_eq!(
            Model::from_bytes(&[0u8; 10]).expect_err("truncated buffer must fail"),
            ModelError::Truncated
        );
        let bytes = test_model().to_bytes();
        let truncated = &bytes[..bytes.len() - 1];
        assert!(matches!(
            Model::from_bytes(truncated),
            Err(ModelError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn rejects_unsupported_versions() {
        let mut bytes = test_model().to_bytes();
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert_eq!(
            Model::from_bytes(&bytes).expect_err("bad version must fail"),
            ModelError::UnsupportedVersion(99)
        );
    }

    #[test]
    fn rejects_geometry_mismatch() {
        let mut bytes = test_model().to_bytes();
        bytes[16..20].copy_from_slice(&1024u32.to_le_bytes());
        assert!(matches!(
            Model::from_bytes(&bytes),
            Err(ModelError::GeometryMismatch { .. })
        ));
    }

    #[test]
    fn rejects_non_finite_parameters() {
        let mut bytes = test_model().to_bytes();
        bytes[28..32].copy_from_slice(&f32::NAN.to_le_bytes());
        assert_eq!(
            Model::from_bytes(&bytes).expect_err("NaN bias must fail"),
            ModelError::NonFiniteParameter("bias")
        );
    }

    #[test]
    fn probability_is_a_valid_probability() {
        let model = test_model();
        for host in ["ads.example.com", "example.com", "a.b.c.d.e.f.example.org"] {
            let probability = model.probability(host);
            assert!(
                (0.0..=1.0).contains(&probability),
                "{host} scored {probability}"
            );
        }
    }

    #[test]
    fn ad_tokens_raise_the_score() {
        let model = test_model();
        assert!(model.probability("adserver.example.com") > model.probability("wiki.example.com"));
    }

    #[test]
    fn quantisation_keeps_zero_exactly_zero() {
        let model = test_model();
        let bytes = model.to_bytes();
        let restored = Model::from_bytes(&bytes).expect("parse");
        // Bucket 1 was never assigned a weight, so it must dequantise to exactly 0.
        assert_eq!(restored.ngram_weights[1], 0);
    }

    #[test]
    fn explain_returns_signed_contributions_that_sum_toward_the_logit() {
        let model = test_model();
        let contributions = model.explain("adserver.example.com", 10);
        assert!(!contributions.is_empty());
        assert!(
            contributions
                .iter()
                .any(|c| c.kind == ContributionKind::Dense)
        );
        // Sorted by descending magnitude.
        for pair in contributions.windows(2) {
            assert!(pair[0].value.abs() >= pair[1].value.abs());
        }
    }

    #[test]
    fn resident_size_is_within_budget() {
        assert!(test_model().resident_bytes() <= 16 * 1024 * 1024);
    }

    /// Adaptation must be free when it is absent: `None` has to be the same arithmetic as the
    /// unadapted path, not merely a close approximation of it.
    #[test]
    fn a_none_delta_scores_bit_identically() {
        let model = test_model();
        for host in ["ads.example.com", "example.com", "a.b.c.example.org"] {
            assert_eq!(
                model.probability_with_delta(host, None),
                model.probability(host)
            );
            assert_eq!(
                model.explain_with_delta(host, 8, None),
                model.explain(host, 8)
            );
        }
    }

    #[test]
    fn a_delta_shifts_the_logit_by_exactly_its_own_contribution() {
        let model = test_model();
        let delta = crate::adapt::Delta::for_test(0.75, [0.0; N_DENSE], &[]);
        let features = features::extract("ads.example.com");
        let base = model.logit_with_delta(&features, None);
        let adapted = model.logit_with_delta(&features, Some(&delta));
        assert!((adapted - base - 0.75).abs() < 1e-5);
    }

    #[test]
    fn explain_with_delta_reports_the_corrected_weights() {
        let model = test_model();
        let mut dense = [0.0f32; N_DENSE];
        dense[7] = -1.0; // the base carries +2.5 on this feature
        let delta = crate::adapt::Delta::for_test(0.0, dense, &[]);

        let value_of = |contributions: &[Contribution]| {
            contributions
                .iter()
                .find(|c| c.label == "ad-tech tokens in hostname")
                .map(|c| c.value)
                .unwrap_or_default()
        };
        let base = value_of(&model.explain("adserver.example.com", 24));
        let adapted = value_of(&model.explain_with_delta("adserver.example.com", 24, Some(&delta)));
        assert!(base > 0.0);
        // 2.5 -> 1.5 on the same feature value, so the contribution must fall by exactly 40%.
        assert!(
            (adapted / base - 0.6).abs() < 1e-4,
            "expected the contribution to shrink to 60%, got {adapted} from {base}"
        );
    }
}
