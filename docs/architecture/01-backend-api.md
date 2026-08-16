# 01 — Backend HTTP API Contract Map (Cogwheel Server)

**Source of truth for this document:**

- `/home/user/Cogwheel-DNS/apps/cogwheel-server/src/main.rs` (6583 lines — read in full)
- `/home/user/Cogwheel-DNS/crates/cogwheel-api/src/lib.rs` (420 lines — read in full)
- Supporting crates read for type definitions: `crates/cogwheel-dns-core/src/lib.rs`, `crates/cogwheel-classifier/src/lib.rs`, `crates/cogwheel-storage/src/lib.rs`, `crates/cogwheel-policy/src/lib.rs`, `crates/cogwheel-services/src/lib.rs`, `crates/cogwheel-lists/src/lib.rs`
- Client consumer: `apps/cogwheel-web/src/lib/api.ts`

Line numbers in this document refer to the files as they exist at the time of writing. Downstream agents should NOT need to re-read the code to implement against this contract.

---

## 1. Router composition

### 1.1 `build_http_app` — `main.rs:747-764`

```rust
fn build_http_app(app_state: ServerState) -> Router {
    let api_app = router(app_state.clone())          // cogwheel_api::router
        .merge(admin_router())                        // main.rs:785
        .route("/favicon.ico", get(favicon));

    let app = if let Some(web_dist_dir) = resolve_web_dist_dir() {
        tracing::info!(path = %web_dist_dir.display(), "serving bundled web assets");
        let index_path = web_dist_dir.join("index.html");
        api_app.fallback_service(
            ServeDir::new(web_dist_dir).not_found_service(ServeFile::new(index_path)),
        )
    } else {
        tracing::warn!("web assets not found; serving API routes only");
        api_app
    };

    app.with_state(app_state).layer(TraceLayer::new_for_http())
}
```

Composition order and consequences:

1. `cogwheel_api::router(app_state.clone())` (`crates/cogwheel-api/src/lib.rs:281-292`) registers the health + metrics routes and immediately calls `.with_state(state)` internally. It is generic over `S where ApiState: FromRef<S>`; `ServerState` satisfies this via `#[derive(FromRef)]`.
2. `.merge(admin_router())` folds in every `/api/v1/*` route (still stateless at that point, typed `Router<ServerState>`).
3. `.route("/favicon.ico", get(favicon))`.
4. `.fallback_service(...)` — the SPA fallback, applied to the whole merged router.
5. `.with_state(app_state)` — second state application, this one satisfies the admin routes.
6. `.layer(TraceLayer::new_for_http())` — `tower_http` HTTP tracing on every request.

**There is no CORS layer, no auth middleware, no compression layer, and no request-body-size layer beyond axum's `Json` default (2 MiB).** The only authentication anywhere in the surface is the sync bearer-token check described in §5.

### 1.2 `admin_router` — `main.rs:785-888`

Returns `Router<ServerState>`. Full registration list with exact source lines is in §4.

### 1.3 Global middleware / cross-cutting behaviour

| Concern | Status |
| --- | --- |
| Tracing | `TraceLayer::new_for_http()` on every route (`main.rs:763`) |
| CORS | **Absent.** Cross-origin dev via `VITE_COGWHEEL_API_BASE` will fail preflight. |
| AuthN/AuthZ | **Absent** except `enforce_sync_transport_policy` on 6 sync routes |
| Rate limiting | Only 2 handlers call `state.rate_limiter` (see §3.2) |
| Body limits | axum default only |
| Compression | Absent |
| Request IDs | Absent |

---

## 2. Response & error conventions

### 2.1 Success envelope

Every JSON-returning handler wraps its payload in `ApiEnvelope<T>` (`crates/cogwheel-api/src/lib.rs:256-259`):

```rust
#[derive(Debug, Serialize)]
pub struct ApiEnvelope<T> {
    pub data: T,
}
```

Wire shape: `{"data": <T>}`. No `meta`, no `errors` key, no pagination envelope.

Exceptions (NOT enveloped):
- `GET /metrics` — raw Prometheus/OpenMetrics text.
- `POST /api/v1/runtime/pause` and `POST /api/v1/runtime/resume` — return `()` → HTTP 200 with an **empty body**.
- `GET /favicon.ico` — HTTP 204, empty body.
- SPA fallback — static files.

The web client (`apps/cogwheel-web/src/lib/api.ts:366-386`) unconditionally does `((await response.json()) as { data: T }).data`, so any new endpoint MUST keep the envelope or the client breaks.

### 2.2 Error shapes — three incompatible styles currently in use

| Style | Return type | Wire shape | Used by |
| --- | --- | --- | --- |
| **A. Bare status** | `Result<Json<ApiEnvelope<T>>, axum::http::StatusCode>` | status code, **empty body** | the large majority of `/api/v1/*` handlers |
| **B. Status + plaintext** | `Result<Json<ApiEnvelope<T>>, (axum::http::StatusCode, String)>` | status code, `text/plain` body containing the message | `upsert_device`, `upsert_block_profile`, `delete_block_profile`, `run_load_test`, `tailscale_exit_node`, `tailscale_rollback`, `tailscale_dns_check` |
| **C. `ApiError`** | `Result<Response, ApiError>` | `500` + `{"error": "<Display>"}` | only `GET /metrics` |

`ApiError` (`crates/cogwheel-api/src/lib.rs:266-279`):

```rust
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("invalid environment value: {0}")]
    InvalidEnv(String),
    #[error("internal server error")]
    Internal,
}
// IntoResponse => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": self.to_string()})))
```

Note `ApiError::InvalidEnv` also maps to 500 despite being a config-parse error — it is only ever produced by `AppConfig::load_from_env`, which runs before the server binds, so it never actually reaches HTTP.

The web client treats any non-2xx as `new Error(bodyText || "<status> <statusText>")` — so style-A errors surface to users as bare `"500 Internal Server Error"` strings with no diagnostic content.

### 2.3 Serde representation rules in force

No `#[serde(rename_all)]` is applied to any request/response **struct** in `main.rs`. All struct fields serialize with their exact Rust identifier (already `snake_case`). The only serde attributes present:

| Type | Attribute | Wire effect |
| --- | --- | --- |
| `SyncProfile` (`main.rs:490-496`) | `#[serde(rename_all = "kebab-case")]` | `"full"`, `"settings-only"`, `"read-only-follower"` |
| `StoredBlockProfileListRecord` (`main.rs:4239-4244`) | `#[serde(untagged)]` | accepts either a bare string id or a full `BlockProfileListRecord` object |
| `StoredBlockProfileRecord` (`main.rs:4246-4258`) | `#[serde(default)]` on `description`, `blocklists`, `allowlists` | those fields optional on read |
| `ResilienceDrillRequest.duration_secs` (`main.rs:2915-2919`) | `#[allow(dead_code)]` | field parsed then discarded |
| `DeploymentProfile` (`cogwheel-api:14-21`) | `#[serde(rename_all = "snake_case")]` | `"dev"`, `"home"`, `"smb"` |

Externally-tagged enums (Rust default) that cross the wire:

| Enum | Definition | JSON |
| --- | --- | --- |
| `ClassifierMode` | `crates/cogwheel-classifier/src/lib.rs:13-18` | `"Off"` \| `"Monitor"` \| `"Protect"` — **PascalCase** |
| `ServiceToggleMode` | `crates/cogwheel-services/src/lib.rs:6-11` | `"Inherit"` \| `"Allow"` \| `"Block"` — **PascalCase** |
| `BlockMode` | `crates/cogwheel-policy/src/lib.rs:8-18` | `"NullIp"` \| `"NxDomain"` \| `"NoData"` \| `"Refused"` \| `{"CustomIp":{"ipv4":<str\|null>,"ipv6":<str\|null>}}` |
| `RuleAction` | `crates/cogwheel-policy/src/lib.rs:26-30` | `"Allow"` \| `"Block"` |
| `RulePattern` | `crates/cogwheel-policy/src/lib.rs:20-24` | `{"Exact":"<domain>"}` \| `{"Suffix":"<domain>"}` |
| `DecisionKind` | `crates/cogwheel-policy/src/lib.rs:40-44` | `"Allowed"` \| `{"Blocked": <BlockMode>}` |
| `SourceKind` | `crates/cogwheel-lists/src/lib.rs:14-19` | `"Domains"` \| `"Hosts"` \| `"Adblock"` |

The mixed PascalCase-enum / snake_case-field convention is a real wire-level inconsistency the frontend already hardcodes (`api.ts` declares `mode: "Inherit" | "Allow" | "Block"`).

Scalar mappings: `Uuid` → hyphenated lowercase string; `chrono::DateTime<Utc>` → RFC 3339 string; `SocketAddr` → `"ip:port"` string; `f32`/`f64` → JSON number; `usize`/`u64` → JSON number.

---

## 3. `ServerState`

### 3.1 Definition — `main.rs:48-65`

```rust
#[derive(Clone, FromRef)]
struct ServerState {
    api_state: ApiState,
    storage: Arc<Storage>,
    dns_runtime: Arc<DnsRuntime>,
    http_client: Client,                                    // reqwest
    notification_settings: Arc<RwLock<NotificationSettings>>,
    threat_intel_settings: Arc<RwLock<ThreatIntelSettings>>,
    federated_learning_settings: Arc<RwLock<FederatedLearningSettings>>,
    recent_dns_activity: Arc<Mutex<VecDeque<DomainActivityRecord>>>,
    protected_domains: Arc<HashSet<String>>,
    runtime_guard: RuntimeGuardConfig,
    sync_seen_nonces: Arc<Mutex<HashMap<String, chrono::DateTime<chrono::Utc>>>>,
    rate_limiter: Arc<RateLimiter>,
    dns_udp_bind_addr: SocketAddr,
    advertised_dns_port: u16,
    advertised_dns_targets: Vec<String>,
}
```

`#[derive(FromRef)]` (axum macro) makes each field extractable as sub-state; only `ApiState` is actually consumed that way (by `cogwheel_api::metrics`).

Field-by-field:

| Field | Type | Constructed at | Purpose / notes |
| --- | --- | --- | --- |
| `api_state` | `ApiState { registry: Arc<Registry> }` | `main.rs:642` | Prometheus registry for `/metrics` |
| `storage` | `Arc<Storage>` | `main.rs:522` | SQLite handle (`Arc<Mutex<rusqlite::Connection>>` inside) + ed25519 node identity |
| `dns_runtime` | `Arc<DnsRuntime>` | `main.rs:604` | Owns resolver, policy engines, device policies, classifier settings, moka caches, atomic stats |
| `http_client` | `reqwest::Client` | `main.rs:600-603` | 5 s timeout; used for **notification webhooks only**. Blocklist refresh builds its own 15 s client each call (`main.rs:3910`, `main.rs:4904`) |
| `notification_settings` | `Arc<RwLock<NotificationSettings>>` | `main.rs:598` | Loaded from SQLite key `notification_settings` at boot; persisted on update |
| `threat_intel_settings` | `Arc<RwLock<ThreatIntelSettings>>` | `main.rs:647` | **In-memory only.** Seeded from `default_threat_intel_settings()`; never persisted; resets on restart |
| `federated_learning_settings` | `Arc<RwLock<FederatedLearningSettings>>` | `main.rs:648` | **In-memory only.** Same caveat |
| `recent_dns_activity` | `Arc<Mutex<VecDeque<DomainActivityRecord>>>` | `main.rs:599` (`with_capacity(4096)`) | Rolling 24 h / 4096-entry ring of `{domain, blocked, observed_at}`; feeds `DomainInsights`. **Volatile — lost on restart** |
| `protected_domains` | `Arc<HashSet<String>>` | `main.rs:549` | Hardcoded to exactly `{"connectivitycheck.gstatic.com"}`. Not configurable |
| `runtime_guard` | `RuntimeGuardConfig` | `main.rs:651` (from `AppConfig`) | Env-driven; surfaced read-only in `GET /api/v1/settings` |
| `sync_seen_nonces` | `Arc<Mutex<HashMap<String, DateTime<Utc>>>>` | `main.rs:652` | Replay cache keyed `"{node_public_key}:{nonce}"`, pruned to 30 min (`main.rs:2484`) |
| `rate_limiter` | `Arc<RateLimiter>` | `main.rs:653` — `RateLimiter::new(100, 60)` | 100 requests / 60 s **per string key** |
| `dns_udp_bind_addr` | `SocketAddr` | `main.rs:654` | Used by `discover_dns_targets` |
| `advertised_dns_port` | `u16` | `main.rs:655-658` | `COGWHEEL_SERVER__ADVERTISED_DNS_PORT`, else the UDP bind port |
| `advertised_dns_targets` | `Vec<String>` | `main.rs:659-669` | `COGWHEEL_SERVER__ADVERTISED_DNS_TARGETS`, comma-split, trimmed, empties dropped; default `[]` |

### 3.2 `RateLimiter` — `main.rs:67-97`

```rust
#[derive(Clone)]
struct RateLimiter {
    requests: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
    max_requests: usize,   // 100
    window_secs: u64,      // 60
}
fn is_allowed(&self, key: &str) -> bool   // sliding window, prunes then checks then pushes
```

Call sites (only two, and both use a **constant global key**, not the client IP):

- `refresh_sources` — key `"refresh_sources"` (`main.rs:3486`)
- `upsert_blocklist` — key `"upsert_blocklist"` (`main.rs:3734`)

