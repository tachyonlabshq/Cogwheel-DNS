# Cogwheel Deep Research

Date: 2026-03-04
Scope: Architecture and implementation research for a Docker-first, Rust-based, AI-managed DNS blocker positioned as an alternative to Pi-hole and AdGuard Home.

## 1. Executive Direction

Cogwheel should be built as a two-plane system:

1) A deterministic DNS data plane for low-latency request handling and strict policy enforcement.
2) An AI-assisted control plane that suggests, verifies, and schedules safe policy changes.

The product differentiator should be:

- Verified blocklist updates before promotion.
- Continuous domain monitoring to reduce breakage and keep blocking effectiveness high.
- Adaptive response behavior to reduce obvious anti-block detection signals.

## 2. Product Requirements Captured From Current Vision

- Monitor commonly connected domain requests.
- Verify updates before changing active blocklists.
- Periodically monitor domains to keep ad blocking effective and reduce bypass/breakage.
- Run reliably in Docker with a Rust backend and production-ready observability.

## 3. Research Findings

### 3.1 Pi-hole and AdGuard architectural patterns worth copying

Findings:

- Pi-hole uses a structured SQLite model (`gravity.db`) with explicit source metadata, parsed domain stats, and compiled effective tables.
- Pi-hole documents deterministic priority ordering between allowlists, denylists, and regex rules.
- AdGuard Home separates filtering pipeline concerns: request-time filtering, response-time checks, query logging, stats, and management APIs.
- Both ecosystems show that update orchestration and rollback safety are critical for DNS reliability.

Implications for Cogwheel:

- Keep raw source data separate from compiled active policy tables.
- Store source health and parsing quality indicators (`invalid_count`, last success, status).
- Enforce a deterministic precedence matrix in one place and test it with fixtures.
- Use staged builds plus atomic promotion, never in-place destructive updates.

### 3.2 Rule syntax and compatibility constraints

Findings:

- DNS filtering commonly supports three input formats: Adblock-style, hosts-file style, and domain-only.
- High-value Adblock modifiers include client scoping, DNS record-type scoping, and DNS rewrite directives.
- Unsupported modifiers should be ignored safely rather than misinterpreted.

Implications for Cogwheel:

- Ingest all three formats into one canonical internal rule model.
- Keep parser behavior strict and explicit: unsupported modifier -> ignored rule with warning.
- Build rule precedence tests for exact, wildcard, regex, exception, client-scoped, and rewrite interactions.

### 3.3 Blocking response modes and anti-detection behavior

Findings:

- Common blocked-answer modes include null IP, NXDOMAIN, NODATA, REFUSED, and custom sinkhole IP.
- Pi-hole documentation recommends null-IP style defaults in many environments due to fewer retries/timeouts.
- AdGuard exposes multiple block response strategies and TTL controls.

Implications for Cogwheel:

- Make blocking response mode configurable globally and overridable by policy profile.
- Add an adaptive mode per domain category (tracking, telemetry, ad CDN, malware) to reduce site breakage.
- Track breakage indicators by mode and support controlled migration between modes.

### 3.4 DNS standards for cache correctness and resiliency

Findings:

- RFC 2308: negative caching keys/semantics differ for NXDOMAIN and NODATA; TTL behavior should follow SOA-derived limits.
- RFC 8767: serve-stale allows returning expired cache entries during upstream failures with bounded timers and short stale TTL.

Implications for Cogwheel:

- Implement standards-aligned negative cache keys and bounded negative TTL caps.
- Add optional serve-stale behavior with conservative defaults and observable metrics.
- Continue background refresh after stale answers to converge quickly after outage.

### 3.5 Rust stack viability

Findings:

- Hickory DNS crate family provides async resolver/server/protocol components suitable for Rust DNS services.
- Public ecosystem notes indicate active DNSSEC hardening, but performance characteristics vary by forwarding workload.
- Moka provides async-safe in-memory caching primitives; SQLx provides async typed persistence with migrations.

Implications for Cogwheel:

- Build on Hickory-based components, but gate releases on load tests and packet-loss checks.
- Use Moka for hot-path transient caches and SQLx for durable policy/audit/query metadata.
- Add tracing and metrics from day one to identify bottlenecks before feature growth.

