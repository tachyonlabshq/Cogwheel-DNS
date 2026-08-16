//! On-device adaptation: a bounded, sparse, additive correction layered over the shipped model.
//!
//! # Why adaptation is shaped like this
//!
//! An appliance that silently retrains itself into blocking a bank is far worse than one that never
//! adapts at all. So the question this module answers is not "how do we learn from feedback?" — a
//! logistic step is three lines — but "what shape of learning cannot go badly wrong?". Four
//! structural decisions do the work, and each removes a failure mode rather than making it unlikely:
//!
//! * **The base model is immutable.** Nothing here ever rewrites `model/cogwheel-ads-v1.cwm`.
//!   Adaptation is a second object that sits beside it, so the shipped behaviour is always
//!   recoverable by discarding a file rather than by restoring one.
//! * **The correction is additive and linear.** The base is
//!   `z = bias + Σ w·x`, so a correction is just another weight vector and
//!   `z' = z + Δbias + Σ Δw·x`. That is what makes this feature tractable *and* exactly auditable:
//!   [`crate::model::Model::explain_with_delta`] still returns real arithmetic, because the sum of a
//!   linear model and a linear correction is a linear model.
//! * **The correction is bounded by construction.** [`Delta::certified_max_logit_shift`] is an exact
//!   upper bound on how far a delta can move *any* score, computed from the delta alone with no
//!   reference to data. It is enforced at training time, re-checked on load, and capped by
//!   [`DELTA_LOGIT_BUDGET`]. See "The budget" below for what that buys.
//! * **The correction is local, not a mood.** Feedback is a self-selected sample, so its class
//!   balance says nothing about how common ad domains really are. [`base_rate_neutral_weights`]
//!   cancels that component out, leaving only the part of the feedback that is about *which* domains
//!   the base misjudges. Without it, ordinary well-labelled feedback taught the delta a uniform
//!   +0.3-logit shift on every hostname in existence, and the gate rejected all of it.
//! * **Promotion is gated on the committed holdout.** [`evaluate_and_gate`] refuses a delta that
//!   ranks worse or blocks more benign domains than the base does. Rollback is deleting the delta.
//!
//! # The budget
//!
//! For a normalised host, [`crate::features::extract`] L2-normalises the hashed block, so
//! `‖x_ngram‖₂ = 1` exactly, and every dense feature lies in `[0, 1.5]`
//! ([`MAX_DENSE_FEATURE_VALUE`], pinned by `features::tests::dense_features_are_bounded`).
//! Cauchy–Schwarz then gives an exact, data-free bound on the correction:
//!
//! ```text
//! |Δz(x)|  ≤  |Δbias| + 1.5·‖Δdense‖₁ + ‖Δngram‖₂
//! ```
//!
//! That right-hand side is [`Delta::certified_max_logit_shift`], and training projects the delta
//! until it is at most [`DELTA_LOGIT_BUDGET`] = 1.5 logits. With the shipped model's Platt slope
//! (0.847) that is ±1.27 calibrated logits, which means a concrete promise that can be stated
//! without qualification: **a domain the base model scores below ≈0.15 cannot be pushed above even
//! the most aggressive blocking threshold, no matter what feedback claims.** The correction is large
//! enough to move a genuinely borderline domain across a threshold and far too small to invent a
//! verdict out of nothing. `budget_cannot_flip_a_confident_allow` pins that.
//!
//! # What this is not
//!
//! It is not federated learning, it is not continual learning, and it does not chase drift on its
//! own. It applies exactly one correction, computed on demand from feedback the household gave
//! deliberately, and only if that correction survives the gate.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::allowlist::Allowlist;
use crate::features::{self, Features, N_BUCKETS, N_DENSE};
use crate::model::{Model, ModelQuality};
use crate::normalize::{self, NormalizeError};

/// File magic. Bump the trailing digit if the layout changes incompatibly.
pub const MAGIC: &[u8; 8] = b"CWDELTA1";

/// Layout version understood by this build.
pub const FORMAT_VERSION: u32 = 1;

/// Byte length of the fixed header preceding the weight blocks.
const HEADER_LEN: usize = 64;

/// Byte offset of the payload checksum field, which is excluded from its own computation.
const CHECKSUM_OFFSET: usize = 44;

/// Maximum number of hashed n-gram entries a delta may carry.
///
/// A long-lived appliance accumulates feedback for years, and an unbounded sparse map would grow
/// with it until the delta was larger than the model it corrects. 50,000 entries is generous — hosts
/// share n-grams heavily, so this only starts binding after several hundred distinct hostnames of
/// feedback — and it makes the worst case a fixed number rather than a function of uptime.
pub const MAX_NGRAM_ENTRIES: usize = 50_000;

/// Serialised size of a delta holding [`MAX_NGRAM_ENTRIES`] entries: 64 B header + 72 B dense +
/// 50,000 × 8 B = **400,136 bytes (391 KiB)**. That is the hard ceiling on the on-disk and in-memory
/// cost of adaptation, against a 1 MiB base model and a 16 MiB engine budget.
pub const MAX_SERIALISED_BYTES: usize = HEADER_LEN + N_DENSE * 4 + MAX_NGRAM_ENTRIES * 8;

/// Largest value any dense feature can take, from `features::dense_features`.
pub const MAX_DENSE_FEATURE_VALUE: f32 = 1.5;

/// Ceiling on [`Delta::certified_max_logit_shift`], in logits. See the module note.
pub const DELTA_LOGIT_BUDGET: f32 = 1.5;

/// Entries whose magnitude falls below this are dropped rather than stored.
///
/// SGD touches every n-gram of every example, so without pruning a delta would carry tens of
/// thousands of buckets holding numerical dust that cannot change a verdict but does consume the
/// entry budget that a genuinely corrective bucket might need.
const PRUNE_EPSILON: f32 = 1e-5;

/// Feedback items required before a delta is eligible for promotion.
///
/// Below this, the gate cannot tell a real correction from one household's idiosyncrasy, and a
/// delta trained on a handful of examples is mostly the noise of its own learning rate.
pub const MIN_FEEDBACK_EXAMPLES: usize = 20;

/// How far below the base's holdout ROC-AUC a delta may sit and still be promoted.
///
/// This is a policy budget, not a confidence interval. Both models are measured on the *same* 25,000
/// rows, so the difference is deterministic — there is no sampling noise to absorb. The tolerance
/// exists only because a correction that genuinely fixes a few dozen domains cannot help but perturb
/// the global ranking slightly, and refusing every such delta would make the feature useless.
/// 0.002 on a base AUC of 0.892 is a 0.2% relative give.
pub const AUC_TOLERANCE: f32 = 0.002;

/// Absolute slack in the false-positive gate, ≈11 of the holdout's 22,265 benign domains.
///
/// Below roughly this many domains, a difference says more about which benign hosts happened to be
/// sampled into the holdout than about the delta, and gating on it would reject useful corrections
/// for noise.
pub const FPR_TOLERANCE_ABS: f32 = 0.0005;

/// Relative slack in the false-positive gate.
///
/// The three operating points span a 20× range of false-positive budget (0.1% to 2%), so a single
/// absolute tolerance is either meaningless at the cautious end or punitive at the aggressive end.
/// The ceiling is therefore `base·1.10 + 0.0005`: adaptation may cost at most a tenth of the
/// false-positive budget the base already spends at that sensitivity.
pub const FPR_TOLERANCE_REL: f32 = 0.10;

/// The holdout committed alongside the model, embedded so the gate needs no filesystem access.
///
/// This costs ~533 KB in the server binary, on top of the 1 MiB model. That is the right trade: a
/// gate whose data lives on disk is a gate that can fail to find its data, and a gate that can fail
/// to find its data is a gate that someone will eventually make optional "just for this one case".
/// The promotion criterion has to be as unconditionally available as the model itself, and 533 KB on
/// an appliance with 4 GB of RAM is not a number worth trading a safety property for.
pub const EMBEDDED_HOLDOUT: &str = include_str!("../model/holdout.tsv");