On rejection both return bare `429 TOO_MANY_REQUESTS`. The map is never garbage-collected across keys (only within an entry), which is bounded here because only 2 keys exist.

### 3.3 `RuntimePolicyCatalog` — `main.rs:99-103`

```rust
#[derive(Clone)]
struct RuntimePolicyCatalog {
    global_policy: Arc<PolicyEngine>,
    profile_policies: HashMap<String, Arc<PolicyEngine>>,
}
```

Built by `build_runtime_policy_catalog` (`main.rs:4984-5032`). Profiles are derived from `ParsedSource.source.profile`; the literal profile `"shared"` is never given its own policy but its rules are folded into every other profile's engine.

### 3.4 Startup sequence — `main.rs:513-718`

1. `init_tracing()` — JSON-formatted `tracing_subscriber` with `EnvFilter::from_default_env()` plus a forced `info` directive (`main.rs:720-727`).
2. **`healthcheck` argv shortcut** — if `argv[1] == "healthcheck"`, the process returns `Ok(())` immediately (`main.rs:517-519`). This is what `docker-compose.yml` uses as its healthcheck. **It performs no actual health check** — it is a bare exit-0 stub.
3. `AppConfig::load()` → `Storage::connect(database_url)`.
4. Insert a hardcoded bootstrap source (`main.rs:524-534`): `id = Uuid::from_u128(1)`, `name = "baseline"`, `url = "data:text/plain,ads.example.com%0Atracker.example.com"`, `kind = "domains"`, `enabled = true`, `refresh_interval_minutes = 60`, `profile = "essential"`, `verification_strictness = "strict"`. This id is treated as **reserved** (`is_reserved_source_id`, `main.rs:5829-5831`) and cannot be disabled or deleted via the API.
5. Parse it inline, `verify_candidate`, `build_policy_engine(..., BlockMode::NullIp)`, `record_ruleset(status="active")`, `activate_ruleset`, audit `ruleset.activated` with `reason: "bootstrap"`.
6. Build Prometheus `Registry`, register `cogwheel_startups_total`, `inc()`.
7. `build_resolver(&config.upstream.servers)` (`main.rs:729-745`) — every upstream is registered twice, once UDP once TCP, via `NameServerConfigGroup`.
8. `load_classifier_settings`, `load_notification_settings`, build reqwest client, construct `DnsRuntime`.
9. Register `set_classification_observer` (`main.rs:605-626`) → spawns `record_security_event_from_classification`.
10. Register `set_query_activity_observer` (`main.rs:627-630`) → `record_recent_dns_activity`.
11. Spawn DNS serve task (UDP + TCP).
12. Build `ServerState`; `warm_runtime_policy_catalog` (failure is logged and swallowed, `main.rs:671-673`); `sync_runtime_device_policies` (failure is **fatal** — `?`).
13. Spawn the scheduled refresh loop: interval = `max(refresh_interval_secs, 30)`, first tick consumed immediately, then `due_source_ids` → `refresh_sources_once(state, "scheduled", Some(&due_ids))` (`main.rs:675-699`).
14. `build_http_app`, bind `http_bind_addr`, `tokio::select!` over {dns task, refresh task, `axum::serve`}. **Any one of the three finishing terminates the process.**

---

## 4. Complete route table

### 4.1 Registered in `cogwheel_api::router` — `crates/cogwheel-api/src/lib.rs:286-291`

| Method | Path | Handler | Line |
| --- | --- | --- | --- |
| GET | `/health/live` | `live` | 287 |
| GET | `/health/ready` | `ready` | 288 |
| POST | `/health/check` | `live` (**same handler as `/health/live`**) | 289 |
| GET | `/metrics` | `metrics` | 290 |

### 4.2 Registered in `build_http_app` — `main.rs`

| Method | Path | Handler | Line |
| --- | --- | --- | --- |
| GET | `/favicon.ico` | `favicon` | 750 |
| * | `<fallback>` | `ServeDir` + `ServeFile(index.html)` | 755-757 |

### 4.3 Registered in `admin_router` — `main.rs:786-887`

| Method | Path | Handler | Reg. line | Handler line | Mutates |
| --- | --- | --- | --- | --- | --- |
| GET | `/api/v1/dashboard` | `dashboard_summary` | 787 | 1000 | no |
| GET | `/api/v1/settings` | `settings_summary` | 788 | 1589 | no |
| POST | `/api/v1/settings/block-profiles` | `upsert_block_profile` | 789 | 4472 | yes |
| POST | `/api/v1/settings/block-profiles/delete` | `delete_block_profile` | 793 | 4555 | yes |
| POST | `/api/v1/settings/blocklists` | `upsert_blocklist` | 797 | 3730 | yes |
| POST | `/api/v1/settings/blocklists/state` | `update_blocklist_state` | 798 | 3792 | yes |
| POST | `/api/v1/settings/blocklists/delete` | `delete_blocklist` | 802 | 3851 | yes |
| GET | `/api/v1/devices` | `list_devices` | 803 | 901 | no |
| POST | `/api/v1/devices` | `upsert_device` | 804 | 912 | yes |
| GET | `/api/v1/security-events` | `list_security_events` | 805 | 989 | no |
| GET | `/api/v1/sources` | `list_sources` | 806 | 890 | no |
| POST | `/api/v1/sources/refresh` | `refresh_sources` | 807 | 3483 | yes |
| GET | `/api/v1/services` | `list_services` | 808 | 3496 | no |
| POST | `/api/v1/services/toggles` | `update_service_toggle` | 809 | 3505 | yes |
| POST | `/api/v1/settings/classifier` | `update_classifier_settings` | 810 | 3546 | yes |
| POST | `/api/v1/settings/notifications` | `update_notification_settings` | 814 | 3576 | yes |
| POST | `/api/v1/settings/notifications/test` | `test_notification_settings` | 818 | 3610 | yes (audit + delivery rows) |
| POST | `/api/v1/settings/notifications/presets` | `update_notification_test_presets` | 822 | 3707 | yes |
| GET | `/api/v1/runtime` | `runtime_snapshot` | 826 | 3411 | no |
| GET | `/api/v1/runtime/health` | `runtime_health` | 827 | 3419 | no |
| POST | `/api/v1/runtime/health/check` | `run_runtime_health_check` | 828 | 3427 | yes (audit; issues live DNS probes) |
| POST | `/api/v1/runtime/pause` | `pause_runtime` | 832 | 3441 | yes |
| POST | `/api/v1/runtime/resume` | `resume_runtime` | 833 | 3466 | yes |
| GET | `/api/v1/resolver-access` | `resolver_access_status` | 834 | 3230 | no (shells out) |
| GET | `/api/v1/false-positive-budget` | `false_positive_budget_status` | 835 | 3094 | no |
| GET | `/api/v1/latency-budget` | `latency_budget_status` | 839 | 3139 | no |
| GET | `/api/v1/tailscale/status` | `tailscale_status` | 840 | 1780 | no (shells out) |
| POST | `/api/v1/tailscale/exit-node` | `tailscale_exit_node` | 841 | 1799 | yes (host state + file + audit) |
| POST | `/api/v1/tailscale/rollback` | `tailscale_rollback` | 842 | 1913 | yes |
| GET | `/api/v1/tailscale/dns-check` | `tailscale_dns_check` | 843 | 2011 | no |
| GET | `/api/v1/sync/status` | `sync_status` | 844 | 2367 | no |
| GET | `/api/v1/sync/profile` | `sync_profile` | 845 | 2262 | no |
| POST | `/api/v1/sync/profile` | `update_sync_profile` | 846 | 2277 | yes |
| GET | `/api/v1/sync/transport` | `sync_transport` | 847 | 2307 | no |
| POST | `/api/v1/sync/transport` | `update_sync_transport` | 848 | 2327 | yes |
| GET | `/api/v1/sync/export` | `export_sync_state` | 849 | 2494 | no (does NOT bump stored revision) |
| POST | `/api/v1/sync/import` | `import_sync_state` | 850 | 2558 | yes (destructive) |
| GET | `/api/v1/rulesets` | `list_rulesets` | 851 | 2684 | no |
| POST | `/api/v1/rulesets/rollback` | `rollback_ruleset` | 852 | 2707 | yes |
| GET | `/api/v1/audit-events` | `list_audit_events` | 853 | 2787 | no |
| GET | `/api/v1/backup` | `backup_data` | 854 | 2820 | no |
| POST | `/api/v1/backup/restore` | `restore_data` | 855 | 2854 | yes (partially — see §4.42) |
| POST | `/api/v1/resilience/upstream-outage` | `simulate_upstream_outage` | 856 | 2921 | no |
| POST | `/api/v1/resilience/db-corruption` | `simulate_db_corruption` | 860 | 2956 | no |
| POST | `/api/v1/resilience/source-failure` | `simulate_source_failure` | 864 | 2989 | no |
| POST | `/api/v1/resilience/sync-partition` | `simulate_sync_partition` | 868 | 3025 | no |
| POST | `/api/v1/load-test` | `run_load_test` | 872 | 1197 | no (but issues real upstream DNS traffic) |
| GET | `/api/v1/benchmark/rust-opts` | `benchmark_rust_opts` | 873 | 1310 | no |
| GET | `/api/v1/config/version` | `config_version` | 874 | 1394 | no |
| GET | `/api/v1/threat-intel/providers` | `threat_intel_settings` | 875 | 1484 | no |
| POST | `/api/v1/threat-intel/providers` | `update_threat_intel_provider` | 876 | 1495 | in-memory + audit only |
| GET | `/api/v1/federated-learning/status` | `federated_learning_settings` | 880 | 1539 | no |
| POST | `/api/v1/federated-learning/status` | `update_federated_learning_settings` | 884 | 1550 | in-memory + audit only |

**Total: 4 health/metrics routes + 1 favicon + 54 admin route registrations across 50 distinct paths (4 paths carry both GET and POST).**

There are **no** `DELETE`, `PUT`, or `PATCH` verbs anywhere. Deletion is modelled as `POST .../delete` with a JSON body. There are **no path parameters** anywhere — every identifier travels in the request body or query string.

---

## 4-detail. Per-route contracts

### 4.1 `GET /health/live` — `live` (`cogwheel-api:294-298`)

- Request: none.
- Response `200`: `{"data":{"status":"ok"}}` where `HealthResponse { status: &'static str }`.
- Errors: none possible.
- Mutates: no.

### 4.2 `GET /health/ready` — `ready` (`cogwheel-api:300-304`)

- Response `200`: `{"data":{"status":"ready"}}`.
- **Stub:** performs no readiness probing at all — no DB ping, no resolver check, no policy check. Always ready.

### 4.3 `POST /health/check` — `live` (`cogwheel-api:289`)

- Identical to `GET /health/live`; the same `live` fn is registered under `post`. Ignores any request body. Looks like an artifact rather than an intentional design.

### 4.4 `GET /metrics` — `metrics` (`cogwheel-api:306-310`)

```rust
async fn metrics(State(state): State<ApiState>) -> Result<Response, ApiError> {
    let mut output = String::new();
    encode(&mut output, &state.registry).map_err(|_| ApiError::Internal)?;
    Ok((StatusCode::OK, output).into_response())
}
```

- Response `200`: raw text, **not** enveloped. `Content-Type` is whatever axum infers for `String` → `text/plain; charset=utf-8` (NOT the OpenMetrics content type Prometheus prefers).
- Errors: `500` + `{"error":"internal server error"}` on encode failure.
- See §7 for the (nearly empty) metric inventory.

### 4.5 `GET /favicon.ico` — `favicon` (`main.rs:3397-3399`)

- Returns bare `204 NO_CONTENT`. Exists solely to stop the SPA fallback from serving `index.html` for favicon requests.

### 4.6 `GET /api/v1/dashboard` — `dashboard_summary` (`main.rs:1000-1101`)

**Query params** — `DashboardQuery` (`main.rs:276-280`), both optional:

```rust
struct DashboardQuery {
    notification_window: Option<usize>,
    notification_history_window: Option<usize>,
}
```

Both pass through `normalize_notification_window` (`main.rs:5430-5437`) which accepts **only** `10 | 50 | 100`; every other value including `None` collapses to `30`. Malformed non-integer query values cause axum's `Query` extractor to reject with `400` and a plaintext body.

**Response** — `ApiEnvelope<DashboardSummary>` (`main.rs:174-191`):

```rust
struct DashboardSummary {
    protection_status: String,                                  // "Paused" | "Needs Attention" | "Protected"
    protection_paused_until: Option<DateTime<Utc>>,
    active_ruleset: Option<RulesetSummary>,
    source_count: usize,
    enabled_source_count: usize,
    service_toggle_count: usize,                                // count of toggles whose mode != Inherit
    device_count: usize,
    runtime_health: RuntimeHealthResponse,
    latest_audit_events: Vec<AuditEvent>,                       // 5 most recent
    recent_security_events: Vec<SecurityEventRecord>,           // 5 most recent (of a 25-row fetch)
    recent_notification_deliveries: Vec<NotificationDeliveryEvent>, // capped at 5 by build_notification_delivery_events
    notification_health: NotificationHealthSummary,
    notification_failure_analytics: NotificationFailureAnalytics,
    security_summary: SecuritySummary,
    domain_insights: DomainInsights,
}
```

Nested types:

