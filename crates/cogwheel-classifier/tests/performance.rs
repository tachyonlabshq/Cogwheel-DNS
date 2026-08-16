//! Performance budget for a Raspberry Pi 5 class device.
//!
//! # How the budget was derived
//!
//! The target is a Raspberry Pi 5: 4× Cortex-A76 @ 2.4 GHz, no GPU, sharing the box with a DNS
//! resolver. The classifier is allowed at most ~1 core-equivalent of sustained CPU.
//!
//! A single inference is one pass over ~90–250 non-zero features: a byte-wise FNV hash of every
//! character n-gram (orders 3–6 across four namespaces), a sort to deduplicate buckets, and a
//! sparse dot product. There is no matrix multiply and no allocation beyond one `Vec`.
//!
//! **These floors are deliberately set far below x86 measurement.** A Cortex-A76 at 2.4 GHz is
//! roughly 3–5× slower per core than the x86 CI runner this suite normally executes on, so the
//! asserted floor is set at ~1/10th of measured x86 throughput. That leaves the test meaningful as
//! a regression guard (a 10× slowdown from an accidental clone-in-a-loop still trips it) without
//! making it flaky on shared runners.
//!
//! No Raspberry Pi 5 measurement is claimed here, because none was taken — only the derivation
//! above. `docs/architecture/05-classifier.md` records how to reproduce it on real hardware.

use std::time::Instant;

use cogwheel_classifier::{
    Allowlist, ClassifierSettings, EngineConfig, embedded_model, engine::ClassifierEngine,
};

/// Throughput floor in domains/second/core, for an optimised build. See the module note.
const THROUGHPUT_FLOOR: f64 = 20_000.0;

/// Worst-case single-inference budget, for an optimised build.
const P99_BUDGET_MICROS: f64 = 250.0;

/// `cargo test` builds without optimisation, where this code runs roughly 20-30x slower than the
/// release build a Pi actually runs. Rather than skip the budget in debug — which would let a real
/// regression through CI unnoticed — the budget is relaxed by a documented factor so the assertion
/// still fires on order-of-magnitude regressions in both profiles.
const DEBUG_ALLOWANCE: f64 = 40.0;

/// Scale factor applied to budgets for the current build profile.
fn allowance() -> f64 {
    if cfg!(debug_assertions) {
        DEBUG_ALLOWANCE
    } else {
        1.0
    }
}

/// Human-readable profile name for test output.
fn profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

fn sample_hosts() -> Vec<String> {
    // A realistic mix: short and long, deep and flat, ad-like and benign.
    let mut hosts = Vec::new();
    for index in 0..2_000 {
        hosts.push(format!("host{index}.example.com"));
        hosts.push(format!("ads{index}.tracker-network{index}.io"));
        hosts.push(format!(
            "a{index}.b{index}.c{index}.deeply.nested.example.org"
        ));
        hosts.push(format!("cdn{index}.assets.example.net"));
        hosts.push(format!(
            "x{index}y{index}z{index}-analytics-beacon.metrics.example.co"
        ));
    }
    hosts
}

#[test]
fn inference_meets_the_throughput_floor() {
    let model = embedded_model().expect("embedded model must parse");
    let hosts = sample_hosts();

    // Warm the caches so the measurement reflects steady state, not first-touch page faults.
    for host in hosts.iter().take(500) {
        std::hint::black_box(model.probability(host));
    }

    let started = Instant::now();
    for host in &hosts {
        std::hint::black_box(model.probability(host));
    }
    let elapsed = started.elapsed();
    let throughput = hosts.len() as f64 / elapsed.as_secs_f64();

    let floor = THROUGHPUT_FLOOR / allowance();
    println!(
        "[{}] measured {throughput:.0} domains/sec/core ({:.1} us/domain) over {} hosts; floor {floor:.0}/s",
        profile(),
        elapsed.as_secs_f64() * 1e6 / hosts.len() as f64,
        hosts.len()
    );
    assert!(
        throughput >= floor,
        "throughput {throughput:.0}/s fell below the {floor:.0}/s floor for the {} profile",
        profile()
    );
}

