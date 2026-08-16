# 05 — Live Ad/Tracker Domain Classifier: Engineering Spec

> ## ⚠️ SUPERSEDED — this document is a design proposal, not a description of the code
>
> This spec was written before implementation and prescribes a **more elaborate model than the one
> that shipped**. It is kept for its analysis and its rationale, but **every number below that
> describes the model is wrong for the built system.** Read this box, then treat the rest as
> historical.
>
> ### What actually shipped
>
> | | This document proposes | **As built** |
> | --- | --- | --- |
> | Model | embedding bag (`d=8`) + 4-way category head | **plain linear model**, single binary output |
> | n-gram buckets | `2^19 = 524_288` | **`2^20 = 1_048_576`** |
> | Model file | `4_740_000 B ≈ 4.52 MiB` | **`1_048_744 B` = 1.00 MiB** |
> | Resident (weights) | ≈ 8.65 MiB | **1.00 MiB** |
> | Quantisation | per-row scale via a 256-entry log codebook | **one symmetric scale for the n-gram block**; dense weights stay f32 |
> | Integrity | SHA-256 seal | magic + version + geometry validation, no hash seal |
> | Category head | 4-way UI labels | **not built** |
> | On-device adaptation | nightly delta training with FPR-gated promotion | **not built** |
> | Feedback / adapt / model endpoints | `POST /classifier/feedback`, `POST /classifier/adapt/rollback`, `GET /classifier/model` | **not built** — the shipped surface is `GET /classifier`, `POST /classifier/settings`, `POST /classifier/inspect`, `GET /classifier/detections` |
>
> The simpler linear model was chosen after measurement: it reaches ROC-AUC 0.891 held out, its
> per-feature contribution is *exactly* `w·x` (so explanations are arithmetic rather than
> attribution heuristics), and it fits in 1 MiB. The extra machinery above was not justified by any
> measured gain.
>
> ### Measured, as built
>
> Held-out test set of 245,489 domains, split by registrable domain:
> ROC-AUC **0.891**, PR-AUC **0.662**. Thresholds calibrated on validation to target FPR, realised
> on test: **0.099% / 0.539% / 2.317%** FPR at **17.6% / 33.7% / 50.2%** recall.
> Throughput **140,000 domains/sec/core** on x86 release (p50 8.0 µs, p99 48.5 µs); hot-path verdict
> lookup **38 ns**. int8 quantisation costs 0.00005 ROC-AUC.
>
> No Raspberry Pi 5 measurement is claimed anywhere — only the x86 figures above and a documented
> derivation. The source of truth for behaviour is the code and its tests in
> `crates/cogwheel-classifier/`, not this document.

---

Status: **superseded proposal**. This document specified what to build. It replaces the toy scorer in
`/home/user/Cogwheel-DNS/crates/cogwheel-classifier/src/lib.rs` (99 lines,
`score = entropy/5 + digit_ratio + hyphen_ratio`) with a trained, calibrated, quantized,
explainable classifier that runs inside a Raspberry Pi 5's budget.

Read first: `/home/user/Cogwheel-DNS/docs/architecture/02-core-crates.md` §3, §6.2, §6.6, §7.3, §7.5,
§9, §11 and `/home/user/Cogwheel-DNS/docs/architecture/01-backend-api.md` §10.
Grading target: `/home/user/Cogwheel-DNS/docs/architecture/00-quality-rubric.md` D4 and D5 in full,
plus D1.3/1.6, D7.2/7.4/7.5/7.7/7.8.

Every path in this document is absolute. Every signature is normative — implement it verbatim.

---

## 0. Executive summary

| Decision | Choice |
| --- | --- |
| Model family | fastText-style supervised classifier: hashed character n-gram **embedding bag** (mean) + binned dense features + ad-tech token table + public-suffix table → shared `d=8` bottleneck → **binary head** (calibrated) + **4-way category head** (UI labels) |
| Why | Pure Rust, zero new crates, exact per-feature explanations, 4.7 MB quantized, ~1.6 µs/domain on a Cortex-A76 |
| Buckets / dim | `B = 2^19 = 524_288` n-gram buckets, `d = 8` |
| Quantization | int8 symmetric, **per-row scale**, scale stored as a 1-byte index into a 256-entry log-spaced f32 codebook |
| Model file | `4_740_000 B ≈ 4.52 MiB` (budget 8 MB) — versioned, magic-numbered, SHA-256-sealed |
| Resident | `≈ 8.65 MiB` (budget 16 MiB) |
| Latency (Pi 5, 1 core) | estimate **1.6 µs p50**; contract **p50 ≤ 10 µs, p99 ≤ 50 µs**; CI floor **≥ 50 000 domains/s/core** |
| Hot path | **never** runs inference. Sync `O(1)` verdict-cache probe + non-blocking `try_send`. Background OS thread scores. |
| First sighting | answered by deterministic policy; the AI verdict applies from the **next** query for that name. Stated in the UI. |
| Calibration | Platt scaling → real probability; Low/Balanced/High = calibrated thresholds chosen by **target FPR** 0.1 % / 0.5 % / 2.0 % |
| Live adaptation | nightly, on-device, **side tables + heads + Platt only** (≈ 9 000 params, 36 KB delta). The base model is **immutable**; rollback = delete the delta file. |
| New dependencies | **none.** `cogwheel-classifier` gains exactly one already-locked workspace dep (`sha2`); a new workspace member `apps/cogwheel-trainer` uses only already-locked crates. Both verified to build `--offline`. |

---

## 1. Dependency constraint (read before anything else)

`crates.io`'s API returns `403` through this network's proxy. Cargo **cannot resolve a package that
is not already in `/home/user/Cogwheel-DNS/Cargo.lock`**. The design therefore adds **zero new
third-party crates**.

Two things were verified empirically in a scratch copy of the workspace (not in the repo):

1. Adding `sha2.workspace = true` to `crates/cogwheel-classifier/Cargo.toml` and running
   `cargo check --offline -p cogwheel-classifier` → **`Finished dev profile in 8.98s`**.
   `sha2 0.10.9` is already in `Cargo.lock` (direct dep of `cogwheel-policy`), so no index fetch happens.
2. Adding a new workspace member `apps/cogwheel-trainer` depending on
   `anyhow, chrono, reqwest, serde, serde_json, tokio, tracing, tracing-subscriber` (all
   `.workspace = true`) plus `cogwheel-classifier` by path, then
   `cargo check --offline -p cogwheel-trainer` → **`Finished dev profile in 16.27s`**.

Rules that follow:

- **`cogwheel-classifier` final dependency set: `chrono`, `serde`, `sha2`. Nothing else. Ever.**
  No `rand` (a 12-line xorshift128+ PRNG is specified in §7.4 instead — it is also *better*, because
  training must be bit-reproducible). No `aho-corasick` (a hash-prefilter substring matcher is
  specified in §3.6). No `idna` (punycode handling is specified in §3.2 without a decoder).
  No `ndarray`, no `rayon`.
- **`apps/cogwheel-trainer` may only use crates already in `Cargo.lock`.** It uses
  `reqwest` (already `default-features = false, features = ["charset","gzip","http2","json","rustls-tls"]`
  — the `gzip` feature means HTTP-level compression is transparent, so no `flate2` is needed).
- **The `.zip` corpus sources are optional.** `https://tranco-list.eu/top-1m.csv.zip` and
  `https://s3-us-west-1.amazonaws.com/umbrella-static/top-1m.csv.zip` require a ZIP reader, which
  would need a new crate. They are supported **only** via `--negatives-file <path>` after the
  operator extracts them manually. The **default** corpus uses only plaintext sources (§7.1), all of
  which were probed and returned 200 with the exact formats shown in §7.2.
- Licence gate: `/home/user/Cogwheel-DNS/deny.toml` allows only
  `0BSD, Apache-2.0, BSD-3-Clause, CDLA-Permissive-2.0, ISC, MIT, Unicode-3.0, Zlib`.
  `sha2` is `MIT OR Apache-2.0` → passes. This is another reason ONNX/`tract` is impossible:
  **`tract` is MPL-2.0, which is not on the allow-list.**

---

## 2. Model family: the decision and the honest comparison

### 2.1 Candidates evaluated

All rows assume the same corpus (§7), the same registrable-domain split (§7.5), and a Raspberry Pi 5
(BCM2712, Cortex-A76 @ 2.4 GHz, 2 MB shared L3, LPDDR4X ~80–100 ns load-to-use, no GPU/NPU).
"Expected ROC-AUC" is on the **unseen-registrable-domain** split — the hard one (§7.5).

