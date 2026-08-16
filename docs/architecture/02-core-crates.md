# 02 — Core Rust Crates: Internals Reference

Status: descriptive (what the code does **today**, 2026-08-16), not aspirational.
Scope: the eight workspace library crates under `/home/user/Cogwheel-DNS/crates/`, plus the workspace
manifest, lint config, and the guardrail docs that constrain changes to them.

Every file path in this document is absolute. Every signature is copied verbatim from the source.
Line numbers are accurate as of this writing; treat them as navigation hints, not contracts.

Implementers: read §1 (dependency graph), §9 (lints / error-handling), and §11 (hazards) before you
touch anything. §6.2 is the authoritative description of the DNS hot path.

---

## 1. Workspace map and dependency edges

### 1.1 Members

`/home/user/Cogwheel-DNS/Cargo.toml`:

```toml
[workspace]
members = [
  "apps/cogwheel-desktop",
  "apps/cogwheel-server",
  "crates/cogwheel-api",
  "crates/cogwheel-classifier",
  "crates/cogwheel-dns-core",
  "crates/cogwheel-lists",
  "crates/cogwheel-policy",
  "crates/cogwheel-services",
  "crates/cogwheel-storage",
  "crates/cogwheel-sync",
]
resolver = "3"

[workspace.package]
edition = "2024"
license = "MIT"
version = "0.1.0"
authors = ["Tachyon Labs"]
repository = "https://github.com/tachyonlabshq/Cogwheel-DNS"
```

`[profile.release]`: `codegen-units = 1`, `lto = "thin"`, `strip = true`.

`/home/user/Cogwheel-DNS/clippy.toml` contains exactly one line: `msrv = "1.85.0"`.
(Note the tension: `edition = "2024"` requires Rust 1.85+, so MSRV and edition agree, but nothing
in CI pins the toolchain — CI uses `dtolnay/rust-toolchain@stable`.)

`/home/user/Cogwheel-DNS/deny.toml`:

```toml
[licenses]
allow = ["0BSD", "Apache-2.0", "BSD-3-Clause", "CDLA-Permissive-2.0", "ISC", "MIT", "Unicode-3.0", "Zlib"]

[advisories]
ignore = []
```

Any new dependency whose license is outside that allow-list breaks `cargo deny check` in CI.
Notably **absent**: `MPL-2.0`, `BSD-2-Clause`, `Apache-2.0 WITH LLVM-exception`, `CC0-1.0`. If you
add an ML runtime crate (ort/tract/candle/ndarray et al.), verify its license graph against this list
first and extend the allow-list in the same PR.

### 1.2 Path-dependency graph (enforced by a test)

```
cogwheel-dns-core  ──► cogwheel-classifier
                   └─► cogwheel-policy

cogwheel-lists     ──► cogwheel-policy
                   └─► cogwheel-services      (declared, but NOT used in code — see §5.6)

cogwheel-services  ──► cogwheel-policy

cogwheel-storage   ──► cogwheel-policy

cogwheel-classifier ──► (none)
cogwheel-sync       ──► (none)
cogwheel-api        ──► (none)

apps/cogwheel-server ──► all eight crates (composition root)
apps/cogwheel-desktop ──► (no dependencies at all; empty [dependencies])
```

This graph is asserted by a **unit test living in the API crate**:
`/home/user/Cogwheel-DNS/crates/cogwheel-api/src/lib.rs:365` —
`fn crate_path_dependencies_match_the_adr_boundaries()`.

It reads each crate manifest as text, extracts every line containing `path =`, takes the token left
of the first `=`, sorts, and compares to a hardcoded expected list (lines 372–392). Consequences:

- Adding **any** new `path = "../…"` dependency to a listed crate fails `cargo test --workspace`
  with the message `"<manifest> drifted from ADR 0001 crate boundaries; update the ADR first if this coupling is intentional"`.
- To legitimately add an edge you must edit *three* places in one commit:
  `/home/user/Cogwheel-DNS/docs/adr/0001-crate-boundaries.md`, the `expected` array at
  `crates/cogwheel-api/src/lib.rs:372`, and the target crate's `Cargo.toml`.
- The matcher is purely textual. A dependency declared as a multi-line table
  (`[dependencies.cogwheel-foo]` / `path = "..."` on its own line) yields a garbage token (`path`)
  and will also fail. Keep single-line `foo = { path = "../foo" }` form.

Related doc: `/home/user/Cogwheel-DNS/docs/crate-boundary-guardrails.md` (restates the same graph and
says the ADR must be updated first).

### 1.3 Workspace dependency versions (`[workspace.dependencies]`)

Use `foo.workspace = true` in crate manifests; do not pin versions per crate.

| Key | Spec |
| --- | --- |
| anyhow | `1.0` |
| async-trait | `0.1` |
| axum | `0.8`, features `["macros"]` |
| base64 | `0.22` |
| bytes | `1.10` |
| chrono | `0.4`, features `["serde"]` |
| config | `0.15`, `default-features = false`, features `["toml"]` |
| futures | `0.3` |
| http | `1.3` |
| hickory-proto | `0.25` |
| hickory-resolver | `0.25` |
| humantime-serde | `1.1` |
| ipnet | `2.11`, features `["serde"]` |
| moka | `0.12`, features `["future"]` (lockfile resolves 0.12.14) |
| prometheus-client | `0.24` |
| reqwest | `0.12`, `default-features = false`, features `["charset","gzip","http2","json","rustls-tls"]` |
| serde | `1.0`, features `["derive"]` |
| serde_json | `1.0` |
| rusqlite | `0.37`, features `["bundled"]` |
| sha2 | `0.10` |
| thiserror | `2.0` |
| tokio | `1.45`, features `["full"]` |
| tokio-stream | `0.1` |
| toml | `0.8` |
| tower | `0.5` |
| tower-http | `0.6`, features `["fs","trace"]` |
| tracing | `0.1` |
| tracing-subscriber | `0.3`, features `["env-filter","fmt","json"]` |
| url | `2.5`, features `["serde"]` |
| uuid | `1.16`, features `["serde","v4"]` |

Non-workspace (crate-local) deps that exist today: `cogwheel-storage` pins
`ed25519-dalek = { version = "2.2.0", features = ["rand_core"] }`, `rand = { version = "0.8", features = ["std","std_rng"] }`,
`getrandom = "0.4.2"`.

### 1.4 CI (`/home/user/Cogwheel-DNS/.github/workflows/ci.yml`)

Single `rust` job on `ubuntu-latest`, triggered on push to `**` and on PRs:

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
3. `cargo test --workspace`
4. `cargo audit`
5. `cargo deny check`

There is **no** web/JS job in this workflow.

---

## 2. `cogwheel-policy`

Path: `/home/user/Cogwheel-DNS/crates/cogwheel-policy/src/lib.rs` (193 lines).
Deps: `chrono`, `serde`, `sha2`, `thiserror`, `tracing`, `uuid`. No path deps.
This is the leaf domain crate — everything on the hot path bottoms out here.

### 2.1 Public types

```rust
pub enum BlockMode {
    NullIp,
    NxDomain,
    NoData,
    Refused,
    CustomIp { ipv4: Option<Ipv4Addr>, ipv6: Option<Ipv6Addr> },
}                        // Debug, Clone, Serialize, Deserialize, PartialEq, Eq

pub enum RulePattern { Exact(String), Suffix(String) }   // Debug, Clone, Ser, De, PartialEq, Eq

pub enum RuleAction { Allow, Block }                     // Debug, Clone, Ser, De, PartialEq, Eq

pub struct Rule {
    pub pattern: RulePattern,
    pub action: RuleAction,
    pub source: String,
    pub comment: Option<String>,
}                        // Debug, Clone, Ser, De, PartialEq, Eq

pub enum DecisionKind { Allowed, Blocked(BlockMode) }    // Debug, Clone, Ser, De, PartialEq, Eq

pub struct Decision {
    pub domain: String,          // normalized
    pub kind: DecisionKind,
    pub matched_rule: Option<Rule>,   // cloned, not borrowed
    pub reason: String,               // free-text, see §2.3
}                        // Debug, Clone, Ser, De, PartialEq, Eq

pub struct RulesetArtifact {
    pub id: Uuid,
    pub hash: String,                        // lowercase hex SHA-256
    pub created_at: DateTime<Utc>,
    pub rules: Vec<Rule>,
    pub protected_domains: HashSet<String>,
    pub block_mode: BlockMode,
}                        // Debug, Clone, Serialize, Deserialize  (NOT PartialEq)

pub struct PolicyEngine { /* private: artifact: RulesetArtifact */ }   // Debug, Clone
```

### 2.2 Public functions

```rust
impl RulesetArtifact {
    pub fn new(rules: Vec<Rule>, protected_domains: HashSet<String>, block_mode: BlockMode) -> Self;
}

impl PolicyEngine {
    pub fn new(artifact: RulesetArtifact) -> Self;
    pub fn artifact(&self) -> &RulesetArtifact;
    pub fn evaluate(&self, domain: &str) -> Decision;
}

pub fn normalize_domain(domain: &str) -> String;   // trim(), trim_end_matches('.'), to_ascii_lowercase()
```

Private: `fn find_rule(&self, domain: &str, action: RuleAction) -> Option<&Rule>` (line 145).

### 2.3 Evaluation semantics / invariants

`PolicyEngine::evaluate` (line 107) is a pure function, no I/O, no interior mutability. Order:

1. `let normalized = normalize_domain(domain);` — always allocates a `String`.
2. If `artifact.protected_domains` **contains the exact normalized string** →
   `Allowed`, `matched_rule: None`, `reason: "protected domain"`.
   Protection is exact-match only; a protected `example.com` does **not** protect `www.example.com`.
3. First rule with `action == Allow` that matches → `Allowed`, `reason: "matched allow rule"`.
4. First rule with `action == Block` that matches → `Blocked(artifact.block_mode.clone())`,
   `reason: "matched block rule"`.
5. Otherwise → `Allowed`, `matched_rule: None`, `reason: "no matching rule"`.

Matching (`find_rule`, line 145):
- `RulePattern::Exact(c)` matches iff `c == domain`.
- `RulePattern::Suffix(c)` matches iff `domain == c || domain.ends_with(&format!(".{c}"))`.

Invariants that callers rely on:
- **Allow always beats Block**, regardless of ordering inside `rules` (two independent passes).
  Test `allow_precedes_block` at line 167 pins this.
- The `BlockMode` returned is always the artifact-wide `block_mode`; there is no per-rule block mode.
- `Decision.reason` strings are matched literally by the test suite. Treat
  `"protected domain"`, `"matched allow rule"`, `"matched block rule"`, `"no matching rule"` as a
  stable enum-in-a-string; if you change them, grep the workspace first.

### 2.4 Performance characteristics (this is the hot path's inner loop)

- `evaluate` is **O(n) over all rules, twice** (once for Allow, once for Block). There is no index,
  no hash map, no trie. A 500k-entry blocklist means up to 1M rule comparisons per cache miss.
- `find_rule` calls `format!(".{candidate}")` **per suffix rule, per evaluation** — one heap
  allocation per rule per query. This directly contradicts the "per-query heap allocations on the
  deterministic path should stay bounded and minimized" line in
  `/home/user/Cogwheel-DNS/docs/reliability-budgets.md`.
- `normalize_domain` allocates again inside `evaluate`, even though `handle_wire_query` has already
  lowercased and dot-trimmed the domain before calling it (see §6.2 step 2).

