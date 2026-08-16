//! Domain policy evaluation for Cogwheel.
//!
//! This is the leaf domain crate: [`PolicyEngine::evaluate`] is the single function that turns a
//! domain name into an allow/block [`Decision`], and everything else that filters DNS traffic
//! (`cogwheel-lists`, `cogwheel-dns-core`, `apps/cogwheel-server`) is built on top of it. It has no
//! path dependencies of its own, so changes here ripple outward rather than in.
//!
//! # Evaluation order
//!
//! [`PolicyEngine::evaluate`] checks, in this fixed order:
//!
//! 1. **Protected domains** — an exact (not suffix) match against
//!    [`RulesetArtifact::protected_domains`] always wins and returns [`DecisionKind::Allowed`]. A
//!    protected `example.com` does *not* protect `www.example.com`.
//! 2. **Allow rules** — the first matching [`RuleAction::Allow`] rule wins.
//! 3. **Block rules** — the first matching [`RuleAction::Block`] rule wins, and the decision
//!    carries the artifact's single, ruleset-wide [`BlockMode`] — there is no per-rule block mode.
//! 4. Otherwise the domain is allowed by default.
//!
//! Allow beats Block regardless of the order rules appear in, because the two actions are matched
//! in two independent passes; the `allow_precedes_block` test pins this.
//!
//! # Hot path
//!
//! [`PolicyEngine::evaluate`] runs once per DNS query that misses the response cache, so its
//! allocation and complexity behaviour is part of the product's latency budget. See its own doc
//! comment for what that behaviour actually is today.
//!
//! # Normalisation
//!
//! Rule patterns and lookup domains must agree on case and trailing-dot form or matching silently
//! fails. [`normalize_domain`] is the one normalisation routine for both; see its docs.

#![warn(missing_docs)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{Ipv4Addr, Ipv6Addr};
use uuid::Uuid;

/// What a client receives for a query resolved to [`DecisionKind::Blocked`].
///
/// This is a property of the whole [`RulesetArtifact`], not of the rule that matched — every
/// block decision made under one artifact returns the same `BlockMode`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BlockMode {
    /// Answer with an all-zeros address (`0.0.0.0` / `::`) in place of the real one.
    NullIp,
    /// Answer `NXDOMAIN`, as if the name did not exist.
    NxDomain,
    /// Answer `NOERROR` with an empty answer section.
    NoData,
    /// Answer `REFUSED`.
    Refused,
    /// Answer with an operator-chosen address in place of the real one.
    CustomIp {
        /// Address returned for `A` queries, if configured.
        ipv4: Option<Ipv4Addr>,
        /// Address returned for `AAAA` queries, if configured.
        ipv6: Option<Ipv6Addr>,
    },
}

/// How a [`Rule`] matches a normalised domain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RulePattern {
    /// Matches only this exact domain.
    Exact(String),
    /// Matches this domain and every subdomain beneath it.
    Suffix(String),
}

/// Whether a matching [`Rule`] allows or blocks a domain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuleAction {
    /// Permit resolution. Checked before, and always wins over, `Block`.
    Allow,
    /// Deny resolution, subject to the ruleset's [`BlockMode`].
    Block,
}

/// A single allow/block rule contributed by one source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rule {
    /// Domain pattern this rule matches against.
    pub pattern: RulePattern,
    /// Whether a match allows or blocks the domain.
    pub action: RuleAction,
    /// Name of the source that contributed this rule (a blocklist name, `"override"`, a
    /// `service:<id>` tag, and so on), carried through to [`Decision::matched_rule`] for
    /// attribution.
    pub source: String,
    /// Free-text note about why the rule exists, if any.
    pub comment: Option<String>,
}

/// The outcome of evaluating a domain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DecisionKind {
    /// The domain resolves normally.
    Allowed,
    /// The domain is blocked; the client receives the response described by this [`BlockMode`].
    Blocked(BlockMode),
}

