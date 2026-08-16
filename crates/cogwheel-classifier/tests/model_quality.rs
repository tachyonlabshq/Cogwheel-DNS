//! Model-quality regression tests against a committed holdout set.
//!
//! `model/holdout.tsv` is 25,000 rows sampled from the test split, which was grouped by registrable
//! domain — so no hostname here shares an eTLD+1 with anything the model trained on. These
//! assertions are the guard against a retrain quietly making the shipped model worse.
//!
//! The false-positive bounds matter more than the AUC. A false positive is a website that stopped
//! working for a real household, so the ceilings below are the product promise, expressed as a test.

use std::collections::HashMap;

use cogwheel_classifier::{Allowlist, embedded_model};

const HOLDOUT: &str = include_str!("../model/holdout.tsv");

struct Holdout {
    hosts: Vec<String>,
    labels: Vec<bool>,
}

fn load_holdout() -> Holdout {
    let mut hosts = Vec::new();
    let mut labels = Vec::new();
    for line in HOLDOUT.lines() {
        let Some((host, label)) = line.rsplit_once('\t') else {
            continue;
        };
        let label = match label.trim() {
            "1" => true,
            "0" => false,
            _ => continue,
        };
        hosts.push(host.to_string());
        labels.push(label);
    }
    Holdout { hosts, labels }
}

fn scores() -> (Vec<f32>, Vec<bool>) {
    let model = embedded_model().expect("embedded model must parse");
    let holdout = load_holdout();
    let scores = holdout
        .hosts
        .iter()
        .map(|host| model.probability(host))
        .collect();
    (scores, holdout.labels)
}