```rust
struct RulesetSummary {                                   // main.rs:152-158
    id: Uuid, hash: String, status: String, created_at: DateTime<Utc>,
}

struct RuntimeHealthResponse {                            // main.rs:167-172
    snapshot: DnsRuntimeSnapshot,
    degraded: bool,
    notes: Vec<String>,
}

// crates/cogwheel-dns-core/src/lib.rs:97-112
pub struct DnsRuntimeSnapshot {
    upstream_failures_total: u64,
    fallback_served_total: u64,
    cache_hits_total: u64,
    cname_uncloaks_total: u64,
    cname_blocks_total: u64,
    queries_total: u64,
    blocked_total: u64,
    cache_hit_latency_avg_ns: u64,
    cache_hit_samples: u64,
    cache_miss_latency_avg_ns: u64,
    cache_miss_samples: u64,
    classifier_latency_avg_ns: u64,
    classifier_latency_samples: u64,
}

// crates/cogwheel-storage/src/lib.rs:75-81
pub struct AuditEvent { id: Uuid, event_type: String, payload: String, created_at: DateTime<Utc> }
// NOTE: `payload` is a JSON *string*, double-encoded on the wire.

// crates/cogwheel-storage/src/lib.rs:101-111
pub struct SecurityEventRecord {
    id: Uuid, device_id: Option<Uuid>, device_name: Option<String>,
    client_ip: String, domain: String, classifier_score: f64,
    severity: String,           // "medium" | "high" | "critical"
    created_at: DateTime<Utc>,
}

struct NotificationDeliveryEvent {                        // main.rs:213-226
    status: String,             // "delivered" | "failed"
    event_type: String, severity: String, title: String, summary: String,
    target: String,             // device_name ?? client_ip
    domain: String,
    device_name: Option<String>,
    client_ip: String,
    attempts: usize,
    created_at: DateTime<Utc>,
}

struct NotificationHealthSummary {                        // main.rs:228-234
    delivered_count: usize, failed_count: usize,
    last_delivery_at: Option<DateTime<Utc>>,
    last_failure_at: Option<DateTime<Utc>>,
}

struct NotificationFailureAnalytics {                     // main.rs:236-240
    success_rate_percent: f32,                            // 100.0 when zero samples; rounded to 1 dp
    top_failed_domains: Vec<NotificationFailureDomain>,   // truncated to 3
}
struct NotificationFailureDomain { domain: String, failure_count: usize }  // main.rs:242-246

struct SecuritySummary {                                  // main.rs:282-288
    medium_count: usize, high_count: usize, critical_count: usize,
    top_devices: Vec<DeviceSecuritySummary>,              // truncated to 3
}
struct DeviceSecuritySummary {                            // main.rs:290-295
    label: String,             // device_name ?? client_ip
    event_count: usize,
    highest_severity: String,
}

struct DomainInsights {                                   // main.rs:199-204
    top_queried_domains: Vec<DomainInsightEntry>,         // truncated to 6
    top_blocked_domains: Vec<DomainInsightEntry>,         // truncated to 6
    observed_queries: usize,
}
struct DomainInsightEntry { domain: String, count: usize } // main.rs:193-197
```

`protection_status` derivation (`main.rs:1068-1080`): `"Paused"` if `protection_paused_until` is in the future; else `"Needs Attention"` if `runtime_health.degraded`; else `"Protected"`.

`runtime_health` here comes from `current_runtime_health` (`main.rs:4661-4688`), which compares the live snapshot against a **synthetic all-zero baseline**. That means the delta equals the absolute lifetime counter, so with the default `Home` profile (`max_upstream_failures_delta = 0`, `max_fallback_served_delta = 0`) the dashboard flips to `"Needs Attention"` permanently after the very first upstream failure and never recovers. This is a latent behavioural bug worth flagging.

- Errors: any storage failure → bare `500`.
- Mutates: no.

### 4.7 `GET /api/v1/settings` — `settings_summary` (`main.rs:1589-1633`)

- Request: none.
- Response — `ApiEnvelope<SettingsSummary>` (`main.rs:297-308`):

```rust
struct SettingsSummary {
    blocklists: Vec<SourceRecord>,
    blocklist_statuses: Vec<BlocklistStatusView>,
    block_profiles: Vec<BlockProfileRecord>,
    devices: Vec<DeviceRecord>,
    services: Vec<ServiceToggleView>,
    classifier: ClassifierSettings,
    notifications: NotificationSettings,
    notification_test_presets: Vec<NotificationTestPreset>,
    runtime_guard: RuntimeGuardConfig,
}
```

Nested:

```rust
// crates/cogwheel-storage/src/lib.rs:54-64
pub struct SourceRecord {
    id: Uuid, name: String, url: String, kind: String,   // "domains"|"hosts"|"adblock"
    enabled: bool, refresh_interval_minutes: i64,
    profile: String, verification_strictness: String,     // "strict"|"balanced"|"relaxed"
}

struct BlocklistStatusView {                              // main.rs:337-343
    id: Uuid, name: String,
    last_refresh_attempt_at: Option<DateTime<Utc>>,
    due_for_refresh: bool,
}

struct BlockProfileRecord {                               // main.rs:434-443
    id: String, emoji: String, name: String, description: String,
    blocklists: Vec<BlockProfileListRecord>,
    allowlists: Vec<String>,
    updated_at: DateTime<Utc>,
}
struct BlockProfileListRecord {                           // main.rs:425-432
    id: String, name: String, url: String, kind: String, family: String,
}

// crates/cogwheel-storage/src/lib.rs:89-99
pub struct DeviceRecord {
    id: Uuid, name: String, ip_address: String,
    policy_mode: String,                                  // "global" | "custom"
    blocklist_profile_override: Option<String>,
    protection_override: String,                          // "inherit" | "bypass"
    allowed_domains: Vec<String>,
    service_overrides: Vec<DeviceServiceOverrideRecord>,
}
pub struct DeviceServiceOverrideRecord { service_id: String, mode: String }  // storage:83-87

struct ServiceToggleView {                                // main.rs:387-391
    manifest: ServiceManifest,
    mode: ServiceToggleMode,                              // "Inherit"|"Allow"|"Block"
}
// crates/cogwheel-services/src/lib.rs:13-22
pub struct ServiceManifest {
    service_id: String, display_name: String, category: String, risk_notes: String,
    allow_domains: Vec<String>, block_domains: Vec<String>, exceptions: Vec<String>,
}

// crates/cogwheel-classifier/src/lib.rs:20-24
pub struct ClassifierSettings { mode: ClassifierMode, threshold: f32 }

struct NotificationSettings {                             // main.rs:310-315
    enabled: bool, webhook_url: Option<String>, min_severity: String,
}

struct NotificationTestPreset {                           // main.rs:267-274
    name: String, domain: String, severity: String, device_name: String, dry_run: bool,
}

// crates/cogwheel-api/src/lib.rs:105-110
pub struct RuntimeGuardConfig {
    probe_domains: Vec<String>,
    max_upstream_failures_delta: u64,
    max_fallback_served_delta: u64,
}
```

- Errors: bare `500` on any storage failure. Note `main.rs:1611-1615` does `.expect("notification settings lock poisoned")` — a poisoned lock **panics the handler task**.
- Mutates: no. `runtime_guard` is read-only here — **there is no endpoint to change it**; it is env-var only.

### 4.8 `POST /api/v1/settings/block-profiles` — `upsert_block_profile` (`main.rs:4472-4553`)

- Body — `UpsertBlockProfileRequest` (`main.rs:445-453`):

```rust
struct UpsertBlockProfileRequest {
    id: Option<String>,
    emoji: String,
    name: String,
    description: Option<String>,
    blocklists: Vec<BlockProfileListRecord>,
    allowlists: Vec<String>,
}
```

- Normalisation: id ← `normalize_block_profile_id(id)` else `normalize_block_profile_id(name)` (lowercase, non-alnum → `-`, collapse/trim dashes); empty emoji → `"🧩"`; `blocklists` → `normalize_block_profile_lists` (`main.rs:4399-4449`: preset ids replaced by canonical preset records, `oisd-small` dropped when `oisd-big` present, `oisd-nsfw-small` dropped when `oisd-nsfw` present, sorted by name, deduped by id **or** url); `allowlists` → `normalize_domain_list` (splits on commas, trims dots, lowercases, sorts, dedupes).
- Response `200`: `ApiEnvelope<Vec<BlockProfileRecord>>` — the **entire normalised profile list**, not just the upserted one.
- Errors (style B, plaintext body): `400 "block profile requires a name"`; `400 "block profile requires a friendly name"`; `500` + `error.to_string()` on load/persist/audit failure.
- Persists: SQLite `settings["block_profiles"]` = JSON array; audit event `block-profile.updated` with payload `{"id","name"}`.
- **Not wired to DNS:** block profiles are a pure settings blob. Nothing in `refresh_sources_once` or `build_runtime_policy_catalog` reads `block_profiles` — the profile→policy mapping goes through `SourceRecord.profile`, a different concept. This is effectively decorative state today.

### 4.9 `POST /api/v1/settings/block-profiles/delete` — `delete_block_profile` (`main.rs:4555-4617`)

- Body — `DeleteBlockProfileRequest { id: String }` (`main.rs:455-458`).
- Response `200`: `ApiEnvelope<Vec<BlockProfileRecord>>` (remaining profiles).
- Errors: `400 "block profile requires an id"`; `404 "block profile not found"`; `500` + message.
- Persists: `settings["block_profiles"]`; audit `block-profile.deleted` `{"id","name"}`.

### 4.10 `POST /api/v1/settings/blocklists` — `upsert_blocklist` (`main.rs:3730-3790`)

- **Rate limited** (key `"upsert_blocklist"`).
- Body — `UpsertBlocklistRequest` (`main.rs:405-416`):

```rust
struct UpsertBlocklistRequest {
    id: Option<Uuid>,                            // absent => Uuid::new_v4()
    name: String,
    url: String,                                 // must parse as url::Url
    kind: String,                                // "domains"|"hosts"|"adblock", case-insensitive
    enabled: bool,
    refresh_interval_minutes: Option<i64>,       // default 60, clamped .max(1)
    profile: Option<String>,                     // default "custom", lowercased/trimmed
    verification_strictness: Option<String>,     // default "balanced"; "strict"|"balanced"|"relaxed"
    refresh_now: Option<bool>,                   // default TRUE
}
```

- Response `200`: `ApiEnvelope<RefreshResponse>` (`main.rs:160-165`):

```rust
struct RefreshResponse {
    outcome: String,                 // "saved" | "activated" | "rejected" | "rolled_back"
    ruleset: Option<RulesetSummary>,
    notes: Vec<String>,
}
```

When `refresh_now != Some(false)` **and** `enabled`, the handler tail-calls `refresh_sources_once(&state, "blocklist-update", None)` and returns its result (so `outcome` may be `activated`/`rejected`/`rolled_back`). Otherwise `{"outcome":"saved","ruleset":null,"notes":["saved blocklist <name>"]}`.

- Errors: `429`; `400` (bad kind, unparseable url, bad profile, bad strictness); `500`.
- Persists: `sources` row via `INSERT OR REPLACE`; audit `blocklist.upserted` (payload = serialized `SourceRecord`); plus everything `refresh_sources_once` writes (§6).
- **Note:** `sources.name` has a `UNIQUE` constraint (migration 0001). `INSERT OR REPLACE` on a duplicate name silently replaces a *different* row's id.

### 4.11 `POST /api/v1/settings/blocklists/state` — `update_blocklist_state` (`main.rs:3792-3849`)

- Body — `UpdateBlocklistStateRequest { id: Uuid, enabled: bool, refresh_now: Option<bool> }` (`main.rs:418-423`), `refresh_now` defaults **true**.
- Response `200`: `ApiEnvelope<RefreshResponse>`; note text `"enabled blocklist <name>"` / `"disabled blocklist <name>"`.
- Errors: `404` if id not found; `409 CONFLICT` when disabling the reserved bootstrap source (`Uuid::from_u128(1)`); `500`.
- Persists: `sources` row; audit `blocklist.state_updated`; then `refresh_sources_once(state, "blocklist-state-update", None)`.

### 4.12 `POST /api/v1/settings/blocklists/delete` — `delete_blocklist` (`main.rs:3851-3903`)

- Body — `DeleteBlocklistRequest { id: Uuid, refresh_now: Option<bool> }` (`main.rs:460-464`), default true.
- Response `200`: `ApiEnvelope<RefreshResponse>`.
- Errors: `409` for the reserved id; `404` if not found (checked twice — pre-lookup and post-delete); `500`.
- Persists: `DELETE FROM sources`; audit `blocklist.deleted`; then refresh.

### 4.13 `GET /api/v1/devices` — `list_devices` (`main.rs:901-910`)

- Response `200`: `ApiEnvelope<Vec<DeviceRecord>>`. Errors: bare `500`.

### 4.14 `POST /api/v1/devices` — `upsert_device` (`main.rs:912-987`)

- Body — `UpsertDeviceRequest` (`main.rs:466-476`):

```rust
struct UpsertDeviceRequest {
    id: Option<Uuid>,                                          // absent => new v4
    name: String,
    ip_address: String,                                        // NOT validated as an IP here
    policy_mode: Option<String>,                               // default "global"; "global"|"custom"
    blocklist_profile_override: Option<String>,
    protection_override: Option<String>,                       // default "inherit"; "inherit"|"bypass"
    allowed_domains: Option<Vec<String>>,
    service_overrides: Option<Vec<DeviceServiceOverrideRecord>>,
}
```

