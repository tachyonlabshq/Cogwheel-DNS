use chrono::{DateTime, Utc};
use cogwheel_policy::{
    BlockMode, PolicyEngine, Rule, RuleAction, RulePattern, RulesetArtifact, normalize_domain,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SourceKind {
    Domains,
    Hosts,
    Adblock,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceDefinition {
    pub id: Uuid,
    pub name: String,
    pub url: Url,
    pub kind: SourceKind,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedSource {
    pub source: SourceDefinition,
    pub fetched_at: DateTime<Utc>,
    pub etag: Option<String>,
    pub checksum: String,
    pub rules: Vec<Rule>,
    pub invalid_lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub passed: bool,
    pub invalid_ratio: f32,
    pub blocked_protected_domains: Vec<String>,
    pub notes: Vec<String>,
}

pub fn parse_source(source: SourceDefinition, body: &str) -> ParsedSource {
    let mut rules = Vec::new();
    let mut invalid_lines = 0usize;

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }

        let parsed = match source.kind {
            SourceKind::Domains => parse_domain_line(trimmed, &source.name),
            SourceKind::Hosts => parse_hosts_line(trimmed, &source.name),
            SourceKind::Adblock => parse_adblock_line(trimmed, &source.name),
        };

        match parsed {
            Some(rule) => rules.push(rule),
            None => invalid_lines += 1,
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());

    ParsedSource {
        source,
        fetched_at: Utc::now(),
        etag: None,
        checksum: format!("{:x}", hasher.finalize()),
        rules,
        invalid_lines,
    }
}

pub fn verify_candidate(
    parsed: &[ParsedSource],
    protected_domains: &HashSet<String>,
) -> VerificationResult {
    let total_rules: usize = parsed
        .iter()
        .map(|entry| entry.rules.len() + entry.invalid_lines)
        .sum();
    let invalid_lines: usize = parsed.iter().map(|entry| entry.invalid_lines).sum();
    let invalid_ratio = if total_rules == 0 {
        0.0
    } else {
        invalid_lines as f32 / total_rules as f32
    };

    let blocked_protected_domains = parsed
        .iter()
        .flat_map(|entry| entry.rules.iter())
        .filter(|rule| matches!(rule.action, RuleAction::Block))
        .filter_map(|rule| match &rule.pattern {
            RulePattern::Exact(domain) | RulePattern::Suffix(domain)
                if protected_domains.contains(domain) =>
            {
                Some(domain.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut notes = Vec::new();
    if invalid_ratio > 0.2 {
        notes.push("invalid ratio exceeds 20%".to_string());
    }
    if !blocked_protected_domains.is_empty() {
        notes.push("candidate blocks protected domains".to_string());
    }

    VerificationResult {
        passed: invalid_ratio <= 0.2 && blocked_protected_domains.is_empty(),
        invalid_ratio,
        blocked_protected_domains,
        notes,
    }
}

pub fn compile_ruleset(
    parsed: Vec<ParsedSource>,
    protected_domains: HashSet<String>,
    block_mode: BlockMode,
) -> RulesetArtifact {
    let rules = parsed
        .into_iter()
        .flat_map(|entry| entry.rules.into_iter())
        .collect();
    RulesetArtifact::new(rules, protected_domains, block_mode)
}

pub fn build_policy_engine(
    parsed: Vec<ParsedSource>,
    protected_domains: HashSet<String>,
    block_mode: BlockMode,
) -> PolicyEngine {
    PolicyEngine::new(compile_ruleset(parsed, protected_domains, block_mode))
}

fn parse_domain_line(line: &str, source: &str) -> Option<Rule> {
    Some(Rule {
        pattern: RulePattern::Exact(normalize_domain(line)),
        action: RuleAction::Block,
        source: source.to_string(),
        comment: None,
    })
}

fn parse_hosts_line(line: &str, source: &str) -> Option<Rule> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }

    Some(Rule {
        pattern: RulePattern::Exact(normalize_domain(parts[1])),
        action: RuleAction::Block,
        source: source.to_string(),
        comment: Some(format!("mapped from {}", parts[0])),
    })
}

fn parse_adblock_line(line: &str, source: &str) -> Option<Rule> {
    let (action, candidate) = if let Some(rest) = line.strip_prefix("@@") {
        (RuleAction::Allow, rest)
    } else {
        (RuleAction::Block, line)
    };

    if let Some(domain) = candidate
        .strip_prefix("||")
        .and_then(|item| item.strip_suffix('^'))
    {
        return Some(Rule {
            pattern: RulePattern::Suffix(normalize_domain(domain)),
            action,
            source: source.to_string(),
            comment: None,
        });
    }

    if candidate.contains('$') || candidate.starts_with('/') {
        return None;
    }

    Some(Rule {
        pattern: RulePattern::Exact(normalize_domain(candidate)),
        action,
        source: source.to_string(),
        comment: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adblock_suffix_and_allow_parse() {
        let source = SourceDefinition {
            id: Uuid::new_v4(),
            name: "test".to_string(),
            url: Url::parse("https://example.com/list.txt").unwrap(),
            kind: SourceKind::Adblock,
            enabled: true,
        };
        let parsed = parse_source(source, "||ads.example.com^\n@@||cdn.example.com^");
        assert_eq!(parsed.rules.len(), 2);
        assert!(matches!(parsed.rules[0].pattern, RulePattern::Suffix(_)));
        assert!(matches!(parsed.rules[1].action, RuleAction::Allow));
    }
}
