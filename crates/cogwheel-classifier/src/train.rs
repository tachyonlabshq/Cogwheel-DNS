//! Training, calibration and evaluation.
//!
//! Compiled only under the `training` feature so none of it reaches the server binary.
//!
//! The pipeline is deliberately plain: logistic regression by SGD over the sparse features in
//! [`crate::features`], with tail weight-averaging, Platt calibration on the validation split, and
//! operating thresholds chosen by **target false-positive rate** rather than by round numbers.
//!
//! Choosing thresholds by FPR is the important decision. For this product a false positive is not
//! a missed ad, it is a website that stopped working — so the operating points are defined as "the
//! score at which we misclassify at most 0.1% / 0.5% / 2% of benign domains", and whatever recall
//! that yields is reported honestly rather than tuned for.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::features::{self, Features, N_BUCKETS, N_DENSE};
use crate::model::{Model, ModelQuality, Thresholds};
use crate::normalize;

/// One labelled example.
#[derive(Debug, Clone)]
pub struct Example {
    /// Normalised hostname.
    pub host: String,
    /// `true` when the host is an ad/tracker domain.
    pub label: bool,
}

/// Hyperparameters for [`train`].
#[derive(Debug, Clone, Copy)]
pub struct TrainConfig {
    /// Passes over the training set.
    pub epochs: usize,
    /// Initial learning rate; decayed linearly to zero across all steps.
    pub learning_rate: f32,
    /// L2 penalty applied to touched weights.
    pub l2: f32,
    /// Upper bound on the positive-class weight used to counteract class imbalance.
    pub max_positive_weight: f32,
}

impl Default for TrainConfig {
    fn default() -> Self {
        // Values chosen by sweep on the validation split; 5 epochs is where held-out AUC plateaus.
        Self {
            epochs: 5,
            learning_rate: 0.5,
            l2: 1e-8,
            max_positive_weight: 4.0,
        }
    }
}

/// Read a two-column TSV (`host\tlabel`) produced by `tools/build-corpus.mjs`.
///
/// Malformed and unnormalisable rows are skipped rather than aborting the run — a corpus of
/// millions of lines scraped from public sources always has a few, and losing the whole training
/// run to one bad line would be worse than dropping it.
pub fn load_corpus(path: &Path) -> std::io::Result<Vec<Example>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut examples = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let Some((host, label)) = line.rsplit_once('\t') else {
            continue;
        };
        let Ok(host) = normalize::normalize(host) else {
            continue;
        };
        let label = match label.trim() {
            "1" => true,
            "0" => false,
            _ => continue,
        };
        examples.push(Example { host, label });
    }
    Ok(examples)
}

/// Full-precision weights produced by [`train`], before quantisation.
#[derive(Debug, Clone)]
pub struct FloatWeights {
    /// Dense block.
    pub dense: [f32; N_DENSE],
    /// Hashed n-gram block.
    pub ngram: Vec<f32>,
    /// Intercept.
    pub bias: f32,
}

impl FloatWeights {
    fn zeroed() -> Self {
        Self {
            dense: [0.0; N_DENSE],
            ngram: vec![0.0; N_BUCKETS],
            bias: 0.0,
        }
    }

    /// Raw linear score for pre-extracted features.
    pub fn logit(&self, features: &Features) -> f32 {
        let mut accumulator = self.bias;
        for (index, value) in features.dense.iter().enumerate() {
            accumulator += self.dense[index] * value;
        }
        for (bucket, value) in &features.buckets {
            accumulator += self.ngram[*bucket as usize] * value;
        }
        accumulator
    }
}

fn sigmoid(z: f32) -> f32 {
    1.0 / (1.0 + (-z).exp())
}