- Validation: `normalize_device_policy_mode`, `normalize_device_protection_override`, `validate_device_service_overrides` (`main.rs:5193-5253`) which requires `policy_mode == "custom"` whenever overrides are non-empty, requires each `service_id` to be one of the built-ins, and requires `mode ∈ {allow, block}`.
- Response `200`: `ApiEnvelope<DeviceRecord>` (the normalised record).
- Errors (style B, plaintext): `400 "device policy mode must be either global or custom"`; `400 "device protection override must be either inherit or bypass"`; `400 "device service overrides require custom policy mode"`; `400 "unknown device service override \`<id>\`; choose one of the built-in services"`; `400 "device service override \`<Display Name>\` must use allow or block mode"`; `400 "device service override \`<Display Name>\` has no device-specific domains for <mode> mode"`; `400 "device service overrides must use known built-in services with allow or block mode"`; `500 "failed to persist device"` / `"failed to serialize device audit payload"` / `"failed to record device audit event"` / `"failed to sync runtime device policies"`.
- Persists: `devices` row (`INSERT OR REPLACE`); audit `device.upserted` (payload = serialized `DeviceRecord`); then `sync_runtime_device_policies` pushes `Vec<DevicePolicyConfig>` into `DnsRuntime` and invalidates the primary DNS cache.
- `ip_address` is only parsed later, in `DnsRuntime::replace_device_policies` (`dns-core:159-174`), which silently `filter_map`s out unparseable addresses. A device saved with a garbage IP persists in SQLite and shows in the UI but never affects DNS.

### 4.15 `GET /api/v1/security-events` — `list_security_events` (`main.rs:989-998`)

- Response `200`: `ApiEnvelope<Vec<SecurityEventRecord>>` — hardcoded `LIMIT 20`, ordered by `created_at DESC`. No pagination, no filtering.
- Errors: bare `500`.

### 4.16 `GET /api/v1/sources` — `list_sources` (`main.rs:890-899`)

- Response `200`: `ApiEnvelope<Vec<SourceRecord>>`, ordered by `name ASC`. Errors: bare `500`.

### 4.17 `POST /api/v1/sources/refresh` — `refresh_sources` (`main.rs:3483-3494`)

- **Rate limited** (key `"refresh_sources"`). Body: none (any body is ignored).
- Response `200`: `ApiEnvelope<RefreshResponse>` from `refresh_sources_once(&state, "manual", None)`.
- Errors: `429`; bare `500` (this swallows the real `anyhow` reason, including the common `"no enabled sources configured"`).
- Persists: see §6.

### 4.18 `GET /api/v1/services` — `list_services` (`main.rs:3496-3503`)

- Response `200`: `ApiEnvelope<Vec<ServiceToggleView>>` — the 3 built-in manifests joined with their stored mode.
- Built-ins (`crates/cogwheel-services/src/lib.rs:79-118`): `google-ads` (Google Ads), `tiktok` (TikTok), `nintendo` (Nintendo Services). Two of the three carry `risk_notes: "Placeholder manifest until curated domain coverage is finalized."` and have `allow_domains == block_domains`.

### 4.19 `POST /api/v1/services/toggles` — `update_service_toggle` (`main.rs:3505-3544`)

- Body — `UpdateServiceToggleRequest { service_id: String, mode: ServiceToggleMode }` (`main.rs:393-397`). `mode` must be PascalCase: `"Inherit" | "Allow" | "Block"`.
- Response `200`: `ApiEnvelope<RefreshResponse>` (tail-calls `refresh_sources_once(state, "service-toggle", None)`).
- Errors: `404` for an unknown `service_id`; `422` from axum for an unrecognised `mode` string; `500`.
- Persists: `settings["service_toggles"]` = `ServiceToggleSnapshot` JSON; audit `service-toggle.updated` `{"service_id","mode"}`; full ruleset rebuild.

### 4.20 `POST /api/v1/settings/classifier` — `update_classifier_settings` (`main.rs:3546-3574`)

- Body — `UpdateClassifierSettingsRequest` (`main.rs:399-403`):

```rust
struct UpdateClassifierSettingsRequest {
    mode: cogwheel_classifier::ClassifierMode,   // "Off" | "Monitor" | "Protect"
    threshold: f32,
}
```

- **No validation on `threshold`.** Negative values, `>1.0`, `NaN` are all accepted and persisted verbatim.
- Response `200`: `ApiEnvelope<ClassifierSettings>`.
- Errors: `422` for an unknown mode string; bare `500` on persist/audit failure.
- Persists: `settings["classifier_settings"]` (JSON of `ClassifierSettings`), then `dns_runtime.replace_classifier_settings(settings)` (hot-swap, no restart), then audit `classifier-settings.updated` with the serialized settings as payload. Ordering note: the DB write happens **before** the in-memory swap, and neither is rolled back if the audit write fails.

### 4.21 `POST /api/v1/settings/notifications` — `update_notification_settings` (`main.rs:3576-3608`)

- Body — `UpdateNotificationSettingsRequest { enabled: bool, webhook_url: Option<String>, min_severity: String }` (`main.rs:317-322`).
- Validation: `normalize_webhook_url` (`main.rs:5479-5492`) — `None`/blank → `None`; must parse as a URL with scheme `http` or `https`, otherwise reject. `normalize_notification_severity` (`main.rs:5470-5477`) — accepts only `medium|high|critical` (case-insensitive, trimmed).
- Response `200`: `ApiEnvelope<NotificationSettings>`.
- Errors: bare `400` for a bad URL or bad severity; bare `500` otherwise.
- Persists: `settings["notification_settings"]`; in-memory `RwLock` swap; audit `notification-settings.updated`.

### 4.22 `POST /api/v1/settings/notifications/test` — `test_notification_settings` (`main.rs:3610-3705`)

- Body — `TestNotificationRequest` (`main.rs:324-330`), all optional:

```rust
struct TestNotificationRequest {
    domain: Option<String>,        // default "notification-test.cogwheel.local"
    severity: Option<String>,      // default = settings.min_severity
    device_name: Option<String>,   // default "Control Plane Test"
    dry_run: Option<bool>,         // default false
}
```

- Response `200`: `ApiEnvelope<NotificationTestResult { outcome: String, target: String }>` (`main.rs:261-265`) with `outcome ∈ {"validated" (dry run), "sent"}` and `target` = the configured webhook URL.
- Errors: bare `400` when no `webhook_url` is configured or the severity is invalid; **`502 BAD_GATEWAY`** when delivery fails after 3 attempts; bare `500` on audit failure.
- Mutates: constructs a synthetic `SecurityEventRecord` (`classifier_score: 1.0`, `client_ip: "127.0.0.1"`) but **does not** write it to `security_events`. Dry run writes audit `notification-settings.tested.dry-run`. Live run calls `deliver_security_notification` which writes rows to `notification_deliveries` and audit events `security.alert_delivery_succeeded` / `security.alert_delivery_failed`, then writes audit `notification-settings.tested`.
- **SSRF surface:** the webhook URL is operator-supplied and unvalidated beyond scheme, and this endpoint triggers an outbound POST on demand with no allowlist and no internal-IP rejection.

### 4.23 `POST /api/v1/settings/notifications/presets` — `update_notification_test_presets` (`main.rs:3707-3728`)

- Body — `UpdateNotificationPresetsRequest { presets: Vec<NotificationTestPreset> }` (`main.rs:332-335`).
- `normalize_notification_test_presets` (`main.rs:5439-5468`): drops entries with an invalid severity or a blank `name`/`domain`/`device_name`; later entries with the same `name` replace earlier ones; sorted by name; **truncated to 8**.
- Response `200`: `ApiEnvelope<Vec<NotificationTestPreset>>` (the normalised list).
- Persists: `settings["notification_test_presets"]`; audit `notification-test-presets.updated`.

### 4.24 `GET /api/v1/runtime` — `runtime_snapshot` (`main.rs:3411-3417`)

- Response `200`: `ApiEnvelope<DnsRuntimeSnapshot>` (all 13 counters, see §4.6). Never errors in practice (the `Result` is always `Ok`).
- **This is the only place the DNS counters are exposed. They are NOT in `/metrics`.**

### 4.25 `GET /api/v1/runtime/health` — `runtime_health` (`main.rs:3419-3425`)

- Response `200`: `ApiEnvelope<RuntimeHealthResponse>` from `current_runtime_health` (zero-baseline comparison — see the caveat in §4.6). Passive; no probes issued.

### 4.26 `POST /api/v1/runtime/health/check` — `run_runtime_health_check` (`main.rs:3427-3434`)

- Body: none. Response `200`: `ApiEnvelope<RuntimeHealthResponse>` from `active_runtime_health_check` (`main.rs:4690-4760`).
- Behaviour: snapshot → `current_runtime_health` → `run_runtime_guard_probes` (issues a real `A` lookup for every `runtime_guard.probe_domains` entry via `dns_runtime.probe_domain`) → re-snapshot; notes are unioned, `degraded = current.degraded || probe_report.degraded`.
- Persists: audit `runtime.health_check_degraded` or `runtime.health_check_passed` with payload `{degraded, notes, snapshot}`. If degraded and notifications permit `"high"`, delivers an operational webhook (`runtime.health_degraded`) which writes `notification_deliveries` rows.
- Errors: bare `500`.

### 4.27 `POST /api/v1/runtime/pause` — `pause_runtime` (`main.rs:3441-3464`)

- Body — `PauseRuntimeRequest { minutes: u32 }` (`main.rs:3436-3439`). No upper bound.
- Response `200` with an **empty body** (returns `()`). This breaks the envelope convention; the web client calls it as `fetchJson<void>` and will throw on `await response.json()` of an empty body.
- Persists: `dns_runtime.pause_protection_until(now + minutes)` — **in-memory only, lost on restart**; audit `runtime.protection_paused` `{"minutes","until"}`.
- Effect: `policy_for_client` (`dns-core:504-518`) swaps in an allow-all `PolicyEngine` under cache scope `"global-pause"` while the pause window is active.

### 4.28 `POST /api/v1/runtime/resume` — `resume_runtime` (`main.rs:3466-3481`)

- Body: none. Response `200`, empty body.
- Persists: clears the in-memory pause; audit `runtime.protection_resumed` with payload `"{}"`.

### 4.29 `GET /api/v1/resolver-access` — `resolver_access_status` (`main.rs:3230-3278`)

- Extracts `HeaderMap` (uses the `Host` header).
- Response `200`: `ApiEnvelope<ResolverAccessStatus>` (`main.rs:3086-3092`):

```rust
struct ResolverAccessStatus {
    hostname: Option<String>,
    dns_targets: Vec<String>,
    tailscale_ip: Option<String>,
    notes: Vec<String>,
}
```

- `discover_dns_targets` (`main.rs:3280-3324`): configured targets first, then the `Host` header (port stripped), then — when the bind address is unspecified — every local IPv4 from `hostname -I` **excluding anything starting with `172.`** (a crude Docker-bridge filter that also drops legitimate `172.16/12` LAN addresses) plus global-scope IPv6 from `ip -6 -o addr show scope global`. Falls back to `127.0.0.1`. Sorted + deduped. `format_dns_target` omits `:port` when the port is 53 or when the host is an IPv4 literal / contains `:`.
- `hostname` ← `$HOSTNAME` else `hostname` command. `tailscale_ip` ← `tailscale ip -4`.
- **Shells out** to `hostname`, `sh -c "ip -6 …"`, `ipconfig` (macOS), `tailscale`. All failures degrade silently to empty.

### 4.30 `GET /api/v1/false-positive-budget` — `false_positive_budget_status` (`main.rs:3094-3137`)

- Response `200`: `ApiEnvelope<FalsePositiveBudgetStatus>` (`main.rs:3057-3067`):

```rust
struct FalsePositiveBudgetStatus {
    release_ready: bool, blocking_rate: f64, blocked_total: u64, queries_total: u64,
    false_positive_estimate: f64, budget_remaining: f64, budget_limit: f64,
    recommendations: Vec<String>,
}
```

- **Placeholder maths:** `budget_limit = 0.001` hardcoded; `false_positive_estimate = blocking_rate * 0.1` with the literal comment `// Assume 10% of blocked are false positives`. There is no false-positive measurement anywhere in the system.

### 4.31 `GET /api/v1/latency-budget` — `latency_budget_status` (`main.rs:3139-3204`)

- Response `200`: `ApiEnvelope<LatencyBudgetStatus>` (`main.rs:3078-3084`):

```rust
struct LatencyBudgetStatus {
    within_budget: bool, cache_hit_rate: f64,
    checks: Vec<LatencyBudgetCheck>, recommendations: Vec<String>,
}
struct LatencyBudgetCheck {                               // main.rs:3069-3076
    label: String,          // "Cache hit" | "Cache miss" | "Classifier monitor path"
    observed_ms: f64, target_p50_ms: f64, sample_count: u64,
    status: String,         // "insufficient-data" | "within-budget" | "over-budget"
}
```

- Targets hardcoded at `main.rs:3146-3165`: cache hit `1.0 ms`, cache miss `8.0 ms`, classifier `10.0 ms`. `observed_ms` is a **mean**, not a p50, despite the field name `target_p50_ms`.

### 4.32 `GET /api/v1/tailscale/status` — `tailscale_status` (`main.rs:1780-1786`)

- Ignores state (`State(_state)`). Response `200`: `ApiEnvelope<TailscaleStatusView>` (`main.rs:1635-1647`):