### 3.6 Docker deployment and runtime constraints

Findings:

- `network_mode: host` is practical for DNS services on Linux but has host port collision constraints.
- Compose health checks and dependency conditions are essential for startup ordering.

Implications for Cogwheel:

- Prefer host-network deployment profile for appliance-like installs on Linux.
- Add startup preflight checks for port 53 conflicts and local resolver collisions.
- Expose robust liveness/readiness probes for DNS and control-plane services.

### 3.7 Metrics and observability direction

Findings:

- Prometheus naming best practices emphasize clear prefixes, units, `_total` for counters, and low-cardinality labels.

Implications for Cogwheel:

- Use a strict metric naming and cardinality policy from the beginning.
- Avoid raw domain/client labels in hot metrics; use bounded buckets, sampling, or separate event logs.

## 4. Recommended System Architecture

## 4.1 Logical services

1) `cogwheel-dns` (data plane)
- UDP/TCP DNS server on :53.
- Fast policy lookup and response generation.
- Upstream forwarding and cache handling.

2) `cogwheel-policy`
- Canonical rule model and precedence engine.
- Allow/deny/rewrite/client-scope decisions.

3) `cogwheel-updater`
- Source fetching, parsing, normalization, verification, and atomic publish.

4) `cogwheel-monitor`
- Periodic domain checks, quality scoring, breakage detection, and recommendation generation.

5) `cogwheel-api`
- Authenticated management API for config, lists, clients, audit, and metrics views.

6) `cogwheel-ai-manager`
- Suggests policy and source updates, but only through verification gates and audit trail.

7) `cogwheel-ui` (optional initial phase)
- Dashboard for status, rule explainability, and pending recommendations.

## 4.2 Suggested storage model

Use SQLite initially for appliance simplicity, with migration path to Postgres for scale.

Core tables to define early:

- `sources`: URL, type, enabled, update_interval, etag, last_modified.
- `source_fetch_runs`: run_id, status, fetched_at, http_status, bytes, parse_errors.
- `rules_raw`: normalized parsed records per source before verification.
- `rules_effective`: compiled active ruleset with priority and hash.
- `clients`: identity (IP/CIDR/MAC/name), tags, policy profile.
- `client_policy_overrides`: per-client exceptions and rewrites.
- `query_events`: sampled/full logs with decision, latency, upstream, answer summary.
- `domain_health`: periodic check outcomes and confidence score.
- `policy_recommendations`: AI/heuristic recommendations with evidence and status.
- `audit_log`: every config/policy update with actor and diff.

## 4.3 DNS query lifecycle (target behavior)

1) Receive query and derive client identity.
2) Evaluate exact/regex/client-scoped allow exceptions.
3) Evaluate deny/rewrite rules (including CNAME-aware follow-up checks).
4) If blocked, generate response via configured mode.
5) If not blocked, resolve from cache or forward upstream.
6) Optionally apply response-time checks and rewritten answers.
7) Emit async event for logs/metrics/monitoring queue.

## 4.4 Verified blocklist update pipeline (must-have)

Proposed stages:

1) Fetch stage: download with ETag/If-Modified-Since and checksum.
2) Parse stage: parse per source format into canonical rules.
3) Sanity stage: reject malformed or absurdly explosive patterns.
4) Quality stage: dedup, invalid-domain ratio checks, volume anomaly checks.
5) Conflict stage: detect collisions with protected allowlist and critical domains.
6) Probe stage: run DNS health probes against sampled high-impact candidate rules.
7) Score stage: compute risk score and confidence for promotion.
8) Canary stage: optional subset-client rollout in monitor mode.
9) Publish stage: atomically swap active ruleset pointer to new compiled set.
10) Rollback stage: auto-revert on SLO breach or sharp false-positive indicators.

Design constraints:

- Never delete previous active set before successful publish.
- Every promoted set must have immutable `ruleset_id`, hash, and provenance.
- All decisions are auditable and reversible.

## 4.5 Periodic domain monitoring strategy

Build a dynamic watchset using:

- Top queried domains in last 1h/24h/7d.
- Newly blocked domains after each update.
- User-marked critical domains.
- Historically problematic anti-block detection endpoints.

Run periodic checks with jittered schedules:

- DNS resolution checks across primary and backup upstreams.
- CNAME chain checks for cloaked trackers and evasive domains.
- Lightweight HTTP status checks (optional) for critical domains.
- Stability scoring (availability, volatility, false-positive signals).

Action outputs:

- Recommendation to keep, downgrade aggressiveness, exception, or remove stale rule.
- Confidence score and supporting evidence.
- Optional auto-apply only for low-risk actions with policy guardrails.

## 4.6 AI manager operating model (safety-first)

AI should not directly mutate production rules without controls.

Allowed:

- Suggest update windows.
- Propose source additions/removals with evidence.
- Suggest temporary exceptions during detected breakage.

Required controls:

- Human approval for high-impact actions.
- Policy guardrails and denylist/allowlist protected zones.
- Explainability payload per recommendation.
- Full audit trail and one-click rollback.

## 5. Security and Privacy Baseline

- Encrypt management endpoints (TLS) and require strong auth.
- Minimize query-log retention; support anonymized client logging mode.
- Never include secrets in recommendation logs.
- Add supply-chain checks for source ingestion and container images.
- Sign release artifacts and publish SBOM.

## 6. Performance and SLO Targets (initial)

- p50 DNS latency under normal load: under 5 ms local cache hit.
- p95 DNS latency under normal load: under 20 ms.
- Blocklist publish operation: no request path interruption.
- Crash recovery: service healthy within 30 seconds.
- Rule decision determinism: 100 percent reproducible across identical ruleset hash.

## 7. Primary Risks and Mitigations

1) False positives from aggressive lists.
- Mitigation: staged verification, confidence scoring, protected domains, rapid rollback.

2) Throughput drop or packet loss under load.
- Mitigation: load tests before feature expansion, async backpressure, cache tuning.

3) Update corruption or partial apply.
- Mitigation: immutable staged artifacts + atomic pointer swap.

4) High-cardinality observability blowups.
- Mitigation: metric label governance and sampled event storage.

5) User trust concerns around AI automation.
- Mitigation: explainability, approval gates, and strict auditability.

## 8. Practical Build Order (first 30 days)

Week 1:
- Bootstrap Rust workspace and base DNS forwarder with cache and metrics.

Week 2:
- Implement policy engine with deterministic precedence and tests.

Week 3:
- Build list fetch/parse/compile pipeline and atomic publish path.

Week 4:
- Add periodic domain monitor, recommendation engine (heuristic-first), and control API endpoints.

## 9. Source References

- Pi-hole domain database documentation: https://docs.pi-hole.net/database/domain-database/
- Pi-hole blocking modes: https://docs.pi-hole.net/ftldns/blockingmode/
- AdGuard Home technical doc: https://raw.githubusercontent.com/AdguardTeam/AdGuardHome/master/AGHTechDoc.md
- AdGuard DNS filtering syntax: https://adguard-dns.io/kb/general/dns-filtering-syntax/
- AdGuard DNS filtering overview: https://adguard-dns.io/kb/general/dns-filtering/
- AdGuard Home configuration wiki: https://github.com/AdguardTeam/AdGuardHome/wiki/Configuration
- Hickory architecture: https://raw.githubusercontent.com/hickory-dns/hickory-dns/main/ARCHITECTURE.md
- Hickory server crate docs: https://docs.rs/hickory-server/latest/hickory_server/
- Moka async cache docs: https://docs.rs/moka/latest/moka/future/index.html
- SQLx crate docs: https://docs.rs/sqlx/latest/sqlx/
- RFC 2308 (negative caching): https://datatracker.ietf.org/doc/html/rfc2308
- RFC 8767 (serve-stale): https://datatracker.ietf.org/doc/html/rfc8767
- Docker host network driver docs: https://docs.docker.com/engine/network/drivers/host/
- Docker compose healthcheck docs: https://docs.docker.com/reference/compose-file/services/#healthcheck
- Prometheus metric naming best practices: https://prometheus.io/docs/practices/naming/