/// Parse [`EMBEDDED_HOLDOUT`] into `(host, is_ad)` rows.
///
/// Malformed lines are skipped rather than failing: the file is committed and tested, so a bad line
/// would be caught long before shipping, and refusing to gate at all is a worse failure than gating
/// on 24,999 rows.
pub fn embedded_holdout() -> Vec<(String, bool)> {
    EMBEDDED_HOLDOUT
        .lines()
        .filter_map(|line| {
            let (host, label) = line.rsplit_once('\t')?;
            let label = match label.trim() {
                "1" => true,
                "0" => false,
                _ => return None,
            };
            Some((host.to_string(), label))
        })
        .collect()
}

/// Errors from loading or validating a delta file.
#[derive(Debug, Clone, PartialEq)]
pub enum DeltaError {
    /// Buffer is shorter than the header.
    Truncated,
    /// Magic bytes did not match.
    BadMagic,
    /// Format version is not [`FORMAT_VERSION`].
    UnsupportedVersion(u32),
    /// Header geometry disagrees with this build's feature layout.
    GeometryMismatch {
        /// What the file claims, as `(n_dense, n_buckets)`.
        found: (u32, u32),
        /// What this build requires.
        expected: (u32, u32),
    },
    /// Declared entry count exceeds [`MAX_NGRAM_ENTRIES`].
    TooManyEntries {
        /// What the file claims.
        found: usize,
        /// The ceiling.
        max: usize,
    },
    /// Declared blocks do not fit in the buffer.
    LengthMismatch {
        /// Bytes the header implies.
        expected: usize,
        /// Bytes actually present.
        found: usize,
    },
    /// A stored float was NaN or infinite.
    NonFiniteParameter(&'static str),
    /// A header field did not fit this platform's word size.
    FieldOutOfRange(&'static str),
    /// Bucket indices were not strictly increasing, or one was out of range.
    MalformedBuckets,
    /// The payload checksum did not match the header.
    ChecksumMismatch {
        /// What the header claims.
        expected: u32,
        /// What the payload computes to.
        found: u32,
    },
    /// The delta's certified worst-case logit shift exceeds [`DELTA_LOGIT_BUDGET`].
    BudgetExceeded {
        /// The delta's certified shift.
        found: f32,
        /// The ceiling.
        budget: f32,
    },
    /// A hex-encoded delta contained a character outside `[0-9a-fA-F]`, or had odd length.
    MalformedHex,
}

impl core::fmt::Display for DeltaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(f, "delta buffer is shorter than the header"),
            Self::BadMagic => write!(f, "delta magic bytes did not match"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported delta format version {v}"),
            Self::GeometryMismatch { found, expected } => write!(
                f,
                "delta geometry {found:?} does not match this build's feature layout {expected:?}"
            ),
            Self::TooManyEntries { found, max } => {
                write!(
                    f,
                    "delta declares {found} n-gram entries, over the {max} cap"
                )
            }
            Self::LengthMismatch { expected, found } => write!(
                f,
                "delta declares {expected} bytes but buffer holds {found}"
            ),
            Self::NonFiniteParameter(name) => write!(f, "delta parameter {name} is not finite"),
            Self::FieldOutOfRange(name) => {
                write!(f, "delta field {name} does not fit this platform")
            }
            Self::MalformedBuckets => {
                write!(
                    f,
                    "delta bucket indices are unsorted, duplicated or out of range"
                )
            }
            Self::ChecksumMismatch { expected, found } => write!(
                f,
                "delta checksum {found:#010x} does not match the declared {expected:#010x}"
            ),
            Self::BudgetExceeded { found, budget } => write!(
                f,
                "delta certified logit shift {found:.4} exceeds the {budget:.4} budget"
            ),
            Self::MalformedHex => write!(f, "delta hex encoding is malformed"),
        }
    }
}

impl std::error::Error for DeltaError {}

/// A sparse additive correction layered on the base model.
///
/// Scoring with a delta is `sigmoid(platt_a·(z_base + Δz) + platt_b)`, where
/// `Δz = Δbias + Σ Δdense·x_dense + Σ Δngram[b]·x[b]`. The n-gram block is sparse because feedback
/// only ever touches the buckets its own hostnames hash into — a few hundred out of 2^20.
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    dense: [f32; N_DENSE],
    ngram: BTreeMap<u32, f32>,
    bias: f32,
    trained_at: i64,
    example_count: usize,
}

impl Default for Delta {
    fn default() -> Self {
        Self::noop()
    }
}

/// FNV-1a over a byte slice. Same hash the feature extractor uses; this only needs to catch a
/// half-written file or a flipped bit, not an adversary.
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c_9dc5u32;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn read_u32(buffer: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
    ])
}

fn read_u64(buffer: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buffer[offset..offset + 8]);
    u64::from_le_bytes(bytes)
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

fn checked(value: f32, name: &'static str) -> Result<f32, DeltaError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(DeltaError::NonFiniteParameter(name))
    }
}

impl Delta {
    /// The all-zero delta: applying it changes no score by any amount.
    pub fn noop() -> Self {
        Self {
            dense: [0.0; N_DENSE],
            ngram: BTreeMap::new(),
            bias: 0.0,
            trained_at: 0,
            example_count: 0,
        }
    }

    /// Whether every coefficient is exactly zero, so scoring is bit-identical to the base.
    pub fn is_noop(&self) -> bool {
        self.bias == 0.0
            && self.dense.iter().all(|weight| *weight == 0.0)
            && self.ngram.values().all(|weight| *weight == 0.0)
    }

    /// The correction this delta adds to the base logit for a pre-extracted feature vector.
    pub fn logit_shift(&self, features: &Features) -> f32 {
        let mut shift = self.bias;
        for (index, value) in features.dense.iter().enumerate() {
            shift += self.dense[index] * value;
        }
        // The map is sparse and the feature vector is sparse; both are small, and the feature side
        // is the shorter of the two, so probe the map rather than iterating it.
        for (bucket, value) in &features.buckets {
            if let Some(weight) = self.ngram.get(bucket) {
                shift += weight * value;
            }
        }
        shift
    }

    /// An exact upper bound on `|Δz|` over every possible input, computed from the delta alone.
    ///
    /// Derived in the module note: the hashed block is L2-normalised (`‖x‖₂ = 1`) so Cauchy–Schwarz
    /// bounds its contribution by `‖Δngram‖₂`, and every dense feature is bounded by
    /// [`MAX_DENSE_FEATURE_VALUE`]. Because this needs no data, it is checkable at load time — which
    /// is what makes the bound a property of the file rather than a property of the training run.
    pub fn certified_max_logit_shift(&self) -> f32 {
        let dense_l1: f32 = self.dense.iter().map(|weight| weight.abs()).sum();
        let ngram_l2: f32 = self
            .ngram
            .values()
            .map(|weight| weight * weight)
            .sum::<f32>()
            .sqrt();
        self.bias.abs() + MAX_DENSE_FEATURE_VALUE * dense_l1 + ngram_l2
    }

    /// Scale the whole delta down until its certified shift fits `budget`.
    ///
    /// [`Self::certified_max_logit_shift`] is positively homogeneous, so one uniform scale factor
    /// brings it to exactly the budget while preserving the *direction* the feedback pointed in —
    /// the correction is weakened, never re-shaped into a different correction.
    fn project_into_budget(&mut self, budget: f32) {
        let certified = self.certified_max_logit_shift();
        if !certified.is_finite() || certified <= budget || certified <= 0.0 {
            return;
        }
        let scale = budget / certified;
        self.bias *= scale;
        for weight in &mut self.dense {
            *weight *= scale;
        }
        for weight in self.ngram.values_mut() {
            *weight *= scale;
        }
    }

    /// Number of hashed n-gram entries carried.
    pub fn ngram_entries(&self) -> usize {
        self.ngram.len()
    }

    /// The dense correction block, aligned with `crate::features::dense_features`.
    pub fn dense(&self) -> &[f32; N_DENSE] {
        &self.dense
    }

    /// The intercept correction.
    pub fn bias(&self) -> f32 {
        self.bias
    }

    /// The correction for one hashed n-gram bucket, or zero if untouched.
    pub fn ngram_weight(&self, bucket: u32) -> f32 {
        self.ngram.get(&bucket).copied().unwrap_or(0.0)
    }

    /// Unix timestamp (seconds) at which this delta was trained.
    pub fn trained_at(&self) -> i64 {
        self.trained_at
    }

    /// How many feedback items produced this delta.
    pub fn example_count(&self) -> usize {
        self.example_count
    }