If you optimize the policy engine, the safe shape is: keep `evaluate`'s signature and `Decision`
semantics identical, and change only `PolicyEngine`'s private representation (build indices in
`PolicyEngine::new`). Nothing outside the crate reads `PolicyEngine`'s fields — only
`artifact()`, `evaluate()`, and `new()`.

### 2.5 Artifact hashing — non-determinism hazard

`RulesetArtifact::new` (line 65) computes:

```rust
for rule in &rules { hasher.update(format!("{:?}:{:?}:{}", rule.action, rule.pattern, rule.source)); }
for domain in &protected_domains { hasher.update(domain.as_bytes()); }
hasher.update(format!("{:?}", block_mode));
```

- The hash depends on `Debug` formatting of `RuleAction`/`RulePattern`/`BlockMode`. Renaming a
  variant or adding a field silently changes every hash.
- `protected_domains` is a `HashSet<String>`; **iteration order is randomized per process**
  (`RandomState`). Therefore **`artifact.hash` is not reproducible across runs whenever
  `protected_domains` is non-empty.** The hash is used as the DNS cache scope key
  (`cogwheel-dns-core`, §6.4) and stored in the `rulesets` table, so this is a real correctness
  smell, not a theoretical one. Fix by sorting protected domains before hashing if you touch this.
- `new()` also mints a fresh `Uuid::new_v4()` and `Utc::now()` on every call — two artifacts built
  from identical inputs are never `==` by id/created_at.

### 2.6 Tests present (1)

`crates/cogwheel-policy/src/lib.rs:167` — `allow_precedes_block`.

---

## 3. `cogwheel-classifier`

Path: `/home/user/Cogwheel-DNS/crates/cogwheel-classifier/src/lib.rs` (99 lines).
Deps: `chrono`, `serde` **only**. No path deps, no HTTP, no ML runtime, no `std::fs`.
This crate is one of the two "hot-path crates" guarded by the manifest regression test (§6.6).

### 3.1 Public types

```rust
pub struct LexicalFeatures {
    pub length: usize,
    pub digit_ratio: f32,
    pub hyphen_ratio: f32,
    pub label_depth: usize,
    pub entropy: f32,
}                            // Debug, Clone, Serialize, Deserialize, PartialEq

pub enum ClassifierMode { Off, Monitor, Protect }   // Debug, Clone, Ser, De, PartialEq

pub struct ClassifierSettings { pub mode: ClassifierMode, pub threshold: f32 }
// Debug, Clone, Ser, De, PartialEq
// Default: { mode: ClassifierMode::Monitor, threshold: 0.92 }

pub struct Classification {
    pub score: f32,
    pub reasons: Vec<String>,
    pub observed_at: DateTime<Utc>,
}                            // Debug, Clone, Ser, De, PartialEq
```

`ClassifierSettings` and `ClassifierMode` serialize with **default serde naming** — the enum
serializes as `"Off"` / `"Monitor"` / `"Protect"` (PascalCase, no `rename_all`). The persisted blob
under settings key `classifier_settings` therefore looks like `{"mode":"Monitor","threshold":0.92}`.
Changing the representation is a storage-compatibility break (see §7.5).

### 3.2 Public functions

```rust
pub fn extract_lexical_features(domain: &str) -> LexicalFeatures;
pub fn classify_domain(domain: &str, settings: &ClassifierSettings) -> Option<Classification>;
```

### 3.3 Behavior / invariants

`extract_lexical_features` (line 42):
- Collects `domain.chars()` into a `Vec<char>` (allocation), `len = chars.len().max(1)`.
- `digit_ratio` = ASCII digits / len; `hyphen_ratio` = `'-'` count / len.
- `label_depth` = `domain.split('.').count()` (so `"example.com"` → 2; note it is not
  dot-trimmed here — a trailing dot inflates the count).
- `entropy` = Shannon entropy in bits over the character multiset, built with a
  `std::collections::HashMap<char, usize>` (allocation).
- `LexicalFeatures.length` is the **clamped** length (`max(1)`), not the true length. An empty
  domain reports `length: 1`.

`classify_domain` (line 68):
- Returns `None` **iff** `settings.mode == ClassifierMode::Off`.
- Otherwise: `score = ((entropy / 5.0) + digit_ratio + hyphen_ratio).min(1.0)`.
  The score is **not** clamped below 0 (it can't go negative given the inputs) and has no
  probabilistic meaning. There is no model, no weights file, no inference.
- `reasons` is always a 3-element `Vec<String>` built with `format!` — three heap allocations per
  call: `"entropy={:.2}"`, `"digit_ratio={:.2}"`, `"hyphen_ratio={:.2}"`.
- `observed_at: Utc::now()` — so `Classification` is **not** a pure function of its inputs
  (timestamp varies). Everything else is deterministic.
- `settings.threshold` is **not applied inside this crate**. `classify_domain` returns a
  `Classification` for every domain in `Monitor` or `Protect` mode; the threshold comparison lives
  in `cogwheel-dns-core` (§6.2 step 3).
- `ClassifierMode::Protect` is **behaviorally identical to `Monitor` today**. Nothing anywhere in
  the workspace reads `Protect` to block a query. That is the single biggest gap if you are wiring
  a real classifier: blocking on classifier output does not exist yet.

### 3.4 Tests present (1)

`crates/cogwheel-classifier/src/lib.rs:92` — `high_entropy_domain_scores_higher`
(asserts `classify_domain("a8d9x0-zz.example", &default).unwrap().score > 0.5`; note this test uses
`.unwrap()` in test code, which is the accepted pattern — see §9).

---

## 4. `cogwheel-services`

Path: `/home/user/Cogwheel-DNS/crates/cogwheel-services/src/lib.rs` (245 lines).
Deps: `chrono`, `serde`, `serde_json`, `url` (declared, unused in code), path dep `cogwheel-policy`.
Purpose: curated per-service toggles that **compile down into `cogwheel_policy::Rule` values** rather
than bypassing policy (ADR requirement).

### 4.1 Public types

```rust
pub enum ServiceToggleMode { Inherit, Allow, Block }      // Debug, Clone, Ser, De, PartialEq, Eq

pub struct ServiceManifest {
    pub service_id: String,
    pub display_name: String,
    pub category: String,
    pub risk_notes: String,
    pub allow_domains: Vec<String>,
    pub block_domains: Vec<String>,
    pub exceptions: Vec<String>,
}                                                          // Debug, Clone, Ser, De, PartialEq, Eq

pub struct ServiceToggle {
    pub service_id: String,
    pub mode: ServiceToggleMode,
    pub updated_at: DateTime<Utc>,
}                                                          // Debug, Clone, Ser, De, PartialEq, Eq

pub struct ServiceToggleSnapshot { pub toggles: Vec<ServiceToggle> }
// Debug, Clone, Default, Ser, De, PartialEq, Eq

pub struct ServiceRuleLayer {
    pub active_toggles: Vec<ServiceToggle>,
    pub rules: Vec<Rule>,          // cogwheel_policy::Rule
    pub notes: Vec<String>,
}                                                          // Debug, Clone, Ser, De, PartialEq, Eq
```

### 4.2 Public functions

```rust
impl ServiceToggleSnapshot {
    pub fn mode_for(&self, service_id: &str) -> ServiceToggleMode;   // default Inherit if absent
    pub fn upsert(&mut self, service_id: &str, mode: ServiceToggleMode);  // sets updated_at = Utc::now()
    pub fn from_json(value: &str) -> serde_json::Result<Self>;
    pub fn to_json(&self) -> serde_json::Result<String>;
}

pub fn built_in_service_manifests() -> Vec<ServiceManifest>;
pub fn compile_service_rule_layer(
    manifests: &[ServiceManifest],
    snapshot: &ServiceToggleSnapshot,
) -> ServiceRuleLayer;
```

Private: `fn service_rule(domain: &str, action: RuleAction, service_id: &str) -> Rule` (line 191) —
always emits `RulePattern::Suffix(normalize_domain(domain))` with `source: format!("service:{service_id}")`
and `comment: None`.

### 4.3 Built-in manifests (line 79)

Three hardcoded entries, all flagged in-source as placeholders:

| `service_id` | `display_name` | `category` | `allow_domains` | `block_domains` | `exceptions` |
| --- | --- | --- | --- | --- | --- |
| `google-ads` | Google Ads | advertising | `doubleclick.net`, `googleadservices.com` | same two | `pagead2.googlesyndication.com` |
| `tiktok` | TikTok | social | `tiktokv.com`, `byteoversea.com` | same two | (empty) |
| `nintendo` | Nintendo Services | gaming | `nintendo.net`, `nintendo.com` | same two | `accounts.nintendo.com` |

`risk_notes` for the first two is literally
`"Placeholder manifest until curated domain coverage is finalized."`.

### 4.4 Compilation semantics (`compile_service_rule_layer`, line 120)

- Builds a `HashMap<&str, &ServiceManifest>` keyed by `service_id`.
- Unknown toggle → pushes note `format!("unknown service toggle ignored: {service_id}")`, skips.
- `Inherit` → skipped entirely (not added to `active_toggles`, emits no rules, no note).
- `Allow` → emits `RuleAction::Allow` suffix rules for `allow_domains ∪ block_domains ∪ exceptions`
  (chained iterators — duplicates are **not** deduped), plus note `format!("allowing service {display_name}")`.
- `Block` → emits `RuleAction::Block` suffix rules for `block_domains`, then `RuleAction::Allow`
  suffix rules for `exceptions`, plus note `format!("blocking service {display_name}")`.
  Because `PolicyEngine` runs its Allow pass first (§2.3), the exception rules win — that is the
  intended mechanism.
- Output rule order is the toggle order in the snapshot; ordering is irrelevant to `PolicyEngine`
  except through the allow-before-block rule.

### 4.5 Tests present (2)

- `crates/cogwheel-services/src/lib.rs:205` — `block_toggle_emits_block_and_exception_rules`
- `crates/cogwheel-services/src/lib.rs:231` — `allow_toggle_emits_allow_rules`

---

## 5. `cogwheel-lists`

Path: `/home/user/Cogwheel-DNS/crates/cogwheel-lists/src/lib.rs` (386 lines).
Deps: `chrono`, `base64`, `reqwest`, `serde`, `sha2`, `tracing`, `url`, `uuid`, path deps
`cogwheel-policy` **and** `cogwheel-services`.
This crate **does** use `reqwest` — it is a control-plane/background crate, explicitly *not* a
hot-path crate, and the LLM/HTTP guardrail test does not cover it.

### 5.1 Public types

```rust
pub enum SourceKind { Domains, Hosts, Adblock }      // Debug, Clone, Ser, De, PartialEq, Eq

pub struct SourceDefinition {
    pub id: Uuid,
    pub name: String,
    pub url: Url,
    pub kind: SourceKind,
    pub enabled: bool,
    pub profile: String,
    pub verification_strictness: String,
}                                                     // Debug, Clone, Ser, De

pub struct ParsedSource {
    pub source: SourceDefinition,
    pub fetched_at: DateTime<Utc>,
    pub etag: Option<String>,      // always None today — never populated
    pub checksum: String,          // hex SHA-256
    pub rules: Vec<Rule>,
    pub invalid_lines: usize,
}                                                     // Debug, Clone, Ser, De

pub struct VerificationResult {
    pub passed: bool,
    pub invalid_ratio: f32,
    pub blocked_protected_domains: Vec<String>,
    pub notes: Vec<String>,
}                                                     // Debug, Clone, Ser, De
```

`SourceKind` serializes as `"Domains"` / `"Hosts"` / `"Adblock"` (no `rename_all`), but the
**storage layer stores `kind` as a free-form `TEXT` column** (`SourceRecord.kind: String`), so the
mapping between `SourceRecord.kind` and `SourceKind` happens in `apps/cogwheel-server`, not here.

### 5.2 Public functions

```rust
pub fn synthetic_source(name: &str, rules: Vec<Rule>) -> ParsedSource;

pub async fn fetch_and_parse_source(
    client: &Client,                 // reqwest::Client
    source: SourceDefinition,
) -> Result<ParsedSource, reqwest::Error>;

pub fn parse_source(source: SourceDefinition, body: &str) -> ParsedSource;

pub fn verify_candidate(
    parsed: &[ParsedSource],
    protected_domains: &HashSet<String>,
) -> VerificationResult;

pub fn compile_ruleset(
    parsed: Vec<ParsedSource>,
    protected_domains: HashSet<String>,
    block_mode: BlockMode,
) -> RulesetArtifact;

pub fn build_policy_engine(
    parsed: Vec<ParsedSource>,
    protected_domains: HashSet<String>,
    block_mode: BlockMode,
) -> PolicyEngine;
```

Private helpers: `fetch_source_body` (206), `parse_data_url` (221), `invalid_ratio_threshold` (237),
`parse_domain_line` (245), `parse_hosts_line` (254), `parse_adblock_line` (268).

### 5.3 Parsing rules (`parse_source`, line 87)

Line preprocessing: `line.trim()`; skip if empty or starts with `#` or `!`. Then dispatch on
`source.kind`. `None` from a parser increments `invalid_lines`; `Some(rule)` is pushed.

- **Domains** (`parse_domain_line`): *never* returns `None`. Every non-comment line becomes
  `RulePattern::Exact(normalize_domain(line))`, `RuleAction::Block`, `source = source.name`,
  `comment: None`. A `Domains` list therefore always reports `invalid_lines == 0`, which means
  strictness thresholds (§5.4) can never fail a `Domains` source.
- **Hosts** (`parse_hosts_line`): splits on whitespace; `None` if fewer than 2 fields; otherwise
  `RulePattern::Exact(normalize_domain(parts[1]))`, `Block`, `comment: Some(format!("mapped from {}", parts[0]))`.
  Only the *second* field is used — a hosts line with multiple hostnames only contributes the first.
- **Adblock** (`parse_adblock_line`):
  - `@@` prefix → `RuleAction::Allow` on the remainder; otherwise `Block`.
  - `||domain^` → `RulePattern::Suffix(normalize_domain(domain))`.
  - Else if the candidate contains `$` or starts with `/` → `None` (counted invalid) — i.e. modifier
    rules and regex rules are rejected.
  - Else → `RulePattern::Exact(normalize_domain(candidate))`.

`checksum` = hex SHA-256 of the **raw body bytes** (`hasher.update(body.as_bytes())`).
`synthetic_source` instead hashes `format!("{:?}:{:?}:{}", action, pattern, source)` per rule — the
same scheme `RulesetArtifact::new` uses — and hardcodes `url: Url::parse("data:text/plain,")`,
`kind: SourceKind::Domains`, `profile: "shared"`, `verification_strictness: "balanced"`.

### 5.4 Verification (`verify_candidate`, line 122)

- `total_rules = Σ (rules.len() + invalid_lines)` across all `ParsedSource`s.
- `invalid_ratio = invalid_lines / total_rules` (0.0 when `total_rules == 0`).
- Builds a throwaway `PolicyEngine` over the union of all rules with an **empty** protected set and
  `BlockMode::NullIp`, then evaluates every domain in the caller's `protected_domains`; any that
  evaluates to `Blocked(_)` is collected into `blocked_protected_domains`.
- Notes emitted:
  - `"invalid ratio exceeds 20%"` when the aggregate ratio > 0.2 (hardcoded).
  - Per source, when its own ratio exceeds `invalid_ratio_threshold(strictness)`:
    `format!("source {name} exceeds {strictness} invalid ratio threshold {:.0}%", allowed*100.0)`.
  - `"candidate blocks protected domains"` when the protected list is non-empty.
- `passed = notes.is_empty()` — strictly "no notes at all".

`invalid_ratio_threshold` (line 237): `"strict" => 0.05`, `"relaxed" => 0.40`, anything else
(including `"balanced"` and typos) `=> 0.20`.

### 5.5 Fetching

`fetch_source_body` (line 206) special-cases `url.scheme() == "data"` and parses the data URL
locally (`parse_data_url`, line 221: splits path on the first `,`; if the metadata ends with
`;base64` it base64-STANDARD-decodes, silently yielding `""` on failure via `unwrap_or_default()`;
otherwise it only un-escapes `%0A` and `%0D`). All other schemes go through
`client.get(url).send().await?.error_for_status()?.text().await`.
There is no timeout set here — the caller's `reqwest::Client` supplies it (the server builds one
with `Duration::from_secs(5)`), no conditional-GET/ETag handling, and no size limit on the body.

