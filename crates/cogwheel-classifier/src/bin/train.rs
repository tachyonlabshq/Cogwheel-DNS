//! Trainer CLI for the Cogwheel ad-domain classifier.
//!
//! ```text
//! node crates/cogwheel-classifier/tools/build-corpus.mjs --out /tmp/corpus --fetch --holdout 25000
//! cargo run --release -p cogwheel-classifier --features training --bin cogwheel-train -- \
//!     --corpus /tmp/corpus --out crates/cogwheel-classifier/model/cogwheel-ads-v1.cwm
//! ```
//!
//! Reads `train.tsv`, `val.tsv` and `test.tsv` from `--corpus`, trains, calibrates on the
//! validation split, evaluates on the test split, and writes the quantised model.
//!
//! Calibration and threshold selection use **validation**; the reported figures come from **test**.
//! Keeping those separate is what stops the operating points from being tuned on the same data they
//! are scored against.

use std::path::PathBuf;
use std::process::ExitCode;

use cogwheel_classifier::model::Model;
use cogwheel_classifier::train::{
    Evaluation, TrainConfig, build_model, evaluate, fit_platt, load_corpus, score_all, train,
};

struct Args {
    corpus: PathBuf,
    out: PathBuf,
    epochs: usize,
    learning_rate: f32,
}