/// The result of [`PolicyEngine::evaluate`] for one domain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Decision {
    /// The domain that was evaluated, after [`normalize_domain`].
    pub domain: String,
    /// Allow or block, and — for a block — the response the client should receive.
    pub kind: DecisionKind,
    /// The rule that produced this decision, cloned rather than borrowed so a `Decision` can
    /// outlive the engine it came from. `None` for a protected-domain hit or the no-match default.
    pub matched_rule: Option<Rule>,
    /// Why this decision was reached: one of `"protected domain"`, `"matched allow rule"`,
    /// `"matched block rule"` or `"no matching rule"`. Treat this as a stable enum encoded as
    /// text — other code matches on these strings literally.
    pub reason: String,
}

/// A compiled, hashable set of rules ready to be loaded into a [`PolicyEngine`].
///
/// `hash` identifies the artifact's content for promotion and rollback: the storage layer records
/// each compiled artifact keyed by this hash with a status of active or previous, and rolling back
/// means loading the previous artifact back into a fresh [`PolicyEngine`]. The hash is also used
/// as the DNS response cache's scope key, so two artifacts sharing a hash are treated as
/// interchangeable for caching purposes.
///
/// # Hash stability
///
/// The hash is derived from the `Debug` formatting of [`RuleAction`], [`RulePattern`] and
/// [`BlockMode`] plus each rule's `source` and each protected domain's bytes (see
/// [`RulesetArtifact::new`]). Renaming an enum variant or adding a field therefore changes every
/// hash produced from otherwise-identical input. `protected_domains` is a `HashSet<String>` whose
/// iteration order is randomised per process, and that order feeds the hasher directly — so the
/// hash is **not** guaranteed reproducible across runs whenever `protected_domains` is non-empty,
/// even for byte-identical input. Anything that compares hashes for equality across process
/// restarts should be aware of this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesetArtifact {
    /// Unique identifier for this artifact, freshly minted by [`RulesetArtifact::new`].
    pub id: Uuid,
    /// Lowercase-hex SHA-256 content hash. See the type-level docs for its non-determinism
    /// caveat.
    pub hash: String,
    /// When this artifact was compiled.
    pub created_at: DateTime<Utc>,
    /// Every rule in the artifact, from every contributing source, in no particular precedence
    /// order — see [`PolicyEngine::evaluate`] for how precedence actually works.
    pub rules: Vec<Rule>,
    /// Domains that are always allowed regardless of `rules`, checked by exact match only.
    pub protected_domains: HashSet<String>,
    /// The response a client receives for any domain this artifact blocks.
    pub block_mode: BlockMode,
}

impl RulesetArtifact {
    /// Compile rules and protected domains into a new artifact, hashing their content.
    ///
    /// Mints a fresh [`Uuid`] and timestamp on every call, so two artifacts built from identical
    /// inputs are never equal by `id` or `created_at` — `hash` is the only content identifier, and
    /// see its docs for why even that is not guaranteed stable.
    pub fn new(
        rules: Vec<Rule>,
        protected_domains: HashSet<String>,
        block_mode: BlockMode,
    ) -> Self {
        let mut hasher = Sha256::new();
        for rule in &rules {
            hasher.update(format!(
                "{:?}:{:?}:{}",
                rule.action, rule.pattern, rule.source
            ));
        }
        // Sort before hashing. `protected_domains` is a `HashSet`, whose iteration order is
        // randomised per process, so hashing it directly produced a DIFFERENT hash for
        // byte-identical input on every restart. That hash is the ruleset's content identity: it
        // scopes the DNS response cache and keys promotion and rollback in storage, so an unstable
        // value meant a restart could not recognise its own active ruleset.
        let mut protected_sorted: Vec<&str> =
            protected_domains.iter().map(String::as_str).collect();
        protected_sorted.sort_unstable();
        for domain in protected_sorted {
            hasher.update(domain.as_bytes());
        }
        hasher.update(format!("{:?}", block_mode));

        Self {
            id: Uuid::new_v4(),
            hash: format!("{:x}", hasher.finalize()),
            created_at: Utc::now(),
            rules,
            protected_domains,
            block_mode,
        }
    }
}