### 5.6 Known wart

`cogwheel-services` is a declared path dependency of `cogwheel-lists` but **is never referenced** in
`crates/cogwheel-lists/src/lib.rs`. It cannot be removed without also editing the expected array in
`crates/cogwheel-api/src/lib.rs:379` and the ADR. Composition of service rules into a ruleset happens
in `apps/cogwheel-server` (via `synthetic_source("service-toggles", layer.rules)`).

### 5.7 Tests present (5)

- `:304` `adblock_suffix_and_allow_parse`
- `:321` `data_url_body_parses`
- `:330` `suffix_rule_can_fail_protected_domain_verification`
- `:351` `synthetic_source_preserves_rules`
- `:366` `strict_source_rejects_high_invalid_ratio`

---

## 6. `cogwheel-dns-core` — the hot path

Path: `/home/user/Cogwheel-DNS/crates/cogwheel-dns-core/src/lib.rs` (940 lines).
Deps: `anyhow`, `chrono`, `hickory-proto`, `hickory-resolver`, `moka` (future), `serde`, `tokio`,
`tracing`, path deps `cogwheel-classifier`, `cogwheel-policy`.
No `reqwest`, no storage, no axum — and a test enforces that (§6.6).

Module constant: `const MAX_CNAME_UNCLOAK_DEPTH: usize = 8;` (line 21).

### 6.1 Public types

```rust
pub struct DnsRuntimeConfig { pub udp_bind_addr: SocketAddr, pub tcp_bind_addr: SocketAddr }  // Debug, Clone

pub struct DnsRuntime { /* all fields private */ }   // Clone (cheap: all Arc)

pub struct ClassificationEvent {
    pub domain: String,
    pub client_ip: Option<String>,
    pub classification: Classification,
    pub observed_at: DateTime<Utc>,
}                                                     // Debug, Clone, Serialize

pub struct QueryActivityEvent {
    pub domain: String,
    pub client_ip: Option<String>,
    pub blocked: bool,
    pub observed_at: DateTime<Utc>,
}                                                     // Debug, Clone, Serialize

pub struct DevicePolicyConfig {
    pub ip_address: String,
    pub policy_mode: String,
    pub blocklist_profile_override: Option<String>,
    pub protection_override: String,
    pub allowed_domains: Vec<String>,
    pub blocked_domains: Vec<String>,
}                                                     // Debug, Clone, Serialize, PartialEq, Eq

pub struct DnsRuntimeStats { /* private AtomicU64 fields */ }   // Debug, Default

pub struct DnsRuntimeSnapshot {
    pub upstream_failures_total: u64,
    pub fallback_served_total: u64,
    pub cache_hits_total: u64,
    pub cname_uncloaks_total: u64,
    pub cname_blocks_total: u64,
    pub queries_total: u64,
    pub blocked_total: u64,
    pub cache_hit_latency_avg_ns: u64,
    pub cache_hit_samples: u64,
    pub cache_miss_latency_avg_ns: u64,
    pub cache_miss_samples: u64,
    pub classifier_latency_avg_ns: u64,
    pub classifier_latency_samples: u64,
}                                                     // Debug, Clone, Serialize, PartialEq, Eq
```

Type aliases (private but shape-relevant to callers of the setters):

```rust
type ClassificationObserver  = Arc<dyn Fn(ClassificationEvent)  + Send + Sync>;
type QueryActivityObserver   = Arc<dyn Fn(QueryActivityEvent)   + Send + Sync>;
```

Note both observers are **synchronous `Fn`**, not async. See §6.5.

`DnsRuntime`'s private fields (line 33), because every extension touches them:

```rust
resolver: TokioResolver,
policy: Arc<RwLock<Arc<PolicyEngine>>>,
allow_all_policy: Arc<RwLock<Arc<PolicyEngine>>>,
profile_policies: Arc<RwLock<HashMap<String, Arc<PolicyEngine>>>>,
devices_by_ip: Arc<RwLock<HashMap<IpAddr, DevicePolicyConfig>>>,
classifier_settings: Arc<RwLock<ClassifierSettings>>,
classification_observer: Arc<RwLock<Option<ClassificationObserver>>>,
query_activity_observer: Arc<RwLock<Option<QueryActivityObserver>>>,
global_pause_until: Arc<RwLock<Option<DateTime<Utc>>>>,
cache: Cache<String, CachedLookup>,             // moka::future::Cache
fallback_cache: Cache<String, CachedLookup>,
stats: Arc<DnsRuntimeStats>,
```

Private struct `CachedLookup { response: Message, blocked: bool }` (line 74).

### 6.2 The exact hot-path chain: UDP packet → response

```
UdpSocket::recv_from
  └─ DnsRuntime::serve_udp                (line 252)
       └─ DnsRuntime::handle_wire_query   (line 301)   ← ALL logic lives here
            ├─ classify_domain            (cogwheel-classifier)
            ├─ DnsRuntime::policy_for_client (line 504)
            ├─ moka cache get             (self.cache)
            ├─ PolicyEngine::evaluate     (cogwheel-policy)
            ├─ DnsRuntime::uncloaked_block_mode (line 599)  ← extra upstream lookups
            ├─ DnsRuntime::resolve_upstream     (line 586)
            └─ moka cache insert
       └─ Message::to_vec  →  UdpSocket::send_to
```

`pub async fn serve(self: Arc<Self>, config: DnsRuntimeConfig) -> Result<()>` (line 244) spawns
`serve_udp` and `serve_tcp` and awaits both (`udp.await??; tcp.await??;`).

**`serve_udp(self: Arc<Self>, bind_addr: SocketAddr)` (line 252)**
1. `UdpSocket::bind(bind_addr).await.context("bind udp socket")?`
2. `let mut buffer = [0u8; 4096];` — a **single stack buffer reused across iterations**.
3. `loop { let (size, peer) = socket.recv_from(&mut buffer).await?; … }`
4. `self.handle_wire_query(&buffer[..size], Some(peer)).await` — **awaited inline, not spawned**.
   On `Err`, logs `tracing::warn!(%error, "failed to handle udp dns query")` and substitutes
   `error_response_for_payload(&buffer[..size])` (SERVFAIL preserving the request id).
5. `socket.send_to(&response.to_vec()?, peer).await?`.

> **Concurrency invariant (important):** UDP query handling is **fully serialized**. One in-flight
> query at a time per process. A single cache-miss that waits on upstream (plus up to 8 sequential
> CNAME lookups, §6.3) head-of-line-blocks every other UDP client. Any latency work should start
> here. Fixing it requires moving off the shared `buffer` (copy the datagram into a `Vec`/`Bytes`
> before spawning) and sharing the socket via `Arc<UdpSocket>`.
> Note also that `?` on `to_vec()`/`send_to()` propagates out of the `loop`, killing the whole UDP
> listener on a single send error.