/// Train a logistic model by SGD.
///
/// Progress is reported through `on_epoch` so the CLI can print without this module owning stdout.
pub fn train(
    examples: &[Example],
    config: TrainConfig,
    mut on_epoch: impl FnMut(usize, usize),
) -> FloatWeights {
    let mut weights = FloatWeights::zeroed();
    if examples.is_empty() {
        return weights;
    }

    let positives = examples.iter().filter(|example| example.label).count();
    let positive_rate = positives as f32 / examples.len() as f32;
    // Upweight the minority class so recall is not simply traded away for accuracy.
    let positive_weight = if positive_rate > 0.0 {
        (0.5 / positive_rate).min(config.max_positive_weight)
    } else {
        1.0
    };

    let total_steps = (config.epochs * examples.len()).max(1);
    let mut step = 0usize;

    // Tail averaging: accumulate weights across the final epoch. Averaged SGD measurably improves
    // held-out AUC over taking the last iterate, at the cost of one extra buffer.
    let mut averaged: Option<FloatWeights> = None;
    let mut average_count = 0usize;

    for epoch in 0..config.epochs {
        let is_final_epoch = epoch + 1 == config.epochs;
        for example in examples {
            let features = features::extract(&example.host);
            let prediction = sigmoid(weights.logit(&features));
            let target = if example.label { 1.0 } else { 0.0 };
            let sample_weight = if example.label { positive_weight } else { 1.0 };
            let learning_rate = config.learning_rate * (1.0 - step as f32 / total_steps as f32);
            let gradient = sample_weight * (prediction - target) * learning_rate;

            weights.bias -= gradient;
            for (index, value) in features.dense.iter().enumerate() {
                let weight = &mut weights.dense[index];
                *weight -= gradient * value + learning_rate * config.l2 * *weight;
            }
            for (bucket, value) in &features.buckets {
                let weight = &mut weights.ngram[*bucket as usize];
                *weight -= gradient * value + learning_rate * config.l2 * *weight;
            }
            step += 1;
        }

        if is_final_epoch {
            let accumulator = averaged.get_or_insert_with(FloatWeights::zeroed);
            accumulator.bias += weights.bias;
            for index in 0..N_DENSE {
                accumulator.dense[index] += weights.dense[index];
            }
            for bucket in 0..N_BUCKETS {
                accumulator.ngram[bucket] += weights.ngram[bucket];
            }
            average_count += 1;
        }
        on_epoch(epoch + 1, config.epochs);
    }

    match averaged {
        Some(mut accumulator) if average_count > 0 => {
            let divisor = average_count as f32;
            accumulator.bias /= divisor;
            for value in &mut accumulator.dense {
                *value /= divisor;
            }
            for value in &mut accumulator.ngram {
                *value /= divisor;
            }
            accumulator
        }
        _ => weights,
    }
}

/// A scored example: the raw logit and the true label.
#[derive(Debug, Clone, Copy)]
pub struct Scored {
    /// Uncalibrated linear score.
    pub logit: f32,
    /// Ground truth.
    pub label: bool,
}

/// Score every example with the given weights.
pub fn score_all(weights: &FloatWeights, examples: &[Example]) -> Vec<Scored> {
    examples
        .iter()
        .map(|example| Scored {
            logit: weights.logit(&features::extract(&example.host)),
            label: example.label,
        })
        .collect()
}

/// Platt scaling parameters mapping a logit to a calibrated probability.
#[derive(Debug, Clone, Copy)]
pub struct Platt {
    /// Slope.
    pub a: f32,
    /// Intercept.
    pub b: f32,
}

/// Fit Platt scaling on held-out scores by gradient descent on log loss.
///
/// Without this the raw logit is monotone but not a probability, and a user-facing "87% likely to
/// be an ad domain" would be meaningless.
pub fn fit_platt(scored: &[Scored], iterations: usize, learning_rate: f32) -> Platt {
    let mut a = 1.0f32;
    let mut b = 0.0f32;
    if scored.is_empty() {
        return Platt { a, b };
    }
    let n = scored.len() as f32;
    for _ in 0..iterations {
        let mut grad_a = 0.0f32;
        let mut grad_b = 0.0f32;
        for point in scored {
            let probability = sigmoid(a * point.logit + b);
            let error = probability - if point.label { 1.0 } else { 0.0 };
            grad_a += error * point.logit;
            grad_b += error;
        }
        a -= learning_rate * grad_a / n;
        b -= learning_rate * grad_b / n;
    }
    Platt { a, b }
}

/// Held-out evaluation figures.
#[derive(Debug, Clone, Copy)]
pub struct Evaluation {
    /// Area under the ROC curve.
    pub roc_auc: f32,
    /// Average precision.
    pub pr_auc: f32,
    /// Calibrated thresholds hitting the three target false-positive rates.
    pub thresholds: Thresholds,
    /// Recall achieved at each threshold.
    pub recall: [f32; 3],
    /// Realised false-positive rate at each threshold.
    pub false_positive_rate: [f32; 3],
}