/// A [`RulesetArtifact`] loaded and ready to evaluate domains against.
///
/// Cheap to construct and clone — it wraps one owned artifact. Callers such as
/// `cogwheel-dns-core` hold one behind an `Arc` per policy scope and swap it wholesale when a new
/// ruleset is activated.
#[derive(Debug, Clone)]
pub struct PolicyEngine {
    artifact: RulesetArtifact,
}

impl PolicyEngine {
    /// Wrap an artifact for evaluation.
    pub fn new(artifact: RulesetArtifact) -> Self {
        Self { artifact }
    }

    /// The artifact this engine evaluates against.
    pub fn artifact(&self) -> &RulesetArtifact {
        &self.artifact
    }

    /// Decide whether `domain` is allowed or blocked.
    ///
    /// This is the inner loop of the DNS hot path: it runs once per query that misses the
    /// response cache. It has no I/O and no interior mutability, but it is not free —
    /// [`normalize_domain`] allocates a new `String` on every call, and rule matching is a linear
    /// scan of `artifact.rules`, performed up to twice (once looking for a matching
    /// [`RuleAction::Allow`] rule, and — only if none matches — again for
    /// [`RuleAction::Block`]). Cost therefore scales with ruleset size: a ruleset with hundreds of
    /// thousands of rules means a proportionally large number of comparisons per cache miss, and
    /// each [`RulePattern::Suffix`] candidate compared during a scan allocates its own `String`
    /// (`format!(".{candidate}")`) to test the suffix. See [`RuleAction`], [`RulePattern`] and
    /// [`DecisionKind`] for what the returned [`Decision`] can contain.
    pub fn evaluate(&self, domain: &str) -> Decision {
        let normalized = normalize_domain(domain);

        if self.artifact.protected_domains.contains(&normalized) {
            return Decision {
                domain: normalized,
                kind: DecisionKind::Allowed,
                matched_rule: None,
                reason: "protected domain".to_string(),
            };
        }

        if let Some(rule) = self.find_rule(&normalized, RuleAction::Allow) {
            return Decision {
                domain: normalized,
                kind: DecisionKind::Allowed,
                matched_rule: Some(rule.clone()),
                reason: "matched allow rule".to_string(),
            };
        }

        if let Some(rule) = self.find_rule(&normalized, RuleAction::Block) {
            return Decision {
                domain: normalized,
                kind: DecisionKind::Blocked(self.artifact.block_mode.clone()),
                matched_rule: Some(rule.clone()),
                reason: "matched block rule".to_string(),
            };
        }

        Decision {
            domain: normalized,
            kind: DecisionKind::Allowed,
            matched_rule: None,
            reason: "no matching rule".to_string(),
        }
    }

    fn find_rule(&self, domain: &str, action: RuleAction) -> Option<&Rule> {
        self.artifact.rules.iter().find(|rule| {
            rule.action == action
                && match &rule.pattern {
                    RulePattern::Exact(candidate) => candidate == domain,
                    // Match the label boundary without building a string. This runs once per rule
                    // per query, and a compiled blocklist holds hundreds of thousands of rules, so
                    // the previous `format!(".{candidate}")` allocated and freed a String on every
                    // comparison -- millions of allocations per second of DNS traffic on a Pi.
                    RulePattern::Suffix(candidate) => {
                        domain == candidate
                            || (domain.len() > candidate.len()
                                && domain.ends_with(candidate.as_str())
                                && domain.as_bytes()[domain.len() - candidate.len() - 1] == b'.')
                    }
                }
        })
    }
}