**`serve_tcp` (line 271)** binds a `TcpListener`, and for each accepted connection spawns
`handle_tcp_stream`. **`handle_tcp_stream` (line 286)** reads a 2-byte big-endian length, reads
exactly that many bytes, calls `handle_wire_query(&payload, Some(peer))`, and writes back
`len:u16 BE` + body. It handles **exactly one message per connection** (no pipelining loop) and has
no read timeout — a client that opens a connection and stalls holds a task indefinitely.

**`handle_wire_query(&self, payload: &[u8], client_addr: Option<SocketAddr>) -> Result<Message>` (line 301)** — step by step:

1. `stats.queries_total.fetch_add(1, Relaxed)` — counted **before** parsing, so malformed packets
   still increment it. `let query_start = Instant::now();`
2. `Message::from_vec(payload)?`; take `request.queries().first().cloned()` or bail with
   `"dns query missing question"`. `let name = query.name().to_utf8();`
   `let domain = name.trim_end_matches('.').to_ascii_lowercase();`
   Only the **first** question is ever considered.
3. **Classifier runs here — before the cache lookup**:
   ```rust
   let classifier_settings = self.classifier_settings();          // RwLock read + clone
   let classifier_start = Instant::now();
   if let Some(classification) = classify_domain(&domain, &classifier_settings) {
       tracing::debug!(domain, score = classification.score, "domain classified");
       if classification.score >= classifier_settings.threshold {
           self.emit_classification_event(&domain, client_addr, classification);
       }
   }
   self.record_classifier_latency(classifier_start.elapsed().as_nanos());
   ```
   Consequences: the classifier cost is paid on **every query including cache hits**; it is inline
   and synchronous; the classification result **never influences the response**.
4. `let (engine, cache_scope, forced_block_mode) = self.policy_for_client(client_addr, &domain);`
   (§6.4) and `let cache_key = policy_cache_key(&cache_scope, &domain);` → `format!("{scope}:{domain}")`.
5. **Cache hit path**: `if let Some(cached) = self.cache.get(&cache_key).await { … }` →
   increments `cache_hits_total`, calls `emit_query_activity(&domain, client_addr, cached.blocked)`,
   records cache-hit latency, and returns `response_for_request(&request, &cached.response)`
   (clones the cached `Message` and overwrites only its id).
6. **Forced device block**: if `forced_block_mode` is `Some(mode)`, build a blocked response,
   `blocked_total += 1`, insert into cache with `blocked: true`, emit activity, record miss latency,
   return.
7. `let decision = engine.evaluate(&domain);`
   `let allow_matched = decision.matched_rule.as_ref().is_some_and(|r| matches!(r.action, RuleAction::Allow));`
   `let blocked = matches!(&decision.kind, DecisionKind::Blocked(_));`
8. `DecisionKind::Blocked(mode)` → `blocked_total += 1`, `build_blocked_response(&request, mode)`.
9. `DecisionKind::Allowed`:
   - If **not** matched by an explicit Allow rule → `self.uncloaked_block_mode(&domain, &engine).await?`
     (§6.3). If it yields `Some(mode)`: `blocked_total += 1`, build blocked response, cache it with
     `blocked: true`, emit activity, record miss latency, early-return.
   - Then `self.resolve_upstream(&request, &domain).await`:
     - `Ok(response)` → also inserted into `self.fallback_cache` keyed by **bare `domain`**
       (no scope) with `blocked: false`.
     - `Err(error)` → `upstream_failures_total += 1`; if `fallback_cache.get(&domain)` hits,
       `fallback_served_total += 1`, warn-log, and serve `response_for_request(&request, &fallback.response)`;
       otherwise `return Err(error)` (which becomes SERVFAIL at the UDP layer).
10. Insert the final response into `self.cache` under `cache_key` with the computed `blocked` flag,
    `emit_query_activity(&domain, client_addr, blocked)`, `record_cache_miss_latency(...)`, return.

`pub async fn probe_domain(&self, domain: &str, record_type: RecordType) -> Result<ResponseCode>`
(line 234) builds a synthetic request via `build_probe_request` and runs the *same*
`handle_wire_query(payload, None)` path — so runtime-guard probes mutate the same counters and
populate the same caches as real traffic.

### 6.3 CNAME uncloaking

```rust
async fn uncloaked_block_mode(&self, domain: &str, engine: &PolicyEngine) -> Result<Option<BlockMode>>
```
(line 599). Loops at most `MAX_CNAME_UNCLOAK_DEPTH` (8) times; a `HashSet<String>` guards cycles
(returns `Ok(None)` on repeat). Each iteration does
`self.resolver.lookup(&current, RecordType::CNAME).await` — **any error is swallowed into
`Ok(None)`** — then `records().iter().find_map(extract_cname_target)`; no CNAME → `Ok(None)`.
On a found target: `cname_uncloaks_total += 1`, `normalize_domain(&target)`, `engine.evaluate(...)`;
if blocked → `cname_blocks_total += 1` and return the mode; else recurse on the target.

Latency impact: for every cache-missing, non-explicitly-allowed domain, this adds **1–8 sequential
upstream round-trips before the actual resolution even starts**, on the serialized UDP path. This is
the primary reason the "cache miss ≤ 8 ms p50" budget in `docs/reliability-budgets.md` is optimistic.

### 6.4 Per-client policy selection and cache scoping

```rust
fn policy_for_client(&self, client_addr: Option<SocketAddr>, domain: &str)
    -> (Arc<PolicyEngine>, String, Option<BlockMode>)
```
(line 504). Returns `(engine, cache_scope, forced_block_mode)`. Precedence, first match wins:

| # | Condition | engine | cache_scope | forced block |
| --- | --- | --- | --- | --- |
| 1 | `protection_paused_until()` is `Some(t)` and `Utc::now() < t` | `allow_all_policy` | `"global-pause"` | `None` |
| 2 | `client_addr` is `None` | global | `global.artifact().hash` | `None` |
| 3 | no `DevicePolicyConfig` for the client IP | global | `global.artifact().hash` | `None` |
| 4 | `device.policy_mode != "custom"` | global | `global.artifact().hash` | `None` |
| 5 | domain matches any `device.blocked_domains` | global | `format!("device-block:{client_ip}")` | `Some(global.artifact().block_mode.clone())` |
| 6 | domain matches any `device.allowed_domains` | `allow_all_policy` | `format!("device-allow:{client_ip}")` | `None` |
| 7 | `device.protection_override == "bypass"` | `allow_all_policy` | `"bypass"` | `None` |
| 8 | `device.blocklist_profile_override` is `Some(p)` and `profile_policies` contains `p` | that profile engine | `format!("profile:{p}")` | `None` |
| 9 | otherwise | global | `global.artifact().hash` | `None` |

Matching helper `fn domain_matches_override(domain: &str, candidate: &str) -> bool` (line 673):
`domain == candidate || domain.strip_suffix(candidate).is_some_and(|prefix| prefix.ends_with('.'))`
— i.e. exact or dot-boundary suffix. `"badexample.com"` does **not** match `"example.com"`.

Magic strings that must stay in sync with `apps/cogwheel-server` and the storage layer:
`"custom"` (policy_mode), `"bypass"` (protection_override), and the `blocklist_profile_override`
keys that index `profile_policies`.

`fn build_allow_all_policy(global_policy: &Arc<PolicyEngine>) -> Arc<PolicyEngine>` (line 664)
constructs `PolicyEngine::new(RulesetArtifact::new(Vec::new(), artifact.protected_domains.clone(), artifact.block_mode.clone()))`
— zero rules, so everything evaluates `Allowed`. It is rebuilt inside `replace_policy_catalog`.

### 6.5 Mutators, observers, stats

```rust
impl DnsRuntime {
    pub fn new(resolver: TokioResolver, policy: Arc<PolicyEngine>, classifier_settings: ClassifierSettings) -> Self;
    pub fn replace_policy(&self, policy: Arc<PolicyEngine>);
    pub fn replace_policy_catalog(&self, policy: Arc<PolicyEngine>, profile_policies: HashMap<String, Arc<PolicyEngine>>);
    pub fn replace_device_policies(&self, devices: Vec<DevicePolicyConfig>);
    pub fn classifier_settings(&self) -> ClassifierSettings;
    pub fn replace_classifier_settings(&self, settings: ClassifierSettings);
    pub fn set_classification_observer(&self, observer: ClassificationObserver);
    pub fn set_query_activity_observer(&self, observer: QueryActivityObserver);
    pub fn snapshot(&self) -> DnsRuntimeSnapshot;
    pub async fn probe_domain(&self, domain: &str, record_type: RecordType) -> Result<ResponseCode>;
    pub async fn serve(self: Arc<Self>, config: DnsRuntimeConfig) -> Result<()>;
    pub fn pause_protection_until(&self, until: DateTime<Utc>);
    pub fn resume_protection(&self);
    pub fn protection_paused_until(&self) -> Option<DateTime<Utc>>;
}
```

- `DnsRuntime::new` builds both caches as `Cache::new(10_000)` — **capacity-only, no TTL, no TTI.**
  DNS record TTLs are entirely ignored; entries live until eviction by capacity or explicit
  invalidation.
- `replace_policy_catalog` rebuilds `allow_all_policy`, swaps all three policy slots, then calls
  `self.cache.invalidate_all()` **and** `self.fallback_cache.invalidate_all()`.
  `replace_device_policies` parses `ip_address` with `str::parse::<IpAddr>()`, silently dropping
  unparsable entries via `filter_map(...ok())`, and invalidates **only** `self.cache`.
- Writer methods use the `if let Ok(mut guard) = self.x.write()` pattern — a poisoned lock is a
  **silent no-op**. Reader methods use `.expect("… lock poisoned")` and **panic**. That asymmetry is
  a live hazard: one panic inside a reader poisons the lock, after which writes silently stop
  applying while reads keep panicking.
- Observers are plain synchronous `Fn` closures invoked **on the hot path**. `emit_classification_event`
  (line 455) and `emit_query_activity` (line 472) clone the `Option<Arc<dyn Fn…>>` under a read lock,
  drop the guard, then call it. Whatever the observer does happens inline in the query's latency.
  The server's classification observer (see `apps/cogwheel-server/src/main.rs:605`) immediately
  `tokio::spawn`s the real work, so the inline cost is just the spawn; the query-activity observer
  (`main.rs:627`) runs `record_recent_dns_activity` **synchronously** under a `Mutex`.
  **If you add an observer, the "don't block the hot path" discipline lives at the call site, not in
  this crate.**
- `snapshot()` reads every counter with `Ordering::Relaxed` and computes averages via
  `fn average_atomic_ns(total: &AtomicU64, samples: u64) -> u64` (line 699), returning `0` when
  `samples == 0`. Counters are cumulative-since-process-start and never reset. `fn saturating_ns(u128) -> u64`
  (line 695) clamps. There is no per-record-type, per-client, or per-decision breakdown.

### 6.6 What "the hot path never blocks on ML inference" actually means in code today

The guarantee documented in `/home/user/Cogwheel-DNS/docs/hot-path-guardrails.md` is enforced by
exactly one mechanism, and it is weaker than the prose suggests. Concretely, **today**:

1. **Enforced mechanically:** a unit test,
   `crates/cogwheel-dns-core/src/lib.rs:908` — `hot_path_crates_remain_llm_and_network_independent`.
   It reads `{CARGO_MANIFEST_DIR}/Cargo.toml` and `{CARGO_MANIFEST_DIR}/../cogwheel-classifier/Cargo.toml`
   as **text** and asserts neither contains the substring `"{dep} ="` for any of:
   `reqwest`, `ureq`, `surf`, `async-openai`, `openai-api-rs`, `ollama-rs`, `rig-core`, `langchain-rust`.
   That is the entire enforcement. Note the loophole: the check is for `"<name> ="`, so
   `foo.workspace = true` style (`reqwest.workspace = true`) **does not contain `reqwest =`** and
   would slip through. A dep added as `reqwest.workspace = true` passes this test. Also, any other
   HTTP/gRPC/ML crate not on the eight-name list (hyper, tonic, ort, tract, candle, …) passes.
2. **Enforced by construction:** `cogwheel-classifier` has only two dependencies (`chrono`, `serde`),
   no filesystem or socket access, and `classify_domain` is a pure arithmetic function over the
   domain string. There is nothing to block on.
3. **NOT enforced anywhere:** that inference is *off* the request path. It is squarely *on* it —
   `classify_domain` is called synchronously in `handle_wire_query` at line 319, **before the cache
   lookup**, so every single query (hit or miss) pays it. The only asynchrony is the *observer*
   side-effect, and that async-ness is supplied by the caller (`tokio::spawn` in the server), not by
   this crate.
4. There is **no timeout, no budget, no circuit breaker, and no fallback** around the classifier
   call. `classifier_latency_avg_ns` / `classifier_latency_samples` in `DnsRuntimeSnapshot` are the
   only observability, and they are unconditional averages, not percentiles.

**Implication for anyone replacing the classifier with a real model:** dropping a real inference
call into `classify_domain` puts model latency directly into every DNS response, including cache
hits, on a serialized UDP loop. The structural fixes to make first are, in order: (a) move the
classifier call *after* the cache-hit early return; (b) make it a bounded, non-blocking submission
(channel to a background scorer, or `try_*` with a hard deadline) so the response never waits on it;
(c) if `ClassifierMode::Protect` should ever block, that decision must be reachable without an
inline await — e.g. a scored-domain cache consulted synchronously, populated asynchronously;
(d) extend the guardrail test in step 1 to cover `X.workspace = true` and any new runtime crates.

### 6.7 Response construction helpers (all private)

```rust
fn extract_cname_target(record: &Record) -> Option<String>;                          // 640
fn build_classification_event(domain: &str, client_addr: Option<SocketAddr>, classification: Classification) -> ClassificationEvent; // 647
fn policy_cache_key(scope: &str, domain: &str) -> String;                            // 660  -> format!("{scope}:{domain}")
fn build_allow_all_policy(global_policy: &Arc<PolicyEngine>) -> Arc<PolicyEngine>;   // 664
fn domain_matches_override(domain: &str, candidate: &str) -> bool;                   // 673
fn build_probe_request(domain: &str, record_type: RecordType) -> Result<Message>;    // 680
fn response_for_request(request: &Message, cached: &Message) -> Message;             // 689
fn saturating_ns(elapsed_ns: u128) -> u64;                                           // 695
fn average_atomic_ns(total: &AtomicU64, samples: u64) -> u64;                        // 699
fn error_response_for_payload(payload: &[u8]) -> Message;                            // 707
fn build_base_response(request: &Message, code: ResponseCode) -> Message;            // 714
fn build_blocked_response(request: &Message, mode: BlockMode) -> Message;            // 729
fn build_ip_response(request: &Message, ipv4: Option<Ipv4Addr>, ipv6: Option<Ipv6Addr>) -> Message; // 743
```

- `build_base_response`: id from request, `MessageType::Response`, op_code copied, `authoritative=false`,
  `recursion_desired` copied, `recursion_available=true`, given response code, and every question
  copied into the response.
- `build_blocked_response` mapping:
  `NxDomain → ResponseCode::NXDomain`; `NoData → NoError` (empty answers);
  `Refused → Refused`; `NullIp → build_ip_response(0.0.0.0, ::)`;
  `CustomIp{ipv4, ipv6} → build_ip_response(ipv4, ipv6)`.
- `build_ip_response` adds an answer **only** for `RecordType::A` (with `RData::A`) and
  `RecordType::AAAA` (with `RData::AAAA`), TTL hardcoded to **60**; every other qtype falls through
  the `_ => {}` arm and yields NoError with no answers.
- `resolve_upstream` (line 586) calls `self.resolver.lookup(domain, query.query_type()).await?` and
  copies `lookup.records()` into the answer section of a fresh `NoError` response. It does **not**
  propagate NXDOMAIN/SERVFAIL/NoData from upstream (a resolver error becomes an `Err`, which the UDP
  layer turns into SERVFAIL), and it does **not** carry authority/additional sections or the
  upstream's AD/TC flags.
- `error_response_for_payload` (line 707) parses the payload to recover the id/op-code and returns
  `Message::error_msg(id, op_code, ResponseCode::ServFail)`; on unparsable input it uses id `0` and
  `OpCode::Query`.

### 6.8 Tests present (10)

All in `crates/cogwheel-dns-core/src/lib.rs`, `mod tests` at line 764 (`use super::*; use std::fs;`):

| Line | Test | What it pins |
| --- | --- | --- |
| 770 | `runtime_snapshot_starts_at_zero` | default stats produce an all-zero snapshot |
| 808 | `extract_cname_target_reads_record_data` | CNAME rdata → `Some("tracker.example.com")` |
| 826 | `build_probe_request_sets_expected_question` | probe is a Query with 1 question of the right type |
| 834 | `cached_response_adopts_request_id` | `response_for_request` overwrites id only |
| 845 | `error_response_uses_original_request_id` | SERVFAIL preserves request id |
| 853 | `build_classification_event_preserves_client_ip` | client ip string + `observed_at` propagation |
| 872 | `policy_cache_key_scopes_by_policy` | `"profile:balanced" + "ads.example.com"` → `"profile:balanced:ads.example.com"` |
| 880 | `build_allow_all_policy_removes_block_rules` | allow-all engine allows a previously blocked domain |
| 901 | `domain_matches_override_supports_suffixes` | dot-boundary suffix matching |
| 908 | `hot_path_crates_remain_llm_and_network_independent` | manifest guardrail (§6.6) |

**There is not a single test that exercises `handle_wire_query`, the cache, `policy_for_client`,
`resolve_upstream`, or `uncloaked_block_mode`.** The hot path itself is untested. There is no
`tests/` directory in any crate — all tests are inline `#[cfg(test)] mod tests`.

---

## 7. `cogwheel-storage`

Path: `/home/user/Cogwheel-DNS/crates/cogwheel-storage/src/lib.rs` (660 lines).
Migrations: `/home/user/Cogwheel-DNS/crates/cogwheel-storage/migrations/*.sql` (10 files).
Deps: `chrono`, `rusqlite` (`bundled`), `serde`, `serde_json`, `thiserror`, `tokio`, `tracing`,
`uuid`, path dep `cogwheel-policy`, plus `ed25519-dalek 2.2.0` (`rand_core`), `base64`,
`rand 0.8` (`std`, `std_rng`), `getrandom 0.4.2`.

### 7.1 Constants and error type

```rust
pub const SCHEMA_VERSION: u32 = 10;          // hand-maintained; NOT checked against the DB
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error(transparent)] Sqlite(#[from] rusqlite::Error),
    #[error(transparent)] Serde(#[from] serde_json::Error),
    #[error(transparent)] Uuid(#[from] uuid::Error),
    #[error(transparent)] Chrono(#[from] chrono::ParseError),
    #[error("internal storage error: {0}")] Internal(String),
}
```

Both constants are read only by `apps/cogwheel-server/src/main.rs:1399-1400` (a version-report
endpoint). Nothing compares `SCHEMA_VERSION` to actual DB state.

### 7.2 Public types

```rust
pub struct Storage { /* connection: Arc<Mutex<Connection>>, node_identity: Arc<NodeIdentity> */ }  // Debug, Clone

pub struct NodeIdentity { pub key: Arc<SigningKey>, pub public_b64: String }   // Debug, Clone

pub struct SourceRecord {
    pub id: Uuid, pub name: String, pub url: String, pub kind: String,
    pub enabled: bool, pub refresh_interval_minutes: i64,
    pub profile: String, pub verification_strictness: String,
}                                                          // Debug, Clone, Ser, De

pub struct RulesetRecord {
    pub id: Uuid, pub hash: String, pub status: String,
    pub created_at: DateTime<Utc>, pub artifact_json: String,
}                                                          // Debug, Clone, Ser, De

pub struct AuditEvent {
    pub id: Uuid, pub event_type: String, pub payload: String, pub created_at: DateTime<Utc>,
}                                                          // Debug, Clone, Ser, De

pub struct DeviceServiceOverrideRecord { pub service_id: String, pub mode: String }
// Debug, Clone, Ser, De, PartialEq, Eq

pub struct DeviceRecord {
    pub id: Uuid, pub name: String, pub ip_address: String, pub policy_mode: String,
    pub blocklist_profile_override: Option<String>, pub protection_override: String,
    pub allowed_domains: Vec<String>,
    pub service_overrides: Vec<DeviceServiceOverrideRecord>,
}                                                          // Debug, Clone, Ser, De

pub struct SecurityEventRecord {
    pub id: Uuid, pub device_id: Option<Uuid>, pub device_name: Option<String>,
    pub client_ip: String, pub domain: String, pub classifier_score: f64,
    pub severity: String, pub created_at: DateTime<Utc>,
}                                                          // Debug, Clone, Ser, De

pub struct NotificationDeliveryRecord {
    pub id: Uuid, pub event_type: String, pub status: String, pub severity: String,
    pub title: String, pub summary: String, pub domain: String,
    pub device_name: Option<String>, pub client_ip: String,
    pub attempts: usize, pub created_at: DateTime<Utc>,
}                                                          // Debug, Clone, Ser, De

pub struct SyncEnvelope {
    pub node_public_key: String,
    pub timestamp: DateTime<Utc>,
    pub nonce: String,
    pub payload_b64: String,
    pub signature_b64: String,
}                                                          // Debug, Clone, Ser, De
```

> Name collision warning: `cogwheel_storage::SyncEnvelope` and `cogwheel_sync::SyncEnvelope` are
> **different, unrelated types with the same name**. `apps/cogwheel-server` imports the *storage*
> one (`main.rs:21-24`). See §8.

`DeviceRecord` has **no `blocked_domains` field**, while `cogwheel_dns_core::DevicePolicyConfig`
does. The server synthesizes that field when converting records to runtime configs
(`runtime_device_policies_from_records`, `main.rs:5104` call site).

### 7.3 Public API — exact signatures