#[test]
fn worst_case_latency_stays_within_budget() {
    let model = embedded_model().expect("parse");
    // The longest name the wire format permits, which is the worst case for n-gram extraction.
    let long_host = format!("{}.example.com", ["averylonglabelhere"; 12].join("."));
    let hosts = [
        "a.io",
        "ads.example.com",
        long_host.as_str(),
        "x1y2z3-4a5b6c-7d8e9f.analytics.tracking.example.co.uk",
    ];

    for host in hosts {
        for _ in 0..100 {
            std::hint::black_box(model.probability(host));
        }
    }

    let mut samples: Vec<f64> = Vec::new();
    for host in hosts {
        for _ in 0..500 {
            let started = Instant::now();
            std::hint::black_box(model.probability(host));
            samples.push(started.elapsed().as_secs_f64() * 1e6);
        }
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p50 = samples[samples.len() / 2];
    let p99 = samples[samples.len() * 99 / 100];
    let budget = P99_BUDGET_MICROS * allowance();
    println!(
        "[{}] p50 {p50:.1} us, p99 {p99:.1} us over {} samples; budget {budget:.0} us",
        profile(),
        samples.len()
    );

    assert!(
        p99 <= budget,
        "p99 latency {p99:.1}us exceeded the {budget:.0}us budget for the {} profile",
        profile()
    );
}

#[test]
fn model_stays_within_its_memory_budget() {
    let model = embedded_model().expect("parse");
    let resident = model.resident_bytes();
    println!("model resident {} KiB", resident / 1024);
    assert!(
        resident <= 16 * 1024 * 1024,
        "model resident size {resident} exceeds 16 MiB"
    );
    assert!(
        cogwheel_classifier::EMBEDDED_MODEL.len() <= 8 * 1024 * 1024,
        "model file exceeds the 8 MiB budget"
    );
}

#[test]
fn hot_path_lookup_is_constant_time_and_allocation_light() {
    // The resolver calls `lookup` on every query. It must not score, and must return promptly even
    // when nothing is cached.
    let (engine, _worker) = ClassifierEngine::new(
        embedded_model().expect("parse"),
        Allowlist::builtin(),
        ClassifierSettings::default(),
        EngineConfig::default(),
    );
    let hosts = sample_hosts();

    let started = Instant::now();
    for host in &hosts {
        std::hint::black_box(engine.lookup(host));
    }
    let elapsed = started.elapsed();
    let per_lookup_nanos = elapsed.as_secs_f64() * 1e9 / hosts.len() as f64;
    let budget = 5_000.0 * allowance();
    println!(
        "[{}] cold lookup {per_lookup_nanos:.0} ns/query; budget {budget:.0} ns",
        profile()
    );

    // A cache miss must be dramatically cheaper than an inference; if someone wires scoring back
    // into `lookup`, this catches it.
    assert!(
        per_lookup_nanos < budget,
        "hot-path lookup cost {per_lookup_nanos:.0}ns — is inference back on the hot path?"
    );
}

#[test]
fn observe_never_blocks_when_the_queue_is_saturated() {
    let (engine, _worker) = ClassifierEngine::new(
        embedded_model().expect("parse"),
        Allowlist::builtin(),
        ClassifierSettings::default(),
        EngineConfig {
            cache_capacity: 256,
            queue_depth: 16,
            ..EngineConfig::default()
        },
    );

    // Nothing drains the queue, so this saturates immediately. It must still return quickly.
    let started = Instant::now();
    for index in 0..5_000 {
        engine.observe(&format!("h{index}.example.com"));
    }
    let elapsed = started.elapsed();
    println!(
        "5000 saturated observes in {:.1} ms",
        elapsed.as_secs_f64() * 1e3
    );
    assert!(
        elapsed.as_millis() < 2_000,
        "observe() blocked under backpressure ({} ms)",
        elapsed.as_millis()
    );
    assert!(
        engine.stats().dropped > 0,
        "expected drops once the queue saturated"
    );
}

#[test]
fn verdict_cache_memory_stays_bounded_under_churn() {
    let (engine, worker) = ClassifierEngine::new(
        embedded_model().expect("parse"),
        Allowlist::builtin(),
        ClassifierSettings::default(),
        EngineConfig {
            cache_capacity: 1_024,
            queue_depth: 256,
            ..EngineConfig::default()
        },
    );
    for index in 0..20_000 {
        engine.observe(&format!("churn{index}.example.com"));
        if index % 100 == 0 {
            worker.run_batch(256);
        }
    }
    worker.run_batch(4_096);
    let cached = engine.stats().cached_entries;
    println!("cached entries after 20k churn: {cached}");
    assert!(
        cached <= 1_024 + 16,
        "verdict cache grew past capacity: {cached}"
    );
}