```rust
struct TailscaleStatusView {
    installed: bool, daemon_running: bool,
    backend_state: Option<String>, hostname: Option<String>, tailnet_name: Option<String>,
    peer_count: usize, exit_node_active: bool, version: Option<String>,
    health_warnings: Vec<String>, last_error: Option<String>,
}
```

- Shells out to `tailscale status --json` and `tailscale version`; also `tailscale debug prefs` via `read_tailscale_exit_node_pref`. **Blocking `std::process::Command` calls inside an async handler** — they block a tokio worker thread. `daemon_running` is computed as `backend_state != Some("Stopped")`, so an empty `{}` response reports `daemon_running: true` (asserted by the test at `main.rs:6536-6542`).

### 4.33 `POST /api/v1/tailscale/exit-node` — `tailscale_exit_node` (`main.rs:1799-1872`)

- Body — `TailscaleExitNodeRequest { enabled: bool }` (`main.rs:1788-1791`).
- Response `200`: `ApiEnvelope<TailscaleExitNodeResult { success: bool, message: String }>` (`main.rs:1793-1797`).
- Errors (style B): `400 "Tailscale is not installed"` / `"Tailscale daemon is not running"` / `"Cannot determine local Tailscale hostname"`; `500` + the `tailscale up` stderr.
- Side effects: runs `tailscale up --advertise-exit-node[=false] --accept-dns=false` (`main.rs:2057-2077`); writes the **previous** value to `.cogwheel_tailscale_state.json` next to the executable (`get_tailscale_state_path`, `main.rs:1881-1887`) as `TailscaleSavedState { exit_node_enabled, saved_at, hostname }`; audit `tailscale.exit_node_updated` `{"enabled","hostname","previous_enabled"}`. The file write error is discarded via `let _ =`.

### 4.34 `POST /api/v1/tailscale/rollback` — `tailscale_rollback` (`main.rs:1913-1979`)

- Body: none. Response `200`: `ApiEnvelope<TailscaleRollbackResult { success: bool, message: String, previous_state: Option<bool> }>` (`main.rs:1906-1911`).
- Errors: `404 "No previous Tailscale state found to rollback"`; `400` install/daemon checks; `500` + message.
- Side effects: re-runs `tailscale up`; deletes the saved-state file; audit `tailscale.rollback_completed`.

### 4.35 `GET /api/v1/tailscale/dns-check` — `tailscale_dns_check` (`main.rs:2011-2055`)

- **Takes no `State` extractor at all** — a pure function of host state.
- Response `200`: `ApiEnvelope<TailscaleDnsCheckResult>` (`main.rs:1981-1987`):

```rust
struct TailscaleDnsCheckResult {
    configured: bool, message: String,
    local_dns_server: Option<String>, suggestions: Vec<String>,
}
```

- `local_dns_server` is the first `nameserver` line of `/etc/resolv.conf` on Linux; `None` elsewhere. Never returns `Err` in practice despite the `Result` signature.

### 4.36 `GET /api/v1/sync/status` — `sync_status` (`main.rs:2367-2451`)

- **Not** guarded by `enforce_sync_transport_policy` — unlike every other sync route. Anyone who can reach the port can read the node's public key and peer topology.
- Response `200`: `ApiEnvelope<SyncNodeStatusView>` (`main.rs:2144-2153`):

```rust
struct SyncNodeStatusView {
    local_node_public_key: String,        // base64url-no-pad ed25519 public key
    profile: String,                      // "full"|"settings-only"|"read-only-follower"
    revision: u64,
    transport_mode: String,               // "opportunistic"|"https-required"
    transport_token_configured: bool,
    replay_cache_entries: usize,
    peers: Vec<SyncPeerStatusView>,
}
struct SyncPeerStatusView {               // main.rs:2135-2142
    node_public_key: String, imports: usize,
    last_import_at: DateTime<Utc>, last_revision: u64, profile: String,
}
```

- Peers are **reconstructed by scanning the last 200 audit events** for `event_type == "sync.state_imported"` and JSON-parsing each payload (`main.rs:2389-2435`). There is no peer table. Peer history silently truncates as the audit log grows.

### 4.37 `GET /api/v1/sync/profile` — `sync_profile` (`main.rs:2262-2275`)

- Guarded by `enforce_sync_transport_policy`. Response `200`: `ApiEnvelope<SyncProfileView { profile: String }>` (`main.rs:2119-2122`).
- Errors: `403` (https required, missing/incorrect `x-forwarded-proto`), `401` (missing/invalid bearer), `500`.

### 4.38 `POST /api/v1/sync/profile` — `update_sync_profile` (`main.rs:2277-2305`)

- Body — `UpdateSyncProfileRequest { profile: String }` (`main.rs:2124-2127`). `normalize_sync_profile` (`main.rs:2173-2179`) maps `"settings-only"`→`SettingsOnly`, `"read-only-follower"`→`ReadOnlyFollower`, **anything else (including typos) → `Full`**. Invalid input is silently coerced, never rejected.
- Response `200`: `ApiEnvelope<SyncProfileView>`. Persists `settings["sync_profile"]`; audit `sync.profile_updated`.

### 4.39 `GET /api/v1/sync/transport` — `sync_transport` (`main.rs:2307-2325`)

- Guarded. Response `200`: `ApiEnvelope<SyncTransportView { mode: String, token_configured: bool }>` (`main.rs:2129-2133`). The token itself is never returned.

### 4.40 `POST /api/v1/sync/transport` — `update_sync_transport` (`main.rs:2327-2365`)

- Body — `UpdateSyncTransportRequest { mode: String, token: Option<String> }` (`main.rs:2155-2159`). `normalize_sync_transport_mode` (`main.rs:2161-2171`): only `"https-required"` is recognised; **everything else → `"opportunistic"`**.
- Persists `settings["sync_transport_mode"]` and `settings["sync_transport_token"]` (empty string when cleared — **stored in plaintext in SQLite**); audit `sync.transport_updated` `{"mode","token_configured"}`.
- **Lockout hazard:** this endpoint is itself guarded by the policy it edits. Setting `https-required` behind a proxy that does not send `x-forwarded-proto: https` makes all sync endpoints permanently unreachable over HTTP; recovery requires editing SQLite directly.

### 4.41 `GET /api/v1/sync/export` — `export_sync_state` (`main.rs:2494-2556`)

- Guarded. Query — `SyncExportQuery { profile: Option<String> }` (`main.rs:2114-2117`); when present it overrides the stored profile for this export only.
- `403` when the effective profile is `ReadOnlyFollower`.
- Builds `SyncStatePayloadV1` (`main.rs:478-488`):

```rust
struct SyncStatePayloadV1 {
    version: u32,                       // always 1
    revision: u64,                      // stored revision + 1 (saturating) — NOT persisted here
    profile: String,
    exported_at: DateTime<Utc>,
    blocklists: Vec<SourceRecord>,      // empty unless profile == Full
    devices: Vec<DeviceRecord>,         // empty unless profile == Full
    classifier: ClassifierSettings,     // always included
    notifications: NotificationSettings, // always included — INCLUDING webhook_url
}
```

- Response `200`: `ApiEnvelope<SyncEnvelope>` (`crates/cogwheel-storage/src/lib.rs:128-135`):

```rust
pub struct SyncEnvelope {
    node_public_key: String,   // base64url-no-pad
    timestamp: DateTime<Utc>,
    nonce: String,             // Uuid v4 string
    payload_b64: String,       // base64url-no-pad of the JSON payload
    signature_b64: String,     // ed25519 over "{rfc3339}|{nonce}|{payload_bytes}"
}
```

- The exported revision is **not** written back to `settings["sync_revision"]`, so repeated exports keep emitting the same `revision + 1`.

### 4.42 `POST /api/v1/sync/import` — `import_sync_state` (`main.rs:2558-2682`)

- Guarded. Body — `ImportSyncEnvelopeRequest { envelope: SyncEnvelope }` (`main.rs:508-511`).
- Pipeline: `Storage::verify_sync_envelope` (ed25519) → JSON-decode `SyncStatePayloadV1` → require `version == 1` → `register_sync_nonce` (`main.rs:2470-2492`: rejects timestamps older than 10 min or more than 30 s in the future, rejects a repeated `{pubkey}:{nonce}`, prunes entries older than 30 min) → last-writer-wins comparison via `is_sync_payload_newer` (`main.rs:2460-2468`: higher revision wins; ties broken by lexicographically greater node public key).
- Response `200`: `ApiEnvelope<SyncImportResult>` (`main.rs:2106-2112`):

```rust
struct SyncImportResult {
    imported_sources: usize, imported_devices: usize,
    applied_revision: u64, profile: String,
}
```

- Errors: `403`/`401` transport; `400` for a bad signature, malformed payload, `version != 1`, or replayed/stale nonce; **`409 CONFLICT`** when the payload is not newer; `500`.
- **Destructive when `profile == Full`:** deletes **every** existing source and **every** existing device before inserting the payload's, and does so **without a transaction** (`main.rs:2592-2632`). A failure mid-way leaves the node with an empty or half-populated source/device set — and deleting all sources means the next refresh fails with `"no enabled sources configured"`.
- Always applies classifier and notification settings regardless of profile (persist + in-memory swap), persists `sync_revision`, calls `sync_runtime_device_policies`, and writes audit `sync.state_imported` `{"from","revision","profile","sources","devices"}`.
- **Any valid ed25519 keypair is accepted — there is no peer allowlist.** The only gate is the optional shared bearer token.

### 4.43 `GET /api/v1/rulesets` — `list_rulesets` (`main.rs:2684-2705`)

- Response `200`: `ApiEnvelope<Vec<RulesetSummary>>`, ordered `created_at DESC`, **unbounded** (every ruleset ever recorded; each refresh adds a row and nothing prunes). `artifact_json` is deliberately not exposed.

### 4.44 `POST /api/v1/rulesets/rollback` — `rollback_ruleset` (`main.rs:2707-2785`)

- Body: none. Response `200`: `ApiEnvelope<RulesetSummary>` with `status: "active"`.
- Errors: `404` when no `previous` ruleset exists; bare `500`.
- Persists: `activate_ruleset(previous.id)` (flips `active`→`previous` and target→`active` in one transaction); hot-swaps the runtime policy catalog (profile policies rebuilt via `load_current_runtime_policy_catalog`, which **re-fetches every enabled source over HTTP**; on failure it degrades to an empty profile map with only a warning); `sync_runtime_device_policies`; audit `ruleset.rollback` `{"ruleset_id","hash"}`; delivers a `high` operational notification if enabled.

### 4.45 `GET /api/v1/audit-events` — `list_audit_events` (`main.rs:2787-2796`)

- Response `200`: `ApiEnvelope<Vec<AuditEvent>>`, hardcoded `LIMIT 20`, `created_at DESC`. No pagination or filtering. `payload` is a JSON-encoded string (double encoding).

### 4.46 `GET /api/v1/backup` — `backup_data` (`main.rs:2820-2852`)

- Response `200`: `ApiEnvelope<BackupData>` (`main.rs:2798-2806`):

```rust
struct BackupData {
    version: String,                     // "1.0"
    created_at: String,                  // RFC3339 string, NOT DateTime
    sources: Vec<SourceRecord>,
    devices: Vec<DeviceRecord>,
    classifier: ClassifierSettings,
    notifications: NotificationSettings, // includes webhook_url in cleartext
}
```

- Not a file download (no `Content-Disposition`). Omits: block profiles, service toggles, notification presets, sync settings, rulesets, audit events, security events, node identity.

### 4.47 `POST /api/v1/backup/restore` — `restore_data` (`main.rs:2854-2905`)

- Body — `RestoreRequest { data: BackupData }` (`main.rs:2808-2811`).
- Response `200`: `ApiEnvelope<BackupResult { success: bool, message: String, size_bytes: usize }>` (`main.rs:2813-2818`). `success` is **hardcoded `true`** — it is never false.
- Known defects in this handler:
  - Per-record errors are swallowed: `let _ = state.storage.insert_source(source).await;` and the same for devices (`main.rs:2863-2869`).
  - `data.classifier` is **never applied** — neither persisted nor pushed to the runtime.
  - `data.notifications` is written to the in-memory `RwLock` **only**; `persist_notification_settings` is never called, so it is lost on restart.
  - `sync_runtime_device_policies` is never called, so restored devices do not affect DNS until another mutation triggers a sync.
  - Uses `.unwrap()` on the `RwLock` write (`main.rs:2872`).
  - It is additive, not a true restore — pre-existing sources/devices that are absent from the backup survive.
- Persists: `sources`, `devices`, audit `backup.restore_completed` `{"version","source_count","device_count","size_bytes"}`.

### 4.48-4.51 Resilience drills — `POST /api/v1/resilience/{upstream-outage,db-corruption,source-failure,sync-partition}`

All four take the same body (`ResilienceDrillRequest { duration_secs: Option<u64> }`, `main.rs:2915-2919`) which is bound to `_request` and **ignored** — `duration_secs` even carries `#[allow(dead_code)]`. All four return `ApiEnvelope<ResilienceDrillResult>` (`main.rs:2907-2913`):

```rust
struct ResilienceDrillResult {
    drill_type: String, success: bool, message: String, recommendations: Vec<String>,
}
```