```rust
impl Storage {
    pub async fn connect(database_url: &str) -> Result<Self, StorageError>;
    pub fn identity(&self) -> Arc<NodeIdentity>;

    // Signed sync envelopes
    pub fn sign_sync_payload(&self, payload: &[u8]) -> SyncEnvelope;
    pub fn verify_sync_envelope(envelope: &SyncEnvelope) -> Result<Vec<u8>, StorageError>;  // associated fn, no &self

    // Key-value settings  (see §7.5)
    pub async fn upsert_setting(&self, key: &str, value: &str) -> Result<(), StorageError>;
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, StorageError>;

    // Sources
    pub async fn insert_source(&self, source: &SourceRecord) -> Result<(), StorageError>;
    pub async fn list_sources(&self) -> Result<Vec<SourceRecord>, StorageError>;
    pub async fn delete_source(&self, source_id: Uuid) -> Result<bool, StorageError>;

    // Devices
    pub async fn upsert_device(&self, device: &DeviceRecord) -> Result<(), StorageError>;
    pub async fn delete_device(&self, device_id: Uuid) -> Result<bool, StorageError>;
    pub async fn list_devices(&self) -> Result<Vec<DeviceRecord>, StorageError>;
    pub async fn find_device_by_ip(&self, ip_address: &str) -> Result<Option<DeviceRecord>, StorageError>;

    // Security events
    pub async fn record_security_event(&self, event: &SecurityEventRecord) -> Result<(), StorageError>;
    pub async fn recent_security_events(&self, limit: i64) -> Result<Vec<SecurityEventRecord>, StorageError>;

    // Rulesets
    pub async fn record_ruleset(&self, ruleset: &RulesetRecord) -> Result<(), StorageError>;
    pub async fn list_rulesets(&self) -> Result<Vec<RulesetRecord>, StorageError>;
    pub async fn activate_ruleset(&self, ruleset_id: Uuid) -> Result<(), StorageError>;
    pub async fn active_ruleset(&self) -> Result<Option<RulesetRecord>, StorageError>;
    pub async fn previous_ruleset(&self) -> Result<Option<RulesetRecord>, StorageError>;
    pub async fn rollback_to_previous_ruleset(&self) -> Result<Option<RulesetArtifact>, StorageError>;

    // Audit
    pub async fn record_audit_event(&self, event: &AuditEvent) -> Result<(), StorageError>;
    pub async fn recent_audit_events(&self, limit: i64) -> Result<Vec<AuditEvent>, StorageError>;

    // Notifications
    pub async fn record_notification_delivery(&self, delivery: &NotificationDeliveryRecord) -> Result<(), StorageError>;
    pub async fn recent_notification_deliveries(&self, limit: i64) -> Result<Vec<NotificationDeliveryRecord>, StorageError>;

    // Config version
    pub fn get_config_version(&self) -> Result<u32, StorageError>;   // NOT async
}
```

Module-private helpers:
```rust
fn sync_signing_message(timestamp: &DateTime<Utc>, nonce: &str, payload: &[u8]) -> Vec<u8>;  // 137
fn apply_migrations(connection: &Connection) -> Result<(), StorageError>;                     // 630
fn decode_ruleset_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RulesetRecord>;            // 644
fn parse_datetime(value: &str) -> Result<DateTime<Utc>, chrono::ParseError>;                  // 654  (RFC3339 only)
fn to_sqlite_error(error: chrono::ParseError) -> rusqlite::Error;                             // 658
```

### 7.4 Connection, threading and async-ness (read this before adding a method)

`connect` (line 198):
1. Strips a leading `sqlite://` prefix (`strip_prefix("sqlite://").unwrap_or(database_url)`), then
   `std::fs::create_dir_all(parent).ok()` — directory-creation failures are **ignored**.
2. `Connection::open(path)?`, then `pragma_update(None, "journal_mode", "WAL")` and
   `pragma_update(None, "foreign_keys", "ON")`. **Foreign keys are ON** — this matters for
   `security_events.device_id → devices.id` and `active_ruleset.ruleset_id → rulesets.id`.
3. `apply_migrations(&connection)?` (§7.6).
4. Loads or generates the node signing key from settings key `node_identity_v1` (§7.7).

Critical structural facts:

- There is **exactly one `rusqlite::Connection`**, wrapped in `Arc<Mutex<Connection>>`
  (`std::sync::Mutex`, not `tokio::sync::Mutex`). All database access is serialized process-wide.
- Every method is declared `async fn` but performs **blocking** SQLite I/O on the calling executor
  thread. There is no `spawn_blocking` anywhere. Under load these block Tokio worker threads.
- Each method acquires the guard with `.lock().expect("storage mutex poisoned")` — 29 occurrences —
  which **panics** if any prior holder panicked.
- **No `.await` currently occurs while a `MutexGuard` is alive.** That is what keeps every returned
  future `Send` (a `std::sync::MutexGuard` is `!Send`). If you add an `await` inside one of these
  methods between the `lock()` and the end of the guard's scope, the future becomes `!Send` and the
  entire axum/tokio call graph in `apps/cogwheel-server` stops compiling with a confusing error.
  Either scope the guard in an inner block that ends before the await, or move to
  `tokio::task::spawn_blocking`.
- `rollback_to_previous_ruleset` is the only composite method; it awaits `previous_ruleset()` then
  `activate_ruleset()`, each of which takes and drops the lock independently — so it is **not
  atomic**, and a concurrent activation can interleave.

### 7.5 The key-value settings mechanism (arbitrary settings blobs)

This is the project's escape hatch for persisting structured configuration without a migration.

Table (`0001_init.sql`):

```sql
CREATE TABLE IF NOT EXISTS settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

API:

```rust
pub async fn upsert_setting(&self, key: &str, value: &str) -> Result<(), StorageError>
```
executes
```sql
INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, CURRENT_TIMESTAMP)
ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = CURRENT_TIMESTAMP
```

```rust
pub async fn get_setting(&self, key: &str) -> Result<Option<String>, StorageError>
```
executes `SELECT value FROM settings WHERE key = ?1 LIMIT 1` and maps a missing row to `Ok(None)`
via `.optional()`.

Properties and rules:

- `value` is an **opaque UTF-8 `String`**. The storage crate does no JSON validation. Callers
  serialize with `serde_json::to_string` and deserialize with `serde_json::from_str`.
- There is **no delete API**, no list-keys API, no prefix scan, no typed wrapper, and no schema
  versioning for individual blobs. Adding one is a pure addition to `impl Storage` (no migration).
- Deserialization failures are the caller's problem. There is **no forward/backward-compat handling**
  in the crate: if you change a settings struct's shape, old rows fail to parse. The server's
  loaders (e.g. `main.rs:4155` for `classifier_settings`) use `let Some(value) = … else { … }` to
  fall back to defaults when the key is *absent*, but a present-but-unparsable value propagates as
  an error. If you evolve a settings struct, prefer `#[serde(default)]` on new fields, or write the
  blob under a **new key name**.

**Every settings key in use today** (exhaustive; from `apps/cogwheel-server/src/main.rs` and
`cogwheel-storage`):

| Key | Written at | Read at | Payload |
| --- | --- | --- | --- |
| `node_identity_v1` | `crates/cogwheel-storage/src/lib.rs:230` | `…/lib.rs:213` | base64 URL-SAFE-NO-PAD of the 32-byte ed25519 signing key |
| `sync_revision` | `main.rs:2455` | `main.rs:2182` | decimal `u64` as text |
| `sync_profile` | `main.rs:2196` | `main.rs:2190` | profile string |
| `sync_transport_mode` | `main.rs:2207` | `main.rs:2202` | mode string |
| `sync_transport_token` | `main.rs:2220` | `main.rs:2213` | token string (empty string when cleared) |
| `service_toggles` | `main.rs:4767` | `main.rs:4148` | `ServiceToggleSnapshot::to_json()` |
| `classifier_settings` | `main.rs:4784` | `main.rs:4155` | `serde_json` of `ClassifierSettings` |
| `notification_settings` | `main.rs:4794` | `main.rs:4162` | `serde_json` of notification settings |
| `notification_test_presets` | `main.rs:4804` | `main.rs:4179` | `serde_json` array of presets |
| `block_profiles` | `main.rs:4205` | `main.rs:4188` | `serde_json` of profile definitions |
| `source_refresh_state` | `main.rs:4774` | `main.rs:4620` | `serde_json` of refresh state |

New tunables (classifier model config, UI preferences, deployment metadata) should follow this
pattern — a new key, a `serde` struct with `#[derive(Default)]`, and load-with-fallback — rather
than a new migration, unless the data needs to be queried or indexed.

### 7.6 Migrations: files, mechanism, and the trap

`apply_migrations` (line 630) is literally:

```rust
connection.execute_batch(MIGRATION_0001)?;
let _ = connection.execute_batch(MIGRATION_0002);
let _ = connection.execute_batch(MIGRATION_0003);
…
let _ = connection.execute_batch(MIGRATION_0010);
Ok(())
```

Each `MIGRATION_000N` is an `include_str!` of the corresponding file (lines 14–23), so migrations
are **compiled into the binary**.

Mechanism notes — these are the rules for adding migration 0011:

- **There is no migration ledger for the SQL files.** No `schema_migrations` table records which of
  0001–0010 ran. Every migration is re-executed on every `connect()`.
- 0001 uses `?`, so a failure aborts startup. **0002 through 0010 are wrapped in `let _ = …`, so all
  errors are silently discarded.** That is deliberate: `ALTER TABLE … ADD COLUMN` has no
  `IF NOT EXISTS` in SQLite, so re-running it always errors, and swallowing that error is how
  re-entrancy is achieved. The cost is that a *genuine* migration failure is also invisible.
- Because `execute_batch` aborts at the first failing statement, **any statement placed after a
  statement that fails on re-run will never execute on an existing database.** 0010 already has this
  problem: `INSERT INTO config_migrations (version, description) VALUES (1, …)` violates the UNIQUE
  constraint on the second run and aborts the batch — harmless today only because it is the last
  statement. **Do not append statements to an existing migration file.**
- Therefore, a new migration must: (a) be a new file `0011_*.sql`; (b) be added as a
  `const MIGRATION_0011: &str = include_str!("../migrations/0011_*.sql");` next to lines 14–23;
  (c) be appended as `let _ = connection.execute_batch(MIGRATION_0011);` in `apply_migrations`;
  (d) use `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS` / `INSERT OR IGNORE` wherever
  possible so re-runs are clean; (e) bump `pub const SCHEMA_VERSION` to 11.
- There is **no down-migration path** and no backup step.

### 7.7 Full schema (every table, every column)

Cumulative result of 0001–0010.

**`settings`** (0001)
| Column | Type | Constraints |
| --- | --- | --- |
| `key` | TEXT | PRIMARY KEY |
| `value` | TEXT | NOT NULL |
| `updated_at` | TEXT | NOT NULL DEFAULT CURRENT_TIMESTAMP |

**`audit_events`** (0001)
| Column | Type | Constraints |
| --- | --- | --- |
| `id` | TEXT | PRIMARY KEY (UUID string) |
| `event_type` | TEXT | NOT NULL |
| `payload` | TEXT | NOT NULL (JSON string) |
| `created_at` | TEXT | NOT NULL DEFAULT CURRENT_TIMESTAMP; written as RFC3339 by `record_audit_event` |

**`sources`** (0001 + 0003 + 0004)
| Column | Type | Constraints |
| --- | --- | --- |
| `id` | TEXT | PRIMARY KEY |
| `name` | TEXT | NOT NULL **UNIQUE** |
| `url` | TEXT | NOT NULL |
| `kind` | TEXT | NOT NULL |
| `enabled` | INTEGER | NOT NULL DEFAULT 1 |
| `created_at` | TEXT | NOT NULL DEFAULT CURRENT_TIMESTAMP |
| `updated_at` | TEXT | NOT NULL DEFAULT CURRENT_TIMESTAMP |
| `refresh_interval_minutes` | INTEGER | NOT NULL DEFAULT 60 *(0003)* |
| `profile` | TEXT | NOT NULL DEFAULT `'balanced'` *(0003)* |
| `verification_strictness` | TEXT | NOT NULL DEFAULT `'balanced'` *(0004)* |

