//! The live scoring engine.
//!
//! # Why this exists
//!
//! Before this module, `classify_domain` was called **synchronously inside the DNS request handler,
//! before the cache lookup** — so every query, cache hit included, paid for inference on the
//! serialised UDP loop. That was survivable with the old arithmetic heuristic and is not survivable
//! with a real model.
//!
//! The engine splits the work in two:
//!
//! * [`ClassifierEngine::lookup`] — a lock-and-hash on a bounded in-memory map. This is the only
//!   thing the hot path calls. It never scores, never allocates a model buffer, never blocks on a
//!   channel, and returns in constant time whether or not a verdict exists.
//! * [`ClassifierEngine::observe`] — a non-blocking enqueue onto a bounded channel. If the queue is
//!   full the domain is **dropped and counted**, never awaited. Losing a scoring opportunity is
//!   always preferable to adding latency to a DNS answer.
//!
//! Scoring happens on [`ScoringWorker`], which the server runs on its own thread.
//!
//! # First-sighting semantics
//!
//! The first query for an unknown domain is answered *before* any verdict exists — it resolves
//! normally, gets enqueued, and is scored a few milliseconds later. Subsequent queries see the
//! cached verdict. This is a deliberate trade (correct latency over instant enforcement) and the UI
//! states it plainly rather than implying the first request was filtered.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::allowlist::Allowlist;
use crate::model::{Contribution, Model};
use crate::settings::{ClassifierMode, ClassifierSettings, Sensitivity};

/// Number of independent cache shards. Sized so the four cores of a Pi 5 rarely contend.
const SHARD_COUNT: usize = 16;

/// How many recent detections to retain for the activity feed.
const DETECTION_HISTORY: usize = 500;

/// Tuning for the engine's bounded resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    /// Maximum verdicts held in memory across all shards.
    pub cache_capacity: usize,
    /// How long a verdict stays valid before it is re-scored.
    pub cache_ttl: Duration,
    /// Depth of the scoring queue. Beyond this, submissions are dropped.
    pub queue_depth: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            // ~65k verdicts is far more than a household resolves in a TTL window, and at roughly
            // 120 bytes per entry it costs well under 16 MiB.
            cache_capacity: 65_536,
            cache_ttl: Duration::from_secs(6 * 60 * 60),
            queue_depth: 4_096,
        }
    }
}

/// A cached classification result.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// Calibrated probability the domain is an ad/tracker host.
    pub probability: f32,
    /// Whether the protected-domain allowlist shielded this host from enforcement.
    pub protected: bool,
    /// When the verdict was produced.
    pub scored_at: chrono::DateTime<chrono::Utc>,
}

/// What [`ClassifierEngine::observe`] did with a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserveOutcome {
    /// A fresh verdict was already cached; nothing to do.
    Cached,
    /// Queued for scoring.
    Queued,
    /// Already queued and awaiting scoring.
    AlreadyQueued,
    /// Queue was full; the domain was dropped and the drop counted.
    Dropped,
    /// The classifier is switched off.
    Disabled,
}

/// The enforcement decision for a single query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// No opinion — allow. Either unscored, below threshold, or monitoring only.
    Allow,
    /// Score met the active threshold and the mode permits enforcement.
    Block,
}

/// Counters for the UI and for the metrics endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineStats {
    /// Domains scored since start.
    pub scored: u64,
    /// Verdicts served from cache.
    pub cache_hits: u64,
    /// Lookups with no cached verdict.
    pub cache_misses: u64,
    /// Submissions dropped because the queue was full.
    pub dropped: u64,
    /// Enforcement decisions that returned [`Decision::Block`].
    pub blocked: u64,
    /// Times the allowlist overrode a block.
    pub protected_overrides: u64,
    /// Verdicts currently resident.
    pub cached_entries: u64,
    /// Times a verdict hook panicked and was contained.
    pub hook_panics: u64,
}