    /// Serialise to the on-disk format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let entries: Vec<(u32, f32)> = self
            .ngram
            .iter()
            .map(|(bucket, weight)| (*bucket, *weight))
            .collect();

        let mut buffer = Vec::with_capacity(HEADER_LEN + N_DENSE * 4 + entries.len() * 8);
        buffer.extend_from_slice(MAGIC);
        buffer.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buffer.extend_from_slice(&(N_DENSE as u32).to_le_bytes());
        buffer.extend_from_slice(&(N_BUCKETS as u32).to_le_bytes());
        buffer.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        buffer.extend_from_slice(&self.bias.to_le_bytes());
        buffer.extend_from_slice(&self.trained_at.to_le_bytes());
        buffer.extend_from_slice(&(self.example_count as u64).to_le_bytes());
        buffer.extend_from_slice(&0u32.to_le_bytes()); // checksum, filled in below
        buffer.extend_from_slice(&[0u8; HEADER_LEN - CHECKSUM_OFFSET - 4]);
        debug_assert_eq!(buffer.len(), HEADER_LEN);

        for weight in self.dense {
            buffer.extend_from_slice(&weight.to_le_bytes());
        }
        for (bucket, weight) in entries {
            buffer.extend_from_slice(&bucket.to_le_bytes());
            buffer.extend_from_slice(&weight.to_le_bytes());
        }

        let checksum = checksum_of(&buffer);
        buffer[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());
        buffer
    }

    /// Parse a delta from its serialised bytes.
    ///
    /// Every field is validated before use. This buffer comes from disk on a device that can lose
    /// power mid-write, and the consequence of accepting a corrupt one is not a crash — it is a
    /// silently wrong verdict on somebody's bank. So the geometry, the entry count, the bucket
    /// ordering, the finiteness of every float, the payload checksum *and* the certified budget are
    /// all re-derived here rather than trusted.
    pub fn from_bytes(buffer: &[u8]) -> Result<Self, DeltaError> {
        if buffer.len() < HEADER_LEN {
            return Err(DeltaError::Truncated);
        }
        if &buffer[0..8] != MAGIC {
            return Err(DeltaError::BadMagic);
        }
        let version = read_u32(buffer, 8);
        if version != FORMAT_VERSION {
            return Err(DeltaError::UnsupportedVersion(version));
        }

        let found = (read_u32(buffer, 12), read_u32(buffer, 16));
        let expected = (N_DENSE as u32, N_BUCKETS as u32);
        if found != expected {
            return Err(DeltaError::GeometryMismatch { found, expected });
        }

        let entry_count = usize::try_from(read_u32(buffer, 20))
            .map_err(|_| DeltaError::FieldOutOfRange("entry_count"))?;
        if entry_count > MAX_NGRAM_ENTRIES {
            return Err(DeltaError::TooManyEntries {
                found: entry_count,
                max: MAX_NGRAM_ENTRIES,
            });
        }

        let bias = checked(read_f32(buffer, 24), "bias")?;
        let trained_at = read_i64(buffer, 28);
        let example_count = usize::try_from(read_u64(buffer, 36))
            .map_err(|_| DeltaError::FieldOutOfRange("example_count"))?;
        let declared_checksum = read_u32(buffer, CHECKSUM_OFFSET);

        let dense_start = HEADER_LEN;
        let ngram_start = dense_start + N_DENSE * 4;
        let total = ngram_start + entry_count * 8;
        if buffer.len() != total {
            return Err(DeltaError::LengthMismatch {
                expected: total,
                found: buffer.len(),
            });
        }

        let actual_checksum = checksum_of(buffer);
        if actual_checksum != declared_checksum {
            return Err(DeltaError::ChecksumMismatch {
                expected: declared_checksum,
                found: actual_checksum,
            });
        }

        let mut dense = [0.0f32; N_DENSE];
        for (index, weight) in dense.iter_mut().enumerate() {
            *weight = checked(read_f32(buffer, dense_start + index * 4), "dense")?;
        }

        let mut ngram = BTreeMap::new();
        let mut previous: Option<u32> = None;
        for index in 0..entry_count {
            let offset = ngram_start + index * 8;
            let bucket = read_u32(buffer, offset);
            // Strictly increasing catches both an out-of-order file and a duplicate bucket, which a
            // `BTreeMap` insert would otherwise absorb by silently discarding one of the two.
            if bucket as usize >= N_BUCKETS || previous.is_some_and(|last| bucket <= last) {
                return Err(DeltaError::MalformedBuckets);
            }
            previous = Some(bucket);
            ngram.insert(bucket, checked(read_f32(buffer, offset + 4), "ngram")?);
        }

        let delta = Self {
            dense,
            ngram,
            bias,
            trained_at,
            example_count,
        };

        // The budget is the safety property; a file claiming a larger correction than this build
        // permits is rejected rather than quietly clamped, because clamping would hide the fact that
        // something produced a delta this build does not understand.
        let certified = delta.certified_max_logit_shift();
        if certified > DELTA_LOGIT_BUDGET * BUDGET_LOAD_TOLERANCE {
            return Err(DeltaError::BudgetExceeded {
                found: certified,
                budget: DELTA_LOGIT_BUDGET,
            });
        }

        Ok(delta)
    }

    /// Serialise to lowercase hex.
    ///
    /// The appliance's `settings` table is a key/value store of `TEXT`, so the delta needs a
    /// text-safe encoding to persist alongside every other setting rather than needing a file path,
    /// a backup rule and a restore path of its own.
    pub fn to_hex(&self) -> String {
        use std::fmt::Write as _;
        let bytes = self.to_bytes();
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            // Writing into a String is infallible; the result is discarded rather than unwrapped so
            // this stays inside the crate's no-panic rule.
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// Parse a hex-encoded delta, validating it exactly as [`Self::from_bytes`] does.
    pub fn from_hex(text: &str) -> Result<Self, DeltaError> {
        let text = text.trim();
        let chunks = text.as_bytes().chunks_exact(2);
        if !chunks.remainder().is_empty() {
            return Err(DeltaError::MalformedHex);
        }
        let mut buffer = Vec::with_capacity(text.len() / 2);
        for pair in chunks {
            let high = (pair[0] as char)
                .to_digit(16)
                .ok_or(DeltaError::MalformedHex)?;
            let low = (pair[1] as char)
                .to_digit(16)
                .ok_or(DeltaError::MalformedHex)?;
            buffer.push((high * 16 + low) as u8);
        }
        Self::from_bytes(&buffer)
    }
}

#[cfg(test)]
impl Delta {
    /// Build a delta with known coefficients.
    ///
    /// Test-only, and deliberately not public: outside tests a delta only ever comes from
    /// [`train_delta`] or [`Delta::from_bytes`], both of which enforce the budget. Tests in sibling
    /// modules need a delta with a *predictable* effect, which a training run does not give them.
    pub(crate) fn for_test(bias: f32, dense: [f32; N_DENSE], ngram: &[(u32, f32)]) -> Self {
        Self {
            dense,
            ngram: ngram.iter().copied().collect(),
            bias,
            trained_at: 0,
            example_count: MIN_FEEDBACK_EXAMPLES,
        }
    }
}

/// Float slack allowed when re-checking the budget on load.
///
/// Serialisation round-trips `f32` bit-exactly and the entries deserialise in the same order they
/// were written, so the recomputed sum is identical in practice. The slack exists so that a future
/// change to summation order cannot turn a valid delta into an unloadable one.
const BUDGET_LOAD_TOLERANCE: f32 = 1.001;