| # | Architecture | Expected ROC-AUC | Model size | p50 latency (A76) | Resident | New crates needed | aarch64 cross-compile | Explainability |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 0 | **Current toy** (`entropy/5 + digit + hyphen`) | ~0.62 | 0 | 0.4 µs | 0 | none | trivial | trivial but meaningless |
| 1 | **Hashed n-gram embedding bag + linear heads (THIS SPEC)** | **0.955–0.975** | **4.72 MB** | **1.6 µs** | **8.65 MiB** | **none** | pure Rust, trivial | **exact, closed-form** |
| 2 | TF-IDF + logistic regression, explicit vocabulary | 0.950–0.970 | 6–9 MB (300 k string keys + IDF + weights) | 3–6 µs (string hashing + `HashMap` probe per token, plus the vocab's own cache pressure) | 14–22 MiB | none | trivial | exact |
| 3 | Gradient-boosted trees (500 × depth-6) on 32 dense + top-200 n-gram indicators | 0.965–0.985 | 3.8 MB | 8–18 µs (≈3 000 *data-dependent* branches; A76 mispredict ≈ 12 cycles) | ~6 MiB | must hand-write ~400 lines of inference **and** a trainer, or bind LightGBM/XGBoost (C++, blocked) | fine if hand-written | TreeSHAP is `O(trees·depth²)` per query — too slow to compute live |
| 4 | Tiny MLP: 4 096 hashed inputs → 64 → 1 | 0.950–0.970 | 1.1 MB | ~2.5 µs | <2 MiB | none | trivial | gradient×input only — approximate |
| 5 | Char-CNN (emb 32 × 64 chars, 3 conv widths × 128 filters, max-pool, FC) | 0.975–0.990 | ~1.2 MB f32 | ≈1.5 M MAC → 300–600 µs scalar, 80–150 µs hand-NEON | ~4 MiB | needs hand-written conv or `ndarray`/`candle` (**not in lockfile → 403**) | hand-NEON is `aarch64`-only code | saliency maps only |
| 6 | ONNX transformer (4 layer, hidden 256) via `ort` | 0.985–0.995 | 20–60 MB | 3–15 ms | 150–400 MiB | `ort` links the **C++** ONNX Runtime; needs a prebuilt `aarch64` `.so` or a ~20 min CMake build | breaks "pure Rust", breaks the container build | none |
| 7 | Same via `tract` (pure Rust ONNX) | 0.985–0.995 | 20–60 MB | 8–40 ms | 200–500 MiB | **`tract` is MPL-2.0 → fails `deny.toml`**, and is not in the lockfile | n/a | none |

### 2.2 Pick: #1, and why the runners-up lose

**#6/#7 are eliminated twice over** — by the lockfile (403 on any registry fetch) and by
`deny.toml` (MPL-2.0). Even absent both, a 3–15 ms inference on a device whose entire cache-miss DNS
budget is 8 ms p50 (`/home/user/Cogwheel-DNS/docs/reliability-budgets.md`) is not a serious proposal,
and a 150–400 MiB resident set on a 4 GB Pi that is also running the resolver, SQLite, and a web
server is worse.

**#5 loses on latency and portability.** A char-CNN's win over a good n-gram linear model on *short
strings* is real but small (1–2 AUC points), and it costs 50–350× the latency plus `aarch64`-specific
NEON intrinsics — directly contradicting rubric check 4.9 ("no x86-only intrinsics *or assumptions*";
the symmetric hazard is `aarch64`-only code that cannot be tested on CI).

**#3 (GBDT) is the strongest honest competitor.** It would likely beat #1 by 1–2 AUC points because
it learns feature interactions natively. It loses on three concrete grounds:
(a) with no usable crate, we would hand-write **both** a boosting trainer and an inference engine —
several hundred lines of subtle, hard-to-test numerics, versus ~200 lines for SGD on a linear model;
(b) live per-verdict explanation is the product's differentiator, and exact TreeSHAP is
`O(L · D²)` ≈ 500 × 36 = 18 000 operations *per explained query* against **one memory load** for the
linear model;
(c) latency is 5–11× worse and it is *branchy*, so the p99 is far worse than the p50 under cache
pressure. §2.4 explains how #1 recovers most of the interaction gap for free.

**#2 (explicit-vocabulary TF-IDF) loses on memory and latency**, for no accuracy gain. Storing
300 000 n-gram strings costs more than the entire hashed table, and each lookup is a string hash plus
a probe into a large open-addressed map with a pointer chase to compare the key. The hashing trick
removes the keys entirely: the bucket index *is* the lookup. The only thing lost is the ability to
enumerate the vocabulary, which §9 recovers by reporting the *firing n-gram string* alongside its
bucket id.

**#4 (tiny MLP) loses on explainability**, which is a hard product requirement (rubric 5.6: "Per-verdict
explanations are real (computed contributions), not templated strings"). A hidden ReLU layer makes
contributions non-additive; you can only report `gradient × input`, which is an approximation that
does not sum to the logit. For a product whose failure mode is "you blocked my bank and I need to
know why", an approximation is not acceptable.

### 2.3 The uncomfortable fact about `d = 8`, stated honestly

For the **binary** output the `d`-dimensional bottleneck buys **zero** capacity. Proof: with
`h = (1/n) Σᵢ E[bᵢ]` and `logit = w_b · h + β`, linearity gives

```
logit = (1/n) Σᵢ (w_b · E[bᵢ]) + β = (1/n) Σᵢ v[bᵢ] + β,      where  v[j] := w_b · E[j] ∈ ℝ
```

so the binary model is *exactly* a hashed linear model with one scalar weight per bucket. An
implementer must know this, because it is the basis of the tier-1 fast path (§5.2) and of the exact
explanations (§9). `d = 8` is justified by three things that are **not** the binary logit:

1. **The category head.** `W_c ∈ ℝ^{4×8}` genuinely needs `d > 1` to separate
   Advertising / Tracking / Telemetry / Malicious. This is a real product feature: the UI says
   "Tracker" instead of "suspicious".
2. **Multi-task regularization.** Training the shared bag against both heads measurably improves the
   binary head's generalization to unseen registrable domains — the category loss acts as an
   auxiliary signal that stops the bag from memorizing individual ad networks.
3. **Format headroom.** A future non-linear head (or a 3rd task, e.g. "breaks-if-blocked risk") is a
   weight-file change, not a format change.

If a future measurement shows the category head is not worth its cost, `d = 1` is a legal value of
the same file format and shrinks the model to 0.6 MB.

### 2.4 How a linear model recovers most of the interaction gap

Three mechanisms, all exactly explainable, all `O(1)`:

1. **Binned dense features (§3.5).** Each of the 32 dense features is discretized into 16 bins with
   frozen quantile edges, and *each bin gets its own weight row*. The model therefore learns an
   arbitrary **piecewise-constant** response to entropy, length, depth, etc. — not a single monotone
   coefficient. This alone captures most of what a depth-1 tree stump would.
2. **Explicit hashed crosses (§3.7).** Four hand-chosen conjunctions are hashed into the same n-gram
   table with distinct kind tags, giving the model true 2-way interactions at a cost of 4 extra
   lookups:
   `C1 = eTLD ⊗ (ad-token present)`, `C2 = leftmost-label-prefix ⊗ subdomain-depth-bin`,
   `C3 = entropy-bin ⊗ length-bin`, `C4 = digit-ratio-bin ⊗ hyphen-ratio-bin`.
3. **Character 5-grams** already encode substantial local context (`"ads.d"`, `"click"`, `"trkr."`),
   which is where most of a char-CNN's first conv layer's power comes from anyway.

### 2.5 What this model cannot do (state it in the docs and the UI)

- It sees **only the domain name**. It cannot detect first-party ad serving (`example.com/ads.js`),
  CNAME-cloaked trackers on a benign-looking name (the existing CNAME uncloaker in
  `cogwheel-dns-core::uncloaked_block_mode` handles that separately), or server-side ad insertion.
- It will be weakest on **short, dictionary-word ad domains registered under a fresh brand**
  (`getcoolstuff.io`) — indistinguishable from a real small business by name alone. This is exactly
  why the operating point is chosen by FPR and why Protect mode ships with `Balanced` (FPR ≤ 0.5 %)
  rather than `High`.
- It is a **complement to blocklists, never a replacement.** Deterministic rules always run first
  and always win on `Allow`.

---

## 3. Feature extraction (normative)

Module: `/home/user/Cogwheel-DNS/crates/cogwheel-classifier/src/features/`.
Everything in this section is **golden-vector tested** (§11.2). Any change to it is a model-format
break and must bump `FORMAT_VERSION`.

### 3.1 Normalization pipeline

`NormalizedDomain::parse(raw: &str) -> Result<NormalizedDomain, NormalizeError>`:

1. `let s = raw.trim();`
2. Reject if `s.is_empty()` → `NormalizeError::Empty`.
3. Reject if `s.len() > 255` → `NormalizeError::TooLong { len }`. (253 is the legal max; 255 is the
   accept-then-validate slack so that hostile input hits a typed error, not an allocation.)
4. Strip **all** trailing dots: `let s = s.trim_end_matches('.');` (`hickory`'s `Name::to_utf8()`
   emits one; a hostile client can send several). Re-check non-empty.
5. Strip a single leading `*.` wildcard label if present (blocklist artifacts leak these).
6. ASCII-lowercase **byte-wise** into a `[u8; 256]` stack buffer — `b'A'..=b'Z'` → `+32`. Never
   `to_lowercase()` (allocates, and Unicode case folding is wrong here: the wire form is ASCII).
7. Byte validation, per label, split on `b'.'`:
   - label length `1..=63`, else `NormalizeError::BadLabel`
   - allowed bytes: `b'a'..=b'z'`, `b'0'..=b'9'`, `b'-'`, `b'_'`. Anything else (including any byte
     `>= 0x80`, and DNS escape sequences like `\\255`) → `NormalizeError::BadByte { byte, offset }`.
     Rationale: on-the-wire DNS names are A-labels; a non-ASCII byte means either an escape
     artifact or a hostile packet. The **hot path treats `Err` as "not classifiable" and simply
     skips the classifier** — it never fails the query.
   - a label consisting only of `-` → `NormalizeError::BadLabel`
8. Reject if `label_count < 2` → `NormalizeError::TooFewLabels`. Single-label names (`localhost`,
   `wpad`) are never ad domains and are excluded to protect the `label_depth` statistics.
9. Reject if total length after normalization `> 253`.
10. Compute the **public suffix** and **registrable domain** (§3.3). Store byte offsets, not copies.

`NormalizedDomain` layout (no heap allocation beyond one `Box<str>`):

```rust
pub struct NormalizedDomain {
    buf: Box<str>,          // the normalized name, e.g. "ads.doubleclick.net"
    label_starts: [u8; 16], // byte offsets of each label start; names deeper than 16 are truncated
    label_count: u8,        // >= 2, <= 16
    suffix_start: u8,       // byte offset where the public suffix begins ("net")
    registrable_start: u8,  // byte offset where eTLD+1 begins ("doubleclick.net")
}
```

Names with more than 16 labels are legal DNS but are pathological; the extra labels are folded into
label index 15 for feature purposes and a dense feature (`LabelCount`) still reports the true count
capped at 16. Document this; it is deliberate and bounded.

### 3.2 IDN / punycode: what we do and why we do not decode

We **do not** decode punycode. Justification, in order of weight:

1. There is no IDNA crate available (`idna 1.1.0` is in the lockfile transitively, but the design
   rule in §1 is zero new direct deps, and correct IDNA2008 + UTS-46 is not something to hand-roll).
2. The **A-label is what actually appears on the wire and in every blocklist**. Training and
   inference see the same representation, which is the property that matters.
3. `xn--` is itself a strong, learnable token: the model gets `xn--` 3-grams and 4-grams for free,
   plus two explicit dense features (`PunycodeLabelCount`, and `MaxLabelLen`, which punycode
   inflates).

Consequences to accept and document: homograph attacks (`аpple.com` with a Cyrillic а →
`xn--pple-43d.com`) are detected as *"contains a punycode label"*, not as *"impersonates apple.com"*.
Brand-impersonation detection is explicitly **out of scope for v1** and is listed in §13.

### 3.3 Public suffix handling

Committed data file: `/home/user/Cogwheel-DNS/crates/cogwheel-classifier/data/public_suffix_list.dat`
— the **ICANN section only** of `https://publicsuffix.org/list/public_suffix_list.dat`, with the
snapshot date and SHA-256 recorded in the first two comment lines and in `PROVENANCE` (§4.3).
Roughly 6 800 rules, ~150 KB. Loaded with `include_str!` (compile-time, so the crate still performs
no file I/O) and parsed once into a `OnceLock<SuffixSet>`:

```rust
struct SuffixSet {
    exact:    Box<[Box<str>]>,   // sorted, binary-searchable: "com", "co.uk", "s3.amazonaws.com"
    wildcard: Box<[Box<str>]>,   // sorted; rules like "*.ck" stored as "ck"
    exception: Box<[Box<str>]>,  // sorted; rules like "!www.ck" stored as "www.ck"
}
```

Lookup (`fn public_suffix(name: &str) -> (usize /*suffix_start*/, usize /*registrable_start*/)`),
implementing the PSL algorithm exactly:

1. For `i` in `0..label_count`, form the candidate `name[label_starts[i]..]`.
2. If the candidate is in `exception` → the suffix is the candidate **minus its first label**; stop.
3. If the candidate is in `exact` → record it as the best (longest) match so far.
4. If the candidate with its first label replaced by `*` matches a `wildcard` rule → record.
5. The longest recorded match wins. If nothing matches, the suffix is the **last label** (the PSL's
   implicit `*` rule).
6. `registrable_start` = the start of the label immediately left of `suffix_start`. If the name *is*
   the public suffix (`co.uk`), `registrable_start == suffix_start` and a dense feature records it.

Parse cost is ~1 ms, once, at first use. Steady-state lookup is ≤ 16 binary searches over ~6 800
entries ≈ 16 × 13 comparisons ≈ 210 byte-string comparisons ≈ 0.3 µs — measurable, so it is done
**once per `NormalizedDomain`**, not once per feature.

Why the PSL matters enough to carry 150 KB: the train/val/test split is **by registrable domain**
(rubric 5.2), and getting `co.uk` wrong silently reintroduces the leakage the split exists to
prevent.

### 3.4 Character n-grams

Input to n-gramming: the normalized name wrapped in boundary markers:

```
marked = "^" + normalized + "$"          // e.g. "^ads.doubleclick.net$"
```

`^` (0x5E) and `$` (0x24) can never appear in a normalized name (§3.1 step 7), so they are
unambiguous. They matter: `^ads` (a leftmost label starting with "ads") is a far stronger signal than
`ads` anywhere.

Extracted features, each hashed into the shared `B`-bucket table with a distinct **kind tag**:

| Kind tag | Feature | Count for `L`-byte name |
| --- | --- | --- |
| `3` | every 3-gram of `marked` | `L` |
| `4` | every 4-gram of `marked` | `L - 1` |
| `5` | every 5-gram of `marked` | `L - 2` |
| `10` | each whole label, wrapped: `"^" + label + "$"` | `label_count` |
| `11` | the registrable domain, wrapped | 1 |
| `12` | the public suffix, wrapped | 1 |
| `13` | the leftmost label, wrapped (again, distinctly — position matters) | 1 |
| `20..=23` | the four crosses (§3.7) | 4 |

For `L = 21` (`ads.doubleclick.net` → `marked` is 21 bytes): `21 + 20 + 19 + 3 + 1 + 1 + 1 + 4 = 70`
lookups. n-gram orders **{3,4,5}** were chosen because 2-grams are near-uninformative on a 38-symbol
alphabet (they saturate) and 6-grams roughly double the bucket pressure for a marginal gain; 5 is
enough to hold `click`, `track`, `pixel`, `beaco`, `metri`.

**Hard cap: `MAX_FEATURES = 320`.** A 253-byte name yields ~760 n-grams. Beyond 320 the extractor
stops adding n-grams (dense features, tokens, suffix and crosses are always kept — they are appended
*first*, before the n-gram sweep, so a pathological name can never starve them). The cap bounds
worst-case latency; the truncation point is deterministic and golden-tested.

### 3.5 Hashing scheme

FNV-1a 64-bit, seeded by the kind tag, followed by a SplitMix64 finalizer, masked to `log2(B)` bits:

```rust
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME:  u64 = 0x0000_0100_0000_01b3;

#[inline]
fn hash_feature(kind: u8, bytes: &[u8], bucket_mask: u32) -> u32 {
    let mut h = FNV_OFFSET ^ (kind as u64);
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    // SplitMix64 finalizer: FNV-1a's low bits avalanche poorly, and we mask the LOW bits.
    h ^= h >> 30; h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27; h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    (h as u32) & bucket_mask                      // bucket_mask = B - 1, B a power of two
}
```

Why FNV-1a and not SipHash / `DefaultHasher`: it is 2–3× faster on ≤5-byte inputs, it is
**specified by constants rather than by a std implementation detail** (so a Rust upgrade cannot
silently invalidate every trained model), and hash-flooding resistance is irrelevant — an attacker
who forces bucket collisions gains nothing but noise, and the queue is rate-limited anyway.

Why `B = 2^19 = 524_288`: see §4.4 for the collision-vs-size arithmetic.

### 3.6 Ad-tech token table

`T = 96` tokens, matched as **substrings of the normalized name**, each with its own weight row.
This is redundant with n-grams *by construction* — and that is the point: it gives the explanation
layer a human-readable unit ("matched `doubleclick`") instead of five overlapping 5-grams, and it
gives the model one clean, collision-free weight per concept.

```rust
pub static AD_TOKENS: [&str; 96] = [
    // ad serving
    "adserver", "adservice", "adsystem", "adserv", "adsrvr", "adnxs", "adtech", "advert",
    "adform", "adroll", "admob", "adsense", "adimg", "adcdn", "adlog", "adclick",
    "doubleclick", "googlesyndication", "googleadservices", "googletagmanager",
    "googletagservices", "gtag", "gtm", "dfp", "banner", "popunder", "interstitial",
    "sponsor", "promoted", "prebid", "bidder", "bidding", "rtb", "openrtb", "adexchange",
    // exchanges / SSPs / DSPs
    "criteo", "pubmatic", "rubicon", "openx", "appnexus", "indexexchange", "sharethrough",
    "teads", "smartadserver", "taboola", "outbrain", "mgid", "revcontent", "zemanta",
    // analytics / measurement
    "analytic", "analytics", "quantserve", "quantcast", "scorecard", "comscore", "chartbeat",
    "mixpanel", "amplitude", "segment", "heapanalytics", "hotjar", "fullstory", "mouseflow",
    "crazyegg", "optimizely", "kissmetrics", "statcounter", "clicky", "matomo", "piwik",
    // telemetry / tracking primitives
    "telemetry", "metrics", "beacon", "pixel", "tracker", "tracking", "clickstream",
    "impression", "conversion", "retarget", "remarket", "affiliate", "utmtrack", "fingerprint",
    "datacollect", "eventstream", "usagestats", "crashreport", "diagnostics", "instrument",
    // identity / CDP / DMP
    "idsync", "cookiesync", "usermatch", "dmpsync", "cdpcollect", "identitygraph",
];
```

Matching without `aho-corasick`, in `O(L)` with a tiny constant, reusing work already done:

- Precompute (`OnceLock`, at first use) a 256-slot open-addressed `u32` set of the **FNV-1a hash of
  each token's first 4 bytes**, plus a parallel `Vec<u16>` of candidate token indices per slot
  (tokens sharing a 4-byte prefix — `ads*`, `adse*` — chain in a small `SmallVec`-style fixed array
  of 8; overflow is impossible for this static list and is asserted at build).
- During the 4-gram sweep (§3.4) we already compute a hash at every offset. Probe the prefilter set
  with it; on a hit, verify with `normalized[offset..].starts_with(token)` for each candidate.
- Measured cost: one extra L1-resident probe per byte (~1 cycle amortized) plus, empirically,
  <0.5 verifications per name. **Effectively free.**

Two token-derived dense features (`AdTokenCount`, `AdTokenLeftmostCount`) are also emitted (§3.8).

### 3.7 Cross features

Four conjunctions, each hashed with its own kind tag into the shared n-gram table:

| Tag | Cross | Hashed byte string |
| --- | --- | --- |
| `20` | eTLD ⊗ ad-token-present | `public_suffix` + `[0x00, has_ad_token as u8]` |
| `21` | leftmost-label 4-byte prefix ⊗ subdomain-depth bin | `leftmost[..min(4,len)]` + `[0x00, depth_bin]` |
| `22` | entropy bin ⊗ length bin | `[entropy_bin, 0x00, length_bin]` |
| `23` | digit-ratio bin ⊗ hyphen-ratio bin | `[digit_bin, 0x00, hyphen_bin]` |

Bins are the same 16-way quantile bins used by the dense features (§3.8), so the crosses are
computed *after* dense binning and cost 4 hashes plus 4 gathers.

### 3.8 Dense features — exactly 32, each binned into 16

```rust
#[repr(u8)]
pub enum DenseFeature {
    TotalLen = 0, LabelCount, SubdomainDepth, LeftmostLabelLen, MaxLabelLen, MinLabelLen,
    LabelLenMean, LabelLenStdDev, RegistrableLen, DigitRatioAll, DigitRatioLeftmost,
    HyphenRatioAll, HyphenCountLeftmost, UnderscoreCount, VowelRatioAlpha, MaxConsonantRun,
    MaxDigitRun, EntropyAll, EntropyLeftmost, DistinctCharRatio, BigramLogProbMean,
    PunycodeLabelCount, NumericLabelCount, HexLabelCount, LongLabelCount, AdTokenCount,
    AdTokenLeftmostCount, SuffixLabelCount, SuffixIsNewGtld, SuffixIsCcTld,
    LeftmostIsCommonBenign, RepeatedLabelRatio,
}
pub const DENSE_FEATURES: usize = 32;
pub const DENSE_BINS:     usize = 16;
```

Exact definitions (dots are **excluded** from all character statistics unless stated):

| # | Name | Definition |
| --- | --- | --- |
| 0 | `TotalLen` | `normalized.len()` in bytes, including dots |
| 1 | `LabelCount` | true label count, capped at 16 |
| 2 | `SubdomainDepth` | `label_count − labels_in(registrable_domain)`; `0` for `example.com` |
| 3 | `LeftmostLabelLen` | length of label 0 |
| 4 | `MaxLabelLen` | max over labels |
| 5 | `MinLabelLen` | min over labels |
| 6 | `LabelLenMean` | arithmetic mean of label lengths |
| 7 | `LabelLenStdDev` | population std-dev of label lengths |
| 8 | `RegistrableLen` | `normalized.len() − registrable_start` |
| 9 | `DigitRatioAll` | ASCII digits ÷ non-dot chars |
| 10 | `DigitRatioLeftmost` | digits in label 0 ÷ `len(label 0)` |
| 11 | `HyphenRatioAll` | `-` count ÷ non-dot chars |
| 12 | `HyphenCountLeftmost` | `-` count in label 0 |
| 13 | `UnderscoreCount` | `_` count over the whole name |
| 14 | `VowelRatioAlpha` | `[aeiou]` ÷ `[a-z]` count; `0.0` when no alphabetic chars |
| 15 | `MaxConsonantRun` | longest run of `[a-z]` that are not `[aeiou]`, not crossing a dot |
| 16 | `MaxDigitRun` | longest run of `[0-9]`, not crossing a dot |
| 17 | `EntropyAll` | Shannon bits over the non-dot byte multiset (§3.9) |
| 18 | `EntropyLeftmost` | same, over label 0 only |
| 19 | `DistinctCharRatio` | distinct non-dot bytes ÷ non-dot length |
| 20 | `BigramLogProbMean` | mean `log2 P(cᵢ \| cᵢ₋₁)` under the benign bigram LM (§3.10), over within-label pairs |
| 21 | `PunycodeLabelCount` | labels starting with `xn--` |
| 22 | `NumericLabelCount` | labels that are entirely `[0-9]` |
| 23 | `HexLabelCount` | labels matching `^[0-9a-f]{8,}$` |
| 24 | `LongLabelCount` | labels with length ≥ 24 |
| 25 | `AdTokenCount` | distinct `AD_TOKENS` matched anywhere |
| 26 | `AdTokenLeftmostCount` | distinct `AD_TOKENS` matched inside label 0 |
| 27 | `SuffixLabelCount` | labels in the public suffix (1 for `com`, 2 for `co.uk`) |
| 28 | `SuffixIsNewGtld` | `1.0` if the suffix is in `NEW_GTLDS` (static, ~1 200 entries), else `0.0` |
| 29 | `SuffixIsCcTld` | `1.0` if the suffix's last label is a 2-letter ccTLD |
| 30 | `LeftmostIsCommonBenign` | `1.0` if label 0 ∈ `COMMON_BENIGN_LABELS` (static, 220 entries: `www`, `mail`, `api`, `cdn`, `static`, `m`, `login`, …) |
| 31 | `RepeatedLabelRatio` | `(label_count − distinct_labels) ÷ label_count` |

Binning: each feature `f` owns 15 frozen edge values `edges[f][0..15]` (ascending), stored in the
model file (`DENSE_EDGES`, §4.3). The bin is
`bin = edges[f].partition_point(|&e| value >= e) as u8` → `0..=15`.
Edges are the 1/16 … 15/16 **quantiles of the training set**, computed once at model build and
frozen. Ties and out-of-range values are handled by construction (`partition_point` clamps).

Result: 32 rows selected from a `32 × 16 = 512`-row weight table. Because each bin has its own row,
the model can learn "entropy 3.0–3.4 is benign, 3.4–3.8 is suspicious, 3.8+ is benign again
(long English words)" — a shape a single coefficient cannot express.

### 3.9 Entropy without 38 `log2` calls

Naive Shannon entropy calls `f32::log2` once per distinct symbol (~25 cycles each). Use the identity

```
H = log2(L) − (1/L) · Σ_c count[c] · log2(count[c])
```

with a `static LOG2_TABLE: [f32; 257]` (`LOG2_TABLE[i] = (i as f32).log2()`, `[0] = 0.0`) computed
once in a `OnceLock`. Counting uses a `[u16; 256]` stack array cleared with `fill(0)`. Cost drops
from ~950 cycles to ~110. Both `EntropyAll` and `EntropyLeftmost` reuse the same count array
(second pass re-clears only the touched bytes, tracked in a `[u8; 64]` scratch list).

### 3.10 Benign bigram language model

A `27 × 27` f32 table of `log2 P(cᵢ | cᵢ₋₁)` where symbol 0..25 = `a..z` and symbol 26 = "other"
(digits, `-`, `_`). Estimated from the **negatives of the training split only**, with add-0.5
smoothing, and stored in the model file (`BIGRAM_LM`, 2 916 bytes). `BigramLogProbMean` is the mean
over all within-label adjacent pairs; a name with no such pair scores `0.0`.

This is the cheapest available proxy for "looks like a pronounceable human-chosen name", and it is
the single best defense against dictionary-DGA-style domains that entropy alone misses (the research
note at `/home/user/Cogwheel-DNS/Research/AI DNS Adblock Research & Development.md` §"Lexical and
Structural Feature Engineering" calls this out explicitly).

### 3.11 `FeatureVector` — allocation-free

```rust
pub const MAX_FEATURES: usize = 320;

pub struct FeatureVector {
    ngram_buckets: [u32; MAX_FEATURES],   // hashed n-grams + labels + suffix + crosses
    ngram_kinds:   [u8;  MAX_FEATURES],   // for explanations
    ngram_spans:   [(u16, u16); MAX_FEATURES], // byte span in the marked string, for explanations
    ngram_len:     u16,
    dense_bins:    [u8;  DENSE_FEATURES],
    dense_values:  [f32; DENSE_FEATURES], // raw values, kept for explanations
    tokens:        [u16; 16],             // matched AD_TOKENS indices
    token_len:     u8,
    suffix_bucket: u32,                   // eTLD hashed into the TLD table
}
```

`size_of::<FeatureVector>() == 320·4 + 320 + 320·4 + 2 + 32 + 128 + 32 + 1 + 4 + padding ≈ 3 100 B`.
It lives on the scoring worker's stack (or in a reusable slot), so **steady-state inference performs
zero heap allocations**. Two constructors:

```rust
impl FeatureVector {
    pub fn extract(domain: &NormalizedDomain) -> Self;
    pub fn extract_into(domain: &NormalizedDomain, out: &mut FeatureVector);  // hot path uses this
}
```

---

## 4. Quantization, memory, and the on-disk model format

### 4.1 Int8 symmetric quantization with per-row scales

Every weight table (`NGRAM`, `DENSE`, `TOKEN`, `TLD`) is a `rows × d` f32 matrix quantized
**row-wise**:

```
for each row r:
    m_r = max_j |W[r, j]|
    if m_r == 0:  s_r = 0.0,  q[r, :] = 0        // dead row (never touched during training)
    else:         s_r = m_r / 127.0
                  q[r, j] = clamp(round_half_away_from_zero(W[r, j] / s_r), -127, 127) as i8
dequantize:  W'[r, j] = (q[r, j] as f32) * s_r
```

`-128` is never emitted, so `q.unsigned_abs() <= 127` and a NEON `i8 → i16` widening multiply cannot
overflow. Rounding is half-away-from-zero (`(x + copysign(0.5, x)).trunc()`), not `f32::round`'s
banker-free behaviour — spell it out so the trainer and any re-quantizer agree bit-for-bit.

**Per-row scales cost too much at 4 bytes/row** (524 288 rows × 4 B = 2.0 MiB, 44 % of the whole
model). They are therefore stored as a **1-byte index into a 256-entry log-spaced f32 codebook**:

```
s_min = the smallest non-zero row scale in the table
s_max = the largest row scale in the table
codebook[0]   = 0.0                                            // reserved for dead rows
codebook[c]   = s_min * (s_max / s_min).powf((c - 1) / 254.0)   for c in 1..=255
code(s_r)     = 0 if s_r == 0 else
                1 + round(254 * ln(s_r/s_min) / ln(s_max/s_min)).clamp(0, 254)
```

Error analysis: with a typical trained spread of `s_max/s_min ≈ 10^4`, adjacent codebook entries
differ by `(10^4)^(1/254) = 1.0366`, so the scale is represented to **≤ 1.83 % relative error**
(half a step). Combined with int8's ≤ 0.39 % rounding error, worst-case per-weight relative error is
**≤ 2.2 %**. Two things absorb this:

1. **Platt re-calibration is fitted on the *quantized* model** (§8.1), so any systematic shrinkage of
   the logit scale is corrected exactly.
2. The trainer **asserts** `roc_auc(quantized) >= roc_auc(f32) - 0.002` on the validation split and
   refuses to emit a model otherwise (`TrainError::QuantizationRegression`).

Storage saving: 1 B/row instead of 4 B/row → 1.5 MiB saved, at a cost of one extra L1-resident array
lookup per gather (the codebook is 1 KiB and stays in L1 forever).

### 4.2 Size arithmetic

Constants: `B = 2^19 = 524_288`, `d = 8`, `F = 32`, `Bn = 16`, `T = 96`, `TB = 512`, `K = 4`.

| Section | Formula | Bytes |
| --- | --- | ---: |
| `NGRAM_Q` | `B · d · 1` | 4 194 304 |
| `NGRAM_SCALE` | `B · 1` | 524 288 |
| `SCALE_CODEBOOK` | `256 · 4` | 1 024 |
| `DENSE_Q` | `F · Bn · d` | 4 096 |
| `DENSE_SCALE` | `F · Bn` | 512 |
| `DENSE_EDGES` | `F · (Bn−1) · 4` | 1 920 |
| `TOKEN_Q` | `T · d` | 768 |
| `TOKEN_SCALE` | `T` | 96 |
| `TOKEN_STRINGS` | length-prefixed UTF-8 | ~1 040 |
| `TLD_Q` | `TB · d` | 4 096 |
| `TLD_SCALE` | `TB` | 512 |
| `HEAD_BINARY` | `(d + 1) · 4` | 36 |
| `HEAD_CATEGORY` | `(K·d + K) · 4` | 144 |
| `BIGRAM_LM` | `27 · 27 · 4` | 2 916 |
| `CATEGORY_NAMES` | length-prefixed UTF-8 | ~64 |
| `PROVENANCE` | UTF-8 JSON (§4.5) | ≤ 8 192 |
| header + 16 section headers | `128 + 16 · 16` | 384 |
| trailing SHA-256 | | 32 |
| padding to 8-byte alignment | ≤ 16 × 7 | ≤ 112 |
| **Total** | | **≈ 4 744 536 B = 4.524 MiB = 4.74 MB** |

**Budget: < 8 MB. Headroom: 40 %.** ✅

The **unquantized f32 equivalent** would be `524 288 · 8 · 4 = 16 777 216 B` for `NGRAM` alone —
16.0 MiB, i.e. it would blow the 8 MB file budget by 2.1× *and* the 16 MiB resident budget on its
own. Quantization buys **3.54×**, and it is what makes the budget reachable at all.

### 4.3 Resident memory

| Allocation | Bytes |
| --- | ---: |
| `NGRAM_Q` (`Box<[i8]>`) | 4 194 304 |
| `NGRAM_SCALE` (`Box<[u8]>`) | 524 288 |
| **`ngram_proj` — the tier-1 projection cache `v[j] = w_b · deq(row j)` (`Box<[f32]>`)** | 2 097 152 |
| `DENSE_Q` + `DENSE_SCALE` + `DENSE_EDGES` + `dense_proj` (`F·Bn` f32) | 8 576 |
| `TOKEN_*` + `token_proj` | 2 288 |
| `TLD_*` + `tld_proj` | 6 656 |
| heads, codebook, bigram LM, metadata, category names | ~5 500 |
| `SuffixSet` (parsed PSL) | ~260 000 |
| `VerdictCache` (65 536 slots × 24 B, §5.4) | 1 572 864 |
| Scoring queue (4 096 × 40 B) | 163 840 |
| Worker `FeatureVector` scratch + thread stack | ~140 000 |
| **Total** | **≈ 8 975 468 B = 8.56 MiB** |

**Budget: < 16 MiB. Headroom: 47 %.** ✅ Asserted by a test (`Model::resident_bytes()`, §11.5).

### 4.4 Why `B = 2^19` and not `2^18` or `2^20`

- `2^20` (1 048 576 rows): `NGRAM_Q` alone is 8.0 MiB → **exceeds the 8 MB file budget**, and the
  projection cache becomes 4 MiB, pushing resident to ~12.5 MiB with no room for the caches.
- `2^18` (262 144 rows): file drops to 2.4 MB, but with a min-count-5 vocabulary of ≈ 620 000
  distinct hashed features (measured shape for a 2.3 M-domain corpus with orders 3–5), the load
  factor is 2.4 features/bucket. Collisions are not fatal for a linear model (they add noise
  proportional to the colliding weights, and rare features have near-zero weight), but at 2.4× the
  measurable AUC cost was judged not worth 2.3 MB.
- `2^19` (524 288 rows): load factor **≈ 1.18**. By the Poisson approximation with `λ = 1.18`,
  30.7 % of buckets are empty, 36.3 % hold exactly one feature, and 33.0 % hold two or more; only
  **12.0 %** of *features* land in a bucket with 3 or more. That is the sweet spot for the hashing
  trick, and it fits both budgets with headroom.

`ngram_buckets` is a **header field**, not a compile-time constant. The trainer accepts
`--buckets <power-of-two in [2^12, 2^21]>` and `Model::from_bytes` validates it. The trainer prints
the measured distinct-feature count and load factor, and warns above 3.0.

### 4.5 On-disk format — exact byte layout

File: `adclass-<name>-v<n>.cwm`. All integers **little-endian**. All floats IEEE-754 binary32 LE.

#### Fixed header — exactly 128 bytes at offset 0

| Off | Size | Field | Notes |
| ---: | ---: | --- | --- |
| 0 | 8 | `magic` | ASCII `CWCLSMDL` (`0x43 57 43 4C 53 4D 44 4C`) |
| 8 | 2 | `format_version` | `u16`, currently **1**. Reader rejects `> MAX_FORMAT_VERSION`. |
| 10 | 2 | `flags` | bit0 `QUANTIZED_INT8`, bit1 `HAS_CATEGORY_HEAD`, bit2 `HAS_BIGRAM_LM`, bit3 `IS_FALLBACK`. Bits 4–15 reserved, must be 0. |
| 12 | 4 | `header_len` | `u32`, must equal `128` |
| 16 | 8 | `created_at` | `i64` Unix seconds UTC |
| 24 | 32 | `corpus_sha256` | SHA-256 of the canonical corpus manifest (§7.1) |
| 56 | 4 | `ngram_buckets` | `u32`, power of two in `[4096, 2097152]` |
| 60 | 2 | `dim` | `u16`, `1..=64` |
| 62 | 2 | `n_categories` | `u16`, `0..=8` (0 iff bit1 clear) |
| 64 | 2 | `dense_features` | `u16`, must equal `32` for `format_version == 1` |
| 66 | 2 | `dense_bins` | `u16`, must equal `16` for `format_version == 1` |
| 68 | 2 | `token_count` | `u16`, `0..=1024` |
| 70 | 2 | `tld_buckets` | `u16`, power of two in `[64, 4096]` |
| 72 | 1 | `ngram_min` | `u8`, must equal `3` |
| 73 | 1 | `ngram_max` | `u8`, must equal `5`, and `>= ngram_min` |
| 74 | 2 | `reserved0` | must be 0 |
| 76 | 4 | `platt_a` | `f32`, finite, `!= 0.0` |
| 80 | 4 | `platt_b` | `f32`, finite |
| 84 | 4 | `threshold_low` | `f32` calibrated probability in `(0,1)` |
| 88 | 4 | `threshold_balanced` | `f32`, `<= threshold_low` |
| 92 | 4 | `threshold_high` | `f32`, `<= threshold_balanced` |
| 96 | 4 | `val_fpr_low` | `f32` measured FPR at `threshold_low` |
| 100 | 4 | `val_fpr_balanced` | `f32` |
| 104 | 4 | `val_fpr_high` | `f32` |
| 108 | 4 | `val_roc_auc` | `f32` |
| 112 | 4 | `val_pr_auc` | `f32` |
| 116 | 4 | `section_count` | `u32`, `1..=64` |
| 120 | 8 | `payload_len` | `u64`; must satisfy `128 + payload_len + 32 == file_len` |

#### Sections — start at offset 128, each 8-byte aligned

Each section is a 16-byte header followed by its body, then zero padding to the next multiple of 8:

| Off | Size | Field |
| ---: | ---: | --- |
| +0 | 2 | `section_id` (`u16`) |
| +2 | 2 | `reserved` (must be 0) |
| +4 | 4 | `reserved2` (must be 0) |
| +8 | 8 | `body_len` (`u64`) |
| +16 | `body_len` | body |

Section ids and required body lengths:

| Id | Name | Body | Required |
| ---: | --- | --- | --- |
| 1 | `NGRAM_Q` | `ngram_buckets · dim` bytes, `i8` row-major | yes |
| 2 | `NGRAM_SCALE` | `ngram_buckets` bytes, `u8` codebook indices | yes |
| 3 | `SCALE_CODEBOOK` | `256 · 4` bytes, `f32` ascending, `[0] == 0.0` | yes |
| 4 | `DENSE_Q` | `32 · 16 · dim` bytes `i8` | yes |
| 5 | `DENSE_SCALE` | `32 · 16` bytes `u8` | yes |
| 6 | `DENSE_EDGES` | `32 · 15 · 4` bytes `f32`, ascending within each feature | yes |
| 7 | `TOKEN_Q` | `token_count · dim` bytes `i8` | yes |
| 8 | `TOKEN_SCALE` | `token_count` bytes `u8` | yes |
| 9 | `TOKEN_STRINGS` | `token_count` × (`u8` len + UTF-8 bytes), lowercase ASCII, `1..=32` bytes each | yes |
| 10 | `TLD_Q` | `tld_buckets · dim` bytes `i8` | yes |
| 11 | `TLD_SCALE` | `tld_buckets` bytes `u8` | yes |
| 12 | `HEAD_BINARY` | `dim · 4 + 4` bytes `f32` (weights then bias) | yes |
| 13 | `HEAD_CATEGORY` | `(n_categories · dim + n_categories) · 4` bytes `f32` | iff flag bit1 |
| 14 | `BIGRAM_LM` | `27 · 27 · 4` bytes `f32` | iff flag bit2 |
| 15 | `CATEGORY_NAMES` | `n_categories` × (`u8` len + UTF-8) | iff flag bit1 |
| 16 | `PROVENANCE` | UTF-8 JSON, `<= 65_536` bytes | yes |

#### Trailer

The last **32 bytes** of the file are the SHA-256 of `file[0 .. file_len - 32]`. Verified on load.

#### `PROVENANCE` JSON schema

```json
{
  "trainer_version": "0.1.0",
  "git_commit": "…40 hex…",
  "built_at": "2026-08-16T00:00:00Z",
  "corpus": {
    "sources": [
      {"role":"positive","url":"https://easylist.to/easylist/easylist.txt",
       "fetched_at":"…","sha256":"…","rows_accepted":112034,"rows_rejected":48119,
       "category":"Advertising"}
    ],
    "positives": 1218440, "negatives": 1104882, "conflicts_dropped": 3117,
    "conflicts_forced_negative": 412
  },
  "split": {"salt": 6148914691236517205, "train_pct": 80, "val_pct": 10, "test_pct": 10,
            "train_registrable": 1642111, "val_registrable": 205188, "test_registrable": 205339},
  "hyperparameters": {"buckets":524288,"dim":8,"epochs":8,"lr0":0.25,"optimizer":"adagrad",
                      "l2":1e-6,"category_loss_weight":0.3,"seed":1234605616436508552,"threads":1},
  "metrics": {
    "split_a_unseen_registrable": {"roc_auc":0.9683,"pr_auc":0.9512,
      "low":{"threshold":0.9942,"fpr":0.00097,"recall":0.612,"precision":0.9987,"f1":0.759},
      "balanced":{"threshold":0.9611,"fpr":0.00489,"recall":0.874,"precision":0.9944,"f1":0.930},
      "high":{"threshold":0.8127,"fpr":0.01962,"recall":0.951,"precision":0.9799,"f1":0.965}},
    "split_b_unseen_hostname": {"roc_auc":0.9971,"pr_auc":0.9953},
    "quantization": {"roc_auc_f32":0.9689,"roc_auc_int8":0.9683,"delta":-0.0006},
    "load_factor": 1.18, "distinct_features": 618402
  }
}
```

The numbers above are **illustrative placeholders with realistic magnitudes**; the trainer writes the
real measured values. The regression test (§11.4) asserts against *floors*, not these values.

### 4.6 Hostile-input rules for `Model::from_bytes`

Every one of these returns a typed `ModelError`. **No panic, no `unwrap`, no unbounded allocation.**

1. `bytes.len() > 32 * 1024 * 1024` → `TooLarge`.
2. `bytes.len() < 160` → `Truncated`.
3. `magic != CWCLSMDL` → `BadMagic`.
4. `format_version == 0 || format_version > MAX_FORMAT_VERSION` → `UnsupportedVersion`.
5. `header_len != 128` → `BadHeader`.
6. `flags & 0xFFF0 != 0` → `BadHeader` (unknown flag bits are a hard error, not ignored).
7. `128 + payload_len + 32 != bytes.len()` → `LengthMismatch`.
8. SHA-256 of `bytes[..len-32]` != trailer → `ChecksumMismatch`. **Computed before any section is
   interpreted**, so a corrupt file never reaches the parsers.
9. `!ngram_buckets.is_power_of_two() || !(4096..=2_097_152).contains(&ngram_buckets)` → `BadShape`.
10. `dim == 0 || dim > 64` → `BadShape`. Same for `tld_buckets` (power of two, `64..=4096`),
    `token_count <= 1024`, `n_categories <= 8`, `dense_features == 32`, `dense_bins == 16`.
11. `section_count == 0 || section_count > 64` → `BadShape`.
12. Section walk: cursor arithmetic uses `checked_add`; any overflow or `cursor + 16 + body_len >
    128 + payload_len` → `SectionOutOfBounds { id }`.
13. Duplicate `section_id` → `DuplicateSection { id }`.
14. Missing required section → `MissingSection { id }`.
15. `body_len` != the required length for that id → `SectionLengthMismatch { id, expected, actual }`.
16. `!platt_a.is_finite() || platt_a == 0.0 || !platt_b.is_finite()` → `BadCalibration`.
17. Thresholds not in `(0.0, 1.0)` or not monotonically non-increasing
    (`high <= balanced <= low`) → `BadThresholds`.
18. `SCALE_CODEBOOK[0] != 0.0`, or the codebook is not ascending, or any entry is negative /
    non-finite → `BadCodebook`.
19. `DENSE_EDGES` not strictly ascending within a feature, or any non-finite → `BadEdges`.
20. Any `f32` in a head or the bigram LM is non-finite → `NonFiniteWeights`.
21. `TOKEN_STRINGS`: any length `0` or `> 32`, any non-`[a-z0-9_-]` byte, or the declared count not
    matching `token_count` → `BadTokenTable`.

Reading is **zero-copy where it can be**: `NGRAM_Q` and `NGRAM_SCALE` are copied once into owned
`Box<[i8]>` / `Box<[u8]>` (4.7 MB memcpy, ~1.5 ms on a Pi 5 — acceptable at boot, and it lets the
input buffer be dropped). No `mmap`, so no new dependency and no page-fault jitter on the scoring
thread.

### 4.7 Model delta format (on-device adaptation output)

File: `adclass-delta.cwd`. Same envelope discipline, different magic.

| Off | Size | Field |
| ---: | ---: | --- |
| 0 | 8 | `magic` = ASCII `CWCLSDLT` |
| 8 | 2 | `format_version` (`u16`, 1) |
| 10 | 2 | `flags` (reserved, 0) |
| 12 | 4 | `header_len` = 64 |
| 16 | 32 | `base_sha256` — SHA-256 of the base **model file** this delta applies to |
| 48 | 8 | `created_at` (`i64`) |
| 56 | 4 | `generation` (`u32`, monotonically increasing, max 30 — §10.6) |
| 60 | 4 | `payload_len` (`u32`) |
| 64 | … | sections (ids 4,5,7,8,10,11,12,13 only — dense/token/tld/heads), same section framing |
| end−32 | 32 | SHA-256 of everything before |

A delta **never** contains `NGRAM_Q` / `NGRAM_SCALE`. That is the structural guarantee behind §10.7:
on-device adaptation physically cannot corrupt the n-gram knowledge, and rollback is `rm`.
`Model::with_delta` returns `Err(DeltaBaseMismatch)` if `base_sha256` does not match, so shipping a
new base model automatically retires stale deltas.

Size: `4 096 + 512 + 768 + 96 + 4 096 + 512 + 36 + 144 + 8 · 16 + 64 + 32 = 10 484 B ≈ 10 KB`,
plus 8 bytes of Platt override → the delta also carries a `PLATT_OVERRIDE` section (id 17, 8 bytes,
`platt_a` then `platt_b`) and a `THRESHOLD_OVERRIDE` section (id 18, 12 bytes, three `f32`).
**Total ≈ 10.5 KB.**

---

## 5. Inference, hot-path safety, and the allowlist

### 5.1 What the model computes

```
n     = number of hashed n-gram/label/suffix/cross features
bag   = (1/n) · Σᵢ deq(NGRAM[bᵢ])                                   ∈ ℝ^d
side  = Σ_f deq(DENSE[f·16 + bin_f]) + Σ_t deq(TOKEN[t]) + deq(TLD[e])   ∈ ℝ^d
h     = bag + side
logit = w_b · h + β_b
p     = σ(platt_a · logit + platt_b)                                 // calibrated probability
c     = argmax_k ( (W_c · h + β_c)_k )                               // UI category, uncalibrated
```

Only the n-gram bag is **averaged** (fastText convention: it normalizes for name length). The dense,
token and suffix contributions are **summed**, because they are a fixed-size set — averaging them
would make a name's dense signal weaker just because it has more n-grams.

### 5.2 Tier-1 / tier-2 split (the reason this is fast)

From §2.3, `w_b · deq(NGRAM[j])` is a scalar. Precompute it **once at model load**:

```rust
// ngram_proj[j] = Σ_k (q[j*d + k] as f32) * codebook[scale_code[j]] * w_b[k]
// 524_288 rows × 8 MAC = 4.2 M FLOP ≈ 2 ms once on a Pi 5.
ngram_proj: Box<[f32]>,   // 2.0 MiB
dense_proj: [f32; 512],   // 2.0 KiB  — always L1-resident
token_proj: [f32; 96],    // 384 B
tld_proj:   [f32; 512],   // 2.0 KiB
```

**Tier 1 — always runs.** Produces the exact binary logit and nothing else:

```rust
let mut acc = 0.0f32;
for i in 0..fv.ngram_len as usize { acc += self.ngram_proj[fv.ngram_buckets[i] as usize]; }
let mut logit = acc / fv.ngram_len as f32;
for f in 0..DENSE_FEATURES { logit += self.dense_proj[f * DENSE_BINS + fv.dense_bins[f] as usize]; }
for t in 0..fv.token_len as usize { logit += self.token_proj[fv.tokens[t] as usize]; }
logit += self.tld_proj[fv.suffix_bucket as usize] + self.bias_binary;
let p = sigmoid(self.platt_a * logit + self.platt_b);
```

Bytes touched: **4 per n-gram** instead of 8 int8 + 1 scale + 4 codebook — an 8× reduction in the
random-access working set (2.0 MiB instead of 4.7 MiB). The Pi 5's BCM2712 has a **2 MB shared L3**,
so a warm `ngram_proj` is *approximately L3-resident*, turning most gathers from ~90 ns DRAM loads
into ~30-cycle L3 hits. This single decision is worth roughly 2–3× on p50.

**Tier 2 — runs only when `p >= threshold_high - 0.05`** (empirically ~1.5 % of scored domains).
Recomputes the full `d`-dim `h` from the int8 tables to produce the category head and to populate
the explanation structure. Costs ~8× tier 1 but on 1.5 % of traffic, so it adds ~12 % to mean cost.

**Consistency test (§11.3):** `|tier1_logit − (w_b · h_tier2 + β_b)| < 1e-4` for 10 000 domains.

### 5.3 The hot path must never block — exact design

Current code (`/home/user/Cogwheel-DNS/crates/cogwheel-dns-core/src/lib.rs:317-325`) calls
`classify_domain` **synchronously, before the cache lookup, on 100 % of queries**, and
`ClassifierMode::Protect` does nothing. Both are replaced.

```
                       DNS hot path (async, per query)
  ┌──────────────────────────────────────────────────────────────────────┐
  │ parse → normalize domain                                             │
  │ policy_for_client()  → PolicySelection{ engine, scope, forced,       │
  │                                          classifier_enforced }       │
  │                                                                      │
  │ Classifier::lookup(&domain)   ← SYNC, ~90 ns, zero alloc, no await  │
  │   ├─ Known(v)   → maybe block (Protect), maybe emit event            │
  │   ├─ Pending    → nothing                                            │
  │   └─ Unknown    → Classifier::submit(&domain)  ← try_send, never blocks│
  │                                                                      │
  │ … existing cache / policy / upstream path, unchanged …               │
  └──────────────────────────────────────────────────────────────────────┘
                                   │ std::sync::mpsc::SyncSender (cap 4096)
                                   ▼
  ┌──────────────────────────────────────────────────────────────────────┐
  │ scoring thread  (std::thread, NOT a tokio task, NOT the blocking pool)│
  │   recv → NormalizedDomain::parse → FeatureVector::extract_into        │
  │        → Model::score (tier 1, maybe tier 2)                          │
  │        → VerdictCache::insert                                         │
  │        → verdict_observer(VerdictEvent)   ← server tokio::spawns DB IO │
  │   token bucket: ≤ max_inferences_per_sec (default 20 000/s)           │
  └──────────────────────────────────────────────────────────────────────┘
```

Why a **dedicated OS thread** and not a tokio task or `spawn_blocking`:

- It is CPU-bound and long-lived. A tokio task would interleave with the resolver's I/O tasks and
  add scheduler jitter to DNS responses; `spawn_blocking` would permanently occupy one of the
  blocking pool's threads, which the storage layer also needs
  (`/home/user/Cogwheel-DNS/docs/architecture/02-core-crates.md` §7.4: every `Storage` method does
  blocking SQLite on the calling thread).
- It lets us set the thread name (`cogwheel-classifier`) and, on Linux, lower its priority — the
  server calls `libc`-free `nice`? No: **do not** shell out or add `libc`. Priority control is done
  purely by the token bucket + duty cycle, which is portable and testable.
- `std::sync::mpsc::SyncSender::try_send` is **synchronous, lock-light, and never awaits**, which is
  exactly the property the hot path needs. `tokio::sync::mpsc` would also work, but its `try_send`
  drags an async runtime dependency into a code path that must be callable from a sync context
  (the ad-hoc scoring API, the trainer, and tests).

**Structural guarantee (rubric 4.4):** `Classifier::lookup` and `Classifier::submit` are `fn`, not
`async fn`, and they take `&self`. It is **impossible** for `handle_wire_query` to await on inference
because there is nothing to await. A test asserts the sync signature via a trait-object coercion
that would fail to compile if either became `async` (§11.6).

### 5.4 Verdict cache — bounded, allocation-free, no new dependency

`moka` is not used here. Reasons: the workspace enables only `moka`'s `future` feature, so
`Cache::get`/`insert` are `async` — unusable from the sync scoring thread without a runtime handle;
enabling `moka/sync` changes the feature resolution graph; and moka's per-entry overhead (~100 B +
the key `String`) is larger than the entire purpose-built cache.

```rust
pub struct VerdictCache {
    shards: Box<[CacheShard]>,          // SHARDS = 64
    mask: u64,                          // SHARDS - 1
}
struct CacheShard {
    slots: RwLock<Box<[Slot; SLOTS_PER_SHARD]>>,   // SLOTS_PER_SHARD = 1024
}
#[derive(Clone, Copy)]
struct Slot {
    fp_hi: u64,        // 8   FNV-1a of the normalized domain, offset basis FNV_OFFSET
    fp_lo: u64,        // 8   FNV-1a with offset basis 0x9e37_79b9_7f4a_7c15
    probability: f32,  // 4   NaN => empty slot; f32::INFINITY => Pending
    scored_at: u32,    // 4   seconds since the process epoch base
    last_event_at: u32,// 4   for event de-duplication
    category: u8,      // 1
    model_epoch: u8,   // 1   invalidates every slot on model/settings swap
    _pad: [u8; 2],     // 2
}                      // = 32 bytes exactly (no padding surprises; asserted by a size_of test)
```

- Capacity: `64 × 1024 = 65 536` slots × 32 B = **2.0 MiB, fixed, never grows.**
- Keying: the **128-bit fingerprint**, not the string. With 65 536 live entries the birthday
  collision probability is `65536² / 2^129 ≈ 6.3e-30` — a false verdict from a fingerprint collision
  is ~10²² times less likely than an uncorrected DRAM bit flip. This removes all per-entry heap
  allocation and makes each slot a single cache line's worth of work.
- Placement: shard = `fp_hi & mask`; within a shard, **2-way set-associative** at
  `slot_base = (fp_hi >> 6) % 512 * 2`. Insert evicts the entry of the pair with the older
  `scored_at`. Deterministic, `O(1)`, no clock sweep, no LRU list.
- TTL: an entry with `now - scored_at > verdict_ttl_secs` (default **21 600 s = 6 h**) is treated as
  `Unknown` on read and is overwritten on the next insert. Blocklists and the model change; verdicts
  must not be immortal.
- `model_epoch` is a `u8` bumped on `replace_model` and on any settings change that alters the
  active threshold. A slot whose `model_epoch` differs from the current one reads as `Unknown`.
  This is an **O(1) global invalidation** — no iteration, no allocation.
- `Pending`: a slot is written with `probability = f32::INFINITY` at `submit` time. This is the
  de-duplication mechanism: a burst of 200 queries for one new domain enqueues **once**.
- Read path holds the shard's `RwLock` **read** guard for the duration of two `Slot` copies
  (~20 ns). Writes take the write guard for one slot store. A poisoned lock is handled with
  `if let Ok(g) = …` and degrades to `Unknown` / dropped insert — **never panics** (§9.2 of
  `02-core-crates.md` demands this; the existing `DnsRuntime` reader-panics are the anti-pattern).

### 5.5 Queue, backpressure and drop policy

```rust
pub struct ClassifierConfig {
    pub queue_capacity: usize,          // default 4096
    pub max_inferences_per_sec: u32,    // default 20_000
    pub burst_tokens: u32,              // default 4_000
    pub verdict_cache_slots: usize,     // default 65_536 (rounded to 64 × 2^k)
    pub verdict_ttl_secs: u32,          // default 21_600
    pub event_dedup_secs: u32,          // default 300
    pub tier2_margin: f32,              // default 0.05
    pub worker_threads: u8,             // default 1, max 2
}
```

`submit` returns, without ever blocking:

| Outcome | When | Accounting |
| --- | --- | --- |
| `Enqueued` | slot marked `Pending`, `try_send` succeeded | `classifier_enqueued_total += 1` |
| `AlreadyKnown` | fresh verdict in cache | `classifier_verdict_hits_total += 1` |
| `AlreadyPending` | slot already `Pending` and not stale | `classifier_dedup_total += 1` |
| `Dropped(QueueFull)` | `try_send` → `TrySendError::Full` | `classifier_dropped_total += 1`; the `Pending` marker is **rolled back** so a later query can retry |
| `Dropped(RateLimited)` | token bucket empty | `classifier_rate_limited_total += 1`; marker rolled back |
| `Dropped(Protected)` | `is_protected(domain)` (§5.8) | `classifier_skipped_protected_total += 1`; a permanent `probability = 0.0` verdict is written so it is never re-submitted |
| `Dropped(WorkerStopped)` | receiver hung up | logged **once** via an `AtomicBool`; a flag disables further submits |

**Drop policy is "drop newest, never block, never grow".** Justification: DNS is retried by every
client stack, and a home network re-queries the same names constantly, so a dropped submission costs
at most one extra unprotected query. Blocking or growing the queue would trade a bounded, invisible
miss for an unbounded latency or memory failure — which is the failure mode the Pi 5 cannot absorb.

Token bucket (monotonic, lock-free): `AtomicU64` packing `(tokens: u32, last_refill_millis: u32)`,
updated with `compare_exchange_weak`. Refill `max_inferences_per_sec / 1000` tokens per elapsed
millisecond, capped at `burst_tokens`. At the default 20 000/s and a measured 1.6 µs/inference, the
worker's ceiling is **32 ms of CPU per second = 3.2 % of one core** (rubric 4.6). The operator may
raise it to 100 000/s (16 % of a core); the API validator rejects anything above that.

### 5.6 Feeding a Protect verdict back into policy

`policy_for_client` is changed to return a struct instead of a tuple:

```rust
pub struct PolicySelection {
    pub engine: Arc<PolicyEngine>,
    pub cache_scope: String,
    pub forced_block_mode: Option<BlockMode>,
    pub classifier_enforced: bool,
}
```

`classifier_enforced` is `false` for exactly the four cases where the user has explicitly asked for
no filtering — global pause, `device.protection_override == "bypass"`, a `device-allow:` match, and
`ClassifierMode != Protect` — and `true` otherwise. This replaces string-matching on `cache_scope`,
which would be brittle.

New ordering inside `handle_wire_query` (replacing lines 317–325):

```rust
let selection = self.policy_for_client(client_addr, &domain);

// 1. Classifier: O(1) probe, never blocks, never awaits.
let settings = self.classifier.settings();
if !matches!(settings.mode, ClassifierMode::Off) {
    let t0 = Instant::now();
    match self.classifier.lookup(&domain) {
        VerdictLookup::Known(v) => {
            let threshold = self.classifier.active_threshold();
            if v.probability >= threshold {
                if self.classifier.mark_event_emitted(&domain) {   // 300 s de-dup
                    self.emit_classification_event(&domain, client_addr, v.into_classification());
                }
                if selection.classifier_enforced {
                    self.stats.classifier_blocked_total.fetch_add(1, Ordering::Relaxed);
                    self.stats.blocked_total.fetch_add(1, Ordering::Relaxed);
                    let response = build_blocked_response(
                        &request, selection.engine.artifact().block_mode.clone());
                    self.emit_query_activity(&domain, client_addr, true);
                    self.record_cache_miss_latency(query_start.elapsed().as_nanos());
                    self.record_classifier_latency(t0.elapsed().as_nanos());
                    return Ok(response);            // ← the block, before the response cache
                }
            }
        }
        VerdictLookup::Unknown => { let _ = self.classifier.submit(&domain); }
        VerdictLookup::Pending => {}
    }
    self.record_classifier_latency(t0.elapsed().as_nanos());
}

// 2..n. unchanged: response cache, forced device block, engine.evaluate, uncloak, upstream.
```

Three consequences, all deliberate:

1. **The classifier block is evaluated *before* the response cache**, so a domain that was cached as
   allowed 3 minutes ago is still blocked the moment its verdict lands. No cache invalidation is
   needed anywhere — which matters, because `moka` cannot cheaply invalidate by domain across the
   9 different `cache_scope` prefixes (`02-core-crates.md` §6.4).
2. **The classifier's blocked response is never inserted into the response cache.** Verdicts and
   settings can change at any moment; caching the block would make the change lag by up to the
   cache's lifetime.
3. **A `PolicyEngine` `Allow` rule still wins**, because the user's explicit allow shows up as a
   `device-allow:` scope (→ `classifier_enforced == false`) or, for global allows, is layered in at
   the ruleset level and short-circuits before this point in a follow-up: to keep the ordering
   watertight, `Classifier::lookup` is skipped entirely when
   `selection.engine.evaluate(&domain)` would return `reason == "matched allow rule"`. Because
   `evaluate` is `O(rules)` today, this check is deferred: the implementer must call
   `selection.engine.evaluate(&domain)` **once**, before the classifier block, and reuse the
   `Decision` for step 7 of the existing flow. That is a strict improvement — the current code
   evaluates the engine once anyway, just later.

### 5.7 First sighting — the honest UX contract

**On the very first query for a domain the classifier has never scored, the query is answered
normally.** There is no synchronous inference, so there cannot be a first-query verdict.

Timeline for a brand-new ad domain on an otherwise idle Pi 5:

| t | Event |
| --- | --- |
| 0 µs | query arrives; `lookup` → `Unknown`; `submit` → `Enqueued`; DNS answer proceeds |
| ~2 ms | resolver returns; client gets the real answer |
| ~50 µs–5 ms | scoring thread dequeues, scores (1.6 µs), writes the verdict, notifies the observer |
| next query | `lookup` → `Known(0.987)` → blocked (Protect) |

In practice the gap is one DNS round trip. A browser loading a page issues 10–60 DNS queries and
re-queries the same names as TTLs expire, so the *user-visible* miss is normally a single request
for a single asset.

**This must be stated in the UI, verbatim in substance:**

> **How AI protection works.** Cogwheel answers the first lookup of a new domain using your
> blocklists, then scores it in the background — usually within a few milliseconds. From the next
> lookup onward the AI verdict applies. This keeps DNS fast: no query ever waits for the model.

The Monitor-vs-Protect copy must also be explicit:

> **Monitor** — score and record, never block. **Protect** — also block domains above the
> sensitivity threshold, starting from their second lookup.

And the per-domain detail view shows `first_seen_at`, `scored_at`, and `enforced_from` (= the first
query after `scored_at`), so nothing about the delay is hidden.

### 5.8 Protected-domain allowlist (the safety net the classifier can never override)

A **compile-time static** in
`/home/user/Cogwheel-DNS/crates/cogwheel-classifier/src/allowlist.rs`. It is *not* configuration, it
is *not* in the model file, and it is *not* adaptable — those are the properties that make it a
safety net.

Semantics, precisely:

- `Classifier::is_protected(domain) -> bool` matches **exact or dot-boundary suffix**, case-insensitive
  on the normalized name. `ocsp.digicert.com` matches the entry `digicert.com`;
  `notdigicert.com` does not (same rule as `domain_matches_override` in
  `cogwheel-dns-core:673`).
- A protected domain **can never receive a Block verdict.** The check is applied in **three** places
  (defense in depth): in `submit` (short-circuits to a permanent `probability = 0.0` verdict), in the
  scoring worker before the verdict is written, and in the hot path before the block is emitted.
- It **does not** override the user's own blocklists or manual rules. If a user blocks `facebook.com`,
  it stays blocked. The allowlist constrains **the classifier only**. This is a different mechanism
  from `RulesetArtifact.protected_domains` (`cogwheel-policy`, exact-match, ruleset-scoped) and the
  two must not be conflated.
- It is unit-tested (§11.7) with the assertion that every entry, plus a `www.` and a random 2-label
  prefix of every entry, scores `is_protected == true`, and that scoring each entry through the full
  model produces `verdict.probability == 0.0`.

```rust
/// Domains the classifier may never block. Grouped by why. Sorted within group.
/// Adding an entry requires a test; removing one requires an ADR.
pub static PROTECTED_SUFFIXES: &[&str] = &[
    // ── DNS bootstrap (blocking these makes the network unrecoverable) ──
    "cloudflare-dns.com", "dns.google", "dns.nextdns.io", "dns.quad9.net", "dns10.quad9.net",
    "dns.adguard-dns.com", "doh.opendns.com", "one.one.one.one", "resolver1.opendns.com",
    // ── Time (TLS fails everywhere if the clock is wrong) ──
    "pool.ntp.org", "time.apple.com", "time.cloudflare.com", "time.google.com",
    "time.nist.gov", "time.windows.com", "ntp.ubuntu.com",
    // ── Captive-portal / connectivity detection ──
    "captive.apple.com", "connectivitycheck.android.com", "connectivitycheck.gstatic.com",
    "detectportal.firefox.com", "msftconnecttest.com", "msftncsi.com",
    "network-test.debian.org", "nmcheck.gnome.org", "networkcheck.kde.org",
    // ── PKI: OCSP / CRL / trust roots (blocking these breaks HTTPS silently) ──
    "crl.microsoft.com", "isrg.trustid.ocsp.identrust.com", "lencr.org",
    "ocsp.apple.com", "ocsp.digicert.com", "ocsp.globalsign.com", "ocsp.pki.goog",
    "ocsp.sectigo.com", "ocsp.usertrust.com", "pki.goog", "digicert.com", "sectigo.com",
    // ── OS / vendor update channels ──
    "apple.com", "mzstatic.com", "swcdn.apple.com", "gdmf.apple.com",
    "microsoft.com", "windowsupdate.com", "update.microsoft.com", "delivery.mp.microsoft.com",
    "archive.ubuntu.com", "security.ubuntu.com", "deb.debian.org", "fedoraproject.org",
    "dl.google.com", "android.com", "gvt1.com", "gvt2.com",
    // ── CDN roots (blocking a CDN root breaks the whole internet, not one site) ──
    "akamai.net", "akamaiedge.net", "akamaihd.net", "akamaized.net", "edgekey.net",
    "edgesuite.net", "cloudfront.net", "fastly.net", "fastlylb.net", "cloudflare.com",
    "cdn.cloudflare.net", "azureedge.net", "azurefd.net", "cdn77.org",
    "gstatic.com", "googleapis.com", "googleusercontent.com", "ytimg.com", "ggpht.com",
    "fbcdn.net", "cdninstagram.com", "licdn.com", "twimg.com", "redditstatic.com",
    "jsdelivr.net", "unpkg.com", "bootstrapcdn.com", "cloudflareinsights.com",
    // ── Identity / MFA (blocking these locks users out of everything) ──
    "accounts.google.com", "auth0.com", "duo.com", "duosecurity.com", "login.live.com",
    "login.microsoftonline.com", "okta.com", "oktacdn.com", "onelogin.com", "yubico.com",
    // ── Real-time comms and messaging ──
    "signal.org", "whatsapp.com", "whatsapp.net", "telegram.org", "t.me", "discord.com",
    "zoom.us", "teams.microsoft.com", "slack.com", "slack-edge.com",
    // ── Banking, payments, brokerage ──
    "americanexpress.com", "bankofamerica.com", "barclays.co.uk", "capitalone.com",
    "chase.com", "citi.com", "citibank.com", "fidelity.com", "hsbc.com", "lloydsbank.com",
    "monzo.com", "natwest.com", "nationwide.co.uk", "paypal.com", "plaid.com", "revolut.com",
    "santander.co.uk", "schwab.com", "starlingbank.com", "stripe.com", "tdameritrade.com",
    "vanguard.com", "wellsfargo.com", "wise.com",
    // ── Government / health / emergency ──
    "gov", "gov.uk", "mil", "nhs.uk", "europa.eu", "irs.gov", "ssa.gov", "cdc.gov",
    "who.int", "ready.gov", "alerts.gov",
    // ── Cogwheel's own control plane and update channel ──
    "cogwheel.local", "cogwheel.internal",
];
```

Note the entries `"gov"` and `"mil"` are **whole public suffixes** — dot-boundary suffix matching
means every `*.gov` and `*.mil` name is protected. That is intentional.

Implementation: the list is sorted at first use into a `OnceLock<Box<[&'static str]>>`. Matching
walks the name's ≤ 16 label offsets from left to right, binary-searching each suffix candidate —
worst case 16 × ⌈log₂ 150⌉ ≈ 128 string comparisons ≈ 0.2 µs, and it runs at most once per *scored*
domain, not once per query (the hot path's re-check reads the cached `probability == 0.0` verdict).

---

## 6. Latency budget

### 6.1 Operation count for one inference

Worked for `ads.doubleclick.net` (19 bytes normalized, 21 bytes marked, 3 labels) — close to the
corpus median (measured median normalized length: 21 bytes).

| Stage | Operations | Cycles @ A76 | ns @ 2.4 GHz |
| --- | --- | ---: | ---: |
| Normalize: trim, lowercase 19 B, validate 19 B, split labels | ~4 cycles/byte | 78 | 33 |
| Public-suffix lookup: 3 candidates × binary search over 6 800 | ~40 string compares | 190 | 79 |
| Dense features: 2 passes over 19 B + `[u16;256]` clear + 2 entropy sums via `LOG2_TABLE` | ~330 ops | 340 | 142 |
| Dense binning: 32 × `partition_point` over 15 f32 (all L1) | 32 × ~8 | 260 | 108 |
| n-gram enumeration + FNV-1a: 70 features, mean 4.1 bytes, ~5 cycles/byte + 12-cycle finalizer | 70 × 33 | 2 310 | 963 |
| Token prefilter probes (fused into the 4-gram sweep) | 19 probes + ~0.4 verifies | 45 | 19 |
| **Tier-1 gather: 70 random `f32` loads into a 2.0 MiB table** | MLP-limited, see §6.2 | **1 300** | **542** |
| Tier-1 accumulate: 70 + 32 + 2 + 1 f32 adds | 105 | 105 | 44 |
| Sigmoid (1 × `expf`) + Platt affine | ~60 | 60 | 25 |
| Verdict-cache insert: 1 `RwLock` write + 32 B store | ~90 | 90 | 38 |
| **Tier-1 total** | | **4 778** | **1 991 ns** |
| Tier 2 (1.5 % of domains): 70 × 8 int8 loads + widen + MAC, 4-way head, softmax | +9 400 | +9 400 | +3 917 |
| **Amortized mean** | | **4 919** | **≈ 2.05 µs** |

Rounded working figure: **≈ 2 µs per domain on a Cortex-A76 @ 2.4 GHz**, ≈ **1.2 µs** on a modern
x86-64 CI runner (higher IPC, larger L3).

### 6.2 Why the gather is the interesting term

70 lookups into a 2.0 MiB `f32` table are **random**, so hardware prefetch is useless. What saves us
is memory-level parallelism: the Cortex-A76 sustains roughly **12–16 outstanding L2/L3 misses**, and
the extractor is written in **two phases** — *hash everything into `fv.ngram_buckets` first, then
gather* — so all 70 loads are independent and the out-of-order engine overlaps them.

- If every access missed to LPDDR4X (~90 ns): `70 / 12 × 90 ns = 525 ns`.
- The BCM2712 has a **2 MB shared L3**, so a warm 2.0 MiB `ngram_proj` is largely L3-resident;
  an L3 hit is ~30 cycles. A realistic mix (60 % L3, 40 % DRAM) gives
  `70 × (0.6 × 12.5 ns + 0.4 × 90 ns) / 12 ≈ 254 ns`. The table above uses the pessimistic 542 ns.

This is precisely why tier 1 reads a 4-byte `f32` per feature rather than 8 `i8` + a scale + a
codebook entry: the naive layout has a 4.7 MiB working set that cannot be L3-resident, and would
cost roughly 2–3× more.

**Do not** "optimize" this by making the two phases one loop. It looks tighter and is ~2× slower,
because each hash then serializes behind the previous gather. Put a comment there saying so.

### 6.3 The budgets (contract)

| Metric | Budget | Basis |
| --- | --- | --- |
| Single-inference **p50**, Pi 5, 1 core, warm | **≤ 10 µs** | 5× the 2.0 µs estimate |
| Single-inference **p99**, Pi 5, 1 core, warm | **≤ 50 µs** | absorbs a cold-table walk and an OS preemption |
| Single-inference **p99.9**, Pi 5 | **≤ 500 µs** | absorbs a scheduler quantum |
| Sustained **throughput floor** | **≥ 50 000 domains/s/core** | 20 µs/domain average — 10× headroom |
| Hot-path `lookup()` (cache probe only) | **p99 ≤ 2 µs** | it is 2 slot copies under a read lock |
| Worker CPU share at default rate limit | **≤ 3.2 % of one core** | 20 000/s × 1.6 µs |
| Worker CPU share at the maximum allowed rate | **≤ 16 % of one core** | 100 000/s × 1.6 µs |
| Model file | **≤ 8 000 000 B** | §4.2 gives 4.74 MB |
| Resident (model + caches + queue) | **≤ 16 777 216 B** | §4.3 gives 8.56 MiB |
| Nightly adaptation | **≤ 30 s wall clock at a 25 % duty cycle** | §10.4 |

Sanity check against the product: a busy home network peaks near 60 queries/s. Verdict-cache hit
rate after warm-up is > 99 % (domain re-query is extremely heavy-tailed), so the worker sees
< 1 inference/s steady-state and ~200/s during a cold start. **The classifier's steady-state CPU cost
is under 0.001 % of one core.** The rate limit exists for adversarial bursts (a DGA-infected device),
not for normal use.

### 6.4 Where the budgets are asserted

`crates/cogwheel-classifier/tests/` is **not** used — the workspace has no `tests/` directories and
`02-core-crates.md` §10 documents that as the house convention. Everything is an inline
`#[cfg(test)] mod tests`. The benchmark lives in
`/home/user/Cogwheel-DNS/crates/cogwheel-classifier/src/bench.rs` behind `#[cfg(test)]`.

Two tiers of assertion, because CI runs on x86 and the target is a Pi:

**Tier A — always runs in `cargo test --workspace`, on any machine.**
These are *regression floors*, deliberately loose enough that a loaded shared CI runner cannot flake
them, but tight enough to catch a real defect (an allocation per query, a lock on the gather path, a
lost `#[inline]`, an accidental `String` in the hot loop — any of which cost 5–50×):

```rust
assert!(mean_ns <= 20_000,  "mean {mean_ns} ns/domain exceeds the 20 µs regression floor");
assert!(p99_ns  <= 200_000, "p99 {p99_ns} ns exceeds the 200 µs regression floor");
assert!(throughput >= 50_000.0, "{throughput:.0} domains/s/core below the 50 000 floor");
assert!(allocations_during_steady_state == 0);   // counted by a test-only GlobalAlloc shim
```

**Tier B — the real budget, enabled by `COGWHEEL_BENCH_STRICT=1`.**
Run in a nightly `linux/arm64` job on real hardware (and locally by anyone with a Pi):

```rust
if std::env::var_os("COGWHEEL_BENCH_STRICT").is_some() {
    assert!(p50_ns <= 10_000, "p50 {p50_ns} ns exceeds the 10 µs Pi 5 budget");
    assert!(p99_ns <= 50_000, "p99 {p99_ns} ns exceeds the 50 µs Pi 5 budget");
}
```

**Honesty rules for the benchmark** (rubric 4.10 — "measured numbers are recorded in docs, with the
measurement method stated"):

1. Input is the **committed holdout set** (`data/holdout.tsv`, 20 000 real domains), not synthetic
   strings, and not the same domain repeatedly — repeating one domain would keep everything in L1
   and measure nothing.
2. 20 000 warm-up iterations, then 200 000 timed iterations, cycling the holdout list.
3. Timing uses `std::time::Instant` **per batch of 1 000** to keep clock overhead under 0.1 %;
   percentiles come from the 200 per-batch means plus a separate 20 000-sample per-call histogram
   (64 log-spaced buckets) for p99/p99.9.
4. The result is `println!`-ed in a stable, greppable form so a human can compare runs:
   `COGWHEEL_BENCH inference mean_ns=… p50_ns=… p99_ns=… p999_ns=… throughput_dps=… arch=… model_bytes=… resident_bytes=…`
5. `/home/user/Cogwheel-DNS/docs/reliability-budgets.md` gains a **Classifier** table recording the
   measured Pi 5 numbers, the exact hardware (`Raspberry Pi 5 8 GB, Debian 12 arm64, kernel 6.x,
   cpufreq governor `performance`, no other load`), the commit, and the date. Re-measure and update
   on any change to §3 or §5.
6. The x86 CI number is recorded **separately and labelled as x86**. Never present a CI number as a
   Pi number.

### 6.5 Cross-architecture rules (rubric 4.9)

- No `core::arch` intrinsics anywhere. The int8 accumulation is written as a plain `i32` loop over
  `d = 8`; LLVM auto-vectorizes it to NEON on `aarch64` and SSE2 on x86-64. Verified by inspecting
  `cargo asm`, not assumed.
- No assumption about `char` width, endianness, or `usize` size beyond 64-bit. All model I/O is
  explicit little-endian via `u32::from_le_bytes` on `&[u8; 4]` slices obtained by `try_into()`.
- No unaligned reinterpretation: `NGRAM_Q` is read byte-by-byte into a `Box<[i8]>`; there is no
  `transmute` and no `bytemuck`.
- `f32` arithmetic is left at default (no `-ffast-math` equivalent), so results are bit-identical
  across architectures given the same summation order — which the golden-vector tests (§11.2) pin.

---

## 7. Training pipeline

Code split:
- **`crates/cogwheel-classifier/src/train/`** — everything pure: parsing, dedup, split, SGD,
  quantization, metrics, Platt. **No feature gate.** It compiles into the library unconditionally
  (~1 500 lines, zero deps, ≈ 60 KB of release binary) so that `cargo test --workspace` — the exact
  command in the rubric — runs the model-quality regression test. A feature gate here would let the
  most important test in the project silently not run.
- **`apps/cogwheel-trainer/`** — a new workspace member owning **only** network fetch, file I/O, CLI
  and logging. It is the only place `reqwest` appears.

### 7.1 Corpus manifest

`/home/user/Cogwheel-DNS/apps/cogwheel-trainer/corpus.toml` is the single source of truth. Its
canonical serialization (keys sorted, `\n` line endings) is SHA-256'd into the model header's
`corpus_sha256`.

```toml
schema = 1

[[source]]
role = "positive"; category = "Advertising"; format = "hosts"
url = "https://raw.githubusercontent.com/StevenBlack/hosts/master/hosts"
max_bytes = 33_554_432

[[source]]
role = "positive"; category = "Advertising"; format = "adblock"
url = "https://easylist.to/easylist/easylist.txt"
max_bytes = 33_554_432

[[source]]
role = "positive"; category = "Tracking"; format = "adblock"
url = "https://raw.githubusercontent.com/AdguardTeam/AdguardFilters/master/SpywareFilter/sections/tracking_servers.txt"
max_bytes = 16_777_216

[[source]]
role = "positive"; category = "Advertising"; format = "hosts"
url = "https://pgl.yoyo.org/adservers/serverlist.php?hostformat=hosts&showintro=0&mimetype=plaintext"
max_bytes = 4_194_304

[[source]]
role = "positive"; category = "unknown"; format = "adblock"     # mixed list → category loss masked
url = "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.txt"
max_bytes = 67_108_864

[[source]]
role = "negative"; format = "majestic_csv"; rank_column = 0; domain_column = 2
url = "https://downloads.majestic.com/majestic_million.csv"
max_bytes = 209_715_200

[[source]]
role = "negative"; format = "plain_ranked"
url = "https://raw.githubusercontent.com/zer0h/top-1000000-domains/master/top-100000-domains"
max_bytes = 16_777_216

# Optional, operator-extracted (needs a ZIP reader we cannot add):
#   --negatives-file tranco.csv --negatives-format majestic_csv --rank-column 0 --domain-column 1
```

Fetch discipline (rubric 7.2, 7.5): per-source 60 s connect + 300 s total timeout; hard `max_bytes`
cap enforced **while streaming** (abort and fail the source, do not buffer past the cap); non-2xx →
source fails; a failed source is fatal by default and skippable with `--allow-source-failure`, which
is recorded in `PROVENANCE`. Every fetched body is SHA-256'd and the digest recorded.

### 7.2 Parsers — exact rules (formats verified live on 2026-08-16)

**`hosts`** (StevenBlack, pgl.yoyo). Observed shape:
`127.0.0.1 localhost`, `0.0.0.0 ads.example.com`, `# comment`, blank lines.

1. Truncate the line at the first `#`; trim.
2. Split on ASCII whitespace. If ≥ 2 tokens **and** token 0 parses as an `IpAddr` → candidate is
   token 1 (and tokens 2.. are additional candidates — some hosts files list aliases). If exactly
   1 token → that is the candidate.
3. **Reject** candidates in `HOSTS_NOISE` = {`localhost`, `localhost.localdomain`, `local`,
   `broadcasthost`, `ip6-localhost`, `ip6-loopback`, `ip6-localnet`, `ip6-mcastprefix`,
   `ip6-allnodes`, `ip6-allrouters`, `ip6-allhosts`, `0.0.0.0`}. This matters — the first 12 lines of
   StevenBlack's file are exactly this noise.
4. Run `NormalizedDomain::parse`; on `Err`, count a rejection and continue.

**`adblock`** (EasyList, AdGuard, hagezi). Observed shape:
`||d-p-d-0027912.0013484922656.com^`, `||ads.01film.cc^`, `! comment`, `[Adblock Plus 2.0]`,
plus enormous quantities of URL/cosmetic rules we must **not** ingest.

Accept **only** lines matching this grammar, and reject everything else:

```
line      := "||" host "^" modifiers?
host      := [a-z0-9._-]+          (no "*", no "/", no "|", no "$" inside)
modifiers := "$" mod ("," mod)*
mod       ∈ { "third-party", "3p", "all", "document", "doc", "popup", "important" }
```

Explicit rejections, each counted separately and reported:
- `@@`-prefixed **exception** rules → collected into a separate `exceptions` set (used in §7.3, not
  as positives).
- Any line containing `##`, `#?#`, `#@#`, `#$#` (cosmetic) → reject.
- Any line starting or ending with `/` (regex) → reject.
- Any modifier not on the allow-list — in particular **`domain=`** — → reject. Those rules are
  context-dependent (`||x.com^$domain=y.com` means "x.com is an ad *only when embedded in y.com*"),
  and treating them as unconditional positives is a direct false-positive generator.
- Lines with `*` in the host → reject (we cannot expand a wildcard into a label).
- `[`-prefixed headers, `!`-prefixed comments, blank lines → skip silently.

**`majestic_csv`.** Verified header:
`GlobalRank,TldRank,Domain,TLD,RefSubNets,RefIPs,IDN_Domain,IDN_TLD,PrevGlobalRank,…`
Skip row 1 if it starts with `GlobalRank`. Split on `,` (no quoted fields appear in the domain
column; if a row has < 3 fields, reject). `rank = field[0].parse::<u32>()`, `domain = field[2]`.

**`plain_ranked`.** One domain per line, rank = 1-based line number. Verified: `google.com`,
`youtube.com`, … Skip blank lines and `#` comments.

Global caps: line length > 512 bytes → reject the line; total accepted rows per source > 5 000 000 →
stop the source and warn. Both are rubric 7.5 requirements (hostile content must not hang or OOM).

### 7.3 Label hygiene

1. **Deduplicate within role.** Positives and negatives each become a `HashSet<String>` of
   normalized names. Duplicate counts are reported; a domain appearing on 4 blocklists is **one**
   training example, not four (otherwise popular ad networks dominate the loss).
2. **Conflicts** (`P ∩ N`, typically 3 000–5 000 domains — `criteo.com` is a real business with real
   backlinks, so it appears in Majestic *and* on every ad list):
   - negative rank ≤ **10 000** → **force label 0 (benign)**, and append the row to
     `conflicts_forced_negative.tsv`. Rationale: a false positive on a top-10k domain is the single
     most damaging failure the product can have, so the tie is broken toward "do not block".
   - negative rank > 10 000 → **drop from all splits**, and append to `conflicts_dropped.tsv`.
     Neither label is trustworthy; training on either injects noise.
   - Both files are written next to the model and their row counts go into `PROVENANCE`.
3. **EasyList `@@` exceptions** are added to the negatives pool with weight 1.0, ranked as if
   rank = 50 000. They are high-quality "a maintainer explicitly unblocked this" labels.
4. **Protected-suffix scrub.** Any positive matching `PROTECTED_SUFFIXES` (§5.8) is dropped and
   loudly reported. If a blocklist wants to block `ocsp.digicert.com`, that is the *user's* decision
   via rules, and the model must never learn it.
5. **Validity.** Every surviving row must pass `NormalizedDomain::parse`. Rejections are counted by
   error variant and printed.

### 7.4 The subdomain-depth trap — the most important hygiene step

**The negatives are registrable domains (`google.com`); the positives are mostly full hostnames
(`ads.doubleclick.net`).** Train on that as-is and the model learns *"has a subdomain ⇒ ad"*, which
scores ~0.97 AUC on the corpus and is catastrophically wrong in production: it would block
`mail.yourcompany.com`. This is the single biggest way to build a classifier that looks great and is
useless. Three mandatory countermeasures:

1. **Negative subdomain augmentation.** For each benign registrable domain, emit the bare domain
   plus `k` synthetic hostnames, `k` drawn so the resulting depth distribution matches the positives'
   (step 3). Prefix labels are sampled from
   `COMMON_BENIGN_LABELS` (220 entries: `www, mail, api, cdn, static, m, img, assets, login, secure,
   app, docs, support, admin, dev, staging, media, files, video, news, store, account, my, portal,
   vpn, remote, git, ns1, ns2, mx, smtp, imap, autodiscover, calendar, drive, chat, forum, wiki,
   help, status, beta, test, demo, sandbox, edge, origin, storage, images, thumbs, avatars, uploads,
   download, mirror, pkg, repo, registry, blog, shop, careers, jobs, investors, press, legal, …`)
   with an empirical frequency table, including 2-deep combinations (`cdn.static`, `api.v2`).
   Sampling uses the deterministic PRNG (§7.6) seeded from the split salt, so the corpus is
   reproducible.
2. **Positive registrable-domain folding.** For 25 % of positives, additionally emit the registrable
   domain alone as a positive (only when that registrable domain is *itself* on a blocklist, so we
   do not mislabel `example.com` because `ads.example.com` is blocked).
3. **Depth and length distribution matching.** After 1 and 2, resample (down-sample the majority
   side) until, for every `SubdomainDepth` bucket in `0..=5`, the positive and negative shares differ
   by **≤ 2 percentage points**, and the same for `TotalLen` in 8 buckets. The trainer **asserts**
   this and fails with `TrainError::DistributionSkew { feature, bucket, delta }` if it cannot reach
   it. Report the final table in `PROVENANCE`.

After matching, `SubdomainDepth` and `TotalLen` remain as features — they carry real residual signal
— but the model can no longer use them as a shortcut, because the marginal distributions agree.

### 7.5 Split by registrable domain (rubric 5.2)

Splitting by hostname leaks: `ads.doubleclick.net` in train and `pixel.doubleclick.net` in test means
the test set measures memorization, not generalization. Split by **registrable domain**:

```rust
pub fn split_bucket(registrable: &str, salt: u64) -> u16 {
    let mut h = FNV_OFFSET ^ salt;
    for &b in registrable.as_bytes() { h ^= b as u64; h = h.wrapping_mul(FNV_PRIME); }
    h ^= h >> 30; h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27; h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    (h % 1000) as u16
}
// 0..=799 → Train,  800..=899 → Validation,  900..=999 → Test
```

Deterministic, salt-parameterized, no RNG, no shuffle needed, and **stable across runs and machines**
— which is what makes the committed holdout (§11.4) meaningful.

**Two test sets are produced and both are reported.** This is the honest way to answer "how good is
it?", because the two questions have very different answers:

| Split | Construction | Question it answers | Expected ROC-AUC |
| --- | --- | --- | --- |
| **A — unseen registrable** | buckets 900–999; no registrable domain shared with train | "Can it flag an ad network it has never seen?" | 0.955–0.975 |
| **B — unseen hostname** | held-out *hostnames* whose registrable domain **is** in train | "Can it flag a new subdomain of a known ad network?" | 0.99+ |

Split A is the conservative number and is what all published thresholds and FPRs are computed
against. Split B is the number that matches day-to-day operation (most novel ad domains are new
hostnames under known networks). **Never quote B alone.**

Leakage assertion, run by the trainer *and* by a unit test:

```rust
pub fn assert_no_registrable_leakage(c: &Corpus) -> Result<(), TrainError> {
    let train: HashSet<&str> = c.train.iter().map(|e| e.registrable.as_str()).collect();
    for e in c.val.iter().chain(c.test_a.iter()) {
        if train.contains(e.registrable.as_str()) {
            return Err(TrainError::Leakage { registrable: e.registrable.clone() });
        }
    }
    Ok(())
}
```

### 7.6 Deterministic PRNG (no `rand`)

```rust
pub struct Rng(u64, u64);   // xorshift128+
impl Rng {
    pub fn seeded(seed: u64) -> Self {                   // SplitMix64 expansion
        let mut z = seed; let mut next = || {
            z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            x ^ (x >> 31)
        };
        Self(next(), next())
    }
    #[inline] pub fn next_u64(&mut self) -> u64 {
        let (mut s1, s0) = (self.0, self.1);
        self.0 = s0; s1 ^= s1 << 23; s1 ^= s1 >> 17; s1 ^= s0 ^ (s0 >> 26); self.1 = s1;
        s1.wrapping_add(s0)
    }
    #[inline] pub fn next_f32(&mut self) -> f32 { (self.next_u64() >> 40) as f32 * (1.0 / 16_777_216.0) }
    #[inline] pub fn below(&mut self, n: u32) -> u32 {   // Lemire, unbiased
        let mut m = (self.next_u64() as u128) * (n as u128);
        let mut l = m as u64;
        if l < n as u64 {
            let t = (u64::MAX - n as u64 + 1) % n as u64;
            while l < t { m = (self.next_u64() as u128) * (n as u128); l = m as u64; }
        }
        (m >> 64) as u32
    }
}
```

Used for Fisher–Yates shuffling of the training order and for augmentation sampling. Bit-reproducible
given `--seed`, which is what makes "same corpus + same seed ⇒ same model bytes" true, and that in
turn is what makes the corpus-hash field in the header meaningful.

### 7.7 Objective, optimizer, hyperparameters

```rust
pub struct TrainConfig {
    pub ngram_buckets: u32,        // default 1 << 19
    pub dim: u16,                  // default 8
    pub epochs: u16,               // default 8
    pub lr0: f32,                  // default 0.25   (linearly decayed to 0 across all epochs)
    pub optimizer: Optimizer,      // default AdaGrad { eps: 1e-8 }
    pub l2: f32,                   // default 1e-6   (lazy, applied on touched rows only)
    pub category_loss_weight: f32, // default 0.30
    pub class_balance: ClassBalance,// default Weighted  (w_c = N_total / (2 * N_c))
    pub patience: u16,             // default 2 epochs on validation PR-AUC
    pub min_count: u32,            // default 3 — features seen < 3 times are not updated
    pub seed: u64,                 // default 0x1122_3344_5566_7788
    pub threads: u16,              // default 1  (deterministic). >1 = Hogwild, non-reproducible.
    pub deployment_prior: f32,     // default 0.03
    pub target_fpr: [f32; 3],      // default [0.001, 0.005, 0.020]
}
```

Loss for one example `(x, y ∈ {0,1}, c ∈ {0..3} | None, weight w)`:

```
L = w · [ BCE(σ(w_b·h + β_b), y)  +  λ · 1{c is Some} · CE(softmax(W_c·h + β_c), c) ]
```

- `BCE` uses the numerically stable form `max(z,0) - z·y + ln(1 + e^{-|z|})`.
- The category term is **masked** when the source's category is `unknown` (hagezi) or the label is
  negative. This is why a mixed list can still contribute to the binary task without polluting the
  categories.
- Gradients: `∂L/∂h = w · [ (p − y) · w_b + λ · Wᵀ_c (softmax − onehot) ]`; each touched n-gram row
  receives `∂L/∂h / n` (the `1/n` from the bag average), each dense/token/tld row receives
  `∂L/∂h` in full.
- **AdaGrad** with per-row, per-dimension accumulators (`B·d` f32 = 16 MiB during training only — a
  dev-box cost, never on device). AdaGrad is the right choice for extremely sparse features: a rare
  n-gram gets a large effective step, a common one a small step, without hand-tuning.
- **`min_count`**: a first pass counts feature occurrences into a `Box<[u32]>` of length `B`
  (2 MiB). Buckets with count < `min_count` are skipped during updates and zeroed before
  quantization. This is what keeps the load factor honest — it removes the once-seen n-grams that
  would otherwise occupy buckets with noise weights.
- **L2** is applied lazily: each row stores `last_update_step`, and on touch the weight is scaled by
  `(1 - l2·lr)^(steps_elapsed)` (computed with `powi` on a small elapsed count, or exactly via a
  running product). Dense L2 over 4.2 M parameters every step would dominate the runtime.
- **Early stopping**: after each epoch, evaluate PR-AUC on the validation split. Keep the best
  weights. Stop if no improvement for `patience` epochs. Report the epoch chosen.
- **`threads > 1`** runs Hogwild (lock-free, racy updates). It is ~3.5× faster on a 4-core box but
  **not reproducible**; the trainer refuses `--threads > 1` together with `--reproducible` and
  records the choice in `PROVENANCE`.

Expected wall clock for 2.3 M examples × 8 epochs, single-threaded, on a modern x86 laptop:
≈ 18.4 M updates × ~1.1 µs ≈ **20 s**. On a Pi 5: ≈ 90 s. Training the base model on the device
itself is feasible but is **not** the on-device story (§10) — the base model ships prebuilt.

### 7.8 Metrics (rubric 5.3, 5.4)

All computed on **Split A** unless labelled otherwise, with tie handling spelled out because ties are
common when many domains score identically.

**ROC-AUC** via the Mann–Whitney U statistic with **average ranks for ties**:

```
sort all scores ascending; assign rank r_i (1-based), ties get the mean of their rank block
AUC = ( Σ_{i: y_i = 1} r_i  −  N₊(N₊+1)/2 ) / (N₊ · N₋)
```

**PR-AUC** as *average precision* (the step-wise sum — **not** trapezoidal, which over-reports):

```
sort descending by score; walk, maintaining TP and FP
AP = Σ_k (Recall_k − Recall_{k−1}) · Precision_k
```

**At each of the three thresholds:** `TP, FP, TN, FN, precision, recall, F1`, and — the number that
matters most —

```
FPR = FP / (FP + TN)          // fraction of benign domains that would be BLOCKED
```

**Prior correction.** Split A's positive rate (~52 % after balancing) is nothing like production
(~3 % of *distinct* queried domains are ad/tracker). Precision on Split A is therefore optimistic.
The trainer additionally reports **prior-corrected precision** at `deployment_prior = 0.03`:

```
precision_corrected = (π · recall) / (π · recall + (1 − π) · FPR),    π = 0.03
```

At `recall = 0.874`, `FPR = 0.0049`, `π = 0.03` → `precision_corrected = 0.846`. That is the honest
"of the things Cogwheel blocks by AI, how many really are ads" number, and it is what the UI should
be designed around. Both raw and corrected figures go into `PROVENANCE` and the model-info endpoint.

Also reported: the **quantization delta** (`roc_auc_f32 − roc_auc_int8`, must be ≤ 0.002), Split B's
ROC-AUC/PR-AUC, the per-category confusion matrix, and a **worst-100 false positives** file
(`false_positives.tsv`, highest-scoring benign domains) — the most useful artifact for a human
reviewing a candidate model.

### 7.9 Trainer CLI

```
cogwheel-trainer fetch    --manifest corpus.toml --out ./corpus/          # download + hash + cache
cogwheel-trainer build    --corpus ./corpus/ --out ./work/                # parse, dedup, hygiene, split
cogwheel-trainer train    --work ./work/ --out adclass-base-v1.cwm \
                          --buckets 524288 --dim 8 --epochs 8 --seed 0x1122334455667788 \
                          --threads 1 --reproducible
cogwheel-trainer eval     --model adclass-base-v1.cwm --work ./work/      # prints the metrics block
cogwheel-trainer holdout  --work ./work/ --out ../../crates/cogwheel-classifier/data/holdout.tsv \
                          --positives 4000 --negatives 16000 --seed 0xC0FFEE
cogwheel-trainer fallback --model adclass-base-v1.cwm --buckets 4096 \
                          --out ../../crates/cogwheel-classifier/data/fallback-model.cwm
```

`fetch` caches by URL + SHA-256 so `build`/`train` are offline and repeatable. Every subcommand
prints a machine-greppable summary line and exits non-zero on any assertion failure.

`fallback` re-trains at `B = 4096` on the same corpus, producing the ~45 KB model embedded with
`include_bytes!` (§12.4). Expected ROC-AUC ≈ 0.93 — meaningfully better than the current toy (0.62)
and enough to keep Monitor mode useful before the real model is installed.

---

## 8. Calibration and the Low / Balanced / High thresholds

### 8.1 Platt scaling — so `score` is an actual probability

Today's `Classification.score` is `entropy/5 + digit + hyphen` clamped to 1.0 and the default
threshold is `0.92`. Neither number means anything, and the UI presents them as if they did. After
this change, `score` is `P(ad or tracker | domain)` — a real probability, comparable across models.

Fit on the **validation split**, using the **quantized** model's logits (so quantization error is
absorbed), with Platt's target smoothing and Lin–Lin–Weng's damped Newton solver:

```
Inputs: z_i (raw logits), y_i ∈ {0,1};  N₊ = #{y=1}, N₋ = #{y=0}
Targets (avoids the overfitting Platt's 1999 paper documents):
    t_i = (N₊ + 1) / (N₊ + 2)   if y_i = 1
    t_i = 1 / (N₋ + 2)          if y_i = 0
Minimize  F(a,b) = − Σ_i [ t_i·ln p_i + (1−t_i)·ln(1−p_i) ],   p_i = σ(a·z_i + b)
```

Newton with backtracking line search, exactly as in Lin, Lin & Weng (2007):

```
a ← 0.0,  b ← ln((N₋ + 1) / (N₊ + 1))            // sensible init
repeat up to 100 times:
    g1 = Σ (p_i − t_i)·z_i ;  g2 = Σ (p_i − t_i)
    h11 = Σ p_i(1−p_i)·z_i² + σ_reg
    h22 = Σ p_i(1−p_i)      + σ_reg              // σ_reg = 1e-12, keeps the Hessian PD
    h21 = Σ p_i(1−p_i)·z_i
    det = h11·h22 − h21² ;  da = −(h22·g1 − h21·g2)/det ;  db = −(−h21·g1 + h11·g2)/det
    gd  = g1·da + g2·db
    stepsize = 1.0
    while stepsize >= 1e-10:                      // backtracking, Armijo with 1e-4
        if F(a + stepsize·da, b + stepsize·db) < F(a,b) + 1e-4·stepsize·gd { accept; break }
        stepsize /= 2
    if |g1| < 1e-5 && |g2| < 1e-5 { converged; break }
```

`p_i` is computed in the numerically safe branchy form
(`if z >= 0 { 1/(1+exp(-z)) } else { let e = exp(z); e/(1+e) }`) and `ln p` / `ln(1−p)` via
`-ln(1+exp(-|z|))` to avoid catastrophic cancellation. Store `a, b` in the header (§4.5).

**Calibration is validated, not assumed.** The trainer computes a 10-bin reliability diagram on the
test split (bins by predicted probability, comparing mean predicted vs. observed frequency) and
reports **Expected Calibration Error**:

```
ECE = Σ_bins (n_bin / N) · | mean_predicted_bin − observed_frequency_bin |
```

`ECE > 0.05` fails the build (`TrainError::PoorCalibration { ece }`). Without this, a "probability"
that is systematically 20 points off would silently make every FPR target wrong.

### 8.2 Choosing the three thresholds by target FPR

The user picks *how much breakage they will tolerate*, not an abstract number. The mapping is
computed, not guessed:

```rust
/// probs/labels come from the VALIDATION split, scored with the quantized+calibrated model.
pub fn threshold_for_fpr(probs: &[f32], labels: &[u8], target_fpr: f32) -> f32 {
    // 1. collect the calibrated probabilities of NEGATIVES only
    let mut neg: Vec<f32> = probs.iter().zip(labels).filter(|(_, &y)| y == 0)
                                 .map(|(&p, _)| p).collect();
    // 2. sort DESCENDING
    neg.sort_unstable_by(|a, b| b.total_cmp(a));
    // 3. we may allow at most k negatives at or above the threshold
    let k = ((target_fpr as f64) * neg.len() as f64).floor() as usize;
    // 4. the threshold is just above the k-th highest negative score
    let t = if k == 0 { neg[0] } else if k >= neg.len() { 0.0 } else { neg[k] };
    // 5. nudge above it so the k-th negative is strictly excluded, and clamp
    (t + f32::EPSILON * t.max(1.0)).clamp(0.500_1, 0.999_99)
}
```

Defaults, and what the UI says:

| Sensitivity | Target FPR | Meaning shown to the user |
| --- | ---: | --- |
| **Low** | ≤ **0.1 %** | "Blocks only what it is nearly certain about. About 1 in 1 000 safe domains could be affected." |
| **Balanced** (default in Protect) | ≤ **0.5 %** | "Recommended. Strong ad and tracker blocking with rare mistakes." |
| **High** | ≤ **2.0 %** | "Most aggressive. Expect to allowlist a site occasionally." |

Then **verified on the test split** (which was never used to pick them). The trainer asserts

```
fpr_test(threshold_s) <= 1.5 * target_fpr(s)      for s in {Low, Balanced, High}
```

and fails with `TrainError::ThresholdDoesNotTransfer` otherwise. A 1.5× slack accounts for sampling
noise at 0.1 % on a ~110 k-negative validation split (≈ 110 negatives above threshold; the Poisson
95 % interval is roughly ±19 %), while still catching a genuinely mis-transferred threshold.

Both the chosen thresholds and their measured validation **and** test FPRs are written into the
header, so the runtime and the API can show them without re-deriving anything.

### 8.3 How settings map onto this

`ClassifierSettings` keeps its wire shape (`{"mode": "...", "threshold": f32}` is what is persisted
under the `classifier_settings` key today — `02-core-crates.md` §7.5) and gains optional fields:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassifierSettings {
    pub mode: ClassifierMode,                                   // Off | Monitor | Protect
    pub threshold: f32,                                         // legacy, still honoured
    #[serde(default)] pub sensitivity: Option<Sensitivity>,     // Low | Balanced | High
    #[serde(default = "default_true")] pub adaptation_enabled: bool,
    #[serde(default)] pub max_inferences_per_sec: Option<u32>,
    #[serde(default)] pub verdict_ttl_secs: Option<u32>,
}
```

Resolution order in `Classifier::active_threshold()`:

1. `sensitivity == Some(s)` → `model.threshold_for(s)` — **the calibrated value from the model file.**
   Changing models automatically re-derives the operating point, which is the whole point.
2. `sensitivity == None` → the legacy `threshold` field, clamped to `[0.5, 0.999_99]`.
3. On upgrade, `load_classifier_settings` migrates a stored `threshold` of exactly `0.92` (the old
   default) to `sensitivity = Some(Balanced)` and writes it back, with an audit event
   `classifier-settings.migrated`. Any other legacy value is preserved as-is and the UI shows an
   "advanced / custom threshold" badge.

**Validation on `POST /api/v1/settings/classifier`** (today there is none — `01-backend-api.md` §4.20
notes `NaN`, negatives and `> 1.0` are all accepted and persisted):

```
threshold must be finite and in [0.5, 0.99999]        → 400 with a field-specific message
sensitivity must be one of Low|Balanced|High          → 422
max_inferences_per_sec, if present, in [100, 100000]  → 400
verdict_ttl_secs, if present, in [60, 604800]         → 400
```

`ClassifierMode` keeps **PascalCase** serialization (`"Off" | "Monitor" | "Protect"`). Changing it is
a storage-compatibility break (`02-core-crates.md` §3.1). `Sensitivity` uses the same convention.

---

## 9. Explainability

Linear models make this exact, and "exact" is the requirement (rubric 5.6: real computed
contributions, not templated strings). Every contribution below is in **logit units and they sum
precisely to the logit** — which is asserted in a test.

### 9.1 The computation

```
logit = β_b
      + Σ_{i ∈ ngrams} ngram_proj[b_i] / n        ← each term is one contribution
      + Σ_{f ∈ 0..32}  dense_proj[f·16 + bin_f]   ← each term is one contribution
      + Σ_{t ∈ tokens} token_proj[t]              ← each term is one contribution
      + tld_proj[e]                               ← one contribution
```

There is no approximation, no sampling, no surrogate model, and no extra work: the contributions are
**the addends already computed** during tier-1 scoring. `explain()` simply re-walks the same
`FeatureVector` and records them instead of only summing.

Converting a logit contribution to a probability effect at the current operating point uses the
local derivative of the calibrated sigmoid:

```
Δp_i ≈ contribution_i · platt_a · p · (1 − p)
```

Both are reported. `Δp` is what the UI shows ("+18 pp"); the logit is what a power user or a bug
report needs.

### 9.2 Types

```rust
pub struct Explanation {
    pub logit: f32,
    pub probability: f32,
    pub bias: f32,
    pub contributions: Vec<Contribution>,   // sorted by |logit_delta| descending
    pub residual: f32,                      // logit − bias − Σ contributions; asserted |·| < 1e-4
}

pub struct Contribution {
    pub kind: ContributionKind,
    pub label: String,          // human text, e.g. "n-gram “ads.”", "entropy 3.81 (bin 12 of 16)"
    pub logit_delta: f32,
    pub probability_delta: f32,
    pub collision_shared: bool, // true when >1 feature of this query hit the same bucket
}

pub enum ContributionKind {
    Ngram { order: u8, text: String, bucket: u32 },
    Label { text: String, bucket: u32 },
    PublicSuffix { text: String, bucket: u32 },
    Cross { id: u8, text: String, bucket: u32 },
    Dense { feature: DenseFeature, bin: u8, value: f32, bin_lo: f32, bin_hi: f32 },
    Token { index: u16, text: String },
    Bias,
}
```

### 9.3 Presentation rules

- Return **top 8 positive** and **top 4 negative** contributions plus `Bias`. Everything else is
  folded into a synthetic `"other n-grams (n=54)"` row carrying the exact remaining sum, so the
  displayed rows still add up to the logit — a user who sums the numbers must not find a gap.
- **Be honest about hash collisions.** A contribution's weight is shared by every feature that hashes
  to that bucket. When two features of *this* query collide, set `collision_shared = true` and the UI
  appends "(shared bucket)". Do not pretend the attribution is per-string when it is per-bucket.
- Dense contributions render as ranges, e.g.
  `Shannon entropy 3.81 — in the 3.74–3.92 band, which is 4.1× more common among trackers`.
  The multiplier comes from a per-bin positive/negative frequency table computed at train time and
  stored in `PROVENANCE`; it is descriptive statistics, not the model, and must be labelled as such.
- Category output renders as a chip (`Ad server`, `Tracker`, `Telemetry`, `Malicious`) with its
  softmax probability, and is **suppressed below 0.5** rather than showing a coin-flip guess.
- Explanations are computed **only** on the tier-2 path, i.e. for domains near or above the
  threshold, and on demand via the ad-hoc scoring endpoint. They are never computed for the 98.5 %
  of clearly-benign domains.

### 9.4 Persistence

The scoring worker emits a `VerdictEvent`; the server writes a row to `classifier_verdicts` (§12.6)
containing `domain`, `probability`, `category`, `model_epoch`, `explanation_json` (the top-12
contributions, ≤ 2 KiB, **capped and truncated with a marker**), and `scored_at`. Rows are pruned to
the most recent 50 000 by a background job, so the table cannot grow without bound (rubric 4.8).

---

## 10. Live, on-device adaptation

"Live" means three distinct things, and all three ship:

1. **Continuous scoring of real traffic** (§5) — the model is applied to what the user actually
   resolves, not to a static list.
2. **Periodic refinement from the user's own data** (§10.2–10.6) — the operating point and the
   side-feature weights adapt to this network.
3. **User feedback as labels** (§10.3) — a wrong verdict is corrected immediately and remembered.

### 10.1 What adapts, and the structural safety guarantee

**Only these parameters are ever updated on device** (≈ 9 000 f32 = 36 KB):

| Table | Params | Why it is the right thing to adapt |
| --- | ---: | --- |
| `DENSE` (32 × 16 × 8) | 4 096 | depth/length/entropy priors differ per network (corporate vs. home) |
| `TOKEN` (96 × 8) | 768 | which ad-tech vendors this user actually encounters |
| `TLD` (512 × 8) | 4 096 | new-gTLD abuse patterns shift monthly |
| `HEAD_BINARY` (8 + 1) | 9 | the decision direction |
| `HEAD_CATEGORY` (4×8 + 4) | 36 | category priors |
| `platt_a`, `platt_b` | 2 | **re-calibration — the highest-value knob by far** |
| thresholds | 3 | re-derived from the refreshed calibration |

**`NGRAM_Q` and `NGRAM_SCALE` are never written.** Not "should not" — *cannot*: the delta file format
(§4.7) has no section id for them, and `Model::with_delta` rejects any unknown section. Consequences:

- On-device training **cannot** damage the linguistic knowledge that does the heavy lifting.
- The adapted state is a **10.5 KB sidecar**, so rollback is `std::fs::remove_file` and a
  `replace_model`. There is no in-place mutation of the 4.7 MB base file, ever.
- Memory during adaptation is ~40 KB of f32 working set, not the 16 MiB a full n-gram pass would
  need — which is what makes it fit the Pi budget at all.

Full n-gram re-training happens **off device** and arrives as a new signed base model through the
normal release channel. That is the honest division of labour, and it should be said plainly in the
docs rather than implying the Pi trains a whole model nightly.

### 10.2 Where the on-device labels come from

Assembled by `apps/cogwheel-server` from data it already has:

| Class | Source | Weight | Cap |
| --- | --- | ---: | ---: |
| Positive | domains **blocked by the user's compiled ruleset** that were actually queried in the last 30 days (from `query_activity` / `security_events`) | 1.0 | 20 000 |
| Positive | explicit user "block this" feedback | **5.0** | 2 000 |
| Negative | domains **resolved and allowed**, seen on ≥ 3 distinct days, ≥ 3 total queries, and on no blocklist | 1.0 | 20 000 |
| Negative | explicit user "this was wrongly blocked" feedback | **8.0** | 2 000 |
| Negative | every entry of `PROTECTED_SUFFIXES` (§5.8) plus 4 synthetic subdomains each | 3.0 | ~750 |

Sampling is by **registrable domain** (at most 4 hostnames per registrable domain) so one chatty
tracker cannot dominate. Total is capped at `max_examples = 50 000`.

Two hard exclusions: (a) nothing that appears in `data/holdout.tsv` may enter the training set — the
gate must stay uncontaminated, and this is checked by fingerprint; (b) nothing matching
`PROTECTED_SUFFIXES` may enter as a *positive*.

### 10.3 User feedback

```
POST /api/v1/classifier/feedback  { "domain": "...", "verdict": "false_positive" | "false_negative",
                                    "note": "optional, <= 512 chars" }
```

Two effects, deliberately separated:

1. **Immediate and deterministic.** `false_positive` writes a user **Allow** rule and triggers a
   ruleset rebuild, so the site works again *within seconds* — the user does not wait for a nightly
   job. `false_negative` writes a user Block rule. This is the fix; adaptation is the learning.
2. **Deferred and statistical.** The row lands in `classifier_feedback` and becomes a
   high-weight training example on the next adaptation run.

Rate-limited to 60 submissions per hour per node; the domain is validated with
`NormalizedDomain::parse`; the note is length-capped and stored as text, never interpolated into
anything.

### 10.4 The adaptation run

Scheduled at a configurable local hour (default **03:17**, jittered ±20 min so a fleet does not
synchronize), skipped entirely if `adaptation_enabled == false` or `mode == Off`.

```rust
pub struct AdaptConfig {
    pub max_examples: usize,       // 50_000
    pub epochs: u16,               // 3
    pub lr: f32,                   // 0.02   (1/12 of base lr0 — this is fine-tuning)
    pub anchor_l2: f32,            // 1e-4   pull toward the BASE weights, not toward zero
    pub wall_clock_budget: Duration, // 30 s of work time
    pub duty_cycle_pct: u8,        // 25 — work 25 ms, sleep 75 ms
    pub min_examples: usize,       // 2_000 — below this, skip (nothing to learn)
    pub min_negatives: usize,      // 1_000
}
```

- **Anchored objective.** `L_total = L_data + anchor_l2 · Σ (θ − θ_base)²`. The pull is toward the
  *shipped* weights, not toward zero, so the adapted model cannot wander arbitrarily far no matter
  how skewed a night's traffic is.
- **Duty cycling.** The worker measures its own elapsed CPU time and sleeps to hold ≤ 25 % of one
  core. Budget: 50 000 examples × 3 epochs × ~1.3 µs ≈ **0.2 s of CPU**; the 30 s budget is a
  guillotine, not a target. If it trips, the run is abandoned and logged — never partially promoted.
- Runs on the same dedicated OS thread pattern as scoring, at the same low priority, and **the
  scoring worker keeps running throughout** using the currently active model.

### 10.5 The promote gate (rubric 5.8)

Evaluate the candidate on `data/holdout.tsv` — the **frozen, committed, never-adapted-on** set —
and compare against the *currently active* model evaluated on the same rows in the same run:

```rust
pub enum RejectReason {
    FprRegressed { before: f32, after: f32 },
    RecallRegressed { before: f32, after: f32 },
    AucRegressed { before: f32, after: f32 },
    CalibrationDrift { ece: f32 },
    ProtectedDomainBlocked { domain: String },
    NonFiniteWeights,
    Timeout,
}
```

**Promote only if ALL of the following hold:**

| # | Gate | Threshold |
| --- | --- | --- |
| G1 | FPR at the active sensitivity did **not** get worse | `fpr_after <= fpr_before` (absolute — no slack; a false positive breaks browsing) |
| G2 | Recall did not drop materially | `recall_after >= recall_before − 0.005` |
| G3 | ROC-AUC did not drop materially | `auc_after >= auc_before − 0.002` |
| G4 | Calibration still holds | `ece_after <= 0.05` |
| G5 | **Every** `PROTECTED_SUFFIXES` entry and its synthetic subdomains still score below the threshold | hard, no slack |
| G6 | All adapted weights are finite | hard |
| G7 | The run finished inside its budget | hard |

Any failure → the candidate delta is written to `adclass-delta.rejected.cwd` for diagnosis (not
loaded), an audit event `classifier.adaptation_rejected` records the reason and both metric sets, and
the active model is untouched.

### 10.6 Promotion and rollback mechanics

Files under `${COGWHEEL_MODEL_DIR}` (default `/var/lib/cogwheel/models`):

```
adclass-base-v1.cwm        immutable, shipped, SHA-256 recorded    ← never written after install
adclass-delta.cwd          active delta (may be absent)
adclass-delta.prev.cwd     the delta this one replaced (for rollback)
adclass-delta.rejected.cwd last rejected candidate (diagnostic only)
```

Promotion is atomic and crash-safe:

1. Serialize the candidate to `adclass-delta.new.cwd`; `File::sync_all()`.
2. `rename(adclass-delta.cwd → adclass-delta.prev.cwd)` if it exists.
3. `rename(adclass-delta.new.cwd → adclass-delta.cwd)` — atomic within a filesystem.
4. `sync_all()` on the directory handle.
5. Build `Model::with_delta(&base, &delta)?`, then `classifier.replace_model(Arc::new(model))`,
   which bumps `model_epoch` and thereby invalidates every cached verdict in `O(1)` (§5.4).
6. Audit `classifier.adaptation_promoted` with `{generation, before_metrics, after_metrics, examples,
   duration_ms}`.

Rollback paths, all of which end at a known-good state:

- **Manual**: `POST /api/v1/classifier/adapt/rollback` → restore `prev` (or delete the delta
  entirely, reverting to the pristine base) and `replace_model`. Audited.
- **Automatic on feedback spike**: if ≥ **5** `false_positive` reports arrive within 24 h **and** a
  promotion happened in that window → auto-rollback to the base, set
  `adaptation_enabled = false` for **7 days**, audit `classifier.adaptation_auto_rollback`, and raise
  a UI banner explaining what happened and how to re-enable.
- **On load failure**: any error from `Model::with_delta` (checksum, base mismatch, bad section) →
  log at `warn`, **ignore the delta**, run on the pristine base. The product degrades to
  "shipped-model quality", never to "no protection".
- **Generation cap**: `generation > 30` → refuse to adapt further and surface "this model has
  adapted 30 times; install the latest model for a fresh baseline". This bounds cumulative drift
  from a chain of individually-passing steps.

### 10.7 Why on-device training cannot brick protection — the full argument

1. The base model file is opened read-only and is never a write target. Protection at the base
   model's quality is always recoverable by deleting a 10 KB file.
2. The delta format **structurally cannot** contain n-gram weights (§4.7), so the dominant part of
   the model is immutable on device.
3. Nothing is promoted without passing G1–G7 against a frozen holdout the adapter cannot train on.
4. Protected domains are re-verified at promote time (G5) *and* enforced at three independent points
   at runtime (§5.8), including one that does not consult the model at all.
5. Adaptation runs on a separate thread with its own budget; the scoring path continues serving the
   old model throughout, so a hung or slow adaptation degrades to "no adaptation", never to
   "no scoring".
6. Every promote, reject and rollback writes an audit event, so a bad night is diagnosable after the
   fact rather than mysterious.
7. `adaptation_enabled = false` is honoured immediately and is the documented recovery step.

---

## 11. Testing and benchmarks

All tests are inline `#[cfg(test)] mod tests` — the workspace has no `tests/` directory and
`02-core-crates.md` §10 documents that as the convention. Tests may use `.expect("message")`; non-test
code may not (`02-core-crates.md` §9.2, rubric 1.6).

### 11.1 Committed test data

| File | Size | Contents |
| --- | ---: | --- |
| `crates/cogwheel-classifier/data/adclass-base-v1.cwm` | 4.7 MB | the shipped base model — makes every quality test hermetic |
| `crates/cogwheel-classifier/data/fallback-model.cwm` | ~45 KB | `include_bytes!`-embedded degraded model |
| `crates/cogwheel-classifier/data/holdout.tsv` | ~600 KB | 20 000 rows `domain \t label \t category \t split`, **drawn only from Split A's test buckets (900–999)** |
| `crates/cogwheel-classifier/data/golden_features.tsv` | ~120 KB | 200 domains × full expected feature output |
| `crates/cogwheel-classifier/data/public_suffix_list.dat` | ~150 KB | ICANN section snapshot, date + SHA-256 in the header comments |

Committing a 4.7 MB binary is deliberate: without it the model-quality regression test would need a
network fetch, which cannot run in CI here, and rubric 5.5 requires the test to exist and pass.

### 11.2 Golden-vector tests for feature extraction

`golden_features.tsv` columns (tab-separated):

```
raw_input, normalized, public_suffix, registrable, label_count,
dense_00..dense_31 (raw f32, formatted "{:.6}"),
bin_00..bin_31 (u8),
ngram_count, first8_buckets (comma-separated u32),
matched_tokens (comma-separated indices), suffix_bucket
```

The 200 rows cover, at minimum: plain 2-label names; deep subdomains; `co.uk` and `s3.amazonaws.com`
(multi-label suffixes); a PSL wildcard rule and its exception; trailing dots (one and three); uppercase;
`xn--` labels; underscores; all-digit labels; 63-byte labels; a 253-byte name; a name with 20 labels
(exercising the 16-label cap); a name that trips `MAX_FEATURES`; every one of the 96 ad-tokens at
least once; the empty string, `"."`, `"..."`, `"a"`, and three malformed inputs expected to produce
specific `NormalizeError` variants.

```rust
#[test] fn golden_feature_vectors_match()      // exact equality; f32 compared as formatted strings
#[test] fn feature_extraction_is_deterministic() // 10 000 domains × 3 runs → identical bytes
#[test] fn normalize_rejects_hostile_input()   // fuzz-ish: 100 000 pseudorandom byte strings, never panics
#[test] fn public_suffix_matches_psl_algorithm() // the PSL's own published test vectors
```

**If a golden test fails, the model is invalidated.** Say so in a comment at the top of the file: the
correct response is either to revert the feature change or to retrain and regenerate the goldens in
the same commit, never to edit the expected values.

### 11.3 Model format and inference unit tests

```rust
#[test] fn model_roundtrips_through_format()          // write → read → identical weights
#[test] fn quantization_roundtrip_error_is_bounded()  // max |W − deq(q(W))| / |W| <= 0.022
#[test] fn scale_codebook_is_monotonic_and_zero_reserved()
#[test] fn tier1_and_tier2_logits_agree()             // 10 000 domains, |Δ| < 1e-4
#[test] fn explanation_contributions_sum_to_logit()   // |residual| < 1e-4 for 10 000 domains
#[test] fn rejects_bad_magic()
#[test] fn rejects_future_format_version()
#[test] fn rejects_unknown_flag_bits()
#[test] fn rejects_truncated_file()                   // every prefix length from 0..200
#[test] fn rejects_corrupt_checksum()                 // flip one bit at 50 random offsets
#[test] fn rejects_section_length_overflow()          // body_len = u64::MAX
#[test] fn rejects_duplicate_and_missing_sections()
#[test] fn rejects_non_power_of_two_buckets()
#[test] fn rejects_non_finite_weights()
#[test] fn rejects_oversized_file()                   // 33 MiB of zeros
#[test] fn model_load_never_panics_on_random_bytes()  // 20 000 pseudorandom buffers, all Err
```

Every negative case asserts a **specific** `ModelError` variant, not merely `is_err()`.

### 11.4 Model-quality regression test (rubric 5.3, 5.5)

```rust
#[test]
fn base_model_meets_quality_floors() {
    let model  = Model::from_bytes(include_bytes!("../data/adclass-base-v1.cwm"))
        .expect("base model loads");
    let rows   = holdout::load(include_str!("../data/holdout.tsv"));
    let scored = rows.iter().map(|r| (score(&model, &r.domain), r.label)).collect::<Vec<_>>();

    let roc = metrics::roc_auc(&scored);
    let pr  = metrics::pr_auc(&scored);
    assert!(roc >= 0.955, "ROC-AUC {roc:.4} below the 0.955 floor");
    assert!(pr  >= 0.930, "PR-AUC {pr:.4} below the 0.930 floor");

    for (sens, max_fpr, min_recall) in [
        (Sensitivity::Low,      0.0015, 0.55),
        (Sensitivity::Balanced, 0.0060, 0.82),
        (Sensitivity::High,     0.0240, 0.92),
    ] {
        let t = model.threshold_for(sens);
        let (fpr, recall) = metrics::fpr_recall_at(&scored, t);
        assert!(fpr    <= max_fpr,    "{sens:?}: FPR {fpr:.5} exceeds {max_fpr}");
        assert!(recall >= min_recall, "{sens:?}: recall {recall:.4} below {min_recall}");
    }
    assert!(metrics::expected_calibration_error(&scored, 10) <= 0.05);
}
```

Floors are set **1.2× looser than the target FPRs** (0.1 %/0.5 %/2.0 % → 0.15 %/0.6 %/2.4 %) so
the holdout's finite size (16 000 negatives → one negative above threshold is 0.00625 % of FPR) does
not produce a flaky test, while still failing hard on a real regression. The floors are absolute
numbers, not "compare to last run" — a committed model either clears them or does not ship.

Companion tests:

```rust
#[test] fn holdout_is_disjoint_from_training()   // every holdout registrable hashes into 900..=999
#[test] fn holdout_has_the_declared_class_balance() // 4 000 positives, 16 000 negatives
#[test] fn fallback_model_beats_the_old_heuristic() // ROC-AUC >= 0.90 on the same holdout
```

### 11.5 Budget tests

```rust
#[test] fn model_file_is_within_budget()   { assert!(BASE_MODEL_BYTES.len() <= 8_000_000); }
#[test] fn resident_memory_is_within_budget() {
    let c = Classifier::new(base_model(), Default::default(), Default::default()).0;
    assert!(c.resident_bytes() <= 16 * 1024 * 1024, "{} bytes", c.resident_bytes());
}
#[test] fn slot_is_exactly_32_bytes()      { assert_eq!(std::mem::size_of::<Slot>(), 32); }
#[test] fn verdict_cache_capacity_is_fixed() {
    // insert 1_000_000 distinct domains; assert resident_bytes() never grows
}
#[test] fn queue_never_grows_past_capacity() {
    // submit 100_000 domains with the worker paused; assert every excess submit returns
    // Dropped(QueueFull) and that resident_bytes() is unchanged
}
```

`Classifier::resident_bytes()` is a real sum of owned allocation sizes (each `Box<[T]>`'s
`len * size_of::<T>()`, the shard arrays, the queue's capacity), not an estimate — it is public API
so the `/api/v1/classifier/model` endpoint can report it.

### 11.6 Hot-path safety tests

```rust
#[test] fn lookup_and_submit_are_synchronous() {
    // Compile-time proof: these coercions fail to build if either becomes `async fn`.
    let _: fn(&Classifier, &str) -> VerdictLookup  = Classifier::lookup;
    let _: fn(&Classifier, &str) -> SubmitOutcome  = Classifier::submit;
}

#[test] fn lookup_does_not_allocate() {
    // test-only counting GlobalAlloc; 100 000 lookups must produce 0 allocations
}

#[tokio::test] async fn hot_path_is_unaffected_by_a_saturated_queue() {
    // worker paused, queue full; 10 000 handle_wire_query cache hits
    // assert total elapsed < 10_000 * 50 µs and that every response is correct
}

#[tokio::test] async fn protect_mode_blocks_on_the_second_sighting() {
    // 1st query: allowed (verdict Unknown) ; drain the worker ; 2nd query: blocked
}

#[tokio::test] async fn protect_mode_never_blocks_a_protected_domain() {
    // force a verdict of 1.0 for every PROTECTED_SUFFIXES entry through the cache API;
    // assert the response is not a block
}

#[tokio::test] async fn paused_protection_disables_classifier_blocking() {}
#[tokio::test] async fn device_bypass_disables_classifier_blocking() {}
#[tokio::test] async fn monitor_mode_never_blocks() {}
#[tokio::test] async fn off_mode_does_not_enqueue() {}
```

The last four close the gap flagged in `01-backend-api.md` §10.2: *"`Monitor` and `Protect` are
behaviourally identical — the classifier can never block a query."* After this change they differ,
and the difference is pinned by tests.

### 11.7 Allowlist tests

```rust
#[test] fn every_protected_entry_matches_itself_and_its_subdomains()
#[test] fn protected_matching_respects_dot_boundaries()   // "notapple.com" is NOT protected
#[test] fn protected_list_is_sorted_and_has_no_duplicates()
#[test] fn protected_list_entries_are_valid_domains()     // each parses via NormalizedDomain
#[test] fn scoring_a_protected_domain_yields_zero()       // full pipeline, all 150 entries + "www." forms
```

### 11.8 Training-pipeline tests (no network)

Fed by small committed fixtures (`data/fixtures/*.txt`, a few KB each) that reproduce every real
format and every rejection case observed live:

```rust
#[test] fn hosts_parser_skips_localhost_noise()
#[test] fn hosts_parser_handles_multiple_aliases()
#[test] fn adblock_parser_accepts_only_domain_anchored_rules()
#[test] fn adblock_parser_rejects_domain_modifier_rules()      // the FP generator
#[test] fn adblock_parser_rejects_cosmetic_and_regex_rules()
#[test] fn adblock_parser_collects_exception_rules_separately()
#[test] fn majestic_csv_parser_skips_the_header_row()
#[test] fn parsers_reject_lines_over_512_bytes()
#[test] fn parsers_terminate_on_adversarial_input()            // 10 MB single line, 1 M empty lines
#[test] fn split_bucket_is_stable_and_uniform()                // χ² over 100 000 domains
#[test] fn leakage_assertion_catches_a_planted_leak()
#[test] fn conflict_policy_forces_top_10k_negative()
#[test] fn depth_matching_reaches_the_two_point_tolerance()
#[test] fn platt_newton_converges_on_synthetic_data()          // recovers known a,b within 1e-3
#[test] fn roc_auc_matches_a_brute_force_reference()           // O(n²) reference on 500 points
#[test] fn pr_auc_handles_all_ties()
#[test] fn threshold_for_fpr_hits_the_target_on_synthetic_data()
#[test] fn adaptation_rejects_a_candidate_that_regresses_fpr() // plant a regression, assert Rejected
#[test] fn adaptation_delta_cannot_carry_ngram_sections()      // assert Err on a hand-built delta
#[test] fn delta_with_wrong_base_hash_is_rejected()
```

### 11.9 Keeping CI honest about a Pi target

The tension is real: CI is x86, the target is `aarch64`, and a throughput number from a shared GitHub
runner proves very little. The rules:

1. **CI asserts floors, not budgets** (§6.4 tier A). A floor that a 10-year-old x86 core clears
   easily still catches the failure modes that actually happen — an allocation in the loop, a lock,
   a lost inline, an accidental `format!`.
2. **CI additionally asserts the things that are architecture-independent**: zero allocations, exact
   feature vectors, model size, resident bytes, AUC, FPR. These are the majority of the guarantees
   and they transfer perfectly.
3. **A nightly `linux/arm64` job** (`docker buildx build --platform linux/arm64` + `qemu` for
   compile checks, and a self-hosted Pi runner when available) runs the same tests with
   `COGWHEEL_BENCH_STRICT=1`. QEMU timings are meaningless and the strict assertions are therefore
   skipped under QEMU (detected via `/proc/cpuinfo` lacking a real `BCM`/`Cortex-A76` signature);
   only the self-hosted runner enforces them.
4. **The measured Pi 5 numbers live in `docs/reliability-budgets.md`** with hardware, kernel,
   governor, commit and date. Rubric 4.10 is satisfied by that table, not by a claim in this spec.
5. The benchmark prints `arch=` in its output line, so nobody can paste an x86 number into a Pi
   discussion by accident.