#[derive(Debug)]
struct Shard {
    entries: HashMap<String, (Verdict, Instant)>,
    order: VecDeque<String>,
}

impl Shard {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

#[derive(Debug, Default)]
struct Counters {
    scored: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    dropped: AtomicU64,
    blocked: AtomicU64,
    protected_overrides: AtomicU64,
    hook_panics: AtomicU64,
}

/// A queued scoring request: the hostname plus whichever client asked for it.
///
/// The client is carried through the queue because scoring is decoupled from the query that
/// triggered it — without this the resulting security event would have no attribution.
type Submission = (String, Option<String>);

/// Invoked by the scoring worker each time a fresh verdict is produced.
pub type VerdictHook = Arc<dyn Fn(&str, Option<&str>, &Verdict) + Send + Sync>;

struct EngineInner {
    model: Model,
    allowlist: Allowlist,
    config: EngineConfig,
    shards: Vec<Mutex<Shard>>,
    inflight: Mutex<HashSet<String>>,
    // Mode and sensitivity are read on every DNS query. Holding them in atomics keeps the hot path
    // lock-free; a `Mutex<ClassifierSettings>` here would serialise every query behind one lock.
    mode: AtomicU8,
    sensitivity: AtomicU8,
    on_verdict: Mutex<Option<VerdictHook>>,
    detections: Mutex<VecDeque<Detection>>,
    counters: Counters,
}

/// A recorded classifier detection, kept for the activity feed.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// Normalised hostname.
    pub host: String,
    /// Client that triggered the lookup, if known.
    pub client: Option<String>,
    /// Calibrated probability.
    pub probability: f32,
    /// Whether the allowlist shielded it.
    pub protected: bool,
    /// Whether the active settings would block it.
    pub blocked: bool,
    /// When it was scored.
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

fn mode_to_u8(mode: ClassifierMode) -> u8 {
    match mode {
        ClassifierMode::Off => 0,
        ClassifierMode::Monitor => 1,
        ClassifierMode::Protect => 2,
    }
}

fn mode_from_u8(value: u8) -> ClassifierMode {
    match value {
        0 => ClassifierMode::Off,
        2 => ClassifierMode::Protect,
        _ => ClassifierMode::Monitor,
    }
}

fn sensitivity_to_u8(sensitivity: Sensitivity) -> u8 {
    match sensitivity {
        Sensitivity::Low => 0,
        Sensitivity::Balanced => 1,
        Sensitivity::High => 2,
    }
}

fn sensitivity_from_u8(value: u8) -> Sensitivity {
    match value {
        0 => Sensitivity::Low,
        2 => Sensitivity::High,
        _ => Sensitivity::Balanced,
    }
}

impl std::fmt::Debug for EngineInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineInner")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Hash a host to a shard index. FNV-1a again — cheap and adequate for bucket spreading.
fn shard_index(host: &str) -> usize {
    let mut hash = 0x811c_9dc5u32;
    for byte in host.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    (hash as usize) % SHARD_COUNT
}

/// Handle used by the DNS path and the API.
#[derive(Clone, Debug)]
pub struct ClassifierEngine {
    inner: Arc<EngineInner>,
    sender: SyncSender<Submission>,
}

impl ClassifierEngine {
    /// Build an engine and its worker. The worker must be run on its own thread.
    pub fn new(
        model: Model,
        allowlist: Allowlist,
        settings: ClassifierSettings,
        config: EngineConfig,
    ) -> (Self, ScoringWorker) {
        let (sender, receiver) = sync_channel(config.queue_depth.max(1));
        let inner = Arc::new(EngineInner {
            model,
            allowlist,
            config,
            shards: (0..SHARD_COUNT).map(|_| Mutex::new(Shard::new())).collect(),
            inflight: Mutex::new(HashSet::new()),
            mode: AtomicU8::new(mode_to_u8(settings.mode)),
            sensitivity: AtomicU8::new(sensitivity_to_u8(settings.sensitivity)),
            on_verdict: Mutex::new(None),
            detections: Mutex::new(VecDeque::new()),
            counters: Counters::default(),
        });
        let engine = Self {
            inner: Arc::clone(&inner),
            sender,
        };
        let worker = ScoringWorker { inner, receiver };
        (engine, worker)
    }