/// Checksum every byte except the checksum field itself.
fn checksum_of(buffer: &[u8]) -> u32 {
    let mut hash = fnv1a(&buffer[..CHECKSUM_OFFSET]);
    for byte in &buffer[CHECKSUM_OFFSET + 4..] {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// One correction a household asked for.
///
/// `is_ad` is the *user's* claim, not the model's. `observed_at` orders competing claims about the
/// same host so the most recent one wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Feedback {
    /// The hostname as reported. Normalised through [`crate::normalize::normalize`] before use.
    pub host: String,
    /// `true` when the household says this is an ad or tracking domain.
    pub is_ad: bool,
    /// When the report was made.
    pub observed_at: DateTime<Utc>,
}

impl Feedback {
    /// The normalised form of [`Self::host`].
    ///
    /// # Errors
    ///
    /// Returns [`NormalizeError`] when the host is not a scoreable name. Unnormalisable feedback is
    /// rejected rather than kept: the model has never seen an IP literal or a single-label name, so
    /// "learning" from one would only smear the correction across whatever n-grams it happened to
    /// produce.
    pub fn normalized_host(&self) -> Result<String, NormalizeError> {
        normalize::normalize(&self.host)
    }
}

/// Hyperparameters for [`train_delta`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptConfig {
    /// Passes over the feedback set.
    pub epochs: usize,
    /// Initial learning rate; decayed linearly to zero across all steps.
    pub learning_rate: f32,
    /// L2 penalty applied to touched weights, which keeps a bucket seen in one example from
    /// drifting on the strength of that one example.
    pub l2: f32,
    /// Wall-clock ceiling on the whole run.
    pub time_budget: Duration,
}

impl Default for AdaptConfig {
    fn default() -> Self {
        // This runs on a Pi 5 alongside the resolver, so both bounds are hard: 12 passes over at
        // most a few thousand hostnames is well under a second, and the time budget is the backstop
        // for the case where "a few thousand" turns out to be wrong. Neither is a quality knob —
        // the gate decides quality — so they are set for predictable cost, not for convergence.
        Self {
            epochs: 12,
            learning_rate: 0.10,
            l2: 1e-6,
            time_budget: Duration::from_secs(2),
        }
    }
}

/// Widest per-class weight the base-rate correction may apply.
///
/// When one class is already almost perfectly predicted its share of the initial gradient is tiny,
/// and the exact cancellation would hand the other class an arbitrarily large multiplier. Clamping
/// keeps the balancing a correction rather than a lever.
const MAX_CLASS_WEIGHT: f32 = 8.0;

/// Per-class sample weights that make the delta's initial gradient sum to zero.
///
/// **This is the difference between a correction and a mood.** Feedback is a self-selected sample —
/// a household reports what annoyed it, not a random draw from its traffic — so the *proportion* of
/// positives in a feedback set says nothing about how common ad domains actually are. Left
/// unweighted, a set that happens to be a third positives teaches the delta a uniformly positive
/// shift on every hostname in the world, because the fastest way to reduce that loss is to raise
/// everything. Measured on real feedback that shift was +0.3 logits at the *first* percentile of the
/// holdout: not a correction to specific domains, just the base rate leaking in through the back
/// door, and the gate correctly refused it every time.
///
/// Weighting the classes so their opening gradients cancel removes exactly that component. What
/// survives is the part of the feedback that is actually about *which* domains the base misjudges,
/// which is the only part a self-selected sample is entitled to speak about. The intercept then has
/// no consistent direction to drift in and stays near zero on its own, rather than being pinned
/// there by fiat.
fn base_rate_neutral_weights(base: &Model, prepared: &[(Features, f32, f32)]) -> (f32, f32) {
    let mut positive_pull = 0.0f32;
    let mut negative_pull = 0.0f32;
    for (_, base_logit, target) in prepared {
        let probability = base.calibrate(*base_logit);
        if *target > 0.5 {
            positive_pull += 1.0 - probability;
        } else {
            negative_pull += probability;
        }
    }
    // One class absent, or already predicted perfectly: there is no base-rate component to cancel,
    // so leave the loss alone rather than inventing a weight for it.
    if positive_pull <= f32::EPSILON || negative_pull <= f32::EPSILON {
        return (1.0, 1.0);
    }
    let total = positive_pull + negative_pull;
    (
        (total / (2.0 * positive_pull)).clamp(1.0 / MAX_CLASS_WEIGHT, MAX_CLASS_WEIGHT),
        (total / (2.0 * negative_pull)).clamp(1.0 / MAX_CLASS_WEIGHT, MAX_CLASS_WEIGHT),
    )
}

/// Train a bounded correction on the residual between the base model and the household's feedback.
///
/// The delta starts at zero and learns only the correction: for each example the base's logit is
/// computed once and held fixed, and SGD moves the delta's weights to close whatever gap remains.
/// Two consequences follow directly, and both are the point:
///
/// * With no usable feedback the delta is all-zeros and scoring is bit-identical to the base.
/// * The correction cannot "relearn" what the base already knows, because the base's own opinion is
///   already in the loss. The delta only ever encodes disagreement.
///
/// The result is pruned, capped at [`MAX_NGRAM_ENTRIES`] entries and projected into
/// [`DELTA_LOGIT_BUDGET`], so a single mislabelled example can shift a score by a bounded amount no
/// matter how confidently it was labelled. Promotion is still [`evaluate_and_gate`]'s decision.
pub fn train_delta(base: &Model, feedback: &[Feedback], config: AdaptConfig) -> Delta {
    let started = Instant::now();
    let mut delta = Delta::noop();

    // Collapse to one claim per host, most recent wins: a household that first reports a domain as
    // an ad and later corrects itself means the correction, and training on both would teach the
    // delta to split the difference.
    let mut ordered: Vec<(String, bool)> = Vec::new();
    let mut position: HashMap<String, usize> = HashMap::new();
    let mut by_time: Vec<&Feedback> = feedback.iter().collect();
    by_time.sort_by_key(|item| item.observed_at);
    for item in by_time {
        let Ok(host) = item.normalized_host() else {
            continue;
        };
        match position.get(&host) {
            Some(index) => {
                if let Some(entry) = ordered.get_mut(*index) {
                    entry.1 = item.is_ad;
                }
            }
            None => {
                position.insert(host.clone(), ordered.len());
                ordered.push((host, item.is_ad));
            }
        }
    }
    if ordered.is_empty() {
        return delta;
    }

    // Extract features and the base logit once. Extraction is ~90% of the per-example cost and the
    // base's opinion never changes during the run, so recomputing either per epoch would multiply
    // the whole training time by `epochs` for nothing.
    let prepared: Vec<(Features, f32, f32)> = ordered
        .iter()
        .map(|(host, is_ad)| {
            let features = features::extract(host);
            let base_logit = base.logit_from_features(&features);
            (features, base_logit, if *is_ad { 1.0 } else { 0.0 })
        })
        .collect();

    let (platt_a, _) = base.calibration();
    let (positive_weight, negative_weight) = base_rate_neutral_weights(base, &prepared);
    let total_steps = (config.epochs * prepared.len()).max(1);
    let mut step = 0usize;
    let mut since_clock_check = 0usize;

    'training: for _ in 0..config.epochs {
        for (features, base_logit, target) in &prepared {
            // Checking the clock every example would cost more than the example does; every 64 is
            // fine-grained enough to stop within a millisecond of the budget.
            if since_clock_check >= 64 {
                since_clock_check = 0;
                if started.elapsed() >= config.time_budget {
                    break 'training;
                }
            }
            since_clock_check += 1;

            let shift = delta.logit_shift(features);
            // The loss is on the *calibrated* probability, because that is what the thresholds and
            // the gate are expressed in. The Platt slope therefore appears in the gradient.
            let probability = base.calibrate(base_logit + shift);
            let learning_rate =
                config.learning_rate * (1.0 - step as f32 / total_steps as f32).max(0.0);
            let sample_weight = if *target > 0.5 {
                positive_weight
            } else {
                negative_weight
            };
            let gradient = sample_weight * (probability - target) * platt_a * learning_rate;

            delta.bias -= gradient;
            for (index, value) in features.dense.iter().enumerate() {
                let weight = &mut delta.dense[index];
                *weight -= gradient * value + learning_rate * config.l2 * *weight;
            }
            for (bucket, value) in &features.buckets {
                if let Some(weight) = delta.ngram.get_mut(bucket) {
                    *weight -= gradient * value + learning_rate * config.l2 * *weight;
                } else if delta.ngram.len() < MAX_NGRAM_ENTRIES {
                    // Once the entry cap is reached, later buckets are refused rather than evicting
                    // an earlier one. Eviction would make the delta depend on feedback order in a
                    // way nobody could reason about; refusal just means the correction stops
                    // learning new n-grams, which is a bounded and explainable loss.
                    delta.ngram.insert(*bucket, -gradient * value);
                }
            }
            step += 1;
        }
    }

    // A non-finite weight means the arithmetic diverged. There is no safe way to ship half of a
    // diverged correction, so discard the whole thing and stay on the base.
    if !delta.bias.is_finite()
        || delta.dense.iter().any(|weight| !weight.is_finite())
        || delta.ngram.values().any(|weight| !weight.is_finite())
    {
        return Delta::noop();
    }

    delta
        .ngram
        .retain(|_, weight| weight.abs() >= PRUNE_EPSILON);
    delta.project_into_budget(DELTA_LOGIT_BUDGET);
    delta.trained_at = Utc::now().timestamp();
    delta.example_count = prepared.len();
    delta
}

