use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{Ipv4Addr, Ipv6Addr};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BlockMode {
    NullIp,
    NxDomain,
    NoData,
    Refused,
    CustomIp {
        ipv4: Option<Ipv4Addr>,
        ipv6: Option<Ipv6Addr>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RulePattern {
    Exact(String),
    Suffix(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuleAction {
    Allow,
    Block,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rule {
    pub pattern: RulePattern,
    pub action: RuleAction,
    pub source: String,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DecisionKind {
    Allowed,
    Blocked(BlockMode),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Decision {
    pub domain: String,
    pub kind: DecisionKind,
    pub matched_rule: Option<Rule>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesetArtifact {
    pub id: Uuid,
    pub hash: String,
    pub created_at: DateTime<Utc>,
    pub rules: Vec<Rule>,
    pub protected_domains: HashSet<String>,
    pub block_mode: BlockMode,
}

impl RulesetArtifact {
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
        for domain in &protected_domains {
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

#[derive(Debug, Clone)]
pub struct PolicyEngine {
    artifact: RulesetArtifact,
}

impl PolicyEngine {
    pub fn new(artifact: RulesetArtifact) -> Self {
        Self { artifact }
    }

    pub fn artifact(&self) -> &RulesetArtifact {
        &self.artifact
    }

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
                    RulePattern::Suffix(candidate) => {
                        domain == candidate || domain.ends_with(&format!(".{candidate}"))
                    }
                }
        })
    }
}

pub fn normalize_domain(domain: &str) -> String {
    domain.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

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