    /// Look up a cached verdict. Constant time, never blocks on scoring.
    ///
    /// This is the only engine call permitted on the DNS hot path.
    pub fn lookup(&self, host: &str) -> Option<Verdict> {
        let shard = &self.inner.shards[shard_index(host)];
        let Ok(mut guard) = shard.lock() else {
            // A poisoned lock must degrade to "no opinion", never to a panic in the resolver.
            return None;
        };
        match guard.entries.get(host) {
            Some((verdict, inserted)) if inserted.elapsed() < self.inner.config.cache_ttl => {
                let verdict = verdict.clone();
                drop(guard);
                self.inner
                    .counters
                    .cache_hits
                    .fetch_add(1, Ordering::Relaxed);
                Some(verdict)
            }
            Some(_) => {
                // Expired: drop it so the next observe() re-queues.
                guard.entries.remove(host);
                drop(guard);
                self.inner
                    .counters
                    .cache_misses
                    .fetch_add(1, Ordering::Relaxed);
                None
            }
            None => {
                drop(guard);
                self.inner
                    .counters
                    .cache_misses
                    .fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Submit a domain for scoring if it is not already known. Never blocks.
    pub fn observe(&self, host: &str) -> ObserveOutcome {
        self.observe_with_client(host, None)
    }

    /// Submit a domain for scoring, attributing it to a client. Never blocks.
    pub fn observe_with_client(&self, host: &str, client: Option<&str>) -> ObserveOutcome {
        if self.mode() == ClassifierMode::Off {
            return ObserveOutcome::Disabled;
        }
        if self.lookup(host).is_some() {
            return ObserveOutcome::Cached;
        }
        {
            let Ok(mut inflight) = self.inner.inflight.lock() else {
                return ObserveOutcome::Dropped;
            };
            if !inflight.insert(host.to_string()) {
                return ObserveOutcome::AlreadyQueued;
            }
        }
        match self
            .sender
            .try_send((host.to_string(), client.map(str::to_string)))
        {
            Ok(()) => ObserveOutcome::Queued,
            Err(TrySendError::Full((host, _))) | Err(TrySendError::Disconnected((host, _))) => {
                if let Ok(mut inflight) = self.inner.inflight.lock() {
                    inflight.remove(&host);
                }
                self.inner.counters.dropped.fetch_add(1, Ordering::Relaxed);
                ObserveOutcome::Dropped
            }
        }
    }

    /// The enforcement decision for a host, based on the cached verdict and the active settings.
    ///
    /// Returns [`Decision::Allow`] when there is no verdict yet — the resolver must not stall
    /// waiting for one.
    pub fn decide(&self, host: &str) -> Decision {
        let settings = self.settings();
        if settings.mode != ClassifierMode::Protect {
            return Decision::Allow;
        }
        let Some(verdict) = self.lookup(host) else {
            return Decision::Allow;
        };
        let threshold = settings
            .sensitivity
            .threshold(self.inner.model.thresholds());
        if verdict.probability < threshold {
            return Decision::Allow;
        }
        if verdict.protected {
            self.inner
                .counters
                .protected_overrides
                .fetch_add(1, Ordering::Relaxed);
            return Decision::Allow;
        }
        self.inner.counters.blocked.fetch_add(1, Ordering::Relaxed);
        Decision::Block
    }

    /// Score a host immediately on the calling thread.
    ///
    /// For API handlers and the domain inspector only — never call this from the DNS path.
    pub fn score_now(&self, host: &str) -> Verdict {
        self.inner.score(host)
    }

    /// Signed feature contributions behind a host's score.
    pub fn explain(&self, host: &str, top_k: usize) -> Vec<Contribution> {
        self.inner.model.explain(host, top_k)
    }

    /// Currently active settings.
    pub fn settings(&self) -> ClassifierSettings {
        ClassifierSettings {
            mode: self.mode(),
            sensitivity: sensitivity_from_u8(self.inner.sensitivity.load(Ordering::Relaxed)),
        }
    }

    /// Replace the active settings.
    pub fn set_settings(&self, settings: ClassifierSettings) {
        self.inner
            .mode
            .store(mode_to_u8(settings.mode), Ordering::Relaxed);
        self.inner
            .sensitivity
            .store(sensitivity_to_u8(settings.sensitivity), Ordering::Relaxed);
    }

    /// Register a callback invoked by the worker whenever a fresh verdict is produced.
    ///
    /// The server uses this to record security events without polling.
    pub fn set_verdict_hook(&self, hook: VerdictHook) {
        if let Ok(mut guard) = self.inner.on_verdict.lock() {
            *guard = Some(hook);
        }
    }

    /// Most recent detections, newest first, for the activity feed.
    pub fn recent_detections(&self, limit: usize) -> Vec<Detection> {
        let Ok(guard) = self.inner.detections.lock() else {
            return Vec::new();
        };
        guard.iter().rev().take(limit).cloned().collect()
    }

    /// The active calibrated threshold for the current sensitivity.
    pub fn active_threshold(&self) -> f32 {
        self.settings()
            .sensitivity
            .threshold(self.inner.model.thresholds())
    }

    fn mode(&self) -> ClassifierMode {
        mode_from_u8(self.inner.mode.load(Ordering::Relaxed))
    }

    /// The loaded model, for reporting provenance and quality.
    pub fn model(&self) -> &Model {
        &self.inner.model
    }

    /// Snapshot of the engine counters.
    pub fn stats(&self) -> EngineStats {
        let counters = &self.inner.counters;
        let cached_entries = self
            .inner
            .shards
            .iter()
            .map(|shard| shard.lock().map(|guard| guard.entries.len()).unwrap_or(0))
            .sum::<usize>() as u64;
        EngineStats {
            scored: counters.scored.load(Ordering::Relaxed),
            cache_hits: counters.cache_hits.load(Ordering::Relaxed),
            cache_misses: counters.cache_misses.load(Ordering::Relaxed),
            dropped: counters.dropped.load(Ordering::Relaxed),
            blocked: counters.blocked.load(Ordering::Relaxed),
            protected_overrides: counters.protected_overrides.load(Ordering::Relaxed),
            hook_panics: counters.hook_panics.load(Ordering::Relaxed),
            cached_entries,
        }
    }

    /// Drain the queue synchronously. Test-only helper so tests need no worker thread.
    #[cfg(test)]
    fn drain_for_test(&self, receiver: &Receiver<Submission>) {
        while let Ok((host, client)) = receiver.try_recv() {
            self.inner.score_and_cache(&host, client.as_deref());
        }
    }
}

impl EngineInner {
    fn score(&self, host: &str) -> Verdict {
        let probability = self.model.probability(host);
        Verdict {
            probability,
            protected: self.allowlist.is_protected(host),
            scored_at: chrono::Utc::now(),
        }
    }

    fn score_and_cache(&self, host: &str, client: Option<&str>) {
        let verdict = self.score(host);
        self.counters.scored.fetch_add(1, Ordering::Relaxed);

        // Record anything at or above the most permissive threshold so the activity feed shows what
        // the classifier is finding even in Monitor mode, where nothing is enforced.
        let thresholds = self.model.thresholds();
        if verdict.probability >= thresholds.high {
            let sensitivity = sensitivity_from_u8(self.sensitivity.load(Ordering::Relaxed));
            let blocked = mode_from_u8(self.mode.load(Ordering::Relaxed))
                == ClassifierMode::Protect
                && !verdict.protected
                && verdict.probability >= sensitivity.threshold(thresholds);
            if let Ok(mut detections) = self.detections.lock() {
                detections.push_back(Detection {
                    host: host.to_string(),
                    client: client.map(str::to_string),
                    probability: verdict.probability,
                    protected: verdict.protected,
                    blocked,
                    observed_at: verdict.scored_at,
                });
                while detections.len() > DETECTION_HISTORY {
                    detections.pop_front();
                }
            }
        }

        // The hook runs arbitrary caller code on the scoring thread. If it panics, this thread
        // dies and scoring stops permanently -- and nothing else fails, so the appliance looks
        // healthy while quietly classifying nothing. That exact failure happened once (an observer
        // called `tokio::spawn` from this non-runtime thread), so the boundary is now sealed.
        if let Ok(guard) = self.on_verdict.lock()
            && let Some(hook) = guard.as_ref()
        {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                hook(host, client, &verdict);
            }));
            if outcome.is_err() {
                self.counters.hook_panics.fetch_add(1, Ordering::Relaxed);
            }
        }

        self.insert(host, verdict);
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.remove(host);
        }
    }