**`rulesets`** (0001 + 0002)
| Column | Type | Constraints |
| --- | --- | --- |
| `id` | TEXT | PRIMARY KEY |
| `hash` | TEXT | NOT NULL |
| `status` | TEXT | NOT NULL — values used: `'active'`, `'previous'`, plus whatever the caller passes |
| `created_at` | TEXT | NOT NULL (RFC3339, written by `record_ruleset`) |
| `artifact_json` | TEXT | NOT NULL DEFAULT `'{}'` *(0002)* — serialized `RulesetArtifact` |

**`active_ruleset`** (0001)
| Column | Type | Constraints |
| --- | --- | --- |
| `slot` | INTEGER | PRIMARY KEY **CHECK (slot = 1)** — single-row table |
| `ruleset_id` | TEXT | FK → `rulesets(id)` |
| `activated_at` | TEXT | |

Seeded with `INSERT OR IGNORE INTO active_ruleset (slot, ruleset_id, activated_at) VALUES (1, NULL, NULL);`.

**`devices`** (0005 + 0006 + 0007 + 0008)
| Column | Type | Constraints |
| --- | --- | --- |
| `id` | TEXT | PRIMARY KEY |
| `name` | TEXT | NOT NULL |
| `ip_address` | TEXT | NOT NULL **UNIQUE** |
| `policy_mode` | TEXT | NOT NULL DEFAULT `'global'` (`"custom"` activates per-device logic in dns-core) |
| `blocklist_profile_override` | TEXT | nullable |
| `created_at` | TEXT | NOT NULL DEFAULT CURRENT_TIMESTAMP |
| `updated_at` | TEXT | NOT NULL DEFAULT CURRENT_TIMESTAMP |
| `protection_override` | TEXT | NOT NULL DEFAULT `'inherit'` *(0006)*; `"bypass"` is honored in dns-core |
| `allowed_domains_json` | TEXT | NOT NULL DEFAULT `'[]'` *(0007)* — JSON array of strings |
| `service_overrides_json` | TEXT | NOT NULL DEFAULT `'[]'` *(0008)* — JSON array of `{service_id, mode}` |

**`security_events`** (0005)
| Column | Type | Constraints |
| --- | --- | --- |
| `id` | TEXT | PRIMARY KEY |
| `device_id` | TEXT | nullable, **FK → devices(id)** (enforced: `foreign_keys=ON`) |
| `device_name` | TEXT | nullable |
| `client_ip` | TEXT | NOT NULL |
| `domain` | TEXT | NOT NULL |
| `classifier_score` | REAL | NOT NULL (mapped to `f64` in Rust; the classifier produces `f32`) |
| `severity` | TEXT | NOT NULL |
| `created_at` | TEXT | NOT NULL (RFC3339) |

No index on `created_at` despite `recent_security_events` doing `ORDER BY created_at DESC LIMIT ?`.

**`notification_deliveries`** (0009)
| Column | Type | Constraints |
| --- | --- | --- |
| `id` | TEXT | PRIMARY KEY |
| `event_type` | TEXT | NOT NULL |
| `status` | TEXT | NOT NULL |
| `severity` | TEXT | NOT NULL |
| `title` | TEXT | NOT NULL |
| `summary` | TEXT | NOT NULL |
| `domain` | TEXT | NOT NULL |
| `device_name` | TEXT | nullable |
| `client_ip` | TEXT | NOT NULL |
| `attempts` | INTEGER | NOT NULL (`usize` in Rust, cast via `as i64` / `as usize`) |
| `created_at` | TEXT | NOT NULL (RFC3339) |

Plus `CREATE INDEX IF NOT EXISTS idx_notification_deliveries_created_at ON notification_deliveries(created_at DESC);`
— the only index in the schema besides implicit primary/unique keys.

**`config_schema`** (0010)
| Column | Type | Constraints |
| --- | --- | --- |
| `id` | INTEGER | PRIMARY KEY **CHECK (id = 1)** |
| `version` | INTEGER | NOT NULL DEFAULT 1 |
| `upgraded_at` | TEXT | NOT NULL DEFAULT `(datetime('now'))` |
| `cogwheel_version` | TEXT | nullable — **never written by any code today** |

Seeded `INSERT OR IGNORE … VALUES (1, 1, datetime('now'))`. Read by `get_config_version()`.

**`config_migrations`** (0010)
| Column | Type | Constraints |
| --- | --- | --- |
| `id` | INTEGER | PRIMARY KEY AUTOINCREMENT |
| `version` | INTEGER | NOT NULL **UNIQUE** |
| `applied_at` | TEXT | NOT NULL DEFAULT `(datetime('now'))` |
| `description` | TEXT | nullable |

Seeded with `INSERT INTO config_migrations (version, description) VALUES (1, 'Initial config schema version');`
— the statement that fails on every subsequent startup (§7.6). No Rust code reads or writes this
table.

**Timestamp format inconsistency (real trap):** `rulesets`, `audit_events`, `security_events` and
`notification_deliveries` store `created_at` as **RFC3339** (written via `.to_rfc3339()`), and
`parse_datetime` (line 654) accepts **RFC3339 only**. `settings.updated_at`, `sources.created_at/updated_at`,
`devices.created_at/updated_at`, `config_schema.upgraded_at` and `config_migrations.applied_at` use
SQLite's `CURRENT_TIMESTAMP` / `datetime('now')` format (`YYYY-MM-DD HH:MM:SS`), which
`parse_datetime` will **reject**. No current code path parses those columns; if you expose them,
parse them with a SQLite-format-aware parser or migrate the writers to RFC3339.

### 7.8 Ruleset lifecycle

- `record_ruleset` does a plain `INSERT` (not `INSERT OR REPLACE`) — a duplicate `id` errors.
- `activate_ruleset` (line 482) runs one transaction:
  ```sql
  UPDATE rulesets SET status = 'previous' WHERE status = 'active';
  UPDATE rulesets SET status = 'active'   WHERE id = ?1;
  UPDATE active_ruleset SET ruleset_id = ?1, activated_at = CURRENT_TIMESTAMP WHERE slot = 1;
  ```
  It does not verify the target exists; activating an unknown id leaves zero `'active'` rows and
  sets `active_ruleset.ruleset_id` to a dangling value (the FK would reject it, so the transaction
  errors — but only because `foreign_keys=ON`).
- `'previous'` rows **accumulate without bound**; `previous_ruleset` picks
  `WHERE status='previous' ORDER BY created_at DESC LIMIT 1` (lexicographic ordering of RFC3339
  strings — correct as long as every writer uses UTC `Z` offsets, which `to_rfc3339()` on a
  `DateTime<Utc>` does).
- `rollback_to_previous_ruleset` returns `Ok(None)` when there is no previous ruleset; otherwise it
  activates it and returns `serde_json::from_str::<RulesetArtifact>(&previous.artifact_json)?`.
  Rows written before migration 0002 have `artifact_json = '{}'` and will fail to deserialize.

### 7.9 Node identity and signed sync envelopes

- On `connect`, the key is loaded from settings key `node_identity_v1` (base64
  `URL_SAFE_NO_PAD`, 32 bytes → `SigningKey::from_bytes`), or generated with
  `SigningKey::generate(&mut OsRng)` and inserted. Wrong length/encoding → `StorageError::Internal`.
- `public_b64` = `URL_SAFE_NO_PAD` of `verifying_key().to_bytes()`.
- Signing message layout (`sync_signing_message`, line 137):
  `timestamp.to_rfc3339().as_bytes() || b'|' || nonce.as_bytes() || b'|' || payload`.
- `sign_sync_payload` mints `Utc::now()` and `Uuid::new_v4().to_string()` as the nonce, signs, and
  returns everything base64 `URL_SAFE_NO_PAD`.
- `verify_sync_envelope` is an **associated function** (no `&self`): decodes the public key
  (must be exactly 32 bytes), the signature (exactly 64 bytes), and the payload, rebuilds the
  message, and verifies. Every failure maps to `StorageError::Internal(<static message>)`:
  `"invalid public key base64"`, `"invalid public key length"`, `"invalid verifying key bytes"`,
  `"invalid signature base64"`, `"invalid signature length"`, `"invalid payload base64"`,
  `"signature verification failed"`.
- **The envelope carries no replay protection of its own** — no timestamp-freshness check and no
  nonce ledger. Replay defense lives in the server (`sync_seen_nonces: Arc<Mutex<HashMap<…>>>` in
  `ServerState`), not here.

### 7.10 Data-decoding hazards

- Every UUID column decode uses `Uuid::parse_str(&row.get::<_, String>(0)?).expect("valid uuid in database")`
  (8 sites: lines 301, 360, 389, 438, 443, 558, 604, 646). A malformed row **panics inside the
  rusqlite row callback**, which will poison the storage mutex and cascade.
- `list_devices` / `find_device_by_ip` decode both JSON columns with
  `serde_json::from_str(...).unwrap_or_default()` — corrupt JSON silently becomes an empty vector,
  meaning a device silently loses its allowlist / service overrides instead of erroring.
- `StorageError::Uuid` and `StorageError::Chrono` variants exist but are effectively unreachable
  through the current code paths (uuid errors panic instead; chrono errors are wrapped via
  `to_sqlite_error` into `StorageError::Sqlite`).

### 7.11 Tests present: **zero**

`crates/cogwheel-storage/src/lib.rs` has no `#[cfg(test)]` module and there is no `tests/`
directory. Migrations, the settings KV, the ruleset lifecycle, and envelope signing/verification are
entirely untested. Any storage work should land with tests — `Storage::connect` accepts a plain path
(the `sqlite://` prefix is optional), so a temp-file DB is a one-liner.

---

## 8. `cogwheel-sync`

Path: `/home/user/Cogwheel-DNS/crates/cogwheel-sync/src/lib.rs` (17 lines — the entire crate).
Deps: `chrono`, `serde`, `uuid`. No path deps.

```rust
pub struct NodeIdentity { pub node_id: Uuid, pub display_name: String }
// Debug, Clone, Serialize, Deserialize, PartialEq, Eq

pub struct SyncEnvelope {
    pub revision: u64,
    pub issued_at: DateTime<Utc>,
    pub node: NodeIdentity,
    pub settings_hash: String,
}
// Debug, Clone, Serialize, Deserialize, PartialEq, Eq
```

No functions, no impls, no tests. **Nothing in the workspace references `cogwheel_sync`** — a grep
across `apps/` and `crates/` for `cogwheel_sync` returns no Rust matches. It is declared as a
dependency of `apps/cogwheel-server` but unused. The sync feature that actually ships is implemented
with `cogwheel_storage::{SyncEnvelope, Storage::sign_sync_payload, Storage::verify_sync_envelope}`
plus server-side state. Both `NodeIdentity` and `SyncEnvelope` here shadow same-named types in
`cogwheel-storage`. ADR 0001 assigns this crate ownership of "node identity, signed envelopes,
revision conflict resolution, replication profiles, and replay protection" — none of which it
implements. Treat it as a reserved namespace, and if you consolidate, update ADR 0001 and the
boundary test together.

---

## 9. Lints and the mandated error-handling patterns

### 9.1 What is declared

`/home/user/Cogwheel-DNS/Cargo.toml`:

```toml
[workspace.lints.clippy]
dbg_macro   = "deny"
todo        = "deny"
unwrap_used = "deny"
panic       = "deny"
```