/// Canonicalise a domain the same way for both rule storage and lookup.
///
/// Trims leading/trailing whitespace, strips a single trailing root dot (`"example.com."` →
/// `"example.com"`), and lowercases ASCII. Every [`Rule`] pattern is expected to have gone
/// through this before being stored, and every domain passed to [`PolicyEngine::evaluate`] is
/// normalised again before matching — callers that skip it, or that normalise differently, will
/// silently fail to match rules that look identical to a human.
pub fn normalize_domain(domain: &str) -> String {
    domain.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suffix_rule(pattern: &str, action: RuleAction) -> Rule {
        Rule {
            action,
            pattern: RulePattern::Suffix(pattern.to_string()),
            source: "test".to_string(),
            comment: None,
        }
    }

    /// The artifact hash is the ruleset's content identity: it scopes the DNS response cache and
    /// keys promotion and rollback. Hashing a `HashSet` in iteration order made it differ between
    /// processes for identical input, so a restart could not recognise its own active ruleset.
    #[test]
    fn artifact_hash_is_stable_across_protected_domain_ordering() {
        let rules = vec![suffix_rule("ads.example", RuleAction::Block)];

        let mut forward = HashSet::new();
        for domain in ["a.example", "b.example", "c.example", "d.example"] {
            forward.insert(domain.to_string());
        }
        let mut reverse = HashSet::new();
        for domain in ["d.example", "c.example", "b.example", "a.example"] {
            reverse.insert(domain.to_string());
        }

        let first = RulesetArtifact::new(rules.clone(), forward, BlockMode::NullIp);
        let second = RulesetArtifact::new(rules, reverse, BlockMode::NullIp);
        assert_eq!(
            first.hash, second.hash,
            "identical content must produce an identical hash regardless of set ordering"
        );
    }

    #[test]
    fn artifact_hash_still_changes_when_content_changes() {
        let protected: HashSet<String> = HashSet::from(["a.example".to_string()]);
        let one = RulesetArtifact::new(
            vec![suffix_rule("ads.example", RuleAction::Block)],
            protected.clone(),
            BlockMode::NullIp,
        );
        let two = RulesetArtifact::new(
            vec![suffix_rule("trackers.example", RuleAction::Block)],
            protected,
            BlockMode::NullIp,
        );
        assert_ne!(one.hash, two.hash, "different rules must hash differently");
    }

    /// Suffix matching must only match on a label boundary, and must not allocate to do it.
    #[test]
    fn suffix_rules_match_only_on_label_boundaries() {
        let artifact = RulesetArtifact::new(
            vec![suffix_rule("example.com", RuleAction::Block)],
            HashSet::new(),
            BlockMode::NullIp,
        );
        let engine = PolicyEngine::new(artifact);

        for blocked in ["example.com", "ads.example.com", "a.b.example.com"] {
            assert!(
                matches!(engine.evaluate(blocked).kind, DecisionKind::Blocked(_)),
                "{blocked} should match the suffix rule"
            );
        }
        for allowed in ["notexample.com", "example.com.evil.net", "myexample.com"] {
            assert!(
                matches!(engine.evaluate(allowed).kind, DecisionKind::Allowed),
                "{allowed} must NOT match the suffix rule"
            );
        }
    }

    #[test]
    fn allow_precedes_block() {
        let rules = vec![
            Rule {
                pattern: RulePattern::Suffix("ads.example.com".to_string()),
                action: RuleAction::Block,
                source: "blocklist".to_string(),
                comment: None,
            },
            Rule {
                pattern: RulePattern::Exact("ads.example.com".to_string()),
                action: RuleAction::Allow,
                source: "override".to_string(),
                comment: None,
            },
        ];

        let engine = PolicyEngine::new(RulesetArtifact::new(
            rules,
            HashSet::new(),
            BlockMode::NullIp,
        ));
        let decision = engine.evaluate("ads.example.com");

        assert!(matches!(decision.kind, DecisionKind::Allowed));
        assert_eq!(decision.reason, "matched allow rule");
    }
}