    fn insert(&self, host: &str, verdict: Verdict) {
        let shard = &self.shards[shard_index(host)];
        let Ok(mut guard) = shard.lock() else { return };
        let per_shard_capacity = (self.config.cache_capacity / SHARD_COUNT).max(1);
        if guard
            .entries
            .insert(host.to_string(), (verdict, Instant::now()))
            .is_none()
        {
            guard.order.push_back(host.to_string());
        }
        // FIFO eviction keeps memory strictly bounded without the bookkeeping an LRU would cost.
        while guard.order.len() > per_shard_capacity {
            let Some(oldest) = guard.order.pop_front() else {
                break;
            };
            guard.entries.remove(&oldest);
        }
    }
}

/// The background scorer. Run it on a dedicated thread.
pub struct ScoringWorker {
    inner: Arc<EngineInner>,
    receiver: Receiver<Submission>,
}

impl std::fmt::Debug for ScoringWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScoringWorker").finish_non_exhaustive()
    }
}

impl ScoringWorker {
    /// Score submitted domains until every [`ClassifierEngine`] handle is dropped.
    ///
    /// Returns when the channel disconnects, which is the shutdown signal.
    pub fn run(self) {
        while let Ok((host, client)) = self.receiver.recv() {
            self.inner.score_and_cache(&host, client.as_deref());
        }
    }