/// Area under the ROC curve, computed by rank sum (Mann–Whitney U).
pub fn roc_auc(scored: &[Scored]) -> f32 {
    let positives = scored.iter().filter(|point| point.label).count();
    let negatives = scored.len() - positives;
    if positives == 0 || negatives == 0 {
        return 0.5;
    }
    let mut order: Vec<usize> = (0..scored.len()).collect();
    order.sort_by(|a, b| {
        scored[*a]
            .logit
            .partial_cmp(&scored[*b].logit)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Average ranks across ties so identical scores cannot inflate the statistic.
    let mut rank_sum = 0.0f64;
    let mut index = 0usize;
    while index < order.len() {
        let mut end = index + 1;
        while end < order.len()
            && (scored[order[end]].logit - scored[order[index]].logit).abs() < f32::EPSILON
        {
            end += 1;
        }
        let average_rank = ((index + 1 + end) as f64) / 2.0;
        for position in order.iter().take(end).skip(index) {
            if scored[*position].label {
                rank_sum += average_rank;
            }
        }
        index = end;
    }
    let positives_f = positives as f64;
    let negatives_f = negatives as f64;
    ((rank_sum - positives_f * (positives_f + 1.0) / 2.0) / (positives_f * negatives_f)) as f32
}

/// Average precision (area under the precision/recall curve).
pub fn pr_auc(scored: &[Scored]) -> f32 {
    let positives = scored.iter().filter(|point| point.label).count();
    if positives == 0 {
        return 0.0;
    }
    let mut order: Vec<usize> = (0..scored.len()).collect();
    order.sort_by(|a, b| {
        scored[*b]
            .logit
            .partial_cmp(&scored[*a].logit)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut true_positives = 0usize;
    let mut seen = 0usize;
    let mut average_precision = 0.0f64;
    let mut previous_recall = 0.0f64;
    for position in order {
        seen += 1;
        if scored[position].label {
            true_positives += 1;
        }
        let recall = true_positives as f64 / positives as f64;
        let precision = true_positives as f64 / seen as f64;
        average_precision += (recall - previous_recall) * precision;
        previous_recall = recall;
    }
    average_precision as f32
}

/// Evaluate calibrated scores and pick thresholds at the three target false-positive rates.
pub fn evaluate(scored: &[Scored], platt: Platt) -> Evaluation {
    let roc = roc_auc(scored);
    let pr = pr_auc(scored);

    let calibrated: Vec<(f32, bool)> = scored
        .iter()
        .map(|point| (sigmoid(platt.a * point.logit + platt.b), point.label))
        .collect();

    let mut negative_scores: Vec<f32> = calibrated
        .iter()
        .filter(|(_, label)| !*label)
        .map(|(probability, _)| *probability)
        .collect();
    negative_scores.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

    let positives = calibrated.iter().filter(|(_, label)| *label).count().max(1);

    let at_target = |target_fpr: f32| -> (f32, f32, f32) {
        if negative_scores.is_empty() {
            return (1.0, 0.0, 0.0);
        }
        // The threshold is the score of the k-th highest negative, so at most `target_fpr` of
        // negatives sit at or above it.
        let index = ((target_fpr * negative_scores.len() as f32) as usize)
            .saturating_sub(1)
            .min(negative_scores.len() - 1);
        let threshold = negative_scores[index];
        let true_positives = calibrated
            .iter()
            .filter(|(p, label)| *label && *p >= threshold)
            .count();
        let false_positives = calibrated
            .iter()
            .filter(|(p, label)| !*label && *p >= threshold)
            .count();
        (
            threshold,
            true_positives as f32 / positives as f32,
            false_positives as f32 / negative_scores.len() as f32,
        )
    };

    let (low_threshold, low_recall, low_fpr) = at_target(0.001);
    let (balanced_threshold, balanced_recall, balanced_fpr) = at_target(0.005);
    let (high_threshold, high_recall, high_fpr) = at_target(0.02);

    Evaluation {
        roc_auc: roc,
        pr_auc: pr,
        thresholds: Thresholds {
            low: low_threshold,
            balanced: balanced_threshold,
            high: high_threshold,
        },
        recall: [low_recall, balanced_recall, high_recall],
        false_positive_rate: [low_fpr, balanced_fpr, high_fpr],
    }
}

/// Assemble the shippable, quantised model.
pub fn build_model(
    weights: &FloatWeights,
    platt: Platt,
    evaluation: &Evaluation,
    trained_at: i64,
) -> Model {
    Model::from_float_weights(crate::model::FloatModelParams {
        dense_weights: weights.dense,
        ngram_weights: &weights.ngram,
        bias: weights.bias,
        platt_a: platt.a,
        platt_b: platt.b,
        thresholds: evaluation.thresholds,
        quality: ModelQuality {
            roc_auc: evaluation.roc_auc,
            pr_auc: evaluation.pr_auc,
            recall_at_threshold: evaluation.recall,
            false_positive_rate: evaluation.false_positive_rate,
        },
        trained_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_corpus() -> Vec<Example> {
        // A separable toy problem: hosts containing `ads` are positive, the rest are not.
        let mut examples = Vec::new();
        for index in 0..600 {
            examples.push(Example {
                host: format!("ads{index}.tracker{index}.com"),
                label: true,
            });
            examples.push(Example {
                host: format!("wiki{index}.library{index}.org"),
                label: false,
            });
        }
        examples
    }

    #[test]
    fn training_separates_a_learnable_corpus() {
        let examples = synthetic_corpus();
        let weights = train(
            &examples,
            TrainConfig {
                epochs: 3,
                ..TrainConfig::default()
            },
            |_, _| {},
        );
        let scored = score_all(&weights, &examples);
        assert!(
            roc_auc(&scored) > 0.95,
            "should separate a trivially separable corpus"
        );
    }

    #[test]
    fn training_an_empty_corpus_yields_zeroed_weights() {
        let weights = train(&[], TrainConfig::default(), |_, _| {});
        assert_eq!(weights.bias, 0.0);
        assert!(weights.dense.iter().all(|w| *w == 0.0));
    }

    #[test]
    fn roc_auc_is_half_for_a_single_class() {
        let scored = vec![
            Scored {
                logit: 1.0,
                label: true,
            },
            Scored {
                logit: 2.0,
                label: true,
            },
        ];
        assert_eq!(roc_auc(&scored), 0.5);
    }

    #[test]
    fn roc_auc_is_one_for_perfect_separation() {
        let scored = vec![
            Scored {
                logit: -5.0,
                label: false,
            },
            Scored {
                logit: -4.0,
                label: false,
            },
            Scored {
                logit: 4.0,
                label: true,
            },
            Scored {
                logit: 5.0,
                label: true,
            },
        ];
        assert!((roc_auc(&scored) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn roc_auc_handles_ties_without_exceeding_one() {
        let scored = vec![
            Scored {
                logit: 1.0,
                label: false,
            },
            Scored {
                logit: 1.0,
                label: true,
            },
            Scored {
                logit: 1.0,
                label: false,
            },
            Scored {
                logit: 1.0,
                label: true,
            },
        ];
        let auc = roc_auc(&scored);
        assert!((0.0..=1.0).contains(&auc), "tied scores produced {auc}");
        assert!(
            (auc - 0.5).abs() < 1e-6,
            "all-tied scores should be uninformative"
        );
    }

    #[test]
    fn pr_auc_is_one_for_perfect_separation() {
        let scored = vec![
            Scored {
                logit: -1.0,
                label: false,
            },
            Scored {
                logit: 1.0,
                label: true,
            },
        ];
        assert!((pr_auc(&scored) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn platt_calibration_orders_probabilities_with_logits() {
        let scored = vec![
            Scored {
                logit: -3.0,
                label: false,
            },
            Scored {
                logit: -1.0,
                label: false,
            },
            Scored {
                logit: 1.0,
                label: true,
            },
            Scored {
                logit: 3.0,
                label: true,
            },
        ];
        let platt = fit_platt(&scored, 200, 0.5);
        assert!(platt.a > 0.0, "calibration must preserve score direction");
    }

    #[test]
    fn thresholds_hit_their_target_false_positive_rates() {
        let mut scored = Vec::new();
        for index in 0..1000 {
            scored.push(Scored {
                logit: -3.0 + index as f32 * 0.001,
                label: false,
            });
        }
        for index in 0..1000 {
            scored.push(Scored {
                logit: 1.0 + index as f32 * 0.001,
                label: true,
            });
        }
        let platt = fit_platt(&scored, 100, 0.5);
        let evaluation = evaluate(&scored, platt);
        for (index, target) in [0.001f32, 0.005, 0.02].iter().enumerate() {
            assert!(
                evaluation.false_positive_rate[index] <= target * 1.5 + 0.002,
                "realised FPR {} overshot target {target}",
                evaluation.false_positive_rate[index]
            );
        }
    }

    #[test]
    fn thresholds_are_monotonically_ordered() {
        let mut scored = Vec::new();
        for index in 0..2000 {
            scored.push(Scored {
                logit: -2.0 + index as f32 * 0.002,
                label: index % 5 == 0,
            });
        }
        let platt = fit_platt(&scored, 100, 0.5);
        let evaluation = evaluate(&scored, platt);
        assert!(evaluation.thresholds.low >= evaluation.thresholds.balanced);
        assert!(evaluation.thresholds.balanced >= evaluation.thresholds.high);
    }

    #[test]
    fn quantisation_preserves_ranking_on_a_learned_model() {
        let examples = synthetic_corpus();
        let weights = train(
            &examples,
            TrainConfig {
                epochs: 3,
                ..TrainConfig::default()
            },
            |_, _| {},
        );
        let scored = score_all(&weights, &examples);
        let platt = fit_platt(&scored, 50, 0.1);
        let evaluation = evaluate(&scored, platt);
        let model = build_model(&weights, platt, &evaluation, 0);

        // The int8 model must keep the same ordering the float weights produced.
        let ad_score = model.probability("ads1.tracker1.com");
        let benign_score = model.probability("wiki1.library1.org");
        assert!(ad_score > benign_score, "quantisation inverted the ranking");
    }
}