| Route | `drill_type` | What it actually does | `success` |
| --- | --- | --- | --- |
| `/resilience/upstream-outage` (`2921`) | `"upstream_outage"` | reads `snapshot.upstream_failures_total` / `fallback_served_total` | `fallback_served_total > 0` |
| `/resilience/db-corruption` (`2956`) | `"db_corruption"` | issues `list_sources()` + `list_devices()` | both `Ok` |
| `/resilience/source-failure` (`2989`) | `"source_failure"` | counts enabled vs total sources | `enabled_count > 0` |
| `/resilience/sync-partition` (`3025`) | `"sync_partition"` | reads transport mode/token | `token.is_some() \|\| mode != "disabled"` |

**None of these simulate or inject anything.** They are read-only status reports with hardcoded advice strings. `simulate_sync_partition`'s `mode != "disabled"` test is dead: `normalize_sync_transport_mode` can only ever yield `"opportunistic"` or `"https-required"`, so `transport_ok` is unconditionally `true`.

### 4.52 `POST /api/v1/load-test` — `run_load_test` (`main.rs:1197-1308`)

- Body — `LoadTestRequest { duration_secs: u64, qps: u32, cache_hit_ratio: f64 }` (`main.rs:1166-1171`). `qps` is `.max(1)`; `cache_hit_ratio` is `.clamp(0.0, 1.0)`; **`duration_secs` is unbounded** — a caller can pin a worker for arbitrarily long.
- Response `200`: `ApiEnvelope<LoadTestResult>` (`main.rs:1173-1185`):

```rust
struct LoadTestResult {
    success: bool,               // failed == 0
    queries_sent: u64, queries_succeeded: u64, queries_failed: u64,
    avg_latency_ms: f64, p95_latency_ms: f64, p99_latency_ms: f64,
    cache_hit_ratio: f64,        // ECHOED BACK from the request, not measured
    throughput_qps: f64,
    errors: Vec<String>,         // first 10 only
}
```

- Behaviour: loops until `duration_secs` elapses, iterating a hardcoded 15-domain list (`main.rs:1213-1229`) and calling `dns_runtime.probe_domain(domain, RecordType::A)`. Real recursive traffic hits the configured upstreams. The `cache_hit_ratio` request field only steers which domain is picked; the response field is the request value verbatim.
- Errors: signature declares `(StatusCode, String)` but the body has no error path — it always returns `Ok`.

### 4.53 `GET /api/v1/benchmark/rust-opts` — `benchmark_rust_opts` (`main.rs:1310-1382`)

- Response `200`: `ApiEnvelope<RustOptimizationBenchmark>` (`main.rs:1187-1195`):

```rust
struct RustOptimizationBenchmark {
    domain_parsing_ns: u64, rule_matching_ns: u64, cache_lookup_ns: u64,
    memory_usage_bytes: u64,      // HARDCODED 0
    allocations_per_query: u64,   // HARDCODED 0
    recommendations: Vec<String>,
}
```

- **Placeholder microbenchmark:** the "domain parsing" measurement times `let _domain: &str = "example.com";` and "rule matching" times `"example.com".contains("example")` — neither touches the real parser or the real policy engine. It runs 10 000 iterations synchronously inside the async handler, blocking the worker.

### 4.54 `GET /api/v1/config/version` — `config_version` (`main.rs:1394-1427`)

- Response `200`: `ApiEnvelope<ConfigVersionStatus>` (`main.rs:1384-1392`):

```rust
struct ConfigVersionStatus {
    schema_version: u32,        // cogwheel_storage::SCHEMA_VERSION == 10
    config_version: u32,        // from config_schema table, .unwrap_or(1)
    cogwheel_version: String,   // env!("CARGO_PKG_VERSION") == "0.1.0"
    migration_count: u32,       // HARDCODED 10
    upgrade_available: bool,    // stored < CONFIG_SCHEMA_VERSION (1) => always false today
    recommendations: Vec<String>,
}
```

- `CONFIG_SCHEMA_VERSION` is `1` (`storage:26`), so `upgrade_available` is currently always `false`. `migration_count` is a literal that will drift from the real migration count.

### 4.55 `GET /api/v1/threat-intel/providers` — `threat_intel_settings` (`main.rs:1484-1493`)

- Response `200`: `ApiEnvelope<ThreatIntelSettings>` (`main.rs:118-122`):

```rust
struct ThreatIntelSettings {
    providers: Vec<ThreatIntelProviderConfig>,
    recommendations: Vec<String>,
}
struct ThreatIntelProviderConfig {                        // main.rs:105-116
    id: String, display_name: String, enabled: bool,
    feed_url: Option<String>, api_key_configured: bool,
    update_interval_minutes: u32,
    last_sync_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    capabilities: Vec<String>,
}
```

- Defaults (`default_threat_intel_settings`, `main.rs:1429-1465`): `alphamountain` with feed `https://api.example.invalid/threat-intel/dns` (an **intentionally invalid** placeholder domain) and `abuse-ch` with the real Feodo Tracker URL. Both `enabled: false`, `api_key_configured: false`.
- Errors: bare `500` if the `RwLock` is poisoned.
- **Entirely non-functional:** nothing anywhere fetches these feeds, sets `last_sync_at`, sets `last_error`, or consults threat intel during DNS resolution.

### 4.56 `POST /api/v1/threat-intel/providers` — `update_threat_intel_provider` (`main.rs:1495-1537`)

- Body — `ThreatIntelProviderUpdate` (`main.rs:124-130`):

```rust
struct ThreatIntelProviderUpdate {
    id: String, enabled: bool, feed_url: Option<String>, update_interval_minutes: u32,
}
```

- `update_interval_minutes` is clamped `.max(5)`. `last_error` is reset to `None` on every update.
- Response `200`: `ApiEnvelope<ThreatIntelSettings>` (the whole settings object).
- Errors: `404` for an unknown provider id (providers cannot be created, only the 2 built-ins edited); bare `500`.
- Persists: **in-memory only.** Writes audit `threat_intel_provider_updated` but never touches the `settings` table — all edits are lost on restart.

### 4.57 `GET /api/v1/federated-learning/status` — `federated_learning_settings` (`main.rs:1539-1548`)

- Response `200`: `ApiEnvelope<FederatedLearningSettings>` (`main.rs:132-143`):

```rust
struct FederatedLearningSettings {
    enabled: bool,
    coordinator_url: Option<String>,
    node_id: String,                     // hardcoded "local-node"
    round_interval_hours: u32,           // default 24
    last_round_at: Option<DateTime<Utc>>,
    last_model_version: Option<String>,
    privacy_mode: String,                // hardcoded "model-updates-only"
    raw_log_export_enabled: bool,        // forced false
    recommendations: Vec<String>,
}
```

- **Entirely non-functional:** no training, no rounds, no coordinator contact anywhere in the codebase. `last_round_at` and `last_model_version` are never written.

### 4.58 `POST /api/v1/federated-learning/status` — `update_federated_learning_settings` (`main.rs:1550-1587`)

- Body — `FederatedLearningUpdate { enabled: bool, coordinator_url: Option<String>, round_interval_hours: u32 }` (`main.rs:145-150`). `round_interval_hours` clamped `.max(1)`; `raw_log_export_enabled` is hard-forced to `false` on every write.
- Response `200`: `ApiEnvelope<FederatedLearningSettings>`.
- Persists: **in-memory only** + audit `federated_learning_updated`.

---

## 5. Sync transport guard — `enforce_sync_transport_policy` (`main.rs:2225-2260`)

```rust
async fn enforce_sync_transport_policy(
    state: &ServerState,
    headers: &HeaderMap,
) -> Result<(), axum::http::StatusCode>
```

1. Loads `settings["sync_transport_mode"]`. If `"https-required"`, requires header `x-forwarded-proto == "https"` (case-insensitive) → else `403 FORBIDDEN`.
2. Loads `settings["sync_transport_token"]`. If non-empty, requires `Authorization: Bearer <token>` with an exact match → else `401 UNAUTHORIZED`.
3. Storage failure at either step → `500`.

Applied to: `GET/POST /api/v1/sync/profile`, `GET/POST /api/v1/sync/transport`, `GET /api/v1/sync/export`, `POST /api/v1/sync/import`.

**Not applied to `GET /api/v1/sync/status`** — an inconsistency that leaks the node public key, revision, transport configuration, and peer list unauthenticated.

Comparison is a plain `!=` on `&str` — **not constant-time**, so the token check is theoretically timing-attackable. The `x-forwarded-proto` check trusts a client-supplied header with no trusted-proxy configuration.

---

## 6. `refresh_sources_once` — the ruleset build pipeline (`main.rs:3905-4145`)

Signature: `async fn refresh_sources_once(state: &ServerState, reason: &str, only_source_ids: Option<&HashSet<Uuid>>) -> Result<RefreshResponse>`

Callers and their `reason` strings:

| Caller | `reason` | `only_source_ids` |
| --- | --- | --- |
| scheduled loop (`main.rs:693`) | `"scheduled"` | `Some(due_ids)` |
| `refresh_sources` (`main.rs:3490`) | `"manual"` | `None` |
| `update_service_toggle` (`main.rs:3540`) | `"service-toggle"` | `None` |
| `upsert_blocklist` (`main.rs:3777`) | `"blocklist-update"` | `None` |
| `update_blocklist_state` (`main.rs:3828`) | `"blocklist-state-update"` | `None` |
| `delete_blocklist` (`main.rs:3890`) | `"blocklist-delete"` | `None` |

Steps:

1. Build a fresh 15 s-timeout `reqwest::Client` (**per call** — no connection reuse across refreshes).
2. Select enabled sources, optionally filtered by `only_source_ids`. `anyhow::ensure!(!selected.is_empty(), "no enabled sources configured")` — this surfaces to HTTP as a bare `500`.
3. `update_source_refresh_attempts` → persists `settings["source_refresh_state"]` (`SourceRefreshState { entries: Vec<SourceRefreshStateEntry { source_id, last_refresh_attempt_at }> }`, `main.rs:351-360`) **before** fetching, so a failed fetch still counts as an attempt.
4. `fetch_and_parse_source` for each — **sequential, not concurrent**; the first error aborts the whole refresh with `?`.
5. Compile the service-toggle layer and append it as a synthetic source named `"service-toggles"` with `profile: "shared"`.
6. `verify_candidate(&parsed_sources, &state.protected_domains)`. On failure: audit `ruleset.refresh_rejected` `{reason, notes, blocked_protected_domains, invalid_ratio}`, optionally deliver a `high` webhook, return `RefreshResponse { outcome: "rejected", ruleset: None, notes }`.
7. `build_runtime_policy_catalog(..., BlockMode::NullIp)` — **block mode is hardcoded; there is no way to select NxDomain/NoData/Refused/CustomIp through the API.**
8. `record_ruleset(status: "candidate")` → snapshot "before" → `activate_ruleset` → `replace_policy_catalog` → `sync_runtime_device_policies`.
9. `post_activation_regressions` (protected-domain probe against the new engine) + `run_runtime_guard_probes` (live DNS probes for `runtime_guard.probe_domains`).
10. On any regression note: `rollback_to_previous_ruleset` (bails hard if none), swap in the rolled-back policy **with an empty profile map** (`HashMap::new()` at `main.rs:4058` — profile policies are silently dropped on auto-rollback), `sync_runtime_device_policies`, audit `ruleset.auto_rollback` `{reason, rolled_back_to, notes}`, optionally deliver a `critical` webhook, return `outcome: "rolled_back"`.
11. Otherwise audit `ruleset.activated` `{ruleset_id, hash, reason}` and return `outcome: "activated"` with notes `["refreshed N source(s)", ...service notes]`.

Note the activation ordering: the policy is swapped into the live runtime **before** the guard probes run, so there is a real window during which a bad ruleset serves traffic.

---

## 7. Metrics

### 7.1 The registry

Created at `main.rs:586-594`:

```rust
let mut registry = Registry::default();
let startup_counter: Counter<u64> = Counter::default();
registry.register(
    "cogwheel_startups_total",
    "Number of server startups",
    startup_counter.clone(),
);
startup_counter.inc();
let registry = Arc::new(registry);
```

**That is the complete metric inventory: exactly one counter, `cogwheel_startups_total`, incremented exactly once per process start.** `grep -rn "registry.register"` across `apps/` and `crates/` returns this single call site.

### 7.2 What is NOT in `/metrics`

Every operationally meaningful number lives in `DnsRuntimeStats` (`crates/cogwheel-dns-core/src/lib.rs:80-95`) as plain `AtomicU64`s and is only reachable through `GET /api/v1/runtime` as JSON:

`upstream_failures_total`, `fallback_served_total`, `cache_hits_total`, `cname_uncloaks_total`, `cname_blocks_total`, `queries_total`, `blocked_total`, `cache_hit_latency_total_ns` + `cache_hit_samples`, `cache_miss_latency_total_ns` + `cache_miss_samples`, `classifier_latency_total_ns` + `classifier_latency_samples`.

Latency is exposed only as an **arithmetic mean** (`average_atomic_ns`, `dns-core:699-705`) — there are no histograms and therefore no true percentiles anywhere in the system, despite `LatencyBudgetCheck.target_p50_ms` and `LoadTestResult.p95_latency_ms`/`p99_latency_ms` implying otherwise. (`run_load_test` does compute genuine p95/p99, but only over its own synthetic sample set.)

Also absent from metrics: HTTP request counts/latency/status codes, notification delivery outcomes, ruleset activation/rollback counts, source refresh success/failure, storage errors, device counts.