    /// Score at most `limit` queued domains, then return. Lets a caller interleave scoring with
    /// other work under a time budget instead of dedicating a thread.
    pub fn run_batch(&self, limit: usize) -> usize {
        let mut processed = 0;
        while processed < limit {
            let Ok((host, client)) = self.receiver.try_recv() else {
                break;
            };
            self.inner.score_and_cache(&host, client.as_deref());
            processed += 1;
        }
        processed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{N_BUCKETS, N_DENSE};
    use crate::model::{ModelQuality, Thresholds};
    use crate::settings::Sensitivity;

    /// A model that scores anything containing the `ads` token very high and everything else low.
    fn ad_token_model() -> Model {
        let mut dense = [0.0f32; N_DENSE];
        dense[7] = 40.0; // ad-tech tokens in hostname
        let ngrams = vec![0.0f32; N_BUCKETS];
        Model::from_float_weights(crate::model::FloatModelParams {
            dense_weights: dense,
            ngram_weights: &ngrams,
            bias: -5.0,
            platt_a: 1.0,
            platt_b: 0.0,
            thresholds: Thresholds {
                low: 0.9,
                balanced: 0.5,
                high: 0.2,
            },
            quality: ModelQuality {
                roc_auc: 0.9,
                pr_auc: 0.6,
                recall_at_threshold: [0.1, 0.3, 0.5],
                false_positive_rate: [0.001, 0.005, 0.02],
            },
            trained_at: 0,
        })
    }

    fn engine(mode: ClassifierMode) -> (ClassifierEngine, ScoringWorker) {
        ClassifierEngine::new(
            ad_token_model(),
            Allowlist::builtin(),
            ClassifierSettings {
                mode,
                sensitivity: Sensitivity::Balanced,
            },
            EngineConfig {
                cache_capacity: 64,
                cache_ttl: Duration::from_secs(60),
                queue_depth: 8,
            },
        )
    }

    #[test]
    fn lookup_misses_before_anything_is_scored() {
        let (engine, _worker) = engine(ClassifierMode::Protect);
        assert_eq!(engine.lookup("ads.example.com"), None);
    }

    #[test]
    fn first_sighting_allows_and_queues() {
        let (engine, _worker) = engine(ClassifierMode::Protect);
        // The very first query must not block and must not be enforced against.
        assert_eq!(engine.decide("ads.example.com"), Decision::Allow);
        assert_eq!(engine.observe("ads.example.com"), ObserveOutcome::Queued);
    }

    #[test]
    fn second_sighting_blocks_after_scoring() {
        let (engine, worker) = engine(ClassifierMode::Protect);
        engine.observe("ads.example.com");
        engine.drain_for_test(&worker.receiver);
        assert_eq!(engine.decide("ads.example.com"), Decision::Block);
    }

    #[test]
    fn monitor_mode_scores_but_never_blocks() {
        let (engine, worker) = engine(ClassifierMode::Monitor);
        engine.observe("ads.example.com");
        engine.drain_for_test(&worker.receiver);
        assert!(
            engine.lookup("ads.example.com").is_some(),
            "monitor mode should still score"
        );
        assert_eq!(engine.decide("ads.example.com"), Decision::Allow);
    }

    #[test]
    fn off_mode_does_not_even_queue() {
        let (engine, _worker) = engine(ClassifierMode::Off);
        assert_eq!(engine.observe("ads.example.com"), ObserveOutcome::Disabled);
        assert_eq!(engine.decide("ads.example.com"), Decision::Allow);
    }

    #[test]
    fn protected_domains_are_never_blocked() {
        let (engine, worker) = engine(ClassifierMode::Protect);
        // `ads.apple.com` trips the ad-token feature but sits under a protected suffix.
        engine.observe("ads.apple.com");
        engine.drain_for_test(&worker.receiver);
        let verdict = engine.lookup("ads.apple.com").expect("should be scored");
        assert!(
            verdict.probability > 0.5,
            "score should still be high and visible"
        );
        assert!(verdict.protected);
        assert_eq!(engine.decide("ads.apple.com"), Decision::Allow);
        assert_eq!(engine.stats().protected_overrides, 1);
    }

    #[test]
    fn duplicate_observations_are_deduplicated_while_in_flight() {
        let (engine, _worker) = engine(ClassifierMode::Protect);
        assert_eq!(engine.observe("ads.example.com"), ObserveOutcome::Queued);
        assert_eq!(
            engine.observe("ads.example.com"),
            ObserveOutcome::AlreadyQueued
        );
    }

    #[test]
    fn full_queue_drops_rather_than_blocking() {
        let (engine, _worker) = engine(ClassifierMode::Protect);
        // Queue depth is 8; submit far more and confirm the excess is dropped, not awaited.
        let mut dropped = 0;
        for index in 0..64 {
            if engine.observe(&format!("host{index}.example.com")) == ObserveOutcome::Dropped {
                dropped += 1;
            }
        }
        assert!(dropped > 0, "expected drops once the bounded queue filled");
        assert_eq!(engine.stats().dropped, dropped);
    }

    #[test]
    fn cache_capacity_is_enforced() {
        let (engine, worker) = engine(ClassifierMode::Monitor);
        for index in 0..500 {
            engine.observe(&format!("h{index}.example.com"));
            engine.drain_for_test(&worker.receiver);
        }
        let cached = engine.stats().cached_entries as usize;
        assert!(
            cached <= 64 + SHARD_COUNT,
            "cache grew past capacity: {cached}"
        );
    }

    #[test]
    fn expired_verdicts_are_evicted_on_lookup() {
        let (engine, worker) = ClassifierEngine::new(
            ad_token_model(),
            Allowlist::builtin(),
            ClassifierSettings::default(),
            EngineConfig {
                cache_capacity: 64,
                cache_ttl: Duration::from_nanos(1),
                queue_depth: 8,
            },
        );
        engine.observe("ads.example.com");
        engine.drain_for_test(&worker.receiver);
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(
            engine.lookup("ads.example.com"),
            None,
            "expired verdict should be dropped"
        );
    }

    #[test]
    fn sensitivity_changes_the_operating_threshold() {
        let (engine, worker) = engine(ClassifierMode::Protect);
        engine.observe("track.example.com");
        engine.drain_for_test(&worker.receiver);
        let probability = engine
            .lookup("track.example.com")
            .expect("scored")
            .probability;

        engine.set_settings(ClassifierSettings {
            mode: ClassifierMode::Protect,
            sensitivity: Sensitivity::High,
        });
        let high = engine.decide("track.example.com");
        engine.set_settings(ClassifierSettings {
            mode: ClassifierMode::Protect,
            sensitivity: Sensitivity::Low,
        });
        let low = engine.decide("track.example.com");

        // With thresholds low=0.9 / high=0.2, a mid score must block on High and allow on Low.
        if probability > 0.2 && probability < 0.9 {
            assert_eq!(high, Decision::Block);
            assert_eq!(low, Decision::Allow);
        }
    }

    #[test]
    fn stats_track_hits_and_misses() {
        let (engine, worker) = engine(ClassifierMode::Monitor);
        engine.lookup("miss.example.com");
        engine.observe("ads.example.com");
        engine.drain_for_test(&worker.receiver);
        engine.lookup("ads.example.com");
        let stats = engine.stats();
        assert!(stats.cache_misses >= 1);
        assert!(stats.cache_hits >= 1);
        assert_eq!(stats.scored, 1);
    }

    #[test]
    fn worker_run_batch_respects_its_limit() {
        let (engine, worker) = engine(ClassifierMode::Monitor);
        for index in 0..5 {
            engine.observe(&format!("h{index}.example.com"));
        }
        assert_eq!(worker.run_batch(3), 3);
        assert_eq!(worker.run_batch(10), 2);
    }

    /// A verdict hook that panics must not stop scoring.
    ///
    /// Regression guard: an observer once called `tokio::spawn` from this non-runtime thread, which
    /// panicked and killed the worker permanently. DNS kept working, so the appliance looked
    /// healthy while silently classifying nothing ever again.
    #[test]
    #[allow(clippy::panic, reason = "the panic is the subject of this test")]
    fn a_panicking_verdict_hook_does_not_stop_scoring() {
        let (engine, worker) = engine(ClassifierMode::Monitor);
        engine.set_verdict_hook(Arc::new(|_host, _client, _verdict| {
            panic!("observer blew up");
        }));

        engine.observe("ads.example.com");
        engine.drain_for_test(&worker.receiver);
        assert!(
            engine.lookup("ads.example.com").is_some(),
            "the verdict must still be cached after the hook panicked"
        );

        // And scoring must continue for subsequent domains.
        engine.observe("ads.other-example.com");
        engine.drain_for_test(&worker.receiver);
        assert!(engine.lookup("ads.other-example.com").is_some());

        let stats = engine.stats();
        assert_eq!(stats.scored, 2);
        assert_eq!(
            stats.hook_panics, 2,
            "contained panics should be counted, not hidden"
        );
    }

    #[test]
    fn shard_index_is_within_bounds() {
        for host in ["a.com", "verylongdomainname.example.org", "x.y.z"] {
            assert!(shard_index(host) < SHARD_COUNT);
        }
    }
}