/// The result of putting a trained delta in front of the promotion gate.
#[derive(Debug, Clone, PartialEq)]
pub enum AdaptationOutcome {
    /// The delta cleared every criterion and may be installed.
    Promoted {
        /// ROC-AUC of base+delta measured on the committed holdout.
        auc: f32,
        /// False-positive rate of base+delta at the three calibrated thresholds.
        false_positive_rate: [f32; 3],
        /// Feedback items behind the delta.
        example_count: usize,
    },
    /// The delta failed at least one criterion and must be discarded.
    Rejected {
        /// Which criterion failed, with the numbers that failed it.
        reason: String,
        /// ROC-AUC of base+delta measured on the committed holdout.
        auc: f32,
        /// False-positive rate of base+delta at the three calibrated thresholds.
        false_positive_rate: [f32; 3],
    },
    /// Too little feedback to judge a correction at all; nothing was measured.
    NotEnoughData {
        /// Feedback items available.
        have: usize,
        /// Feedback items required.
        need: usize,
    },
}

/// Names of the three operating points, for gate messages.
const THRESHOLD_NAMES: [&str; 3] = ["low", "balanced", "high"];

/// Decide whether a delta may be promoted, by measuring it against the committed holdout.
///
/// A delta is promoted **only if all** of the following hold, and is otherwise rejected with the
/// specific criterion that failed:
///
/// 1. **Ranking does not regress.** ROC-AUC of base+delta is within [`AUC_TOLERANCE`] of the base's
///    AUC *measured on this same holdout*. The header's recorded AUC is deliberately not used for
///    this comparison: it was measured on the full test split, so comparing against it would confuse
///    "the delta made things worse" with "this holdout is a slightly different sample".
/// 2. **No sensitivity gets more false positives.** At each of the three calibrated thresholds the
///    delta's false-positive rate stays under `base·(1 + `[`FPR_TOLERANCE_REL`]`) + `
///    [`FPR_TOLERANCE_ABS`], where `base` is the worse of the header's recorded rate and the rate
///    the base actually achieves on this holdout. This is the criterion that matters most: a false
///    positive is a website that stopped working for a real household.
/// 3. **The protected allowlist still holds.** Asserted rather than assumed — see
///    [`crate::allowlist`].
///
/// `base_quality` is the model's recorded quality, normally `base.quality()`; it is a parameter so a
/// caller can gate against a stricter promise than the one in the file.
pub fn evaluate_and_gate(
    base: &Model,
    delta: &Delta,
    holdout: &[(String, bool)],
    base_quality: ModelQuality,
) -> AdaptationOutcome {
    if delta.example_count() < MIN_FEEDBACK_EXAMPLES {
        return AdaptationOutcome::NotEnoughData {
            have: delta.example_count(),
            need: MIN_FEEDBACK_EXAMPLES,
        };
    }

    // Extract once, score twice. Feature extraction dominates inference cost, and the gate needs
    // both models' opinion of the same 25,000 hosts.
    let mut base_scores: Vec<f32> = Vec::with_capacity(holdout.len());
    let mut delta_scores: Vec<f32> = Vec::with_capacity(holdout.len());
    let mut labels: Vec<bool> = Vec::with_capacity(holdout.len());
    for (host, label) in holdout {
        let features = features::extract(host);
        base_scores.push(base.probability_from_features_with_delta(&features, None));
        delta_scores.push(base.probability_from_features_with_delta(&features, Some(delta)));
        labels.push(*label);
    }

    let thresholds = base.thresholds();
    let threshold_values = [thresholds.low, thresholds.balanced, thresholds.high];
    let delta_fpr = false_positive_rates(&delta_scores, &labels, threshold_values);

    let positives = labels.iter().filter(|label| **label).count();
    if positives == 0 || positives == labels.len() {
        return AdaptationOutcome::Rejected {
            reason: format!(
                "holdout has {positives} positives out of {}; it cannot separate anything",
                labels.len()
            ),
            auc: 0.5,
            false_positive_rate: delta_fpr,
        };
    }

    let base_auc = roc_auc(&base_scores, &labels);
    let delta_auc = roc_auc(&delta_scores, &labels);
    let base_fpr = false_positive_rates(&base_scores, &labels, threshold_values);

    if delta_auc < base_auc - AUC_TOLERANCE {
        return AdaptationOutcome::Rejected {
            reason: format!(
                "ROC-AUC fell to {delta_auc:.5} from the base's {base_auc:.5} on the committed holdout, past the {AUC_TOLERANCE} tolerance"
            ),
            auc: delta_auc,
            false_positive_rate: delta_fpr,
        };
    }

    for index in 0..3 {
        // The worse of the two baselines: the recorded figure is the product promise, the measured
        // one is what the base actually does on these exact rows. Gating on the tighter of the two
        // would reject deltas for a gap the base already has.
        let reference = base_quality.false_positive_rate[index].max(base_fpr[index]);
        let ceiling = reference * (1.0 + FPR_TOLERANCE_REL) + FPR_TOLERANCE_ABS;
        if delta_fpr[index] > ceiling {
            return AdaptationOutcome::Rejected {
                reason: format!(
                    "false-positive rate at {} sensitivity rose to {:.5} against a ceiling of {ceiling:.5} (base {reference:.5})",
                    THRESHOLD_NAMES[index], delta_fpr[index]
                ),
                auc: delta_auc,
                false_positive_rate: delta_fpr,
            };
        }
    }

    if let Some(reason) = protected_domains_still_shielded(base, delta) {
        return AdaptationOutcome::Rejected {
            reason,
            auc: delta_auc,
            false_positive_rate: delta_fpr,
        };
    }

    AdaptationOutcome::Promoted {
        auc: delta_auc,
        false_positive_rate: delta_fpr,
        example_count: delta.example_count(),
    }
}

/// Check that no protected domain would be blocked under the delta.
///
/// The allowlist is consulted after scoring and can only ever prevent a block, so under the current
/// engine this cannot fail — which is exactly why it is asserted here rather than assumed. The
/// condition below is the same conjunction `ClassifierEngine::decide` evaluates, so if the two ever
/// drift apart, adaptation stops rather than the safety net quietly going missing.
fn protected_domains_still_shielded(base: &Model, delta: &Delta) -> Option<String> {
    let allowlist = Allowlist::builtin();
    // The most aggressive threshold is the one that blocks the most, so clearing it clears all three.
    let threshold = base.thresholds().high;
    for suffix in allowlist.suffixes() {
        for host in [suffix.clone(), format!("ads.{suffix}")] {
            let probability = base.probability_with_delta(&host, Some(delta));
            if probability >= threshold && !allowlist.is_protected(&host) {
                return Some(format!(
                    "protected domain {host} would be blocked at {probability:.4} under this delta"
                ));
            }
        }
    }
    None
}

/// False-positive rate at each of three thresholds, in `[low, balanced, high]` order.
fn false_positive_rates(scores: &[f32], labels: &[bool], thresholds: [f32; 3]) -> [f32; 3] {
    let negatives = labels.iter().filter(|label| !**label).count();
    if negatives == 0 {
        return [0.0; 3];
    }
    let mut rates = [0.0f32; 3];
    for (index, threshold) in thresholds.iter().enumerate() {
        let false_positives = scores
            .iter()
            .zip(labels)
            .filter(|(score, label)| !**label && **score >= *threshold)
            .count();
        rates[index] = false_positives as f32 / negatives as f32;
    }
    rates
}