### 9.2 What is actually enforced — read this carefully

**No crate in the workspace declares `[lints] workspace = true`.** A grep for `lints` across every
`Cargo.toml` returns exactly one hit: the `[workspace.lints.clippy]` table itself. Cargo only
applies workspace lints to packages that opt in with a `[lints] workspace = true` section, so
**today these four denies are inert** — `cargo clippy --workspace --all-targets --all-features -- -D warnings`
(the CI command) does not fail on `unwrap()`/`panic!` in non-test code. That is consistent with the
codebase containing 16 `.expect(...)` calls in `cogwheel-dns-core` and 29 in `cogwheel-storage`,
many in non-test code.

**Treat the declared policy as binding anyway.** Two reasons: (a) the intent is unambiguous, and
(b) a one-line fix (`[lints]\nworkspace = true` in each crate manifest) turns them on, at which
point every existing `.expect` becomes a hard error. Write new code as if the lints were live:

- **Never** `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`, `dbg!` in non-test code.
- If you *do* flip the lints on, you must first convert the existing `.expect` sites listed in
  §9.4 — that is a real, separately-scoped piece of work, not a drive-by.

### 9.3 Approved error-handling patterns already in the codebase

Copy these; they are the house style.

**Library crates return typed errors via `thiserror`.**
`cogwheel-storage` (line 28) and `cogwheel-api` (line 266) both do this:

```rust
#[derive(Debug, Error)]
pub enum StorageError {
    #[error(transparent)] Sqlite(#[from] rusqlite::Error),
    #[error(transparent)] Serde(#[from] serde_json::Error),
    #[error("internal storage error: {0}")] Internal(String),
}
```
with `?` doing the conversion, and manual mapping where no `From` exists:
```rust
.map_err(|_| StorageError::Internal("invalid public key base64".to_string()))?
```

**Binary-adjacent / orchestration code uses `anyhow` with `.context(...)`.**
`cogwheel-dns-core` returns `anyhow::Result<T>` and adds context at I/O boundaries:
```rust
let socket = UdpSocket::bind(bind_addr).await.context("bind udp socket")?;
let listener = TcpListener::bind(bind_addr).await.context("bind tcp listener")?;
let query = request.queries().first().cloned().context("dns query missing question")?;
```

**`let … else` for early returns instead of unwrap.** Used heavily in `policy_for_client`:
```rust
let Some(client_ip) = client_addr.map(|addr| addr.ip()) else {
    return (global.clone(), global.artifact().hash.clone(), None);
};
let Some(device) = devices.get(&client_ip) else { … };
```
and in `cogwheel-services`:
```rust
let Some(manifest) = manifest_map.get(toggle.service_id.as_str()) else {
    notes.push(format!("unknown service toggle ignored: {}", toggle.service_id));
    continue;
};
```

**`if let Ok(mut guard) = lock.write()` for infallible-by-design writes** (dns-core lines 146, 149,
152, 170, 184, 190, 196, 489, 495) — a poisoned lock becomes a silent no-op rather than a panic.

**`filter_map(...ok())` / `unwrap_or_default()` to drop bad input without failing** (dns-core
`replace_device_policies`; storage JSON column decode; `parse_data_url`).

**Swallow-and-degrade at the request boundary**: `serve_udp` converts any handler error into a
SERVFAIL response plus `tracing::warn!` rather than dropping the listener; `uncloaked_block_mode`
converts resolver errors into `Ok(None)`.

**Tests may use `unwrap`/`expect` freely** — that is the established convention
(`crates/cogwheel-lists/src/lib.rs:308`, `crates/cogwheel-classifier/src/lib.rs:95`,
`crates/cogwheel-dns-core/src/lib.rs:812`). Prefer `.expect("descriptive message")` over bare
`.unwrap()` in tests, matching dns-core's style.

### 9.4 Existing non-test panic sites (inventory for whoever enables the lints)

`crates/cogwheel-dns-core/src/lib.rs` — 9 non-test sites, all lock reads:
lines 179, 465, 476, 514, 520, 528, 554, 566, 578, all of the form
`.read().expect("<name> lock poisoned")`.

`crates/cogwheel-storage/src/lib.rs` — 29 non-test sites:
23 × `.lock().expect("storage mutex poisoned")` (lines 254, 264, 276, 295, 317, 326, 345, 354, 381,
409, 431, 458, 473, 483, 502, 514, 538, 552, 573, 598, 623) and 8 × UUID decode
`.expect("valid uuid in database")` / `.expect("valid optional uuid in database")`
(lines 301, 360, 389, 438, 443, 558, 604, 646).

`crates/cogwheel-lists/src/lib.rs:54` — `Url::parse("data:text/plain,").expect("valid synthetic url")`.

`crates/cogwheel-api/src/lib.rs` — several `"…".parse().expect("valid default addr")` in `Default`
impls and `for_profile` (lines 44, 59–61, 209–215, 222–229, 234–240), plus
`unwrap_or_else(|error| panic!(…))` inside the boundary test at line 396 (test code).

---

## 10. Test inventory (whole workspace, library crates)

No crate has a `tests/` directory; there are no integration tests, no benches, no doctests of
consequence. Everything is an inline `#[cfg(test)] mod tests`.

| Crate | Tests | Names |
| --- | --- | --- |
| `cogwheel-dns-core` | **10** | `runtime_snapshot_starts_at_zero`, `extract_cname_target_reads_record_data`, `build_probe_request_sets_expected_question`, `cached_response_adopts_request_id`, `error_response_uses_original_request_id`, `build_classification_event_preserves_client_ip`, `policy_cache_key_scopes_by_policy`, `build_allow_all_policy_removes_block_rules`, `domain_matches_override_supports_suffixes`, `hot_path_crates_remain_llm_and_network_independent` |
| `cogwheel-lists` | **5** | `adblock_suffix_and_allow_parse`, `data_url_body_parses`, `suffix_rule_can_fail_protected_domain_verification`, `synthetic_source_preserves_rules`, `strict_source_rejects_high_invalid_ratio` |
| `cogwheel-api` | **5** | `defaults_to_home_profile`, `dev_profile_applies_local_safe_ports`, `explicit_env_overrides_profile_defaults`, `invalid_profile_is_rejected`, `crate_path_dependencies_match_the_adr_boundaries` |
| `cogwheel-services` | **2** | `block_toggle_emits_block_and_exception_rules`, `allow_toggle_emits_allow_rules` |
| `cogwheel-policy` | **1** | `allow_precedes_block` |
| `cogwheel-classifier` | **1** | `high_entropy_domain_scores_higher` |
| `cogwheel-storage` | **0** | — |
| `cogwheel-sync` | **0** | — |

Two of these 24 tests are **architecture guardrails**, not behavior tests
(`hot_path_crates_remain_llm_and_network_independent`, `crate_path_dependencies_match_the_adr_boundaries`);
breaking either is a signal to update the corresponding doc, not to weaken the test silently.

Coverage gaps that matter most: the entire DNS request path, all of storage, the moka caches, device
policy resolution, and CNAME uncloaking.

---

## 11. Hazards and extension guidance (ranked)

Things an implementer will trip over. Each is a statement about current code, with the file to fix.

1. **The DNS cache key ignores the record type.**
   `policy_cache_key(scope, domain)` → `"{scope}:{domain}"` (dns-core:660), and the cached value is a
   whole `Message` whose answers are type-specific. An `A` lookup for `example.com` populates the
   entry; a subsequent `AAAA` lookup for the same name hits it and receives the **A-record response**
   with only the id rewritten. Same for MX/TXT/etc. Fix requires adding `query.query_type()` to the
   key (and to `fallback_cache`'s bare-`domain` key).
2. **The cache has no TTL.** `Cache::new(10_000)` (dns-core:130-131) is capacity-only. Upstream DNS
   TTLs are discarded; entries persist until LRU eviction or a policy swap. Add
   `moka::future::Cache::builder().max_capacity(..).time_to_live(..)` and derive the TTL from the
   minimum answer TTL.
3. **UDP serving is single-threaded and serialized** (dns-core:252-269) — see §6.2. Highest-leverage
   latency fix in the codebase.
4. **`PolicyEngine::evaluate` is O(rules) with a `format!` allocation per suffix rule** (policy:145).
   Second-highest-leverage fix; contained entirely behind `PolicyEngine`'s private field.
5. **The classifier runs before the cache lookup on every query** (dns-core:317-325), and
   `ClassifierMode::Protect` does nothing. See §6.6 for the required restructuring before any real
   model lands.
6. **`RulesetArtifact.hash` is non-deterministic** when `protected_domains` is non-empty
   (policy:65, HashSet iteration order) — and that hash is the DNS cache scope. Sort before hashing.
7. **Upstream NXDOMAIN becomes SERVFAIL.** `resolve_upstream` (dns-core:586) turns any resolver
   error into `Err`, which `serve_udp` maps to SERVFAIL. Negative answers are neither cached nor
   faithfully relayed.
8. **Migration 0010 fails on every restart by design** and any statement appended after its final
   `INSERT` would never run (§7.6). New migrations = new files only.
9. **Storage does blocking SQLite on async worker threads through one global mutex** (§7.4), and
   adding an `.await` while a guard is alive breaks `Send` across the whole server.
10. **Reader locks panic, writer locks silently no-op** in `DnsRuntime` (§6.5). One panic poisons the
    lock permanently.
11. **Workspace clippy denies are declared but not wired up** (§9.2). Fixing this is a real task with
    ~45 call sites of fallout.
12. **Observers are synchronous and run inline on the hot path** (dns-core:455/472). The `tokio::spawn`
    that makes them safe lives in `apps/cogwheel-server/src/main.rs:605`. Any new observer must do
    the same.
13. **`cogwheel-sync` is dead code** and its `SyncEnvelope`/`NodeIdentity` shadow the live
    `cogwheel-storage` types (§8). Import paths matter.
14. **`cogwheel-lists` declares an unused dependency on `cogwheel-services`** that cannot be removed
    without editing the ADR and the boundary test (§5.6).
15. **New dependencies must clear `deny.toml`'s license allow-list** (§1.1) and, for the two hot-path
    crates, the substring guardrail in dns-core:908 — which the `foo.workspace = true` form currently
    evades (§6.6).

### Quick "where do I put this?" table

| Change | Crate | Also update |
| --- | --- | --- |
| New rule-matching semantics / precedence | `cogwheel-policy` | policy tests; hash scheme if `Debug` reprs change |
| New blocklist format parser | `cogwheel-lists` (`parse_source` dispatch + a `parse_*_line`) | `SourceKind`, server mapping of `SourceRecord.kind` |
| New curated service bundle | `cogwheel-services` (`built_in_service_manifests`) | nothing else — it compiles to `Rule`s |
| Real ML scoring | `cogwheel-classifier` + call-site restructuring in `cogwheel-dns-core` | §6.6 steps (a)–(d); `deny.toml`; guardrail test |
| New per-device behavior | `cogwheel-dns-core::policy_for_client` + `DevicePolicyConfig` | `DeviceRecord`, a migration, the server's record→config conversion |
| New persisted tunable | `cogwheel-storage` settings KV (**no migration**) | add key to the §7.5 table |
| New queryable/indexed entity | new migration `0011_*.sql` + `apply_migrations` + `SCHEMA_VERSION` | §7.6 rules |
| New HTTP contract type | `cogwheel-api` | must not gain path deps (boundary test) |
| Process wiring / routes / background jobs | `apps/cogwheel-server` | — |