---

## 8. Static file serving & SPA fallback

### 8.1 `resolve_web_dist_dir` — `main.rs:766-783`

```rust
fn resolve_web_dist_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("COGWHEEL_WEB_DIST_DIR") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.join("apps/cogwheel-web/dist"));
        candidates.push(current_dir.join("dist"));
    }
    candidates.push(PathBuf::from("/app/web"));
    candidates.into_iter().find(|c| c.join("index.html").is_file())
}
```

Resolution order (first candidate whose `index.html` **is a file** wins):

1. `$COGWHEEL_WEB_DIST_DIR`
2. `$PWD/apps/cogwheel-web/dist`
3. `$PWD/dist`
4. `/app/web`

`COGWHEEL_WEB_DIST_DIR` is read **directly via `std::env::var`**, not through `AppConfig` — it is not part of the config struct and has no profile default. `Dockerfile:18` sets `ENV COGWHEEL_WEB_DIST_DIR=/app/web`.

### 8.2 Fallback behaviour — `main.rs:752-761`

- **Found:** `fallback_service(ServeDir::new(dir).not_found_service(ServeFile::new(dir/index.html)))`. Any request not matching a registered route is served from disk; a miss serves `index.html` with `200` so client-side routing works. Logs `INFO "serving bundled web assets"` with the path.
- **Not found:** no fallback is installed; unmatched paths get axum's default `404` with an empty body. Logs `WARN "web assets not found; serving API routes only"`.

Consequences:

- An unknown `/api/v1/...` path returns **`index.html` with HTTP 200**, not a 404, whenever web assets are present. The web client would then fail on `response.json()`. There is no `/api` route-scoped 404 guard.
- `GET /favicon.ico` is explicitly registered to `204` so it does not fall through to `index.html`.
- No cache-control headers are configured — `ServeDir` defaults apply, so the hashed Vite bundle and the `index.html` shell get the same caching treatment.

### 8.3 Web client base URL

`apps/cogwheel-web/src/lib/api.ts:357`:

```ts
const API_BASE = import.meta.env.VITE_COGWHEEL_API_BASE
  ?? (typeof window !== "undefined" ? window.location.origin : "http://127.0.0.1:8080");
```

Same-origin by default. Every request sends `Content-Type: application/json` and `X-Requested-With: XMLHttpRequest` (the server ignores both). Using `VITE_COGWHEEL_API_BASE` cross-origin **will fail** — the server has no CORS layer.

---

## 9. Configuration

### 9.1 `AppConfig` — `crates/cogwheel-api/src/lib.rs:125-249`

```rust
pub struct AppConfig {
    pub profile: DeploymentProfile,     // Dev | Home (default) | Smb
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub upstream: UpstreamConfig,
    pub updater: UpdaterConfig,
    pub runtime_guard: RuntimeGuardConfig,
}
pub struct ServerConfig { http_bind_addr: SocketAddr, dns_udp_bind_addr: SocketAddr, dns_tcp_bind_addr: SocketAddr }
pub struct StorageConfig { database_url: String }
pub struct UpstreamConfig { servers: Vec<String> }
pub struct UpdaterConfig { refresh_interval_secs: u64 }
pub struct RuntimeGuardConfig { probe_domains: Vec<String>, max_upstream_failures_delta: u64, max_fallback_served_delta: u64 }
```

Load order (`load_from_env`, `cogwheel-api:140-198`): read `COGWHEEL_PROFILE` → `AppConfig::for_profile(profile)` → apply each explicit env override on top.

**Despite `config = { version = "0.15", features = ["toml"] }` being a declared dependency of `cogwheel-api`, no TOML/file-based configuration is ever loaded. There is no config file. `humantime-serde` is likewise declared and unused.**

### 9.2 Profile defaults — `for_profile` (`cogwheel-api:200-248`)

| Setting | `dev` | `home` (default) | `smb` |
| --- | --- | --- | --- |
| `server.http_bind_addr` | `127.0.0.1:30080` | `0.0.0.0:8080` | `0.0.0.0:8080` |
| `server.dns_udp_bind_addr` | `127.0.0.1:30053` | `0.0.0.0:5353` | `0.0.0.0:53` |
| `server.dns_tcp_bind_addr` | `127.0.0.1:30053` | `0.0.0.0:5353` | `0.0.0.0:53` |
| `storage.database_url` | `sqlite://data/cogwheel-dev.db` | `sqlite://data/cogwheel.db` | `sqlite://data/cogwheel-smb.db` |
| `updater.refresh_interval_secs` | `120` | `300` | `600` |
| `runtime_guard.max_upstream_failures_delta` | `2` | `0` | `1` |
| `runtime_guard.max_fallback_served_delta` | `5` | `0` | `2` |
| `upstream.servers` | `["1.1.1.1:53","1.0.0.1:53"]` | same | same |
| `runtime_guard.probe_domains` | `["example.com","connectivitycheck.gstatic.com"]` | same | same |

Note `home` inherits the struct `Default` of `0` for both guard deltas (the `Home` arm does not set them), which is what makes the dashboard's zero-baseline health comparison latch to degraded so easily.

### 9.3 Complete `COGWHEEL_*` environment variable inventory

| Variable | Read at | Type / parsing | Default | On parse failure |
| --- | --- | --- | --- | --- |
| `COGWHEEL_PROFILE` | `cogwheel-api:144` | `"dev"\|"home"\|"smb"` | `home` | `ApiError::InvalidEnv` → startup abort |
| `COGWHEEL_SERVER__HTTP_BIND_ADDR` | `cogwheel-api:150` | `SocketAddr` | profile | `InvalidEnv` → abort |
| `COGWHEEL_SERVER__DNS_UDP_BIND_ADDR` | `cogwheel-api:154` | `SocketAddr` | profile | `InvalidEnv` → abort |
| `COGWHEEL_SERVER__DNS_TCP_BIND_ADDR` | `cogwheel-api:158` | `SocketAddr` | profile | `InvalidEnv` → abort |
| `COGWHEEL_STORAGE__DATABASE_URL` | `cogwheel-api:162` | raw string (`sqlite://` prefix stripped by `Storage::connect`) | profile | n/a |
| `COGWHEEL_UPSTREAM__SERVERS` | `cogwheel-api:165` | comma-separated `SocketAddr` list, trimmed, empties dropped | `1.1.1.1:53,1.0.0.1:53` | not validated here; `build_resolver` fails at startup |
| `COGWHEEL_UPDATER__REFRESH_INTERVAL_SECS` | `cogwheel-api:173` | `u64` | profile | `InvalidEnv` → abort |
| `COGWHEEL_RUNTIME_GUARD__PROBE_DOMAINS` | `cogwheel-api:178` | comma-separated strings | `example.com,connectivitycheck.gstatic.com` | n/a |
| `COGWHEEL_RUNTIME_GUARD__MAX_UPSTREAM_FAILURES_DELTA` | `cogwheel-api:186` | `u64` | profile | `InvalidEnv` → abort |
| `COGWHEEL_RUNTIME_GUARD__MAX_FALLBACK_SERVED_DELTA` | `cogwheel-api:191` | `u64` | profile | `InvalidEnv` → abort |
| `COGWHEEL_SERVER__ADVERTISED_DNS_PORT` | **`main.rs:655`** (bypasses `AppConfig`) | `u16`, `.ok().and_then(parse)` | `dns_udp_bind_addr.port()` | **silently ignored** |
| `COGWHEEL_SERVER__ADVERTISED_DNS_TARGETS` | **`main.rs:659`** (bypasses `AppConfig`) | comma-separated strings | `[]` | n/a |
| `COGWHEEL_WEB_DIST_DIR` | **`main.rs:769`** (bypasses `AppConfig`) | path | see §8.1 chain | falls through to next candidate |

Non-`COGWHEEL_` environment variables consulted:

| Variable | Read at | Purpose |
| --- | --- | --- |
| `RUST_LOG` (via `EnvFilter::from_default_env`) | `main.rs:723` | tracing filter; an `info` directive is always added |
| `HOSTNAME` | `main.rs:3241` | first choice for `ResolverAccessStatus.hostname` |
| `VITE_COGWHEEL_API_BASE` | `api.ts:357` (build-time, web only) | client API base URL |

The double-underscore convention (`COGWHEEL_SECTION__FIELD`) is applied by hand in `load_from_env`, not by the `config` crate. The three `main.rs`-level variables (`ADVERTISED_DNS_PORT`, `ADVERTISED_DNS_TARGETS`, `WEB_DIST_DIR`) **do not appear in `AppConfig` at all** and are therefore invisible to the config tests in `cogwheel-api`.

### 9.4 Persisted configuration — SQLite `settings` table (key/value/updated_at)

| Key | Written by | Read by | Value format |
| --- | --- | --- | --- |
| `node_identity_v1` | `Storage::connect` (`storage:230-233`) | `Storage::connect` | base64url-no-pad ed25519 **private** signing key (32 bytes) — **stored unencrypted** |
| `classifier_settings` | `persist_classifier_settings` (`main.rs:4779`) | `load_classifier_settings` (`main.rs:4154`) | `{"mode":"Monitor","threshold":0.92}` |
| `notification_settings` | `persist_notification_settings` (`main.rs:4789`) | `load_notification_settings` (`main.rs:4161`) | `NotificationSettings` JSON incl. webhook URL |
| `notification_test_presets` | `persist_notification_test_presets` (`main.rs:4799`) | `load_notification_test_presets` (`main.rs:4178`) | JSON array, max 8 |
| `service_toggles` | `persist_service_toggle_snapshot` (`main.rs:4762`) | `load_service_toggle_snapshot` (`main.rs:4147`) | `ServiceToggleSnapshot` JSON |
| `source_refresh_state` | `persist_source_refresh_state` (`main.rs:4772`) | `load_source_refresh_state` (`main.rs:4619`) | `SourceRefreshState` JSON |
| `block_profiles` | `persist_block_profiles` (`main.rs:4203`) | `load_block_profiles` (`main.rs:4187`) | `Vec<BlockProfileRecord>` JSON |
| `sync_revision` | `persist_sync_revision` (`main.rs:2453`) | `load_sync_revision` (`main.rs:2181`) | decimal `u64` string |
| `sync_profile` | `persist_sync_profile` (`main.rs:2194`) | `load_sync_profile` (`main.rs:2189`) | `"full"\|"settings-only"\|"read-only-follower"` |
| `sync_transport_mode` | `persist_sync_transport_mode` (`main.rs:2206`) | `load_sync_transport_mode` (`main.rs:2201`) | `"opportunistic"\|"https-required"` |
| `sync_transport_token` | `persist_sync_transport_token` (`main.rs:2218`) | `load_sync_transport_token` (`main.rs:2211`) | **plaintext shared secret**, `""` when cleared |

Every `load_*` helper swallows deserialization errors with `.unwrap_or_default()` / `.unwrap_or(...)`. A corrupted `classifier_settings` row therefore silently resets to `Monitor @ 0.92` with no log line — a genuinely dangerous failure mode for a security setting.

### 9.5 Database schema

`crates/cogwheel-storage/migrations/000{1..10}_*.sql`, applied by `apply_migrations` (`storage:630-642`). **Only migration 0001 is checked (`?`); migrations 0002-0010 use `let _ = ...` and their errors are discarded entirely.** A failed migration produces a silently broken schema. `SCHEMA_VERSION = 10`, `CONFIG_SCHEMA_VERSION = 1`.

Tables: `settings`, `audit_events`, `sources`, `rulesets`, `active_ruleset`, `devices`, `security_events`, `notification_deliveries`, `config_schema`, `config_migrations`.

Connection: single `rusqlite::Connection` behind `Arc<Mutex<...>>` — **all storage access is serialized through one mutex, and every `Storage` method is `async` while doing blocking I/O on the tokio runtime without `spawn_blocking`.** PRAGMAs: `journal_mode = WAL`, `foreign_keys = ON`.

---

## 10. Classifier: invocation sites and persistence

### 10.1 The classifier itself — `crates/cogwheel-classifier/src/lib.rs`

```rust
pub struct LexicalFeatures { length: usize, digit_ratio: f32, hyphen_ratio: f32, label_depth: usize, entropy: f32 }  // :4-11
pub enum ClassifierMode { Off, Monitor, Protect }                                                                     // :13-18
pub struct ClassifierSettings { mode: ClassifierMode, threshold: f32 }                                                // :20-24
impl Default for ClassifierSettings { mode: Monitor, threshold: 0.92 }                                                // :26-33
pub struct Classification { score: f32, reasons: Vec<String>, observed_at: DateTime<Utc> }                            // :35-40

pub fn extract_lexical_features(domain: &str) -> LexicalFeatures;                                                     // :42-66
pub fn classify_domain(domain: &str, settings: &ClassifierSettings) -> Option<Classification>;                        // :68-85
```

The entire model is three lines (`lib.rs:73-74`):

```rust
let features = extract_lexical_features(domain);
let score = ((features.entropy / 5.0) + features.digit_ratio + features.hyphen_ratio).min(1.0);
```

There is no model file, no training, no weights, no feature normalisation, and `label_depth`/`length` are computed but **never used in the score**. `reasons` is a fixed 3-element vector of formatted feature values. `entropy` is Shannon entropy over the raw character distribution of the full domain string (including dots), divided by an arbitrary constant `5.0`.