/// Area under the ROC curve by rank sum (Mann–Whitney U), with ties averaged.
///
/// Duplicated from `train::roc_auc` on purpose: that module is behind the `training` feature and
/// never reaches the appliance, but the gate has to run *on* the appliance.
fn roc_auc(scores: &[f32], labels: &[bool]) -> f32 {
    let positives = labels.iter().filter(|label| **label).count();
    let negatives = labels.len() - positives;
    if positives == 0 || negatives == 0 {
        return 0.5;
    }
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|a, b| {
        scores[*a]
            .partial_cmp(&scores[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
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
    ((rank_sum - positives_f * (positives_f + 1.0) / 2.0) / (positives_f * negatives_f)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedded_model;

    fn feedback(host: &str, is_ad: bool) -> Feedback {
        Feedback {
            host: host.to_string(),
            is_ad,
            observed_at: Utc::now(),
        }
    }

    /// Feedback claiming a set of clearly ad-like hosts really are ads — the delta should agree with
    /// the base and barely move anything.
    fn agreeable_feedback() -> Vec<Feedback> {
        let mut items = Vec::new();
        for index in 0..30 {
            items.push(feedback(
                &format!("ads{index}.tracker-net{index}.example"),
                true,
            ));
            items.push(feedback(
                &format!("wiki{index}.library{index}.example"),
                false,
            ));
        }
        items
    }

    fn trained(feedback: &[Feedback]) -> Delta {
        let base = embedded_model().expect("embedded model must parse");
        train_delta(&base, feedback, AdaptConfig::default())
    }

    #[test]
    fn round_trips_through_the_binary_format() {
        let delta = trained(&agreeable_feedback());
        assert!(!delta.is_noop(), "the fixture should produce a real delta");
        let bytes = delta.to_bytes();
        let restored = Delta::from_bytes(&bytes).expect("delta should parse");
        assert_eq!(restored, delta);
        assert_eq!(restored.trained_at(), delta.trained_at());
        assert_eq!(restored.example_count(), delta.example_count());
        assert_eq!(restored.ngram_entries(), delta.ngram_entries());

        // And the scores it produces must be bit-identical, which is the property that actually
        // matters after a reboot.
        let base = embedded_model().expect("parse");
        for host in ["ads1.tracker-net1.example", "example.com", "chase.com"] {
            assert_eq!(
                base.probability_with_delta(host, Some(&restored)),
                base.probability_with_delta(host, Some(&delta))
            );
        }
    }

    #[test]
    fn hex_round_trips() {
        let delta = trained(&agreeable_feedback());
        let restored = Delta::from_hex(&delta.to_hex()).expect("hex should parse");
        assert_eq!(restored, delta);
    }

    #[test]
    fn rejects_malformed_hex() {
        assert_eq!(Delta::from_hex("abc"), Err(DeltaError::MalformedHex));
        assert_eq!(Delta::from_hex("zzzz"), Err(DeltaError::MalformedHex));
    }

    #[test]
    fn noop_delta_round_trips_and_changes_nothing() {
        let delta = Delta::noop();
        assert!(delta.is_noop());
        let restored = Delta::from_bytes(&delta.to_bytes()).expect("parse");
        assert_eq!(restored, delta);
        let base = embedded_model().expect("parse");
        for host in ["ads.example.com", "chase.com", "doubleclick.net"] {
            assert_eq!(
                base.probability_with_delta(host, Some(&delta)),
                base.probability(host)
            );
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = trained(&agreeable_feedback()).to_bytes();
        bytes[0] = b'X';
        assert_eq!(
            Delta::from_bytes(&bytes).expect_err("bad magic must fail"),
            DeltaError::BadMagic
        );
    }

    #[test]
    fn rejects_truncated_buffers() {
        assert_eq!(
            Delta::from_bytes(&[0u8; 10]).expect_err("truncated must fail"),
            DeltaError::Truncated
        );
        let bytes = trained(&agreeable_feedback()).to_bytes();
        let truncated = &bytes[..bytes.len() - 8];
        assert!(matches!(
            Delta::from_bytes(truncated),
            Err(DeltaError::LengthMismatch { .. })
        ));
    }

    /// A file half-written when the power went out is the realistic corruption on an appliance.
    #[test]
    fn rejects_every_truncation_point() {
        let bytes = trained(&agreeable_feedback()).to_bytes();
        for cut in (1..bytes.len()).step_by(97) {
            assert!(
                Delta::from_bytes(&bytes[..cut]).is_err(),
                "a delta truncated to {cut} bytes was accepted"
            );
        }
    }

    #[test]
    fn rejects_unsupported_versions() {
        let mut bytes = trained(&agreeable_feedback()).to_bytes();
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert_eq!(
            Delta::from_bytes(&bytes).expect_err("bad version must fail"),
            DeltaError::UnsupportedVersion(99)
        );
    }

    #[test]
    fn rejects_geometry_mismatch() {
        let mut bytes = trained(&agreeable_feedback()).to_bytes();
        bytes[16..20].copy_from_slice(&1024u32.to_le_bytes());
        assert!(matches!(
            Delta::from_bytes(&bytes),
            Err(DeltaError::GeometryMismatch { .. })
        ));
    }

    #[test]
    fn rejects_an_entry_count_over_the_cap() {
        let mut bytes = trained(&agreeable_feedback()).to_bytes();
        bytes[20..24].copy_from_slice(&(MAX_NGRAM_ENTRIES as u32 + 1).to_le_bytes());
        assert!(matches!(
            Delta::from_bytes(&bytes),
            Err(DeltaError::TooManyEntries { .. })
        ));
    }

    /// A single flipped bit anywhere in the payload must be caught, because the failure mode it
    /// produces otherwise is not a crash but a wrong verdict.
    #[test]
    fn rejects_a_flipped_bit_in_the_weights() {
        let bytes = trained(&agreeable_feedback()).to_bytes();
        let mut corrupted = bytes.clone();
        let target = bytes.len() - 3;
        corrupted[target] ^= 0b0000_0100;
        assert!(
            matches!(
                Delta::from_bytes(&corrupted),
                Err(DeltaError::ChecksumMismatch { .. })
            ),
            "a flipped payload bit slipped through"
        );
    }

    #[test]
    fn rejects_a_flipped_bit_in_the_header() {
        let mut bytes = trained(&agreeable_feedback()).to_bytes();
        bytes[30] ^= 0b0001_0000; // inside trained_at
        assert!(matches!(
            Delta::from_bytes(&bytes),
            Err(DeltaError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn rejects_non_finite_weights() {
        let mut bytes = trained(&agreeable_feedback()).to_bytes();
        bytes[24..28].copy_from_slice(&f32::NAN.to_le_bytes());
        let checksum = checksum_of(&bytes);
        bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(
            Delta::from_bytes(&bytes).expect_err("NaN bias must fail"),
            DeltaError::NonFiniteParameter("bias")
        );
    }

    #[test]
    fn rejects_unsorted_or_duplicated_buckets() {
        let delta = trained(&agreeable_feedback());
        assert!(delta.ngram_entries() >= 2);
        let mut bytes = delta.to_bytes();
        let first = HEADER_LEN + N_DENSE * 4;
        let second = first + 8;
        // Make the second bucket index equal the first, which is neither sorted nor unique.
        let duplicate = bytes[first..first + 4].to_vec();
        bytes[second..second + 4].copy_from_slice(&duplicate);
        let checksum = checksum_of(&bytes);
        bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(
            Delta::from_bytes(&bytes).expect_err("duplicate buckets must fail"),
            DeltaError::MalformedBuckets
        );
    }

    #[test]
    fn rejects_a_delta_over_the_logit_budget() {
        let mut delta = Delta::noop();
        delta.bias = DELTA_LOGIT_BUDGET * 4.0;
        delta.example_count = 100;
        let bytes = delta.to_bytes();
        assert!(matches!(
            Delta::from_bytes(&bytes),
            Err(DeltaError::BudgetExceeded { .. })
        ));
    }

    #[test]
    fn serialised_size_at_the_cap_matches_the_documented_budget() {
        assert_eq!(MAX_SERIALISED_BYTES, 400_136);
        let mut delta = Delta::noop();
        for bucket in 0..MAX_NGRAM_ENTRIES as u32 {
            delta.ngram.insert(bucket, 1e-4);
        }
        assert_eq!(delta.to_bytes().len(), MAX_SERIALISED_BYTES);
    }

    #[test]
    fn zero_feedback_produces_a_noop_delta() {
        let base = embedded_model().expect("parse");
        let delta = train_delta(&base, &[], AdaptConfig::default());
        assert!(delta.is_noop());
        assert_eq!(delta.example_count(), 0);
        assert_eq!(delta.ngram_entries(), 0);
        for host in ["ads.example.com", "chase.com", "doubleclick.net", "a.io"] {
            assert_eq!(
                base.probability_with_delta(host, Some(&delta)),
                base.probability(host),
                "{host} moved under a no-op delta"
            );
        }
    }

    #[test]
    fn unnormalisable_feedback_is_rejected_rather_than_learned_from() {
        let base = embedded_model().expect("parse");
        let items = vec![
            feedback("192.168.1.1", true),
            feedback("localhost", true),
            feedback("not a domain", true),
            feedback("", false),
        ];
        let delta = train_delta(&base, &items, AdaptConfig::default());
        assert!(delta.is_noop(), "unscoreable hosts must not train anything");
        assert_eq!(delta.example_count(), 0);
        assert!(items[0].normalized_host().is_err());
    }

    #[test]
    fn the_most_recent_claim_about_a_host_wins() {
        let base = embedded_model().expect("parse");
        let early = Feedback {
            host: "contested.example.com".to_string(),
            is_ad: true,
            observed_at: DateTime::from_timestamp(1_000, 0).unwrap_or_default(),
        };
        let late = Feedback {
            host: "contested.example.com".to_string(),
            is_ad: false,
            observed_at: DateTime::from_timestamp(2_000, 0).unwrap_or_default(),
        };
        let delta = train_delta(
            &base,
            &[late.clone(), early.clone()],
            AdaptConfig::default(),
        );
        assert_eq!(delta.example_count(), 1, "one host means one example");
        // The surviving claim is "not an ad", so the correction must push the score down.
        assert!(
            base.probability_with_delta("contested.example.com", Some(&delta))
                < base.probability("contested.example.com")
        );
    }

    #[test]
    fn training_respects_its_time_budget() {
        let base = embedded_model().expect("parse");
        let mut items = Vec::new();
        for index in 0..4_000 {
            items.push(feedback(
                &format!("h{index}.example{index}.com"),
                index % 3 == 0,
            ));
        }
        let started = Instant::now();
        let delta = train_delta(
            &base,
            &items,
            AdaptConfig {
                epochs: 500,
                time_budget: Duration::from_millis(150),
                ..AdaptConfig::default()
            },
        );
        let elapsed = started.elapsed();
        // Feature extraction happens before the loop and is not interruptible, so the assertion
        // bounds the *training* phase generously rather than the whole call tightly.
        assert!(
            elapsed < Duration::from_secs(20),
            "500 epochs over 4000 hosts ignored the budget: {elapsed:?}"
        );
        assert_eq!(delta.example_count(), 4_000);
    }

    #[test]
    fn a_trained_delta_always_fits_the_logit_budget() {
        // Deliberately absurd feedback: every well-known benign domain reported as an ad, at a
        // learning rate that would otherwise diverge.
        let mut items = Vec::new();
        for host in [
            "chase.com",
            "apple.com",
            "wikipedia.org",
            "github.com",
            "letsencrypt.org",
            "example.com",
            "bbc.co.uk",
            "gov.uk",
        ] {
            for _ in 0..8 {
                items.push(feedback(host, true));
            }
        }
        let base = embedded_model().expect("parse");
        let delta = train_delta(
            &base,
            &items,
            AdaptConfig {
                epochs: 50,
                learning_rate: 5.0,
                ..AdaptConfig::default()
            },
        );
        let certified = delta.certified_max_logit_shift();
        assert!(
            certified <= DELTA_LOGIT_BUDGET * BUDGET_LOAD_TOLERANCE,
            "certified shift {certified} escaped the {DELTA_LOGIT_BUDGET} budget"
        );
        assert!(Delta::from_bytes(&delta.to_bytes()).is_ok());
    }

    /// The budget's product promise: no amount of feedback can turn a confident allow into a block.
    #[test]
    fn budget_cannot_flip_a_confident_allow() {
        let base = embedded_model().expect("parse");
        let (platt_a, _) = base.calibration();
        let high = base.thresholds().high;
        // Worst case: a delta spending its entire budget pushing this one host upward.
        let ceiling_shift = DELTA_LOGIT_BUDGET * platt_a;
        let threshold_logit = (high / (1.0 - high)).ln();
        let safe_probability = 1.0 / (1.0 + (-(threshold_logit - ceiling_shift)).exp());
        assert!(
            safe_probability > 0.14,
            "the budget no longer protects confidently-allowed domains: {safe_probability}"
        );
        for host in ["chase.com", "apple.com", "wikipedia.org", "letsencrypt.org"] {
            let probability = base.probability(host);
            assert!(
                probability < safe_probability,
                "{host} scores {probability}, inside the range a delta could push over {high}"
            );
        }
    }

    #[test]
    fn too_little_feedback_is_not_enough_data() {
        let base = embedded_model().expect("parse");
        let items: Vec<Feedback> = (0..5)
            .map(|index| feedback(&format!("h{index}.example.com"), true))
            .collect();
        let delta = train_delta(&base, &items, AdaptConfig::default());
        let outcome = evaluate_and_gate(&base, &delta, &embedded_holdout(), base.quality());
        assert_eq!(
            outcome,
            AdaptationOutcome::NotEnoughData {
                have: 5,
                need: MIN_FEEDBACK_EXAMPLES
            }
        );
    }

    /// A delta trained on feedback that agrees with the base is a genuine, if small, improvement and
    /// must be promoted.
    #[test]
    fn a_helpful_delta_is_promoted() {
        let base = embedded_model().expect("parse");
        let holdout = embedded_holdout();

        // Take real holdout rows the base already ranks correctly and hand them back as feedback:
        // the household confirming what the model believes. Any correction learned from this is by
        // construction in the same direction the base already leans.
        let items: Vec<Feedback> = holdout
            .iter()
            .filter(|(host, label)| {
                let probability = base.probability(host);
                if *label {
                    probability > 0.8
                } else {
                    probability < 0.05
                }
            })
            .take(120)
            .map(|(host, label)| feedback(host, *label))
            .collect();
        assert!(items.len() >= MIN_FEEDBACK_EXAMPLES);

        let delta = train_delta(&base, &items, AdaptConfig::default());
        let outcome = evaluate_and_gate(&base, &delta, &holdout, base.quality());
        assert!(
            matches!(outcome, AdaptationOutcome::Promoted { .. }),
            "a confirming delta should have been promoted, got {outcome:?}"
        );
        if let AdaptationOutcome::Promoted {
            auc,
            false_positive_rate,
            example_count,
        } = outcome
        {
            assert!(auc > 0.85, "promoted with a suspiciously low AUC {auc}");
            assert!(false_positive_rate[1] < 0.01);
            assert_eq!(example_count, items.len());
        }
    }

    /// The gate's whole reason to exist: a delta that would break more benign websites is refused,
    /// even though it is a perfectly good fit to the feedback it was given.
    #[test]
    fn a_delta_that_regresses_false_positives_is_rejected() {
        let base = embedded_model().expect("parse");
        let holdout = embedded_holdout();

        // Mislabel a block of genuinely benign holdout domains as ads. This is exactly the shape of
        // a poisoning attempt, and also of an honest household clicking the wrong button 60 times.
        let items: Vec<Feedback> = holdout
            .iter()
            .filter(|(host, label)| !*label && base.probability(host) < 0.3)
            .take(200)
            .map(|(host, _)| feedback(host, true))
            .collect();
        assert!(items.len() >= MIN_FEEDBACK_EXAMPLES);

        let delta = train_delta(&base, &items, AdaptConfig::default());
        let outcome = evaluate_and_gate(&base, &delta, &holdout, base.quality());
        assert!(
            matches!(outcome, AdaptationOutcome::Rejected { .. }),
            "a false-positive-inducing delta must be rejected, got {outcome:?}"
        );
        if let AdaptationOutcome::Rejected { reason, .. } = &outcome {
            assert!(
                reason.contains("false-positive") || reason.contains("ROC-AUC"),
                "rejected for the wrong reason: {reason}"
            );
        }
    }

    /// The gate must reject on the measurement, not on a heuristic about the feedback — so a delta
    /// hand-built to raise every score is refused even though no feedback produced it.
    #[test]
    fn a_hand_built_score_raising_delta_is_rejected() {
        let base = embedded_model().expect("parse");
        let mut delta = Delta::noop();
        delta.bias = DELTA_LOGIT_BUDGET;
        delta.example_count = 500;
        delta.trained_at = Utc::now().timestamp();

        let outcome = evaluate_and_gate(&base, &delta, &embedded_holdout(), base.quality());
        assert!(
            matches!(outcome, AdaptationOutcome::Rejected { .. }),
            "a uniform score-raising delta must be rejected, got {outcome:?}"
        );
        if let AdaptationOutcome::Rejected {
            reason,
            false_positive_rate,
            ..
        } = &outcome
        {
            assert!(
                reason.contains("false-positive"),
                "unexpected reason: {reason}"
            );
            assert!(
                false_positive_rate[1] > base.quality().false_positive_rate[1],
                "the reported FPR should show the regression"
            );
        }
    }

    #[test]
    fn the_protected_allowlist_still_holds_under_a_delta() {
        let base = embedded_model().expect("parse");
        let allowlist = Allowlist::builtin();
        let mut delta = Delta::noop();
        // Spend the entire budget pushing every score upward.
        delta.bias = DELTA_LOGIT_BUDGET;
        delta.example_count = MIN_FEEDBACK_EXAMPLES;

        for suffix in allowlist.suffixes() {
            for host in [
                suffix.clone(),
                format!("ads.{suffix}"),
                format!("track.{suffix}"),
            ] {
                assert!(
                    allowlist.is_protected(&host),
                    "{host} lost its protection under a delta"
                );
                // The score may well rise past the threshold; the allowlist is what stops the block,
                // and it is consulted after scoring, so the delta cannot reach it.
                let probability = base.probability_with_delta(&host, Some(&delta));
                assert!((0.0..=1.0).contains(&probability));
            }
        }
        assert!(protected_domains_still_shielded(&base, &delta).is_none());
    }

    #[test]
    fn gate_reports_the_numbers_it_decided_on() {
        let base = embedded_model().expect("parse");
        let holdout = embedded_holdout();
        let items: Vec<Feedback> = holdout
            .iter()
            .filter(|(host, label)| *label && base.probability(host) > 0.8)
            .take(60)
            .map(|(host, label)| feedback(host, *label))
            .collect();
        let delta = train_delta(&base, &items, AdaptConfig::default());
        let outcome = evaluate_and_gate(&base, &delta, &holdout, base.quality());
        let measured = match &outcome {
            AdaptationOutcome::Promoted {
                auc,
                false_positive_rate,
                ..
            }
            | AdaptationOutcome::Rejected {
                auc,
                false_positive_rate,
                ..
            } => Some((*auc, *false_positive_rate)),
            AdaptationOutcome::NotEnoughData { .. } => None,
        };
        assert!(
            measured.is_some(),
            "60 items is enough data to measure, got {outcome:?}"
        );
        if let Some((auc, fpr)) = measured {
            assert!((0.0..=1.0).contains(&auc));
            for rate in fpr {
                assert!((0.0..=1.0).contains(&rate));
            }
            // Ordering is structural: a lower threshold cannot admit fewer false positives.
            assert!(fpr[0] <= fpr[1] && fpr[1] <= fpr[2]);
        }
    }

    #[test]
    fn an_empty_holdout_is_rejected_rather_than_waved_through() {
        let base = embedded_model().expect("parse");
        let mut delta = Delta::noop();
        delta.example_count = 100;
        let outcome = evaluate_and_gate(&base, &delta, &[], base.quality());
        assert!(matches!(outcome, AdaptationOutcome::Rejected { .. }));
    }

    #[test]
    fn a_single_class_holdout_is_rejected() {
        let base = embedded_model().expect("parse");
        let mut delta = Delta::noop();
        delta.example_count = 100;
        let holdout: Vec<(String, bool)> = (0..50)
            .map(|index| (format!("h{index}.example.com"), false))
            .collect();
        assert!(matches!(
            evaluate_and_gate(&base, &delta, &holdout, base.quality()),
            AdaptationOutcome::Rejected { .. }
        ));
    }

    #[test]
    fn embedded_holdout_matches_the_committed_file() {
        let holdout = embedded_holdout();
        assert_eq!(holdout.len(), 25_000);
        let positives = holdout.iter().filter(|(_, label)| *label).count();
        assert!((1_000..12_000).contains(&positives));
    }

    /// Regression guard for the failure that made the gate reject everything.
    ///
    /// Correctly-labelled feedback that happens to be a third positives used to teach the delta a
    /// uniform +0.3 logit shift on every domain in the world — the feedback's class balance leaking
    /// in as if it were the base rate. The correction has to be about *which* domains are misjudged,
    /// so the median shift across the holdout must sit near zero.
    #[test]
    fn feedback_class_balance_does_not_shift_every_score() {
        let base = embedded_model().expect("parse");
        let holdout = embedded_holdout();

        let mut items: Vec<Feedback> = Vec::new();
        for (host, label) in holdout.iter().filter(|(_, label)| !*label).take(220) {
            items.push(feedback(host, *label));
        }
        for (host, label) in holdout.iter().filter(|(_, label)| *label).take(120) {
            items.push(feedback(host, *label));
        }

        let delta = train_delta(&base, &items, AdaptConfig::default());
        let mut shifts: Vec<f32> = holdout
            .iter()
            .take(3_000)
            .map(|(host, _)| delta.logit_shift(&features::extract(host)))
            .collect();
        shifts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let median = shifts[shifts.len() / 2];
        assert!(
            median.abs() < 0.10,
            "the delta shifted the whole distribution by {median} logits instead of correcting specific domains"
        );
        assert!(
            delta.bias().abs() < 0.10,
            "the intercept absorbed the feedback's class balance: {}",
            delta.bias()
        );
        // A correction that moves nothing is not a correction either.
        assert!(
            shifts[shifts.len() - 1] - shifts[0] > 0.05,
            "the delta is flat; it corrected nothing"
        );
    }

    #[test]
    fn class_weights_cancel_the_opening_gradient() {
        let base = embedded_model().expect("parse");
        let holdout = embedded_holdout();
        let prepared: Vec<(Features, f32, f32)> = holdout
            .iter()
            .take(400)
            .map(|(host, label)| {
                let extracted = features::extract(host);
                let logit = base.logit_from_features(&extracted);
                (extracted, logit, if *label { 1.0 } else { 0.0 })
            })
            .collect();

        let (positive_weight, negative_weight) = base_rate_neutral_weights(&base, &prepared);
        let net: f32 = prepared
            .iter()
            .map(|(_, logit, target)| {
                let weight = if *target > 0.5 {
                    positive_weight
                } else {
                    negative_weight
                };
                weight * (base.calibrate(*logit) - target)
            })
            .sum();
        let magnitude: f32 = prepared
            .iter()
            .map(|(_, logit, target)| (base.calibrate(*logit) - target).abs())
            .sum();
        assert!(
            net.abs() < magnitude * 0.01,
            "the opening gradient did not cancel: net {net} against total pull {magnitude}"
        );
    }

    /// One class on its own has no balance to correct, so the weights must stay neutral rather than
    /// dividing by a pull of zero.
    #[test]
    fn class_weights_are_neutral_for_single_class_feedback() {
        let base = embedded_model().expect("parse");
        let prepared: Vec<(Features, f32, f32)> = ["ads.example.com", "track.example.net"]
            .iter()
            .map(|host| {
                let extracted = features::extract(host);
                let logit = base.logit_from_features(&extracted);
                (extracted, logit, 1.0)
            })
            .collect();
        assert_eq!(base_rate_neutral_weights(&base, &prepared), (1.0, 1.0));
        assert_eq!(base_rate_neutral_weights(&base, &[]), (1.0, 1.0));
    }

    #[test]
    fn certified_shift_bounds_the_real_shift() {
        let delta = trained(&agreeable_feedback());
        let certified = delta.certified_max_logit_shift();
        for host in [
            "ads1.tracker-net1.example",
            "wiki2.library2.example",
            "doubleclick.net",
            "a.io",
            "x1y2z3-4a5b6c-7d8e9f.analytics.tracking.example.co.uk",
        ] {
            let shift = delta.logit_shift(&features::extract(host)).abs();
            assert!(
                shift <= certified + 1e-4,
                "{host} shifted {shift}, past the certified {certified}"
            );
        }
    }
}