fn roc_auc(scores: &[f32], labels: &[bool]) -> f64 {
    let positives = labels.iter().filter(|label| **label).count();
    let negatives = labels.len() - positives;
    assert!(
        positives > 0 && negatives > 0,
        "holdout must contain both classes"
    );

    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|a, b| {
        scores[*a]
            .partial_cmp(&scores[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Average ranks over ties so a block of identical scores cannot inflate the statistic.
    let mut rank_sum = 0.0f64;
    let mut index = 0usize;
    while index < order.len() {
        let mut end = index + 1;
        while end < order.len() && (scores[order[end]] - scores[order[index]]).abs() < f32::EPSILON
        {
            end += 1;
        }
        let average_rank = ((index + 1 + end) as f64) / 2.0;
        for position in order.iter().take(end).skip(index) {
            if labels[*position] {
                rank_sum += average_rank;
            }
        }
        index = end;
    }
    let positives_f = positives as f64;
    let negatives_f = negatives as f64;
    (rank_sum - positives_f * (positives_f + 1.0) / 2.0) / (positives_f * negatives_f)
}

/// False-positive rate and recall at a threshold.
fn rates(scores: &[f32], labels: &[bool], threshold: f32) -> (f64, f64) {
    let mut false_positives = 0usize;
    let mut true_positives = 0usize;
    let mut positives = 0usize;
    let mut negatives = 0usize;
    for (score, label) in scores.iter().zip(labels) {
        if *label {
            positives += 1;
            if *score >= threshold {
                true_positives += 1;
            }
        } else {
            negatives += 1;
            if *score >= threshold {
                false_positives += 1;
            }
        }
    }
    (
        false_positives as f64 / negatives.max(1) as f64,
        true_positives as f64 / positives.max(1) as f64,
    )
}

#[test]
fn holdout_is_well_formed() {
    let holdout = load_holdout();
    assert_eq!(
        holdout.hosts.len(),
        25_000,
        "holdout size changed unexpectedly"
    );
    let positives = holdout.labels.iter().filter(|label| **label).count();
    assert!(
        (1_000..12_000).contains(&positives),
        "holdout class balance looks wrong: {positives} positives"
    );
}

#[test]
fn roc_auc_meets_the_shipped_floor() {
    let (scores, labels) = scores();
    let auc = roc_auc(&scores, &labels);
    assert!(auc >= 0.86, "ROC-AUC regressed to {auc:.5} (floor 0.86)");
}

#[test]
fn false_positive_rate_stays_within_budget_at_every_sensitivity() {
    let model = embedded_model().expect("parse");
    let thresholds = model.thresholds();
    let (scores, labels) = scores();

    // Ceilings are the target FPR with headroom for holdout sampling noise. Exceeding one of these
    // means the shipped model breaks more real websites than the product promises.
    for (name, threshold, ceiling) in [
        ("low", thresholds.low, 0.004),
        ("balanced", thresholds.balanced, 0.012),
        ("high", thresholds.high, 0.040),
    ] {
        let (false_positive_rate, _) = rates(&scores, &labels, threshold);
        assert!(
            false_positive_rate <= ceiling,
            "{name} sensitivity false-positive rate {false_positive_rate:.5} exceeds ceiling {ceiling}"
        );
    }
}

#[test]
fn recall_meets_the_shipped_floor_at_every_sensitivity() {
    let model = embedded_model().expect("parse");
    let thresholds = model.thresholds();
    let (scores, labels) = scores();

    for (name, threshold, floor) in [
        ("low", thresholds.low, 0.10),
        ("balanced", thresholds.balanced, 0.22),
        ("high", thresholds.high, 0.35),
    ] {
        let (_, recall) = rates(&scores, &labels, threshold);
        assert!(
            recall >= floor,
            "{name} sensitivity recall {recall:.4} fell below floor {floor}"
        );
    }
}

#[test]
fn header_quality_matches_measured_holdout_quality() {
    // The figures baked into the model header are shown in the UI. If they drift from what the
    // model actually does, the UI is lying to the user.
    let model = embedded_model().expect("parse");
    let (scores, labels) = scores();
    let measured = roc_auc(&scores, &labels);
    let declared = f64::from(model.quality().roc_auc);
    assert!(
        (measured - declared).abs() < 0.05,
        "declared ROC-AUC {declared:.4} disagrees with measured {measured:.4}"
    );
}

#[test]
fn protected_domains_are_never_the_top_scorers() {
    // A protected domain scoring high is fine — the allowlist catches it — but if the allowlist
    // were ever bypassed these must not be the first things blocked.
    let model = embedded_model().expect("parse");
    let allowlist = Allowlist::builtin();
    let thresholds = model.thresholds();
    for host in [
        "apple.com",
        "chase.com",
        "windowsupdate.com",
        "letsencrypt.org",
        "dns.google",
    ] {
        let probability = model.probability(host);
        assert!(
            probability < thresholds.high || allowlist.is_protected(host),
            "{host} scored {probability} above the aggressive threshold and is unprotected"
        );
    }
}

#[test]
fn well_known_ad_domains_are_caught_at_the_default_sensitivity() {
    let model = embedded_model().expect("parse");
    let balanced = model.thresholds().balanced;
    // These are unambiguous ad/tracking hosts; the default setting should catch them.
    for host in [
        "ads.example.com",
        "adserver.example.net",
        "pixel.tracking-example.com",
    ] {
        let probability = model.probability(host);
        assert!(
            probability >= balanced,
            "{host} scored {probability}, below the balanced threshold {balanced}"
        );
    }
}

#[test]
fn scores_are_deterministic() {
    let model = embedded_model().expect("parse");
    let first: Vec<f32> = ["a.example.com", "ads.example.com"]
        .iter()
        .map(|h| model.probability(h))
        .collect();
    let second: Vec<f32> = ["a.example.com", "ads.example.com"]
        .iter()
        .map(|h| model.probability(h))
        .collect();
    assert_eq!(first, second);
}

#[test]
fn every_holdout_host_scores_within_zero_to_one() {
    let (scores, _) = scores();
    for score in &scores {
        assert!(
            score.is_finite() && (0.0..=1.0).contains(score),
            "score outside [0,1]: {score}"
        );
    }
}

#[test]
fn score_distribution_is_not_degenerate() {
    // A model that collapsed to a constant would still pass a naive threshold test; this catches it.
    let (scores, _) = scores();
    let mut histogram: HashMap<u32, usize> = HashMap::new();
    for score in &scores {
        *histogram.entry((score * 20.0) as u32).or_default() += 1;
    }
    assert!(
        histogram.len() >= 8,
        "scores collapsed into {} buckets",
        histogram.len()
    );
}