The crate has **zero dependencies on any other workspace crate**, and `crates/cogwheel-dns-core/src/lib.rs:912-937` contains a test asserting the classifier manifest declares no network/ML dependencies ("classifier inference must remain local and deterministic").

### 10.2 The single hot-path call site — `crates/cogwheel-dns-core/src/lib.rs:317-325`

Inside `DnsRuntime::handle_wire_query` (the one function that serves **both** UDP and TCP, and also every `probe_domain` call):

```rust
317        let classifier_settings = self.classifier_settings();
318        let classifier_start = Instant::now();
319        if let Some(classification) = classify_domain(&domain, &classifier_settings) {
320            tracing::debug!(domain, score = classification.score, "domain classified");
321            if classification.score >= classifier_settings.threshold {
322                self.emit_classification_event(&domain, client_addr, classification);
323            }
324        }
325        self.record_classifier_latency(classifier_start.elapsed().as_nanos());
```

Critical positional facts:

- Line **306**: `queries_total` incremented. Line **308**: message parsed. Line **315**: domain lowercased and trailing dot stripped.
- The classifier runs at line **319**, i.e. **before** the cache lookup (line 330), **before** policy selection (line 327), and **before** any block decision. It therefore executes on **100 % of queries including cache hits**, with a `RwLock` read acquired per query at line 317.
- `ClassifierMode::Off` is the only mode that short-circuits — `classify_domain` returns `None` (`classifier lib.rs:69-71`).
- **`ClassifierMode::Monitor` and `ClassifierMode::Protect` are behaviourally identical.** Nothing anywhere matches on `Protect`. The classifier can observe and alert, but it can **never block a query**. The score never reaches `policy_for_client`, `engine.evaluate`, or `build_blocked_response`.
- Above-threshold hits call `emit_classification_event` → `build_classification_event` (`dns-core:647-658`) → the registered `ClassificationObserver`.

Other classifier touchpoints in `dns-core`:

| Line(s) | What |
| --- | --- |
| `39` | `classifier_settings: Arc<RwLock<ClassifierSettings>>` field |
| `40` | `classification_observer: Arc<RwLock<Option<ClassificationObserver>>>` field |
| `93-94` | `classifier_latency_total_ns`, `classifier_latency_samples` atomics |
| `110-111` | `classifier_latency_avg_ns`, `classifier_latency_samples` on `DnsRuntimeSnapshot` |
| `176-181` | `classifier_settings()` getter — `.expect("classifier settings lock poisoned")` |
| `183-187` | `replace_classifier_settings()` — hot-swap, silently no-ops if the lock is poisoned |
| `189-193` | `set_classification_observer()` |
| `204-207`, `226-230` | snapshot aggregation |
| `430-437` | `record_classifier_latency` |
| `455-470` | `emit_classification_event` |

### 10.3 The observer chain — `main.rs:605-626` → `record_security_event_from_classification` (`main.rs:5775-5827`)

```rust
dns_runtime.set_classification_observer(Arc::new({ ... move |event| {
    tokio::spawn(async move {
        if let Err(error) = record_security_event_from_classification(
            storage, http_client, notification_settings, event).await {
            tracing::warn!(%error, "failed to record security event");
        }
    });
}}));
```

`record_security_event_from_classification`:

1. **Returns `Ok(())` immediately when `event.client_ip` is `None`** (`main.rs:5781-5783`). Since `probe_domain` passes `client_addr: None` (`dns-core:240`), health-check and load-test traffic never produces security events. Real UDP/TCP traffic always has a peer address.
2. `storage.find_device_by_ip(&client_ip)` for device attribution.
3. `severity_for_classifier_score` (`main.rs:5255-5263`): `>= 0.99` → `"critical"`; `>= 0.96` → `"high"`; else `"medium"`.
4. `storage.record_security_event(&SecurityEventRecord { ... classifier_score: f64::from(score) ... })`.
5. If severity is `high`/`critical`, writes audit `security.alert_raised`.
6. If `should_deliver_notification` passes, calls `deliver_security_notification` (3 attempts, backoff `250ms << attempt` capped at 16× — `notification_retry_delay`, `main.rs:5500-5503`).

Each above-threshold classification spawns an independent tokio task that performs a DB lookup, a DB insert, possibly a second DB insert, and possibly up to 3 outbound HTTP requests — all funnelled through the single storage `Mutex`. **There is no queue, no batching, no backpressure, and no de-duplication**, so a burst of DGA-like traffic fans out into unbounded task spawning contending on one lock.

### 10.4 Classifier settings persistence

| Path | Detail |
| --- | --- |
| Load at boot | `load_classifier_settings(&storage)` (`main.rs:4154-4159`) reads `settings["classifier_settings"]`; missing → `ClassifierSettings::default()`; **malformed → silently `default()`** |
| Injected into runtime | `DnsRuntime::new(resolver, policy, classifier_settings)` (`main.rs:604`) |
| Write via API | `POST /api/v1/settings/classifier` → `persist_classifier_settings` then `dns_runtime.replace_classifier_settings` then audit `classifier-settings.updated` (`main.rs:3546-3574`) |
| Write via sync import | `import_sync_state` (`main.rs:2634-2639`) — persist + hot-swap, no audit of the classifier change specifically |
| Write via restore | **Never.** `restore_data` carries `classifier` in `BackupData` and drops it on the floor |
| Read via API | `GET /api/v1/settings` → `SettingsSummary.classifier` (`main.rs:1627`, sourced from `dns_runtime.classifier_settings()`, i.e. live runtime state not the DB) |
| Exported | `GET /api/v1/sync/export` → `SyncStatePayloadV1.classifier` (always, regardless of profile) and `GET /api/v1/backup` → `BackupData.classifier` |

**There is no endpoint that exposes a classification result, a per-domain score, feature vectors, model metadata, or a way to run the classifier ad hoc on a supplied domain.** The only observable output is `SecurityEventRecord.classifier_score` on already-classified live traffic.

---

## 11. Streaming / realtime endpoints

**There are none.** Exhaustive check:

- No `axum::response::sse`, no `Sse<...>`, no `text/event-stream`, no `Event::default()`.
- No `axum::extract::ws`, no `WebSocketUpgrade`, no `tokio-tungstenite`.
- No chunked/streaming body responses; every handler returns a fully-materialised `Json<...>` or a `String`.
- `tokio-stream` is declared in `[workspace.dependencies]` but is **not** a dependency of `cogwheel-server` or `cogwheel-api` and is unused.

The web client polls. Any live-updating UI (query log, activity feed, live counters) requires new transport to be built from scratch — `GET /api/v1/runtime` and `GET /api/v1/dashboard` are the current polling targets.

---

## 12. Dead code, stubs, placeholders, and defects

### 12.1 Whole components that do nothing

| Item | Location | Status |
| --- | --- | --- |
| `cogwheel-sync` crate | `crates/cogwheel-sync/src/lib.rs` (17 lines) | **Entirely unused.** Declares its own `NodeIdentity` and `SyncEnvelope { revision, issued_at, node, settings_hash }` that nothing imports. The real sync uses `cogwheel_storage::SyncEnvelope`. It is listed as a dependency in `apps/cogwheel-server/Cargo.toml` but `grep -rn "cogwheel_sync"` finds zero usages |
| `cogwheel-desktop` app | `apps/cogwheel-desktop/src/main.rs` (3 lines) | `println!("Cogwheel desktop shell scaffold placeholder")` |
| Threat intel subsystem | `main.rs:105-130`, `1429-1465`, `1484-1537` | UI-visible, in-memory-only, never fetches a feed, never influences DNS |
| Federated learning subsystem | `main.rs:132-150`, `1467-1482`, `1539-1587` | Same — no coordinator contact anywhere |
| Block profiles | `main.rs:425-458`, `4187-4617` | Persisted and CRUD-able but **never consumed by the policy pipeline** |
| Resilience drills (4 endpoints) | `main.rs:2907-3055` | Read-only status reports named "simulate_*"; inject nothing |
| `benchmark_rust_opts` | `main.rs:1310-1382` | Times no-op statements; two response fields hardcoded to `0` |
| `healthcheck` argv mode | `main.rs:517-519` | Returns `Ok(())` unconditionally; wired as the Docker healthcheck |
| `GET /health/ready` | `cogwheel-api:300-304` | Always `"ready"`; probes nothing |
| `config` + `humantime-serde` deps | `crates/cogwheel-api/Cargo.toml` | Declared, never used; no config file support exists |

### 12.2 Placeholder data shipped as production defaults

- `google-ads` and `tiktok` service manifests carry `risk_notes: "Placeholder manifest until curated domain coverage is finalized."` and have `allow_domains == block_domains` (`crates/cogwheel-services/src/lib.rs:85-105`).
- Threat-intel provider `alphamountain` ships `feed_url: "https://api.example.invalid/threat-intel/dns"` — the `.invalid` TLD is reserved and will never resolve (`main.rs:1436`).
- The bootstrap source blocks exactly `ads.example.com` and `tracker.example.com` via a `data:` URL (`main.rs:527`) and is **undeletable** (reserved id).
- `protected_domains` is a hardcoded single-element set (`main.rs:549`) with no configuration path.
- `false_positive_estimate = blocking_rate * 0.1` (`main.rs:3102`).
- `migration_count: 10` hardcoded (`main.rs:1422`).
- Latency budget targets `1.0 / 8.0 / 10.0` ms hardcoded (`main.rs:3146-3165`).

### 12.3 Concrete defects worth fixing during any rework

1. **`restore_data` silently discards the classifier settings and never persists notification settings** (`main.rs:2854-2905`). `success` is hardcoded `true`.
2. **`import_sync_state` deletes all sources and devices outside a transaction** (`main.rs:2592-2632`).
3. **`current_runtime_health` compares against an all-zero baseline** (`main.rs:4661-4681`), so on the `home` profile the dashboard latches to `"Needs Attention"` after the first upstream failure, permanently.
4. **Auto-rollback drops profile policies** — `replace_policy_catalog(policy, HashMap::new())` at `main.rs:4056-4059`.
5. **`GET /api/v1/sync/status` skips the transport guard** while its 6 siblings enforce it.
6. **`simulate_sync_partition`'s `mode != "disabled"` branch is unreachable** (`main.rs:3036`).
7. **`export_sync_state` never persists the incremented revision** (`main.rs:2512-2515`), so successive exports emit an identical revision.
8. **Unknown `/api/v1/*` paths return `index.html` with HTTP 200** when web assets are present (§8.2).
9. **`pause_runtime` / `resume_runtime` break the response envelope** (empty body vs `{"data":...}`), and the pause window is memory-only so a restart silently re-enables protection.
10. **Blocking `std::process::Command` calls inside async handlers** — `tailscale_status`, `tailscale_exit_node`, `tailscale_rollback`, `tailscale_dns_check`, `resolver_access_status`, plus `discover_local_ipv4s`/`discover_local_ipv6s`/`discover_tailscale_ipv4`.
11. **All SQLite access is blocking I/O on the async runtime through a single `Mutex`** (`crates/cogwheel-storage/src/lib.rs:44`), with `async fn` signatures that never yield.
12. **Migrations 0002-0010 discard their errors** (`storage:632-640`).
13. **`.expect(...)` on lock acquisition in HTTP handlers** — e.g. `main.rs:1613`, `2547`, `2839`, `3593`, `3617`, `4079` — a poisoned lock panics the request task. `restore_data` uses a bare `.unwrap()` (`main.rs:2872`).
14. **`Uuid::parse_str(...).expect("valid uuid in database")`** in storage row decoders (`storage:301`, `:558`, `:604`, `:646`) will panic the handler on any malformed row.
15. **Rate limiting keys are global constants, not per-client** — one noisy client locks out everyone from refresh/upsert.
16. **No auth on 48 of the 54 admin routes**, including `POST /api/v1/tailscale/exit-node` which reconfigures host networking and `POST /api/v1/backup/restore` which mutates persistence.
17. **`sync_transport_token` and the ed25519 private key are stored unencrypted in SQLite**; the notification `webhook_url` is returned in cleartext from `GET /api/v1/settings`, `GET /api/v1/backup`, and `GET /api/v1/sync/export`.
18. **`update_classifier_settings` does not validate `threshold`** — `NaN`, negatives, and values `> 1.0` are persisted.
19. **`normalize_sync_profile` and `normalize_sync_transport_mode` silently coerce invalid input** to `Full` / `"opportunistic"` instead of returning `400`.
20. **`run_load_test` has an unbounded `duration_secs`** and blocks a worker for the whole run.
21. **Workspace clippy lints are not enforced.** `[workspace.lints.clippy]` in the root `Cargo.toml` denies `unwrap_used`, `panic`, `todo`, and `dbg_macro`, but **no crate in the workspace declares a `[lints] workspace = true` section**, so none of it applies. That is why the `.unwrap()`/`.expect()` calls above compile cleanly.
22. **`sources.name` is `UNIQUE`** but `upsert_blocklist` uses `INSERT OR REPLACE`, so saving a new source with an existing name silently replaces the other row.
23. **`refresh_sources_once` fetches sources sequentially and aborts the entire refresh on the first fetch error** (`main.rs:3946-3948`).
24. **The new policy is activated before the guard probes run** (`main.rs:4036-4047`), leaving a window where a bad ruleset serves traffic.
25. **Unbounded list endpoints** — `GET /api/v1/rulesets` returns every ruleset ever recorded with no pruning or pagination; `security-events` and `audit-events` are fixed at 20 rows with no way to page.