fn parse_args() -> Result<Args, String> {
    let mut corpus: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut epochs = TrainConfig::default().epochs;
    let mut learning_rate = TrainConfig::default().learning_rate;

    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        match flag.as_str() {
            "--corpus" => {
                corpus = Some(PathBuf::from(argv.next().ok_or("--corpus needs a path")?));
            }
            "--out" => out = Some(PathBuf::from(argv.next().ok_or("--out needs a path")?)),
            "--epochs" => {
                epochs = argv
                    .next()
                    .ok_or("--epochs needs a value")?
                    .parse()
                    .map_err(|_| "--epochs must be an integer".to_string())?;
            }
            "--learning-rate" => {
                learning_rate = argv
                    .next()
                    .ok_or("--learning-rate needs a value")?
                    .parse()
                    .map_err(|_| "--learning-rate must be a float".to_string())?;
            }
            "--help" | "-h" => {
                println!(
                    "usage: cogwheel-train --corpus <dir> --out <model.cwm> [--epochs N] [--learning-rate F]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Args {
        corpus: corpus.ok_or("--corpus is required")?,
        out: out.ok_or("--out is required")?,
        epochs,
        learning_rate,
    })
}

fn report(name: &str, evaluation: &Evaluation) {
    println!("\n=== {name} ===");
    println!("  ROC-AUC              {:.5}", evaluation.roc_auc);
    println!("  PR-AUC               {:.5}", evaluation.pr_auc);
    for (index, label) in ["Low", "Balanced", "High"].iter().enumerate() {
        println!(
            "  {:<8} threshold {:.4}  recall {:>6.2}%  FPR {:>6.3}%",
            label,
            match index {
                0 => evaluation.thresholds.low,
                1 => evaluation.thresholds.balanced,
                _ => evaluation.thresholds.high,
            },
            evaluation.recall[index] * 100.0,
            evaluation.false_positive_rate[index] * 100.0,
        );
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    let load = |name: &str| -> Result<Vec<cogwheel_classifier::train::Example>, String> {
        let path = args.corpus.join(name);
        let examples =
            load_corpus(&path).map_err(|error| format!("reading {}: {error}", path.display()))?;
        if examples.is_empty() {
            return Err(format!("{} contained no usable rows", path.display()));
        }
        let positives = examples.iter().filter(|example| example.label).count();
        println!(
            "{:<10} {:>9} rows  {:>8} positive ({:.1}%)",
            name,
            examples.len(),
            positives,
            100.0 * positives as f32 / examples.len() as f32
        );
        Ok(examples)
    };

    println!("loading corpus from {}", args.corpus.display());
    let training = load("train.tsv")?;
    let validation = load("val.tsv")?;
    let test = load("test.tsv")?;

    let config = TrainConfig {
        epochs: args.epochs,
        learning_rate: args.learning_rate,
        ..TrainConfig::default()
    };
    println!(
        "\ntraining: {} epochs, lr {}, l2 {}",
        config.epochs, config.learning_rate, config.l2
    );
    let started = std::time::Instant::now();
    let weights = train(&training, config, |epoch, total| {
        println!("  epoch {epoch}/{total}");
    });
    println!("trained in {:.1}s", started.elapsed().as_secs_f32());

    // Calibrate and choose thresholds on validation, never on test.
    let validation_scores = score_all(&weights, &validation);
    let platt = fit_platt(&validation_scores, 300, 0.5);
    println!("\nplatt calibration: a={:.4} b={:.4}", platt.a, platt.b);
    let validation_evaluation = evaluate(&validation_scores, platt);
    report(
        "VALIDATION (thresholds chosen here)",
        &validation_evaluation,
    );

    // Report generalisation on test, holding the validation-chosen thresholds fixed.
    let test_scores = score_all(&weights, &test);
    let mut test_evaluation = evaluate(&test_scores, platt);
    test_evaluation.thresholds = validation_evaluation.thresholds;

    // Recompute recall and FPR on test at the validation thresholds — the numbers a user actually
    // experiences.
    let calibrated: Vec<(f32, bool)> = test_scores
        .iter()
        .map(|point| {
            (
                1.0 / (1.0 + (-(platt.a * point.logit + platt.b)).exp()),
                point.label,
            )
        })
        .collect();
    let positives = calibrated.iter().filter(|(_, label)| *label).count().max(1);
    let negatives = calibrated
        .iter()
        .filter(|(_, label)| !*label)
        .count()
        .max(1);
    for (index, threshold) in [
        validation_evaluation.thresholds.low,
        validation_evaluation.thresholds.balanced,
        validation_evaluation.thresholds.high,
    ]
    .iter()
    .enumerate()
    {
        let true_positives = calibrated
            .iter()
            .filter(|(p, label)| *label && p >= threshold)
            .count();
        let false_positives = calibrated
            .iter()
            .filter(|(p, label)| !*label && p >= threshold)
            .count();
        test_evaluation.recall[index] = true_positives as f32 / positives as f32;
        test_evaluation.false_positive_rate[index] = false_positives as f32 / negatives as f32;
    }
    report(
        "TEST (held out; validation thresholds applied)",
        &test_evaluation,
    );

    let trained_at = chrono::Utc::now().timestamp();
    let model = build_model(&weights, platt, &test_evaluation, trained_at);

    // Quantisation happens inside build_model; confirm it did not move the numbers materially
    // before we ship the file.
    let quantised_scores: Vec<f32> = test
        .iter()
        .map(|example| model.probability(&example.host))
        .collect();
    let float_probabilities: Vec<f32> = test_scores
        .iter()
        .map(|point| 1.0 / (1.0 + (-(platt.a * point.logit + platt.b)).exp()))
        .collect();
    let max_delta = quantised_scores
        .iter()
        .zip(&float_probabilities)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    let mean_delta = quantised_scores
        .iter()
        .zip(&float_probabilities)
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>()
        / quantised_scores.len().max(1) as f32;
    let quantised_auc = {
        let scored: Vec<cogwheel_classifier::train::Scored> = quantised_scores
            .iter()
            .zip(&test)
            .map(
                |(probability, example)| cogwheel_classifier::train::Scored {
                    // Logit is monotone in probability, so ranking metrics are unaffected by the map.
                    logit: *probability,
                    label: example.label,
                },
            )
            .collect();
        cogwheel_classifier::train::roc_auc(&scored)
    };
    println!("\nint8 quantisation:");
    println!("  max probability delta   {max_delta:.5}");
    println!("  mean probability delta  {mean_delta:.5}");
    println!("  ROC-AUC after quantise  {quantised_auc:.5}");

    let bytes = model.to_bytes();
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    std::fs::write(&args.out, &bytes)
        .map_err(|error| format!("writing {}: {error}", args.out.display()))?;

    // Read it back to prove the artifact we just wrote actually loads.
    let reloaded = std::fs::read(&args.out)
        .map_err(|error| format!("re-reading {}: {error}", args.out.display()))?;
    Model::from_bytes(&reloaded).map_err(|error| {
        format!(
            "model written to {} does not parse: {error}",
            args.out.display()
        )
    })?;

    println!(
        "\nwrote {} ({:.2} MiB)",
        args.out.display(),
        bytes.len() as f32 / (1024.0 * 1024.0)
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
