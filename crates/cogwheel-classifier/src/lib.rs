//! Ad and tracker domain classification for Cogwheel.
//!
//! # What this is
//!
//! A calibrated linear classifier over character n-grams of a hostname, trained on 2.57 million
//! labelled domains drawn from public ad/tracker blocklists (positives) and popular-domain rankings
//! (negatives). It exists to catch ad and tracking domains that are **not yet on any blocklist** —
//! blocklists remain the primary defence and are far more precise; this is the long tail.
//!
//! # What it is not
//!
//! It is not a replacement for blocklists, and it is not certain. Measured on a held-out test split
//! grouped by registrable domain, it achieves ROC-AUC ≈ 0.89. At the default Balanced setting it
//! catches roughly a third of unlisted ad domains at a ~0.5% false-positive rate. Those numbers are
//! recorded in the model file itself and surfaced in the UI rather than hidden, because the honest
//! framing — "a useful extra filter, occasionally wrong" — is what lets a user pick a sensitivity
//! sensibly.
//!
//! # Design constraints
//!
//! Everything here targets a Raspberry Pi 5 (4× Cortex-A76 @ 2.4 GHz, no GPU) sharing the box with
//! a DNS resolver:
//!
//! * **No inference on the DNS hot path.** See [`engine`] — the resolver only ever does a cache
//!   lookup and a non-blocking enqueue.
//! * **Pure Rust, no runtime dependency.** No ONNX, no BLAS, no C++ toolchain, so an `aarch64`
//!   cross-build is just a cross-build.
//! * **Bounded memory.** 1 MiB of int8 weights plus a capped verdict cache.
//! * **Exact explanations.** A linear model's per-feature contribution *is* `w·x`, so
//!   "why was this blocked?" has a real answer.
//!
//! # Layout
//!
//! | Module | Responsibility |
//! | --- | --- |
//! | [`normalize`] | Hostname canonicalisation shared by training and inference |
//! | [`features`] | Dense + hashed n-gram feature extraction |
//! | [`model`] | On-disk format, int8 dequantisation, scoring, explanation |
//! | [`allowlist`] | Domains the classifier may never block |
//! | [`engine`] | Verdict cache, bounded queue, background scorer |
//! | [`settings`] | The two user-facing knobs |
//! | `train` | Corpus loading, SGD, calibration, evaluation (feature `training`) |

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod allowlist;
pub mod engine;
pub mod features;
pub mod model;
pub mod normalize;
pub mod settings;

#[cfg(feature = "training")]
pub mod train;

pub use allowlist::Allowlist;
pub use engine::{
    ClassifierEngine, Decision, EngineConfig, EngineStats, ObserveOutcome, ScoringWorker, Verdict,
};
pub use model::{
    Contribution, ContributionKind, FloatModelParams, Model, ModelError, ModelQuality, Thresholds,
};
pub use normalize::{NormalizeError, normalize};
pub use settings::{ClassifierMode, ClassifierSettings, Sensitivity};

/// The model shipped with the binary.
///
/// Embedding it means a fresh install classifies correctly on first boot with no download, which
/// matters for an appliance that may be brought up on a network with no internet access yet.
pub const EMBEDDED_MODEL: &[u8] = include_bytes!("../model/cogwheel-ads-v1.cwm");

/// Load the embedded model.
///
/// # Errors
///
/// Returns [`ModelError`] if the embedded bytes do not match this build's feature geometry, which
/// indicates the model was not retrained after a change to [`features`].
pub fn embedded_model() -> Result<Model, ModelError> {
    Model::from_bytes(EMBEDDED_MODEL)
}

/// Build a ready-to-run engine from the embedded model and the built-in allowlist.
///
/// # Errors
///
/// Propagates [`ModelError`] from [`embedded_model`].
pub fn engine_from_embedded(
    settings: ClassifierSettings,
    config: EngineConfig,
) -> Result<(ClassifierEngine, ScoringWorker), ModelError> {
    let model = embedded_model()?;
    Ok(ClassifierEngine::new(
        model,
        Allowlist::builtin(),
        settings,
        config,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_model_parses() {
        let model = embedded_model().expect("embedded model must parse");
        assert!(model.resident_bytes() <= 16 * 1024 * 1024);
    }

    #[test]
    fn embedded_model_carries_its_measured_quality() {
        let model = embedded_model().expect("parse");
        let quality = model.quality();
        assert!(
            quality.roc_auc > 0.85,
            "shipped model ROC-AUC regressed to {}",
            quality.roc_auc
        );
        assert!(
            quality.pr_auc > 0.55,
            "shipped model PR-AUC regressed to {}",
            quality.pr_auc
        );
    }

    #[test]
    fn embedded_thresholds_are_ordered() {
        let thresholds = embedded_model().expect("parse").thresholds();
        assert!(thresholds.low > thresholds.balanced);
        assert!(thresholds.balanced > thresholds.high);
    }

    #[test]
    fn known_ad_domains_score_above_known_good_domains() {
        let model = embedded_model().expect("parse");
        let ad_domains = [
            "doubleclick.net",
            "adservice.google.com",
            "scorecardresearch.com",
        ];
        let good_domains = ["wikipedia.org", "github.com", "chase.com", "apple.com"];
        let worst_ad = ad_domains
            .iter()
            .map(|host| model.probability(host))
            .fold(f32::INFINITY, f32::min);
        let best_good = good_domains
            .iter()
            .map(|host| model.probability(host))
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            worst_ad > best_good,
            "ad domains ({worst_ad}) must outrank benign domains ({best_good})"
        );
    }

    #[test]
    fn normalisation_feeds_the_model_consistently() {
        let model = embedded_model().expect("parse");
        let direct = model.probability("ads.example.com");
        let normalised = normalize("ADS.Example.COM.").expect("normalise");
        assert!((model.probability(&normalised) - direct).abs() < 1e-6);
    }
}
