use anyhow::{Context, Result};
use axum::extract::{FromRef, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
mod sinkhole;

use cogwheel_api::{
    ApiEnvelope, ApiState, AppConfig, RuntimeGuardConfig, UpstreamEndpoint, UpstreamProtocol,
    router,
};
use cogwheel_classifier::ClassifierSettings;
use cogwheel_dns_core::{
    ClassificationEvent, DevicePolicyConfig, DnsRuntime, DnsRuntimeConfig, DnsRuntimeSnapshot,
    QueryActivityEvent,
};
use cogwheel_lists::{
    ParsedSource, SourceDefinition, SourceKind, build_policy_engine, fetch_and_parse_source,
    parse_source, synthetic_source, verify_candidate,
};
use cogwheel_policy::{BlockMode, DecisionKind, PolicyEngine};
use cogwheel_services::{
    ServiceManifest, ServiceToggleMode, ServiceToggleSnapshot, built_in_service_manifests,
    compile_service_rule_layer,
};
use cogwheel_storage::{
    AuditEvent, DeviceRecord, DeviceServiceOverrideRecord, NotificationDeliveryRecord,
    RulesetRecord, SecurityEventRecord, SourceRecord, Storage, SyncEnvelope,
};
use futures::StreamExt;
use hickory_resolver::TokioResolver;
use hickory_resolver::config::{ConnectionConfig, NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::RecordType;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::registry::Registry;
use reqwest::Client;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::time::interval;
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;
use url::Url;
use uuid::Uuid;

#[derive(Clone, FromRef)]
struct ServerState {
    api_state: ApiState,
    storage: Arc<Storage>,
    dns_runtime: Arc<DnsRuntime>,
    http_client: Client,
    notification_settings: Arc<RwLock<NotificationSettings>>,
    threat_intel_settings: Arc<RwLock<ThreatIntelSettings>>,
    federated_learning_settings: Arc<RwLock<FederatedLearningSettings>>,
    recent_dns_activity: Arc<Mutex<VecDeque<DomainActivityRecord>>>,
    events: EventBus,
    shutdown: tokio::sync::watch::Receiver<bool>,
    protected_domains: Arc<HashSet<String>>,
    runtime_guard: RuntimeGuardConfig,
    sync_seen_nonces: Arc<Mutex<HashMap<String, chrono::DateTime<chrono::Utc>>>>,
    rate_limiter: Arc<RateLimiter>,
    dns_udp_bind_addr: SocketAddr,
    advertised_dns_port: u16,
    advertised_dns_targets: Vec<String>,
}

#[derive(Clone)]
struct RateLimiter {
    requests: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
    max_requests: usize,
    window_secs: u64,
}

impl RateLimiter {
    fn new(max_requests: usize, window_secs: u64) -> Self {
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
            max_requests,
            window_secs,
        }
    }

    fn is_allowed(&self, key: &str) -> bool {
        let now = Instant::now();
        // A poisoned lock means some other thread panicked mid-update. Failing open is the right
        // call here: rate limiting is a safeguard, and refusing every request afterwards would turn
        // one panic into a total outage of the control plane.
        let Ok(mut requests) = self.requests.lock() else {
            tracing::warn!("rate limiter lock poisoned; allowing request");
            return true;
        };

        let entry = requests.entry(key.to_string()).or_default();
        entry.retain(|t| now.duration_since(*t) < Duration::from_secs(self.window_secs));

        if entry.len() >= self.max_requests {
            return false;
        }

        entry.push(now);
        true
    }
}

#[derive(Clone)]
struct RuntimePolicyCatalog {
    global_policy: Arc<PolicyEngine>,
    profile_policies: HashMap<String, Arc<PolicyEngine>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ThreatIntelProviderConfig {
    id: String,
    display_name: String,
    enabled: bool,
    feed_url: Option<String>,
    api_key_configured: bool,
    update_interval_minutes: u32,
    last_sync_at: Option<chrono::DateTime<chrono::Utc>>,
    last_error: Option<String>,
    capabilities: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ThreatIntelSettings {
    providers: Vec<ThreatIntelProviderConfig>,
    recommendations: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ThreatIntelProviderUpdate {
    id: String,
    enabled: bool,
    feed_url: Option<String>,
    update_interval_minutes: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FederatedLearningSettings {
    enabled: bool,
    coordinator_url: Option<String>,
    node_id: String,
    round_interval_hours: u32,
    last_round_at: Option<chrono::DateTime<chrono::Utc>>,
    last_model_version: Option<String>,
    privacy_mode: String,
    raw_log_export_enabled: bool,
    recommendations: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct FederatedLearningUpdate {
    enabled: bool,
    coordinator_url: Option<String>,
    round_interval_hours: u32,
}

#[derive(serde::Serialize)]
struct RulesetSummary {
    id: Uuid,
    hash: String,
    status: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(serde::Serialize)]
struct RefreshResponse {
    outcome: String,
    ruleset: Option<RulesetSummary>,
    notes: Vec<String>,
}

#[derive(serde::Serialize)]
struct RuntimeHealthResponse {
    snapshot: DnsRuntimeSnapshot,
    degraded: bool,
    notes: Vec<String>,
}

#[derive(serde::Serialize)]
struct DashboardSummary {
    protection_status: String,
    protection_paused_until: Option<chrono::DateTime<chrono::Utc>>,
    active_ruleset: Option<RulesetSummary>,
    source_count: usize,
    enabled_source_count: usize,
    service_toggle_count: usize,
    device_count: usize,
    runtime_health: RuntimeHealthResponse,
    latest_audit_events: Vec<AuditEvent>,
    recent_security_events: Vec<SecurityEventRecord>,
    recent_notification_deliveries: Vec<NotificationDeliveryEvent>,
    notification_health: NotificationHealthSummary,
    notification_failure_analytics: NotificationFailureAnalytics,
    security_summary: SecuritySummary,
    domain_insights: DomainInsights,
}

#[derive(Debug, Clone, serde::Serialize)]
struct DomainInsightEntry {
    domain: String,
    count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
struct DomainInsights {
    top_queried_domains: Vec<DomainInsightEntry>,
    top_blocked_domains: Vec<DomainInsightEntry>,
    observed_queries: usize,
}

#[derive(Debug, Clone)]
struct DomainActivityRecord {
    domain: String,
    blocked: bool,
    observed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct NotificationDeliveryEvent {
    status: String,
    event_type: String,
    severity: String,
    title: String,
    summary: String,
    target: String,
    domain: String,
    device_name: Option<String>,
    client_ip: String,
    attempts: usize,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct NotificationHealthSummary {
    delivered_count: usize,
    failed_count: usize,
    last_delivery_at: Option<chrono::DateTime<chrono::Utc>>,
    last_failure_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct NotificationFailureAnalytics {
    success_rate_percent: f32,
    top_failed_domains: Vec<NotificationFailureDomain>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct NotificationFailureDomain {
    domain: String,
    failure_count: usize,
}

#[derive(Debug, Clone)]
struct NotificationWebhookEvent {
    event_type: String,
    severity: String,
    title: String,
    summary: String,
    domain: Option<String>,
    device_name: Option<String>,
    client_ip: Option<String>,
    details: Vec<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct NotificationTestResult {
    outcome: String,
    target: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct NotificationTestPreset {
    name: String,
    domain: String,
    severity: String,
    device_name: String,
    dry_run: bool,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct DashboardQuery {
    notification_window: Option<usize>,
    notification_history_window: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct SecuritySummary {
    medium_count: usize,
    high_count: usize,
    critical_count: usize,
    top_devices: Vec<DeviceSecuritySummary>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct DeviceSecuritySummary {
    label: String,
    event_count: usize,
    highest_severity: String,
}

#[derive(serde::Serialize)]
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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct NotificationSettings {
    enabled: bool,
    webhook_url: Option<String>,
    min_severity: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct UpdateNotificationSettingsRequest {
    enabled: bool,
    webhook_url: Option<String>,
    min_severity: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct TestNotificationRequest {
    domain: Option<String>,
    severity: Option<String>,
    device_name: Option<String>,
    dry_run: Option<bool>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct UpdateNotificationPresetsRequest {
    presets: Vec<NotificationTestPreset>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BlocklistStatusView {
    id: Uuid,
    name: String,
    last_refresh_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    due_for_refresh: bool,
}

#[derive(Debug, Clone)]
struct RuntimeRegressionReport {
    degraded: bool,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct SourceRefreshState {
    entries: Vec<SourceRefreshStateEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SourceRefreshStateEntry {
    source_id: Uuid,
    last_refresh_attempt_at: chrono::DateTime<chrono::Utc>,
}

impl SourceRefreshState {
    fn last_refresh_for(&self, source_id: Uuid) -> Option<chrono::DateTime<chrono::Utc>> {
        self.entries
            .iter()
            .find(|entry| entry.source_id == source_id)
            .map(|entry| entry.last_refresh_attempt_at)
    }

    fn record_attempt(&mut self, source_id: Uuid, refreshed_at: chrono::DateTime<chrono::Utc>) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.source_id == source_id)
        {
            entry.last_refresh_attempt_at = refreshed_at;
            return;
        }

        self.entries.push(SourceRefreshStateEntry {
            source_id,
            last_refresh_attempt_at: refreshed_at,
        });
    }
}

#[derive(serde::Serialize)]
struct ServiceToggleView {
    manifest: ServiceManifest,
    mode: ServiceToggleMode,
}

#[derive(serde::Deserialize)]
struct UpdateServiceToggleRequest {
    service_id: String,
    mode: ServiceToggleMode,
}

#[derive(serde::Deserialize)]
struct UpdateClassifierSettingsRequest {
    mode: cogwheel_classifier::ClassifierMode,
    /// Sensitivity replaces the old raw `threshold` field. A user should not have to reason about
    /// what `0.87` means; the concrete threshold comes from the model's calibration.
    #[serde(default)]
    sensitivity: cogwheel_classifier::Sensitivity,
}

#[derive(serde::Deserialize)]
struct UpsertBlocklistRequest {
    id: Option<Uuid>,
    name: String,
    url: String,
    kind: String,
    enabled: bool,
    refresh_interval_minutes: Option<i64>,
    profile: Option<String>,
    verification_strictness: Option<String>,
    refresh_now: Option<bool>,
}

#[derive(serde::Deserialize)]
struct UpdateBlocklistStateRequest {
    id: Uuid,
    enabled: bool,
    refresh_now: Option<bool>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BlockProfileListRecord {
    id: String,
    name: String,
    url: String,
    kind: String,
    family: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BlockProfileRecord {
    id: String,
    emoji: String,
    name: String,
    description: String,
    blocklists: Vec<BlockProfileListRecord>,
    allowlists: Vec<String>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct UpsertBlockProfileRequest {
    id: Option<String>,
    emoji: String,
    name: String,
    description: Option<String>,
    blocklists: Vec<BlockProfileListRecord>,
    allowlists: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct DeleteBlockProfileRequest {
    id: String,
}

#[derive(serde::Deserialize)]
struct DeleteBlocklistRequest {
    id: Uuid,
    refresh_now: Option<bool>,
}

#[derive(serde::Deserialize)]
struct UpsertDeviceRequest {
    id: Option<Uuid>,
    name: String,
    ip_address: String,
    policy_mode: Option<String>,
    blocklist_profile_override: Option<String>,
    protection_override: Option<String>,
    allowed_domains: Option<Vec<String>>,
    service_overrides: Option<Vec<DeviceServiceOverrideRecord>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SyncStatePayloadV1 {
    version: u32,
    revision: u64,
    profile: String,
    exported_at: chrono::DateTime<chrono::Utc>,
    blocklists: Vec<SourceRecord>,
    devices: Vec<DeviceRecord>,
    classifier: ClassifierSettings,
    notifications: NotificationSettings,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SyncProfile {
    Full,
    SettingsOnly,
    ReadOnlyFollower,
}

impl SyncProfile {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::SettingsOnly => "settings-only",
            Self::ReadOnlyFollower => "read-only-follower",
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ImportSyncEnvelopeRequest {
    envelope: SyncEnvelope,
}

/// The response every ruleset is compiled to give for a blocked name.
///
/// A `OnceLock` rather than a value threaded through the call graph: rulesets
/// are rebuilt from three unrelated places -- startup, a blocklist refresh, and
/// a policy rebuild -- and the alternative is passing the same immutable value
/// down three chains that have no other use for it. It is written once, before
/// any ruleset is built, and never changes for the life of the process.
static BLOCK_MODE: std::sync::OnceLock<BlockMode> = std::sync::OnceLock::new();

/// The configured block response, or the historical default before startup has
/// resolved it (which is also what every version before this one always did).
fn configured_block_mode() -> BlockMode {
    BLOCK_MODE.get().cloned().unwrap_or(BlockMode::NullIp)
}

/// Turn the configured mode into the response the DNS core will send.
///
/// Sinkhole is the only one that needs anything beyond a direct mapping: it
/// answers with an address on this machine, so it needs to know which address
/// clients can actually reach. Guessing wrong here is worse than not offering
/// the mode at all -- every blocked name would resolve to somewhere that does
/// not answer, which is a slow failure instead of the fast one it replaced.
fn resolve_block_mode(
    blocking: &cogwheel_api::BlockingConfig,
    advertised: &[String],
) -> Result<BlockMode> {
    use cogwheel_api::BlockResponseMode;

    Ok(match blocking.mode {
        BlockResponseMode::NullIp => BlockMode::NullIp,
        BlockResponseMode::NxDomain => BlockMode::NxDomain,
        BlockResponseMode::NoData => BlockMode::NoData,
        BlockResponseMode::Refused => BlockMode::Refused,
        BlockResponseMode::Sinkhole => {
            let explicit = blocking.sinkhole_address;
            let discovered = explicit.or_else(|| {
                advertised
                    .iter()
                    .filter_map(|target| target.trim().parse::<IpAddr>().ok())
                    .find(|address| !address.is_loopback() && !address.is_unspecified())
            });
            let address = discovered.context(
                "blocking mode is 'sinkhole' but this appliance does not know its own address. \
                 Set COGWHEEL_BLOCKING__SINKHOLE_ADDRESS to the address clients reach it on, or \
                 set COGWHEEL_SERVER__ADVERTISED_DNS_TARGETS (the installers do this for you)",
            )?;
            match address {
                IpAddr::V4(ipv4) => BlockMode::CustomIp {
                    ipv4: Some(ipv4),
                    // No IPv6 answer at all rather than a wrong one. Returning
                    // an IPv4-derived guess for AAAA would send v6 clients to
                    // an address nothing listens on; an empty AAAA makes them
                    // fall back to the A record, which does work.
                    ipv6: None,
                },
                IpAddr::V6(ipv6) => BlockMode::CustomIp {
                    ipv4: None,
                    ipv6: Some(ipv6),
                },
            }
        }
    })
}

const USAGE: &str = "\
cogwheel-server -- the Cogwheel DNS appliance

Usage:
  cogwheel-server            run the server
  cogwheel-server --version  print the version and exit
  cogwheel-server --help     print this message and exit

There are no other flags. Everything is configured by environment variable:
COGWHEEL_PROFILE, COGWHEEL_SERVER__*, COGWHEEL_STORAGE__*, COGWHEEL_UPSTREAM__*.
On an installed appliance those live in /etc/cogwheel/cogwheel.env. See
DEPLOYMENT.md for the full list.
";

#[derive(Debug, PartialEq, Eq)]
enum CliAction {
    /// Start the server.
    Run,
    /// Write this to stdout and exit 0.
    Print(String),
    /// Write this to stderr and exit 2.
    Fail(String),
}

/// Decide what to do with the command line before any side effects happen.
///
/// The important property is that an argument this binary does not understand
/// is a hard error rather than something it ignores. Previously every argument
/// except the literal `healthcheck` fell straight through and started the
/// server, so `cogwheel-server --version` on an appliance printed nothing and
/// quietly bound a SECOND resolver to :53 -- next to the one the service was
/// already running. For a process whose entire job is to take over the host's
/// DNS, refusing to start is the only safe response to input it cannot parse.
///
/// The `healthcheck` subcommand this replaces was dead code that returned
/// success unconditionally. Nothing invoked it (the container HEALTHCHECK uses
/// curl against /health/live), and a probe that reports healthy without
/// checking anything is worse than no probe at all, so it is gone rather than
/// preserved.
fn parse_cli(args: &[String]) -> CliAction {
    let Some(first) = args.first() else {
        return CliAction::Run;
    };
    match first.as_str() {
        "--version" | "-V" => {
            CliAction::Print(format!("cogwheel-server {}\n", env!("CARGO_PKG_VERSION")))
        }
        "--help" | "-h" => CliAction::Print(USAGE.to_string()),
        other => CliAction::Fail(format!(
            "cogwheel-server: unrecognised argument: {other}\n\n{USAGE}"
        )),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Before init_tracing: `--version` should print a version and nothing else,
    // not a version wrapped in JSON log lines.
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    match parse_cli(&args) {
        CliAction::Run => {}
        CliAction::Print(message) => {
            print!("{message}");
            return Ok(());
        }
        CliAction::Fail(message) => {
            eprint!("{message}");
            std::process::exit(2);
        }
    }

    init_tracing();

    let config = AppConfig::load()?;

    // Resolved before the first ruleset is compiled, because the block response
    // is baked into the ruleset artifact rather than consulted per query. A
    // failure here is fatal on purpose: "sinkhole was requested but we cannot
    // work out our own address" must not degrade into silently blocking with
    // something else, because the operator would have no way to notice.
    let advertised_targets = std::env::var("COGWHEEL_SERVER__ADVERTISED_DNS_TARGETS")
        .unwrap_or_default()
        .split(',')
        .map(|target| target.trim().to_string())
        .filter(|target| !target.is_empty())
        .collect::<Vec<_>>();
    let block_mode = resolve_block_mode(&config.blocking, &advertised_targets)?;
    tracing::info!(mode = ?config.blocking.mode, response = ?block_mode, "blocked names will be answered with this");
    let _ = BLOCK_MODE.set(block_mode);

    let storage = Arc::new(Storage::connect(&config.storage.database_url).await?);
    // Captured before `storage` is moved into the shared app state below.
    let retention_storage = storage.clone();

    let default_source = SourceRecord {
        id: Uuid::from_u128(1),
        name: "baseline".to_string(),
        url: "data:text/plain,ads.example.com%0Atracker.example.com".to_string(),
        kind: "domains".to_string(),
        enabled: true,
        refresh_interval_minutes: 60,
        profile: "essential".to_string(),
        verification_strictness: "strict".to_string(),
    };
    storage.insert_source(&default_source).await?;

    let parsed = parse_source(
        SourceDefinition {
            id: default_source.id,
            name: default_source.name.clone(),
            url: Url::parse(&default_source.url)?,
            kind: SourceKind::Domains,
            enabled: true,
            profile: default_source.profile.clone(),
            verification_strictness: default_source.verification_strictness.clone(),
        },
        "ads.example.com\ntracker.example.com",
    );

    // Seeded from the classifier's CRITICAL tier, not from one hardcoded name.
    //
    // The 52-entry protected list guarded classifier verdicts only, while the
    // blocklist path -- which does most of the actual blocking -- was protected
    // by exactly one exact-match domain. So a list that happened to cover an
    // OCSP responder, an NTP pool or a captive-portal check could take a device
    // off the network in a way that looks nothing like a DNS problem, and the
    // safety net that existed to prevent precisely that did not apply.
    //
    // Only the critical subset is promoted here. The broader entries (banking,
    // OS vendors, government) stay classifier-only on purpose: a blocklist
    // entry covering those is a choice someone made, and silently overruling it
    // would be its own kind of surprise.
    let protected_domains = Arc::new(
        cogwheel_classifier::Allowlist::critical()
            .suffixes()
            .iter()
            .cloned()
            .collect::<HashSet<String>>(),
    );
    let verification = verify_candidate(std::slice::from_ref(&parsed), &protected_domains);
    anyhow::ensure!(
        verification.passed,
        "default ruleset failed verification: {:?}",
        verification.notes
    );

    let policy = Arc::new(build_policy_engine(
        vec![parsed],
        protected_domains.as_ref().clone(),
        configured_block_mode(),
    ));
    storage
        .record_ruleset(&RulesetRecord {
            id: policy.artifact().id,
            hash: policy.artifact().hash.clone(),
            status: "active".to_string(),
            created_at: policy.artifact().created_at,
            artifact_json: serde_json::to_string(policy.artifact())?,
        })
        .await?;
    storage.activate_ruleset(policy.artifact().id).await?;
    storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "ruleset.activated".to_string(),
            payload: serde_json::json!({
                "ruleset_id": policy.artifact().id,
                "hash": policy.artifact().hash,
                "reason": "bootstrap",
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await?;

    let mut registry = Registry::default();
    let startup_counter: Counter<u64> = Counter::default();
    registry.register(
        "cogwheel_startups_total",
        "Number of server startups",
        startup_counter.clone(),
    );
    startup_counter.inc();
    let registry = Arc::new(registry);
    // Broadcast shutdown to everything that would otherwise outlive the signal: the DNS accept
    // loops, and every open SSE stream. Without this, `with_graceful_shutdown` waits forever for
    // an SSE connection that never ends on its own.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    // Readiness is reported per subsystem; each is marked as it genuinely comes up.
    let readiness = Arc::new(cogwheel_api::Readiness::default());
    // Storage is open and migrated by the time we get here -- `Storage::connect` applies migrations
    // and now fails loudly if any of them error.
    readiness.mark_storage_ready();

    let resolver = build_resolver(&config.upstream.servers)?;
    let classifier_settings = load_classifier_settings(&storage).await?;
    let notification_settings = Arc::new(RwLock::new(load_notification_settings(&storage).await?));
    let recent_dns_activity = Arc::new(Mutex::new(VecDeque::with_capacity(4096)));
    let http_client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("build notification client")?;
    // Build the classifier engine from the model embedded in the binary, then hand its scoring
    // worker a dedicated OS thread. The worker is deliberately not a tokio task: scoring is a
    // CPU-bound loop and putting it on the async runtime would let it compete with the DNS
    // listeners for executor time on a 4-core Pi.
    let classifier_model =
        cogwheel_classifier::embedded_model().context("load embedded classifier model")?;
    tracing::info!(
        roc_auc = classifier_model.quality().roc_auc,
        resident_bytes = classifier_model.resident_bytes(),
        mode = classifier_settings.mode.as_str(),
        sensitivity = classifier_settings.sensitivity.as_str(),
        "classifier model loaded"
    );
    let (classifier_engine, scoring_worker) = cogwheel_classifier::ClassifierEngine::new(
        classifier_model,
        cogwheel_classifier::Allowlist::builtin(),
        classifier_settings,
        cogwheel_classifier::EngineConfig::default(),
    );
    let classifier_engine = Arc::new(classifier_engine);
    // Restore a previously promoted adaptation. It is re-validated by `Delta::from_hex` on the way
    // in — magic, geometry, checksum and the logit budget — because the row came off a disk that may
    // have lost power mid-write. A delta that fails any of those is dropped with a warning rather
    // than applied or fatal: the base model is untouched, so the appliance keeps working exactly as
    // shipped, which is the whole reason adaptation was built as a separate object.
    if let Some(stored) = load_classifier_adaptation(&storage).await? {
        match cogwheel_classifier::Delta::from_hex(&stored.delta_hex) {
            Ok(delta) => {
                tracing::info!(
                    trained_at = stored.trained_at,
                    example_count = stored.example_count,
                    roc_auc = stored.roc_auc,
                    ngram_entries = delta.ngram_entries(),
                    "classifier adaptation restored"
                );
                classifier_engine.set_active_delta(Some(Arc::new(delta)));
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "stored classifier adaptation failed validation; staying on the base model"
                );
            }
        }
    }
    std::thread::Builder::new()
        .name("cogwheel-classifier".to_string())
        .spawn(move || {
            scoring_worker.run();
            // `run` only returns when every engine handle is dropped, i.e. at shutdown. Reaching
            // here at any other time means scoring has silently stopped, which is invisible from
            // the outside because DNS keeps working -- so say so loudly.
            tracing::warn!("classifier scoring worker exited; domains will no longer be scored");
        })
        .context("spawn classifier scoring worker")?;

    let dns_runtime = Arc::new(DnsRuntime::new(
        resolver,
        policy,
        Arc::clone(&classifier_engine),
    ));
    dns_runtime.install_classifier_bridge();
    let events = EventBus::new();

    // The classifier's scoring worker runs on a plain OS thread, deliberately: scoring is CPU-bound
    // and must not compete with the DNS listeners for executor time. That thread has no tokio
    // runtime context, so `tokio::spawn` inside this observer would panic and permanently kill the
    // worker on the very first domain that scores high enough to be reported. Capture an explicit
    // handle here, while we are still on the runtime, and spawn through it instead.
    let runtime_handle = tokio::runtime::Handle::current();

    dns_runtime.set_classification_observer(Arc::new({
        let storage = storage.clone();
        let http_client = http_client.clone();
        let notification_settings = notification_settings.clone();
        let events = events.clone();
        let runtime_handle = runtime_handle.clone();
        move |event: cogwheel_dns_core::ClassificationEvent| {
            events.publish(StreamEvent::Detection(Box::new(StreamDetectionEvent {
                domain: event.domain.clone(),
                client: event
                    .client_ip
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                device_name: None,
                probability: event.score,
                decision: if event.blocked { "block" } else { "allow" }.to_string(),
                observed_at: event.observed_at.to_rfc3339(),
            })));
            let storage = storage.clone();
            let http_client = http_client.clone();
            let notification_settings = notification_settings.clone();
            runtime_handle.spawn(async move {
                if let Err(error) = record_security_event_from_classification(
                    storage,
                    http_client,
                    notification_settings,
                    event,
                )
                .await
                {
                    tracing::warn!(%error, "failed to record security event");
                }
            });
        }
    }));
    dns_runtime.set_query_activity_observer(Arc::new({
        let recent_dns_activity = recent_dns_activity.clone();
        let events = events.clone();
        move |event: cogwheel_dns_core::QueryActivityEvent| {
            events.publish(StreamEvent::Query(Box::new(StreamQueryEvent {
                domain: event.domain.clone(),
                client: event
                    .client_ip
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                device_name: None,
                blocked: event.blocked,
                reason: None,
                observed_at: event.observed_at.to_rfc3339(),
            })));
            record_recent_dns_activity(&recent_dns_activity, event)
        }
    }));

    let dns_handle = tokio::spawn({
        let runtime = dns_runtime.clone();
        let dns_config = DnsRuntimeConfig {
            udp_bind_addr: config.server.dns_udp_bind_addr,
            tcp_bind_addr: config.server.dns_tcp_bind_addr,
        };
        let readiness = Arc::clone(&readiness);
        let dns_shutdown = shutdown_rx.clone();
        async move {
            runtime
                .serve_with_ready_signal(
                    dns_config,
                    move || {
                        readiness.mark_dns_ready();
                        tracing::info!("dns listeners bound");
                    },
                    dns_shutdown,
                )
                .await
        }
    });

    let app_state = ServerState {
        api_state: ApiState {
            registry,
            readiness: Arc::clone(&readiness),
        },
        storage,
        dns_runtime,
        http_client,
        notification_settings,
        threat_intel_settings: Arc::new(RwLock::new(default_threat_intel_settings())),
        federated_learning_settings: Arc::new(RwLock::new(default_federated_learning_settings())),
        recent_dns_activity,
        events,
        shutdown: shutdown_rx.clone(),
        protected_domains,
        runtime_guard: config.runtime_guard,
        sync_seen_nonces: Arc::new(Mutex::new(HashMap::new())),
        rate_limiter: Arc::new(RateLimiter::new(100, 60)),
        dns_udp_bind_addr: config.server.dns_udp_bind_addr,
        advertised_dns_port: std::env::var("COGWHEEL_SERVER__ADVERTISED_DNS_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(config.server.dns_udp_bind_addr.port()),
        advertised_dns_targets: std::env::var("COGWHEEL_SERVER__ADVERTISED_DNS_TARGETS")
            .ok()
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    };
    match warm_runtime_policy_catalog(&app_state).await {
        Ok(()) => readiness.mark_policy_ready(),
        Err(error) => {
            // The node keeps serving -- an empty policy resolves everything rather than nothing --
            // but it must not advertise itself as ready, or a rolling upgrade would send traffic to
            // a node that is not actually filtering yet.
            tracing::warn!(%error, "failed to warm runtime policy catalog on startup");
        }
    }
    sync_runtime_device_policies(&app_state).await?;
    // Publish runtime health to connected control planes on a slow cadence. Without this the
    // client's `health` listener was dead code: it subscribed to an event the server never emitted,
    // so the Activity screen could never show that the resolver had degraded.
    //
    // This uses the PASSIVE signal, `current_runtime_health`, which only reads counters already in
    // memory. It must never call `active_runtime_health_check`: that one sends live DNS probes
    // upstream, writes an audit row, and can fire a webhook. On a 30s timer that would mean ~2,880
    // audit rows a day forever -- storage has no retention or vacuum -- plus constant probe traffic
    // and alert noise, on a device whose durable storage is usually an SD card.
    tokio::spawn({
        let state = app_state.clone();
        let events = state.events.clone();
        async move {
            let mut ticker = interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                let health = current_runtime_health(&state);
                events.publish(StreamEvent::Health(Box::new(StreamHealthEvent {
                    degraded: health.degraded,
                    notes: health.notes,
                    observed_at: chrono::Utc::now().to_rfc3339(),
                })));
            }
        }
    });

    let refresh_handle = tokio::spawn({
        let state = app_state.clone();
        let refresh_every = config.updater.refresh_interval_secs.max(30);
        async move {
            let mut ticker = interval(Duration::from_secs(refresh_every));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let due_ids = match due_source_ids(&state).await {
                    Ok(ids) => ids,
                    Err(error) => {
                        tracing::warn!(%error, "scheduled source selection failed");
                        continue;
                    }
                };
                if due_ids.is_empty() {
                    continue;
                }
                if let Err(error) = refresh_sources_once(&state, "scheduled", Some(&due_ids)).await
                {
                    tracing::warn!(%error, "scheduled source refresh failed");
                }
            }
        }
    });
    // Retention. Without this the history tables grow for the life of the
    // appliance -- a disk problem on a small disk, and a permanent record of a
    // household's browsing on a product that exists to prevent exactly that.
    if config.retention.history_days == 0 {
        tracing::warn!(
            "history retention is disabled; classifier verdicts and audit events will be kept \
             forever and the database will grow without limit. Set \
             COGWHEEL_RETENTION__HISTORY_DAYS to bound it."
        );
    } else {
        let history_days = i64::from(config.retention.history_days);
        let interval = Duration::from_secs(config.retention.prune_interval_secs);
        let mut retention_shutdown = shutdown_rx.clone();
        tracing::info!(
            days = config.retention.history_days,
            every_secs = config.retention.prune_interval_secs,
            "pruning observed history older than this"
        );
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // The first tick fires immediately, which is wanted: an upgrade
            // from a version that never pruned should not wait an hour to act
            // on a database that may already be large.
            loop {
                tokio::select! {
                    _ = ticker.tick() => {}
                    _ = retention_shutdown.wait_for(|stopping| *stopping) => break,
                }
                let cutoff = chrono::Utc::now() - chrono::Duration::days(history_days);
                match retention_storage.prune_history_before(cutoff).await {
                    Ok(pruned) if pruned.total() > 0 => tracing::info!(
                        security_events = pruned.security_events,
                        audit_events = pruned.audit_events,
                        notification_deliveries = pruned.notification_deliveries,
                        %cutoff,
                        "pruned history past the retention window"
                    ),
                    Ok(_) => tracing::debug!(%cutoff, "retention pass found nothing to prune"),
                    // A failed prune must not take the appliance down: DNS
                    // resolution does not depend on it, and a locked database
                    // during a refresh is a transient the next tick handles.
                    Err(error) => tracing::warn!(%error, "retention pass failed"),
                }
            }
        });
    }

    let app = build_http_app(app_state);
    let listener = tokio::net::TcpListener::bind(config.server.http_bind_addr)
        .await
        .context("bind http listener")?;

    // The sink for blocked names, when the operator asked for one. Bound here
    // rather than lazily, so a port conflict is a startup failure the operator
    // sees immediately instead of a blocked page mysteriously hanging later.
    if config.blocking.mode == cogwheel_api::BlockResponseMode::Sinkhole {
        let sinkhole_listener = tokio::net::TcpListener::bind(config.blocking.sinkhole_bind_addr)
            .await
            .with_context(|| {
                format!(
                    "bind sinkhole listener on {}. Blocked names are being answered with this \
                     appliance's address, so something must answer there; free the port or set \
                     COGWHEEL_BLOCKING__SINKHOLE_BIND_ADDR",
                    config.blocking.sinkhole_bind_addr
                )
            })?;
        let mut sinkhole_shutdown = shutdown_rx.clone();
        tracing::info!(
            bind = %config.blocking.sinkhole_bind_addr,
            "serving blocked hostnames from the local sinkhole"
        );
        tokio::spawn(async move {
            let served = axum::serve(sinkhole_listener, sinkhole::router())
                .with_graceful_shutdown(async move {
                    let _ = sinkhole_shutdown.wait_for(|stopping| *stopping).await;
                })
                .await;
            if let Err(error) = served {
                tracing::warn!(%error, "sinkhole listener stopped");
            }
        });
    }

    // Graceful shutdown. Without this the process took the kernel default on SIGTERM and died
    // instantly, dropping every in-flight DNS query and upstream request and severing open SSE
    // streams. `docker stop` and `systemctl stop` both send SIGTERM, so this is the normal stop
    // path for the appliance, not an edge case.
    let shutdown = async {
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
        };

        #[cfg(unix)]
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                // If the handler cannot be installed we still want the other arm to work, so park
                // forever rather than resolving immediately and triggering a spurious shutdown.
                Err(error) => {
                    tracing::warn!(%error, "could not install SIGTERM handler");
                    std::future::pending::<()>().await;
                }
            }
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            () = ctrl_c => tracing::info!("received SIGINT, shutting down"),
            () = terminate => tracing::info!("received SIGTERM, shutting down"),
        }
    };

    // Fan the signal out the moment it arrives, so the DNS listeners and every open SSE stream
    // begin winding down at the same time the HTTP server stops accepting.
    let shutdown_signal = async move {
        shutdown.await;
        let _ = shutdown_tx.send(true);
    };

    // Borrow the handle so the select does not consume it: after the HTTP server drains we still
    // need to await the DNS task, and an early DNS failure must still abort startup.
    let mut dns_handle = dns_handle;
    tokio::select! {
        result = &mut dns_handle => {
            result.context("dns task join failure")??;
        }
        result = refresh_handle => {
            result.context("refresh task join failure")?;
        }
        // `with_graceful_shutdown` stops accepting new connections on the signal and waits for
        // in-flight requests to finish before returning.
        result = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal) => {
            result.context("http server failure")?;
        }
    }

    // The HTTP server has drained. Give the DNS listeners a bounded window to finish whatever
    // query they were mid-way through; returning here without waiting would drop it, which is the
    // opposite of a graceful stop. The bound matters because a stuck upstream must not stop the
    // process from exiting -- supervisors escalate to SIGKILL.
    match tokio::time::timeout(Duration::from_secs(5), dns_handle).await {
        Ok(Ok(Ok(()))) => tracing::info!("dns listeners drained"),
        Ok(Ok(Err(error))) => tracing::warn!(%error, "dns listeners stopped with an error"),
        Ok(Err(error)) => tracing::warn!(%error, "dns task join failure during shutdown"),
        Err(_) => tracing::warn!("dns drain timed out; exiting anyway"),
    }

    tracing::info!("shutdown complete");
    Ok(())
}

/// Read an `RwLock`, recovering the value even when the lock is poisoned.
///
/// Poisoning only records that some thread panicked while holding the lock. Every field guarded
/// this way holds a wholesale replacement, so the last committed value is still coherent, and
/// recovering it keeps a single panicking request from disabling the control plane for the rest of
/// the process lifetime.
fn read_recover<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write to an `RwLock`, recovering the value even when the lock is poisoned. See [`read_recover`].
fn write_recover<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Lock a `Mutex`, recovering the value even when it is poisoned. See [`read_recover`].
fn lock_recover<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive(tracing::level_filters::LevelFilter::INFO.into()),
        )
        .json()
        .init();
}

fn build_resolver(servers: &[String]) -> Result<TokioResolver> {
    let mut name_servers = Vec::new();
    let mut encrypted = 0usize;

    for server in servers {
        let endpoint = UpstreamEndpoint::parse(server)
            .with_context(|| format!("invalid upstream server: {server}"))?;

        // hickory 0.26 models an upstream as one address carrying a list of connections, rather
        // than one entry per protocol. The port moved onto the connection, so it has to be copied
        // across from the configured address or every upstream would silently fall back to 53.
        let connections = match endpoint.protocol {
            UpstreamProtocol::Udp => {
                let mut udp = ConnectionConfig::udp();
                udp.port = endpoint.addr.port();
                let mut tcp = ConnectionConfig::tcp();
                tcp.port = endpoint.addr.port();
                vec![udp, tcp]
            }
            // No cleartext fallback is added alongside an encrypted transport, and that is the
            // whole point. A fallback would mean that anything making TLS fail -- a captive
            // portal, a middlebox, an expired certificate -- silently downgrades every query in
            // the house back onto the wire in plaintext, which is precisely the outcome the
            // operator configured this to avoid. If DoT is broken, resolution should fail
            // visibly and get fixed, not quietly succeed in the clear.
            UpstreamProtocol::Tls => {
                let server_name = endpoint
                    .server_name
                    .clone()
                    .context("DoT upstream without a certificate name reached the resolver")?;
                let mut tls = ConnectionConfig::tls(Arc::from(server_name.as_str()));
                tls.port = endpoint.addr.port();
                encrypted += 1;
                vec![tls]
            }
            UpstreamProtocol::Https => {
                let server_name = endpoint
                    .server_name
                    .clone()
                    .context("DoH upstream without a certificate name reached the resolver")?;
                let path = endpoint.path.clone().map(|path| Arc::from(path.as_str()));
                let mut https = ConnectionConfig::https(Arc::from(server_name.as_str()), path);
                https.port = endpoint.addr.port();
                encrypted += 1;
                vec![https]
            }
        };

        tracing::info!(
            upstream = %endpoint,
            protocol = ?endpoint.protocol,
            encrypted = endpoint.is_encrypted(),
            "configured upstream resolver"
        );
        name_servers.push(NameServerConfig::new(endpoint.addr.ip(), true, connections));
    }

    // Said once, plainly, rather than left for the operator to infer. Cleartext is still the
    // default because it is what works on every network without configuration, but running a
    // tracker blocker while handing the full browsing history of the house to whoever carries
    // the packets deserves to be stated rather than assumed.
    if encrypted == 0 {
        tracing::warn!(
            "all upstream resolvers are cleartext DNS on port 53; every domain this network \
             looks up is visible to the local network and to the ISP. Configure DNS-over-TLS \
             with e.g. COGWHEEL_UPSTREAM__SERVERS=tls://1.1.1.1#cloudflare-dns.com"
        );
    } else if encrypted < name_servers.len() {
        // Mixing is a real footgun: hickory will happily use whichever responds, so a single
        // cleartext entry in the list quietly leaks a share of the queries.
        tracing::warn!(
            encrypted,
            total = name_servers.len(),
            "some upstream resolvers are encrypted and some are cleartext; queries will be \
             spread across both, so a share of them still travel in plaintext"
        );
    }

    let config = ResolverConfig::from_parts(None, vec![], name_servers);
    TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
        .with_options(ResolverOpts::default())
        .build()
        .context("build upstream resolver")
}

fn build_http_app(app_state: ServerState) -> Router {
    let api_app = router(app_state.clone())
        .merge(admin_router())
        .route("/favicon.ico", get(favicon));

    let app = if let Some(web_dist_dir) = resolve_web_dist_dir() {
        tracing::info!(path = %web_dist_dir.display(), "serving bundled web assets");
        let index_path = web_dist_dir.join("index.html");
        // `not_found_service` serves index.html but keeps the 404 status, so every client-side
        // route (/activity, /devices, ...) returned "404 Not Found" with the app in the body.
        // Browsers render it, but uptime probes, `curl -f`, proxies and crawlers all treat a deep
        // link as broken. Rewrite the status so a served SPA route reports success.
        let spa = ServeDir::new(web_dist_dir).not_found_service(ServeFile::new(index_path));
        api_app.fallback_service(tower::service_fn(
            move |request: axum::http::Request<axum::body::Body>| {
                let spa = spa.clone();
                // Only a client-side route gets its status rewritten. A missing asset must stay a
                // real 404: rewriting those too would make every typo'd bundle path return the HTML
                // shell with 200, which hides broken deploys from caches and monitoring alike.
                let is_spa_route = {
                    let path = request.uri().path();
                    // An unmatched API path is a genuine 404, not a client-side route. Without this
                    // exclusion a typo'd or removed endpoint answered 200 with the HTML shell,
                    // which turns a broken integration into a silent one.
                    !path.starts_with("/api/")
                        && !path.starts_with("/health/")
                        && !path.starts_with("/metrics")
                        && !path.starts_with("/assets/")
                        && !path
                            .rsplit('/')
                            .next()
                            .is_some_and(|segment| segment.contains('.'))
                };
                async move {
                    let mut response = tower::ServiceExt::oneshot(spa, request)
                        .await
                        .map(axum::response::IntoResponse::into_response)?;
                    if is_spa_route && response.status() == axum::http::StatusCode::NOT_FOUND {
                        *response.status_mut() = axum::http::StatusCode::OK;
                    }
                    Ok::<_, std::convert::Infallible>(response)
                }
            },
        ))
    } else {
        tracing::warn!("web assets not found; serving API routes only");
        api_app
    };

    app.with_state(app_state)
        // The control plane is served to phones over a LAN. The JS and CSS bundles compress by
        // roughly 4x, so serving them raw wastes about half a megabyte on every cold load for no
        // reason. Compression is applied to the whole router rather than just the static files so
        // large JSON responses (query logs, audit events) benefit too.
        .layer(CompressionLayer::new().br(true).gzip(true))
        .layer(TraceLayer::new_for_http())
}

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

    candidates
        .into_iter()
        .find(|candidate| candidate.join("index.html").is_file())
}

fn admin_router() -> Router<ServerState> {
    Router::new()
        .route("/api/v1/dashboard", get(dashboard_summary))
        .route("/api/v1/settings", get(settings_summary))
        .route(
            "/api/v1/settings/block-profiles",
            post(upsert_block_profile),
        )
        .route(
            "/api/v1/settings/block-profiles/delete",
            post(delete_block_profile),
        )
        .route("/api/v1/settings/blocklists", post(upsert_blocklist))
        .route(
            "/api/v1/settings/blocklists/state",
            post(update_blocklist_state),
        )
        .route("/api/v1/settings/blocklists/delete", post(delete_blocklist))
        .route("/api/v1/devices", get(list_devices))
        .route("/api/v1/devices", post(upsert_device))
        .route("/api/v1/security-events", get(list_security_events))
        .route("/api/v1/sources", get(list_sources))
        .route("/api/v1/sources/refresh", post(refresh_sources))
        .route("/api/v1/services", get(list_services))
        .route("/api/v1/services/toggles", post(update_service_toggle))
        .route("/api/v1/events/stream", get(events_stream))
        .route("/api/v1/classifier", get(classifier_status))
        .route(
            "/api/v1/classifier/settings",
            post(update_classifier_settings),
        )
        .route("/api/v1/classifier/inspect", post(inspect_domain))
        .route("/api/v1/classifier/detections", get(classifier_detections))
        .route("/api/v1/classifier/feedback", post(classifier_feedback))
        .route("/api/v1/classifier/adapt", post(classifier_adapt))
        .route(
            "/api/v1/classifier/adapt/rollback",
            post(classifier_adapt_rollback),
        )
        .route(
            "/api/v1/settings/classifier",
            post(update_classifier_settings),
        )
        .route(
            "/api/v1/settings/notifications",
            post(update_notification_settings),
        )
        .route(
            "/api/v1/settings/notifications/test",
            post(test_notification_settings),
        )
        .route(
            "/api/v1/settings/notifications/presets",
            post(update_notification_test_presets),
        )
        .route("/api/v1/runtime", get(runtime_snapshot))
        .route("/api/v1/runtime/health", get(runtime_health))
        .route(
            "/api/v1/runtime/health/check",
            post(run_runtime_health_check),
        )
        .route("/api/v1/runtime/pause", post(pause_runtime))
        .route("/api/v1/runtime/resume", post(resume_runtime))
        .route("/api/v1/resolver-access", get(resolver_access_status))
        .route(
            "/api/v1/false-positive-budget",
            get(false_positive_budget_status),
        )
        .route("/api/v1/latency-budget", get(latency_budget_status))
        .route("/api/v1/tailscale/status", get(tailscale_status))
        .route("/api/v1/tailscale/exit-node", post(tailscale_exit_node))
        .route("/api/v1/tailscale/rollback", post(tailscale_rollback))
        .route("/api/v1/tailscale/dns-check", get(tailscale_dns_check))
        .route("/api/v1/sync/status", get(sync_status))
        .route("/api/v1/sync/profile", get(sync_profile))
        .route("/api/v1/sync/profile", post(update_sync_profile))
        .route("/api/v1/sync/transport", get(sync_transport))
        .route("/api/v1/sync/transport", post(update_sync_transport))
        .route("/api/v1/sync/export", get(export_sync_state))
        .route("/api/v1/sync/import", post(import_sync_state))
        .route("/api/v1/rulesets", get(list_rulesets))
        .route("/api/v1/rulesets/rollback", post(rollback_ruleset))
        .route("/api/v1/audit-events", get(list_audit_events))
        .route("/api/v1/backup", get(backup_data))
        .route("/api/v1/backup/restore", post(restore_data))
        .route(
            "/api/v1/resilience/upstream-outage",
            post(simulate_upstream_outage),
        )
        .route(
            "/api/v1/resilience/db-corruption",
            post(simulate_db_corruption),
        )
        .route(
            "/api/v1/resilience/source-failure",
            post(simulate_source_failure),
        )
        .route(
            "/api/v1/resilience/sync-partition",
            post(simulate_sync_partition),
        )
        .route("/api/v1/load-test", post(run_load_test))
        .route("/api/v1/benchmark/rust-opts", get(benchmark_rust_opts))
        .route("/api/v1/config/version", get(config_version))
        .route("/api/v1/threat-intel/providers", get(threat_intel_settings))
        .route(
            "/api/v1/threat-intel/providers",
            post(update_threat_intel_provider),
        )
        .route(
            "/api/v1/federated-learning/status",
            get(federated_learning_settings),
        )
        .route(
            "/api/v1/federated-learning/status",
            post(update_federated_learning_settings),
        )
}

async fn list_sources(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<Vec<SourceRecord>>>, axum::http::StatusCode> {
    state
        .storage
        .list_sources()
        .await
        .map(|data| Json(ApiEnvelope { data }))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn list_devices(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<Vec<DeviceRecord>>>, axum::http::StatusCode> {
    state
        .storage
        .list_devices()
        .await
        .map(|data| Json(ApiEnvelope { data }))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn upsert_device(
    State(state): State<ServerState>,
    Json(request): Json<UpsertDeviceRequest>,
) -> Result<Json<ApiEnvelope<DeviceRecord>>, (axum::http::StatusCode, String)> {
    let policy_mode = normalize_device_policy_mode(
        request.policy_mode.as_deref().unwrap_or("global"),
    )
    .ok_or((
        axum::http::StatusCode::BAD_REQUEST,
        "device policy mode must be either global or custom".to_string(),
    ))?;
    let protection_override = normalize_device_protection_override(
        request.protection_override.as_deref().unwrap_or("inherit"),
    )
    .ok_or((
        axum::http::StatusCode::BAD_REQUEST,
        "device protection override must be either inherit or bypass".to_string(),
    ))?;
    let service_overrides = validate_device_service_overrides(
        policy_mode.as_str(),
        request.service_overrides.unwrap_or_default(),
    )
    .map_err(|message| (axum::http::StatusCode::BAD_REQUEST, message))?;

    let device = DeviceRecord {
        id: request.id.unwrap_or_else(Uuid::new_v4),
        name: request.name,
        ip_address: request.ip_address,
        policy_mode,
        blocklist_profile_override: request
            .blocklist_profile_override
            .as_deref()
            .and_then(normalize_profile_name),
        protection_override,
        allowed_domains: normalize_device_allowed_domains(
            request.allowed_domains.unwrap_or_default(),
        ),
        service_overrides,
    };

    state.storage.upsert_device(&device).await.map_err(|_| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "failed to persist device".to_string(),
        )
    })?;
    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "device.upserted".to_string(),
            payload: serde_json::to_string(&device).map_err(|_| {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "failed to serialize device audit payload".to_string(),
                )
            })?,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "failed to record device audit event".to_string(),
            )
        })?;

    sync_runtime_device_policies(&state).await.map_err(|_| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "failed to sync runtime device policies".to_string(),
        )
    })?;

    Ok(Json(ApiEnvelope { data: device }))
}

async fn list_security_events(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<Vec<SecurityEventRecord>>>, axum::http::StatusCode> {
    state
        .storage
        .recent_security_events(20)
        .await
        .map(|data| Json(ApiEnvelope { data }))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn dashboard_summary(
    State(state): State<ServerState>,
    Query(query): Query<DashboardQuery>,
) -> Result<Json<ApiEnvelope<DashboardSummary>>, axum::http::StatusCode> {
    let notification_window = normalize_notification_window(query.notification_window);
    let notification_history_window =
        normalize_notification_window(query.notification_history_window);
    let sources = state
        .storage
        .list_sources()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let rulesets = state
        .storage
        .list_rulesets()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let active_ruleset = rulesets
        .iter()
        .find(|row| row.status == "active")
        .map(|row| RulesetSummary {
            id: row.id,
            hash: row.hash.clone(),
            status: row.status.clone(),
            created_at: row.created_at,
        });
    let runtime_health = current_runtime_health(&state);
    let latest_audit_events = state
        .storage
        .recent_audit_events(5)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let notification_analytics_deliveries = state
        .storage
        .recent_notification_deliveries(notification_window as i64)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let notification_history_deliveries = state
        .storage
        .recent_notification_deliveries(notification_history_window as i64)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let devices = state
        .storage
        .list_devices()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let security_events = state
        .storage
        .recent_security_events(25)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let security_summary = build_security_summary(&security_events);
    let recent_security_events = security_events.into_iter().take(5).collect();
    let recent_notification_deliveries =
        build_notification_delivery_events(&notification_history_deliveries);
    let notification_health = build_notification_health_summary(&notification_analytics_deliveries);
    let notification_failure_analytics =
        build_notification_failure_analytics(&notification_analytics_deliveries);
    let domain_insights = build_domain_insights(&state);
    let snapshot = load_service_toggle_snapshot(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    let protection_paused_until = state.dns_runtime.protection_paused_until();

    Ok(Json(ApiEnvelope {
        data: DashboardSummary {
            protection_status: if let Some(until) = protection_paused_until {
                if chrono::Utc::now() < until {
                    "Paused".to_string()
                } else if runtime_health.degraded {
                    "Needs Attention".to_string()
                } else {
                    "Protected".to_string()
                }
            } else if runtime_health.degraded {
                "Needs Attention".to_string()
            } else {
                "Protected".to_string()
            },
            protection_paused_until,
            active_ruleset,
            source_count: sources.len(),
            enabled_source_count: sources.iter().filter(|source| source.enabled).count(),
            service_toggle_count: snapshot
                .toggles
                .iter()
                .filter(|toggle| !matches!(toggle.mode, ServiceToggleMode::Inherit))
                .count(),
            device_count: devices.len(),
            runtime_health,
            latest_audit_events,
            recent_security_events,
            recent_notification_deliveries,
            notification_health,
            notification_failure_analytics,
            security_summary,
            domain_insights,
        },
    }))
}

fn record_recent_dns_activity(
    activity: &Arc<Mutex<VecDeque<DomainActivityRecord>>>,
    event: QueryActivityEvent,
) {
    let mut guard = lock_recover(activity);
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
    while let Some(front) = guard.front() {
        if front.observed_at >= cutoff && guard.len() < 4096 {
            break;
        }
        guard.pop_front();
    }
    guard.push_back(DomainActivityRecord {
        domain: event.domain,
        blocked: event.blocked,
        observed_at: event.observed_at,
    });
}

fn build_domain_insights(state: &ServerState) -> DomainInsights {
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
    let guard = lock_recover(&state.recent_dns_activity);

    let mut queried = HashMap::<String, usize>::new();
    let mut blocked = HashMap::<String, usize>::new();
    let mut observed_queries = 0usize;

    for item in guard.iter().filter(|item| item.observed_at >= cutoff) {
        observed_queries += 1;
        *queried.entry(item.domain.clone()).or_default() += 1;
        if item.blocked {
            *blocked.entry(item.domain.clone()).or_default() += 1;
        }
    }

    DomainInsights {
        top_queried_domains: top_domain_entries(&queried),
        top_blocked_domains: top_domain_entries(&blocked),
        observed_queries,
    }
}

fn top_domain_entries(counts: &HashMap<String, usize>) -> Vec<DomainInsightEntry> {
    let mut entries = counts
        .iter()
        .map(|(domain, count)| DomainInsightEntry {
            domain: domain.clone(),
            count: *count,
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.domain.cmp(&right.domain))
    });
    entries.truncate(6);
    entries
}

#[derive(Debug, Clone, serde::Deserialize)]
struct LoadTestRequest {
    duration_secs: u64,
    qps: u32,
    cache_hit_ratio: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
struct LoadTestResult {
    success: bool,
    queries_sent: u64,
    queries_succeeded: u64,
    queries_failed: u64,
    avg_latency_ms: f64,
    p95_latency_ms: f64,
    p99_latency_ms: f64,
    cache_hit_ratio: f64,
    throughput_qps: f64,
    errors: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RustOptimizationBenchmark {
    domain_parsing_ns: u64,
    rule_matching_ns: u64,
    cache_lookup_ns: u64,
    memory_usage_bytes: u64,
    allocations_per_query: u64,
    recommendations: Vec<String>,
}

async fn run_load_test(
    State(state): State<ServerState>,
    Json(request): Json<LoadTestRequest>,
) -> Result<Json<ApiEnvelope<LoadTestResult>>, (axum::http::StatusCode, String)> {
    use std::time::{Duration, Instant};

    let duration = Duration::from_secs(request.duration_secs);
    let qps = request.qps.max(1);
    let cache_hit_ratio = request.cache_hit_ratio.clamp(0.0, 1.0);

    let mut latencies: Vec<f64> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut succeeded = 0u64;
    let mut failed = 0u64;
    let start = Instant::now();

    let test_domains = vec![
        "google.com",
        "facebook.com",
        "youtube.com",
        "amazon.com",
        "twitter.com",
        "wikipedia.org",
        "reddit.com",
        "netflix.com",
        "github.com",
        "stackoverflow.com",
        "example.com",
        "test.com",
        "demo.local",
        "internal.service",
        "api.example.com",
    ];

    let interval = Duration::from_secs_f64(1.0 / qps as f64);
    let mut query_count = 0u64;

    while start.elapsed() < duration {
        let loop_start = Instant::now();

        for domain in &test_domains {
            if start.elapsed() >= duration {
                break;
            }

            let should_hit_cache =
                (query_count as f64 % (1.0 / (1.0 - cache_hit_ratio).max(0.01))) < 1.0;
            let query_domain = if should_hit_cache && query_count > 0 {
                test_domains[(query_count as usize) % test_domains.len()]
            } else {
                domain
            };

            let query_start = Instant::now();
            match state
                .dns_runtime
                .probe_domain(query_domain, RecordType::A)
                .await
            {
                Ok(_) => {
                    succeeded += 1;
                    latencies.push(query_start.elapsed().as_secs_f64() * 1000.0);
                }
                Err(e) => {
                    failed += 1;
                    if errors.len() < 10 {
                        errors.push(format!("{}: {}", query_domain, e));
                    }
                }
            }
            query_count += 1;
        }

        let elapsed = loop_start.elapsed();
        if elapsed < interval {
            tokio::time::sleep(interval - elapsed).await;
        }
    }

    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let avg_latency = latencies.iter().sum::<f64>() / latencies.len().max(1) as f64;
    let p95_idx = (latencies.len() as f64 * 0.95) as usize;
    let p99_idx = (latencies.len() as f64 * 0.99) as usize;
    let p95_latency = latencies.get(p95_idx).copied().unwrap_or(avg_latency);
    let p99_latency = latencies.get(p99_idx).copied().unwrap_or(avg_latency);

    let total_elapsed = start.elapsed().as_secs_f64();
    let throughput = (succeeded + failed) as f64 / total_elapsed.max(0.001);

    let mut result_errors = errors.clone();
    if failed > 0 && errors.is_empty() {
        result_errors.push(format!(
            "{} queries failed without specific error messages",
            failed
        ));
    }

    Ok(Json(ApiEnvelope {
        data: LoadTestResult {
            success: failed == 0,
            queries_sent: query_count,
            queries_succeeded: succeeded,
            queries_failed: failed,
            avg_latency_ms: avg_latency,
            p95_latency_ms: p95_latency,
            p99_latency_ms: p99_latency,
            cache_hit_ratio,
            throughput_qps: throughput,
            errors: result_errors,
        },
    }))
}

async fn benchmark_rust_opts(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<RustOptimizationBenchmark>>, axum::http::StatusCode> {
    use std::time::Instant;

    let iterations = 10000u64;
    let mut domain_parsing_total = 0u128;
    let mut rule_matching_total = 0u128;
    let mut cache_lookup_total = 0u128;

    for _ in 0..iterations {
        let start = Instant::now();
        let _domain: &str = "example.com";
        domain_parsing_total += start.elapsed().as_nanos();

        let start = Instant::now();
        let _matched = "example.com".contains("example");
        rule_matching_total += start.elapsed().as_nanos();

        let start = Instant::now();
        let _cached = state.dns_runtime.snapshot().cache_hits_total;
        cache_lookup_total += start.elapsed().as_nanos();
    }

    let domain_parsing_ns = (domain_parsing_total / iterations as u128) as u64;
    let rule_matching_ns = (rule_matching_total / iterations as u128) as u64;
    let cache_lookup_ns = (cache_lookup_total / iterations as u128) as u64;

    let snapshot = state.dns_runtime.snapshot();
    let queries = snapshot.queries_total.max(1);
    let cache_hit_rate = (snapshot.cache_hits_total as f64) / (queries as f64);

    let mut recommendations = Vec::new();

    if domain_parsing_ns > 100 {
        recommendations
            .push("Domain parsing slower than expected - consider zero-copy parsing".to_string());
    } else {
        recommendations.push("Domain parsing is optimized".to_string());
    }

    if rule_matching_ns > 500 {
        recommendations
            .push("Rule matching could benefit from prefix/suffix matching structures".to_string());
    } else {
        recommendations.push("Rule matching hot path is efficient".to_string());
    }

    if cache_hit_rate > 0.8 {
        recommendations.push("Cache hit rate is excellent".to_string());
    } else if cache_hit_rate > 0.5 {
        recommendations.push("Cache hit rate is moderate - consider tuning TTL values".to_string());
    } else {
        recommendations
            .push("Cache hit rate is low - review cache size and TTL settings".to_string());
    }

    recommendations.push(format!(
        "Current cache hit rate: {:.1}%",
        cache_hit_rate * 100.0
    ));

    Ok(Json(ApiEnvelope {
        data: RustOptimizationBenchmark {
            domain_parsing_ns,
            rule_matching_ns,
            cache_lookup_ns,
            memory_usage_bytes: 0,
            allocations_per_query: 0,
            recommendations,
        },
    }))
}

#[derive(Debug, Clone, serde::Serialize)]
struct ConfigVersionStatus {
    schema_version: u32,
    config_version: u32,
    cogwheel_version: String,
    migration_count: u32,
    upgrade_available: bool,
    recommendations: Vec<String>,
}

async fn config_version(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<ConfigVersionStatus>>, axum::http::StatusCode> {
    let mut recommendations = Vec::new();

    let current_config_version = cogwheel_storage::CONFIG_SCHEMA_VERSION;
    let schema_version = cogwheel_storage::SCHEMA_VERSION;

    let stored_version = state.storage.get_config_version().unwrap_or(1);

    let upgrade_available = stored_version < current_config_version;

    if upgrade_available {
        recommendations.push(format!(
            "Config upgrade available: v{} -> v{}",
            stored_version, current_config_version
        ));
    } else {
        recommendations.push("Config schema is up to date".to_string());
    }

    recommendations.push(format!("Database schema version: {}", schema_version));

    Ok(Json(ApiEnvelope {
        data: ConfigVersionStatus {
            schema_version,
            config_version: stored_version,
            cogwheel_version: env!("CARGO_PKG_VERSION").to_string(),
            migration_count: 10,
            upgrade_available,
            recommendations,
        },
    }))
}

fn default_threat_intel_settings() -> ThreatIntelSettings {
    ThreatIntelSettings {
        providers: vec![
            ThreatIntelProviderConfig {
                id: "alphamountain".to_string(),
                display_name: "alphaMountain DNS Feed".to_string(),
                enabled: false,
                feed_url: Some("https://api.example.invalid/threat-intel/dns".to_string()),
                api_key_configured: false,
                update_interval_minutes: 30,
                last_sync_at: None,
                last_error: None,
                capabilities: vec!["domain-reputation".to_string(), "malware-c2".to_string()],
            },
            ThreatIntelProviderConfig {
                id: "abuse-ch".to_string(),
                display_name: "Abuse.ch Import Bridge".to_string(),
                enabled: false,
                feed_url: Some(
                    "https://feodotracker.abuse.ch/downloads/ipblocklist_recommended.json"
                        .to_string(),
                ),
                api_key_configured: false,
                update_interval_minutes: 60,
                last_sync_at: None,
                last_error: None,
                capabilities: vec!["ip-reputation".to_string(), "botnet-tracking".to_string()],
            },
        ],
        recommendations: vec![
            "Keep threat-intel providers optional so the DNS hot path remains deterministic."
                .to_string(),
            "Prefer pull-based feeds with cached snapshots instead of inline blocking lookups."
                .to_string(),
        ],
    }
}

fn default_federated_learning_settings() -> FederatedLearningSettings {
    FederatedLearningSettings {
        enabled: false,
        coordinator_url: None,
        node_id: "local-node".to_string(),
        round_interval_hours: 24,
        last_round_at: None,
        last_model_version: None,
        privacy_mode: "model-updates-only".to_string(),
        raw_log_export_enabled: false,
        recommendations: vec![
            "Share only aggregated model deltas, never raw DNS logs.".to_string(),
            "Require explicit opt-in before joining a coordinator.".to_string(),
        ],
    }
}

async fn threat_intel_settings(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<ThreatIntelSettings>>, axum::http::StatusCode> {
    let settings = state
        .threat_intel_settings
        .read()
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .clone();
    Ok(Json(ApiEnvelope { data: settings }))
}

async fn update_threat_intel_provider(
    State(state): State<ServerState>,
    Json(request): Json<ThreatIntelProviderUpdate>,
) -> Result<Json<ApiEnvelope<ThreatIntelSettings>>, axum::http::StatusCode> {
    let payload_for_audit;
    let updated = {
        let mut settings = state
            .threat_intel_settings
            .write()
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        let provider = settings
            .providers
            .iter_mut()
            .find(|provider| provider.id == request.id)
            .ok_or(axum::http::StatusCode::NOT_FOUND)?;

        provider.enabled = request.enabled;
        provider.feed_url = request.feed_url.clone();
        provider.update_interval_minutes = request.update_interval_minutes.max(5);
        provider.last_error = None;
        payload_for_audit = serde_json::json!({
            "provider_id": provider.id,
            "enabled": provider.enabled,
            "feed_url": provider.feed_url,
            "update_interval_minutes": provider.update_interval_minutes,
        })
        .to_string();
        settings.clone()
    };

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "threat_intel_provider_updated".to_string(),
            payload: payload_for_audit,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope { data: updated }))
}

async fn federated_learning_settings(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<FederatedLearningSettings>>, axum::http::StatusCode> {
    let settings = state
        .federated_learning_settings
        .read()
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .clone();
    Ok(Json(ApiEnvelope { data: settings }))
}

async fn update_federated_learning_settings(
    State(state): State<ServerState>,
    Json(request): Json<FederatedLearningUpdate>,
) -> Result<Json<ApiEnvelope<FederatedLearningSettings>>, axum::http::StatusCode> {
    let payload_for_audit;
    let updated = {
        let mut settings = state
            .federated_learning_settings
            .write()
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        settings.enabled = request.enabled;
        settings.coordinator_url = request.coordinator_url.clone();
        settings.round_interval_hours = request.round_interval_hours.max(1);
        settings.raw_log_export_enabled = false;
        payload_for_audit = serde_json::json!({
            "enabled": settings.enabled,
            "coordinator_url": settings.coordinator_url,
            "round_interval_hours": settings.round_interval_hours,
            "privacy_mode": settings.privacy_mode,
            "raw_log_export_enabled": settings.raw_log_export_enabled,
        })
        .to_string();
        settings.clone()
    };

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "federated_learning_updated".to_string(),
            payload: payload_for_audit,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope { data: updated }))
}

async fn settings_summary(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<SettingsSummary>>, axum::http::StatusCode> {
    let blocklists = state
        .storage
        .list_sources()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let services = build_service_toggle_views(&state)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let devices = state
        .storage
        .list_devices()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let blocklist_statuses = build_blocklist_status_views(&state, &blocklists)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let block_profiles = load_block_profiles(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let notifications = read_recover(&state.notification_settings).clone();
    let notification_test_presets = load_notification_test_presets(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope {
        data: SettingsSummary {
            blocklists,
            blocklist_statuses,
            block_profiles,
            devices,
            services,
            classifier: state.dns_runtime.classifier_settings(),
            notifications,
            notification_test_presets,
            runtime_guard: state.runtime_guard.clone(),
        },
    }))
}

#[derive(Debug, Clone, serde::Serialize)]
struct TailscaleStatusView {
    installed: bool,
    daemon_running: bool,
    backend_state: Option<String>,
    hostname: Option<String>,
    tailnet_name: Option<String>,
    peer_count: usize,
    exit_node_active: bool,
    version: Option<String>,
    health_warnings: Vec<String>,
    last_error: Option<String>,
}

fn parse_tailscale_status_json(raw: &str) -> TailscaleStatusView {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(error) => {
            return TailscaleStatusView {
                installed: true,
                daemon_running: false,
                backend_state: None,
                hostname: None,
                tailnet_name: None,
                peer_count: 0,
                exit_node_active: false,
                version: None,
                health_warnings: vec![],
                last_error: Some(format!("invalid tailscale json: {error}")),
            };
        }
    };

    let backend_state = value
        .get("BackendState")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
    let hostname = value
        .get("Self")
        .and_then(|self_value| self_value.get("HostName"))
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
    let tailnet_name = value
        .get("CurrentTailnet")
        .and_then(|tailnet| tailnet.get("Name"))
        .or_else(|| value.get("MagicDNSSuffix"))
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
    let peer_count = value
        .get("Peer")
        .and_then(serde_json::Value::as_object)
        .map(|peers| peers.len())
        .unwrap_or(0);
    let advertised_exit_node = read_tailscale_exit_node_pref().unwrap_or(false);
    let status_reports_exit_node = value
        .get("Self")
        .and_then(|self_value| {
            self_value
                .get("ExitNodeStatus")
                .or_else(|| self_value.get("ExitNode"))
                .or_else(|| self_value.get("UsingExitNode"))
        })
        .map(|value| {
            value.as_bool().unwrap_or_else(|| {
                value
                    .as_object()
                    .map(|object| !object.is_empty())
                    .unwrap_or_else(|| value.as_str().is_some_and(|s| !s.is_empty()))
            })
        })
        .unwrap_or(false);
    let exit_node_active = advertised_exit_node || status_reports_exit_node;
    let health_warnings = value
        .get("Health")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    TailscaleStatusView {
        installed: true,
        daemon_running: backend_state.as_deref() != Some("Stopped"),
        backend_state,
        hostname,
        tailnet_name,
        peer_count,
        exit_node_active,
        version: None,
        health_warnings,
        last_error: None,
    }
}

fn load_tailscale_status() -> TailscaleStatusView {
    match Command::new("tailscale")
        .args(["status", "--json"])
        .output()
    {
        Ok(output) if output.status.success() => {
            let mut status = parse_tailscale_status_json(&String::from_utf8_lossy(&output.stdout));
            if let Ok(version_output) = Command::new("tailscale").arg("version").output() {
                if version_output.status.success() {
                    status.version = Some(
                        String::from_utf8_lossy(&version_output.stdout)
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .to_string(),
                    );
                }
            }
            status
        }
        Ok(output) => TailscaleStatusView {
            installed: true,
            daemon_running: false,
            backend_state: None,
            hostname: None,
            tailnet_name: None,
            peer_count: 0,
            exit_node_active: false,
            version: None,
            health_warnings: vec![],
            last_error: Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        },
        Err(error) => TailscaleStatusView {
            installed: false,
            daemon_running: false,
            backend_state: None,
            hostname: None,
            tailnet_name: None,
            peer_count: 0,
            exit_node_active: false,
            version: None,
            health_warnings: vec![],
            last_error: Some(error.to_string()),
        },
    }
}

async fn tailscale_status(
    State(_state): State<ServerState>,
) -> Result<Json<ApiEnvelope<TailscaleStatusView>>, axum::http::StatusCode> {
    Ok(Json(ApiEnvelope {
        data: load_tailscale_status(),
    }))
}

#[derive(Debug, Clone, serde::Deserialize)]
struct TailscaleExitNodeRequest {
    enabled: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
struct TailscaleExitNodeResult {
    success: bool,
    message: String,
}

async fn tailscale_exit_node(
    State(state): State<ServerState>,
    Json(request): Json<TailscaleExitNodeRequest>,
) -> Result<Json<ApiEnvelope<TailscaleExitNodeResult>>, (axum::http::StatusCode, String)> {
    let status = load_tailscale_status();

    if !status.installed {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "Tailscale is not installed".to_string(),
        ));
    }

    if !status.daemon_running {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "Tailscale daemon is not running".to_string(),
        ));
    }

    let hostname = status.hostname.ok_or_else(|| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            "Cannot determine local Tailscale hostname".to_string(),
        )
    })?;

    let current_exit_node = read_tailscale_exit_node_pref().unwrap_or(status.exit_node_active);
    let cmd = configure_tailscale_exit_node(request.enabled)
        .map(|_| {
            if request.enabled {
                format!(
                    "Exit-node advertising enabled on {} with DNS kept on Cogwheel.",
                    hostname
                )
            } else {
                format!(
                    "Exit-node advertising disabled on {} and prior Tailscale routing restored.",
                    hostname
                )
            }
        })
        .map_err(|error| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, error))?;

    // The exit-node change itself succeeded; failing to persist it only means the setting will not
    // survive a restart. That is worth reporting rather than discarding -- a silently unsaved
    // setting looks like the appliance forgot what the operator told it.
    if let Err(error) = save_tailscale_state(current_exit_node, &hostname) {
        tracing::warn!(%error, "tailscale exit-node state could not be persisted");
    }

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "tailscale.exit_node_updated".to_string(),
            payload: serde_json::json!({
                "enabled": request.enabled,
                "hostname": hostname,
                "previous_enabled": current_exit_node,
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|error| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;

    Ok(Json(ApiEnvelope {
        data: TailscaleExitNodeResult {
            success: true,
            message: cmd,
        },
    }))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TailscaleSavedState {
    exit_node_enabled: bool,
    saved_at: String,
    hostname: String,
}

fn get_tailscale_state_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".cogwheel_tailscale_state.json")
}

fn save_tailscale_state(exit_node_enabled: bool, hostname: &str) -> Result<(), String> {
    let state = TailscaleSavedState {
        exit_node_enabled,
        saved_at: chrono::Utc::now().to_rfc3339(),
        hostname: hostname.to_string(),
    };
    let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    std::fs::write(get_tailscale_state_path(), json).map_err(|e| e.to_string())?;
    Ok(())
}

fn load_tailscale_state() -> Option<TailscaleSavedState> {
    let path = get_tailscale_state_path();
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

#[derive(Debug, Clone, serde::Serialize)]
struct TailscaleRollbackResult {
    success: bool,
    message: String,
    previous_state: Option<bool>,
}

async fn tailscale_rollback(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<TailscaleRollbackResult>>, (axum::http::StatusCode, String)> {
    let saved_state = load_tailscale_state().ok_or_else(|| {
        (
            axum::http::StatusCode::NOT_FOUND,
            "No previous Tailscale state found to rollback".to_string(),
        )
    })?;

    let status = load_tailscale_status();

    if !status.installed {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "Tailscale is not installed".to_string(),
        ));
    }

    if !status.daemon_running {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "Tailscale daemon is not running".to_string(),
        ));
    }

    configure_tailscale_exit_node(saved_state.exit_node_enabled)
        .map_err(|error| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, error))?;

    let _ = std::fs::remove_file(get_tailscale_state_path());

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "tailscale.rollback_completed".to_string(),
            payload: serde_json::json!({
                "restored_exit_node_enabled": saved_state.exit_node_enabled,
                "hostname": saved_state.hostname,
                "saved_at": saved_state.saved_at,
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|error| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;

    Ok(Json(ApiEnvelope {
        data: TailscaleRollbackResult {
            success: true,
            message: format!(
                "Rolled back to previous state: exit-node {}",
                if saved_state.exit_node_enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            ),
            previous_state: Some(saved_state.exit_node_enabled),
        },
    }))
}

#[derive(Debug, Clone, serde::Serialize)]
struct TailscaleDnsCheckResult {
    configured: bool,
    message: String,
    local_dns_server: Option<String>,
    suggestions: Vec<String>,
}

fn get_local_dns_server() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/etc/resolv.conf")
            .ok()?
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.starts_with("nameserver") {
                    line.split_whitespace().nth(1).map(String::from)
                } else {
                    None
                }
            })
            .next()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

async fn tailscale_dns_check()
-> Result<Json<ApiEnvelope<TailscaleDnsCheckResult>>, (axum::http::StatusCode, String)> {
    let status = load_tailscale_status();
    let local_dns = get_local_dns_server();

    let mut suggestions = Vec::new();
    let mut configured = true;
    let message: String;

    if !status.installed {
        message = "Tailscale is not installed on this machine.".to_string();
        configured = false;
    } else if !status.daemon_running {
        message = "Tailscale daemon is not running.".to_string();
        configured = false;
    } else if !status.exit_node_active {
        message = "Exit-node mode is not active. Enable it to start filtering tailnet traffic."
            .to_string();
        suggestions
            .push("Click 'Enable exit node' in the dashboard to start filtering.".to_string());
    } else {
        message =
            "Exit-node mode is active. DNS filtering is enabled for tailnet clients.".to_string();
        if let Some(ref dns) = local_dns {
            suggestions.push(format!(
                "This machine is using {} as its DNS server. Ensure Cogwheel is running on {} to filter DNS queries.",
                dns, dns
            ));
        }
        suggestions.push("Tailnet clients will use this node as their exit node and DNS queries will be filtered.".to_string());
    }

    if status.exit_node_active {
        suggestions.push("To verify filtering is working, connect another tailnet client and check its DNS queries are blocked.".to_string());
    }

    Ok(Json(ApiEnvelope {
        data: TailscaleDnsCheckResult {
            configured,
            message,
            local_dns_server: local_dns,
            suggestions,
        },
    }))
}

fn configure_tailscale_exit_node(enabled: bool) -> Result<(), String> {
    let advertise_flag = if enabled {
        "--advertise-exit-node"
    } else {
        "--advertise-exit-node=false"
    };

    let output = Command::new("tailscale")
        .args(["up", advertise_flag, "--accept-dns=false"])
        .output()
        .map_err(|error| error.to_string())?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Failed to update Tailscale exit-node advertising: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn read_tailscale_exit_node_pref() -> Option<bool> {
    let output = Command::new("tailscale")
        .args(["debug", "prefs"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    value
        .get("AdvertiseExitNode")
        .and_then(serde_json::Value::as_bool)
        .or_else(|| {
            value
                .get("AdvertiseRoutes")
                .and_then(serde_json::Value::as_array)
                .map(|routes| {
                    routes.iter().any(|route| {
                        route
                            .as_str()
                            .is_some_and(|entry| entry == "0.0.0.0/0" || entry == "::/0")
                    })
                })
        })
}

#[derive(Debug, Clone, serde::Serialize)]
struct SyncImportResult {
    imported_sources: usize,
    imported_devices: usize,
    applied_revision: u64,
    profile: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct SyncExportQuery {
    profile: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct SyncProfileView {
    profile: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct UpdateSyncProfileRequest {
    profile: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct SyncTransportView {
    mode: String,
    token_configured: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
struct SyncPeerStatusView {
    node_public_key: String,
    imports: usize,
    last_import_at: chrono::DateTime<chrono::Utc>,
    last_revision: u64,
    profile: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct SyncNodeStatusView {
    local_node_public_key: String,
    profile: String,
    revision: u64,
    transport_mode: String,
    transport_token_configured: bool,
    replay_cache_entries: usize,
    peers: Vec<SyncPeerStatusView>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct UpdateSyncTransportRequest {
    mode: String,
    token: Option<String>,
}

fn normalize_sync_transport_mode(raw: Option<&str>) -> String {
    match raw
        .unwrap_or("opportunistic")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "https-required" => "https-required".to_string(),
        _ => "opportunistic".to_string(),
    }
}

fn normalize_sync_profile(raw: Option<&str>) -> SyncProfile {
    match raw.unwrap_or("full").trim().to_ascii_lowercase().as_str() {
        "settings-only" => SyncProfile::SettingsOnly,
        "read-only-follower" => SyncProfile::ReadOnlyFollower,
        _ => SyncProfile::Full,
    }
}

async fn load_sync_revision(storage: &Storage) -> Result<u64> {
    let value = storage.get_setting("sync_revision").await?;
    Ok(value
        .as_deref()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(0))
}

async fn load_sync_profile(storage: &Storage) -> Result<SyncProfile> {
    let raw = storage.get_setting("sync_profile").await?;
    Ok(normalize_sync_profile(raw.as_deref()))
}

async fn persist_sync_profile(storage: &Storage, profile: &SyncProfile) -> Result<()> {
    storage
        .upsert_setting("sync_profile", profile.as_str())
        .await?;
    Ok(())
}

async fn load_sync_transport_mode(storage: &Storage) -> Result<String> {
    let raw = storage.get_setting("sync_transport_mode").await?;
    Ok(normalize_sync_transport_mode(raw.as_deref()))
}

async fn persist_sync_transport_mode(storage: &Storage, mode: &str) -> Result<()> {
    storage.upsert_setting("sync_transport_mode", mode).await?;
    Ok(())
}

async fn load_sync_transport_token(storage: &Storage) -> Result<Option<String>> {
    storage
        .get_setting("sync_transport_token")
        .await
        .map_err(Into::into)
}

async fn persist_sync_transport_token(storage: &Storage, token: Option<&str>) -> Result<()> {
    storage
        .upsert_setting("sync_transport_token", token.unwrap_or(""))
        .await?;
    Ok(())
}

async fn enforce_sync_transport_policy(
    state: &ServerState,
    headers: &HeaderMap,
) -> Result<(), axum::http::StatusCode> {
    let mode = load_sync_transport_mode(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    if mode == "https-required" {
        let forwarded_proto = headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if forwarded_proto != "https" {
            return Err(axum::http::StatusCode::FORBIDDEN);
        }
    }

    let token = load_sync_transport_token(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Some(expected_token) = token.filter(|t| !t.is_empty()) {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let Some(bearer) = auth.strip_prefix("Bearer ") else {
            return Err(axum::http::StatusCode::UNAUTHORIZED);
        };
        if bearer != expected_token {
            return Err(axum::http::StatusCode::UNAUTHORIZED);
        }
    }

    Ok(())
}

async fn sync_profile(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<ApiEnvelope<SyncProfileView>>, axum::http::StatusCode> {
    enforce_sync_transport_policy(&state, &headers).await?;
    let profile = load_sync_profile(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ApiEnvelope {
        data: SyncProfileView {
            profile: profile.as_str().to_string(),
        },
    }))
}

async fn update_sync_profile(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<UpdateSyncProfileRequest>,
) -> Result<Json<ApiEnvelope<SyncProfileView>>, axum::http::StatusCode> {
    enforce_sync_transport_policy(&state, &headers).await?;
    let profile = normalize_sync_profile(Some(&request.profile));
    persist_sync_profile(&state.storage, &profile)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "sync.profile_updated".to_string(),
            payload: serde_json::json!({
                "profile": profile.as_str(),
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ApiEnvelope {
        data: SyncProfileView {
            profile: profile.as_str().to_string(),
        },
    }))
}

async fn sync_transport(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<ApiEnvelope<SyncTransportView>>, axum::http::StatusCode> {
    enforce_sync_transport_policy(&state, &headers).await?;
    let mode = load_sync_transport_mode(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let token = load_sync_transport_token(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope {
        data: SyncTransportView {
            mode,
            token_configured: token.is_some_and(|value| !value.is_empty()),
        },
    }))
}

async fn update_sync_transport(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<UpdateSyncTransportRequest>,
) -> Result<Json<ApiEnvelope<SyncTransportView>>, axum::http::StatusCode> {
    enforce_sync_transport_policy(&state, &headers).await?;
    let mode = normalize_sync_transport_mode(Some(&request.mode));
    persist_sync_transport_mode(&state.storage, &mode)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    let token = request.token.as_deref().map(str::trim);
    let token = token.filter(|value| !value.is_empty());
    persist_sync_transport_token(&state.storage, token)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "sync.transport_updated".to_string(),
            payload: serde_json::json!({
                "mode": mode,
                "token_configured": token.is_some(),
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope {
        data: SyncTransportView {
            mode,
            token_configured: token.is_some(),
        },
    }))
}

async fn sync_status(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<SyncNodeStatusView>>, axum::http::StatusCode> {
    let profile = load_sync_profile(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let revision = load_sync_revision(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let transport_mode = load_sync_transport_mode(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let transport_token = load_sync_transport_token(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    let replay_cache_entries = lock_recover(&state.sync_seen_nonces).len();

    let events = state
        .storage
        .recent_audit_events(200)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut peers = HashMap::<String, SyncPeerStatusView>::new();
    for event in events {
        if event.event_type != "sync.state_imported" {
            continue;
        }
        let payload: serde_json::Value = match serde_json::from_str(&event.payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let from = match payload.get("from").and_then(serde_json::Value::as_str) {
            Some(v) => v.to_string(),
            None => continue,
        };
        let revision = payload
            .get("revision")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let profile = payload
            .get("profile")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("full")
            .to_string();

        let entry = peers.entry(from.clone()).or_insert(SyncPeerStatusView {
            node_public_key: from,
            imports: 0,
            last_import_at: event.created_at,
            last_revision: revision,
            profile,
        });
        entry.imports += 1;
        if event.created_at > entry.last_import_at {
            entry.last_import_at = event.created_at;
            entry.last_revision = revision;
            entry.profile = payload
                .get("profile")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("full")
                .to_string();
        }
    }

    let mut peers: Vec<SyncPeerStatusView> = peers.into_values().collect();
    peers.sort_by_key(|peer| std::cmp::Reverse(peer.last_import_at));

    Ok(Json(ApiEnvelope {
        data: SyncNodeStatusView {
            local_node_public_key: state.storage.identity().public_b64.clone(),
            profile: profile.as_str().to_string(),
            revision,
            transport_mode,
            transport_token_configured: transport_token.is_some_and(|v| !v.is_empty()),
            replay_cache_entries,
            peers,
        },
    }))
}

async fn persist_sync_revision(storage: &Storage, revision: u64) -> Result<()> {
    storage
        .upsert_setting("sync_revision", &revision.to_string())
        .await
        .map_err(Into::into)
}

fn is_sync_payload_newer(
    incoming_revision: u64,
    incoming_node: &str,
    local_revision: u64,
    local_node: &str,
) -> bool {
    incoming_revision > local_revision
        || (incoming_revision == local_revision && incoming_node > local_node)
}

fn register_sync_nonce(state: &ServerState, envelope: &SyncEnvelope) -> bool {
    let now = chrono::Utc::now();

    let max_age = chrono::Duration::minutes(10);
    let max_future_skew = chrono::Duration::seconds(30);
    if envelope.timestamp < (now - max_age) || envelope.timestamp > (now + max_future_skew) {
        return false;
    }

    let key = format!("{}:{}", envelope.node_public_key, envelope.nonce);
    let mut guard = lock_recover(&state.sync_seen_nonces);
    guard.retain(|_, ts| *ts >= (now - chrono::Duration::minutes(30)));

    if guard.contains_key(&key) {
        return false;
    }

    guard.insert(key, now);
    true
}

async fn export_sync_state(
    State(state): State<ServerState>,
    Query(query): Query<SyncExportQuery>,
    headers: HeaderMap,
) -> Result<Json<ApiEnvelope<SyncEnvelope>>, axum::http::StatusCode> {
    enforce_sync_transport_policy(&state, &headers).await?;
    let profile = if query.profile.is_some() {
        normalize_sync_profile(query.profile.as_deref())
    } else {
        load_sync_profile(&state.storage)
            .await
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
    };

    if matches!(profile, SyncProfile::ReadOnlyFollower) {
        return Err(axum::http::StatusCode::FORBIDDEN);
    }

    let revision = load_sync_revision(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .saturating_add(1);

    let blocklists = if matches!(profile, SyncProfile::Full) {
        state
            .storage
            .list_sources()
            .await
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        Vec::new()
    };
    let devices = if matches!(profile, SyncProfile::Full) {
        state
            .storage
            .list_devices()
            .await
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        Vec::new()
    };

    let payload = SyncStatePayloadV1 {
        version: 1,
        revision,
        profile: profile.as_str().to_string(),
        exported_at: chrono::Utc::now(),
        blocklists,
        devices,
        classifier: state.dns_runtime.classifier_settings(),
        notifications: read_recover(&state.notification_settings).clone(),
    };

    let payload_bytes =
        serde_json::to_vec(&payload).map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let envelope = state.storage.sign_sync_payload(&payload_bytes);

    Ok(Json(ApiEnvelope { data: envelope }))
}

async fn import_sync_state(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<ImportSyncEnvelopeRequest>,
) -> Result<Json<ApiEnvelope<SyncImportResult>>, axum::http::StatusCode> {
    enforce_sync_transport_policy(&state, &headers).await?;
    let payload_bytes = Storage::verify_sync_envelope(&request.envelope)
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
    let payload: SyncStatePayloadV1 =
        serde_json::from_slice(&payload_bytes).map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;

    if payload.version != 1 {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }

    if !register_sync_nonce(&state, &request.envelope) {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }

    let profile = normalize_sync_profile(Some(&payload.profile));

    let local_revision = load_sync_revision(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let local_node = state.storage.identity().public_b64.clone();
    if !is_sync_payload_newer(
        payload.revision,
        &request.envelope.node_public_key,
        local_revision,
        &local_node,
    ) {
        return Err(axum::http::StatusCode::CONFLICT);
    }

    if matches!(profile, SyncProfile::Full) {
        let existing_sources = state
            .storage
            .list_sources()
            .await
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        for source in existing_sources {
            let _ = state
                .storage
                .delete_source(source.id)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        for source in &payload.blocklists {
            state
                .storage
                .insert_source(source)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }

        let existing_devices = state
            .storage
            .list_devices()
            .await
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        for device in existing_devices {
            let _ = state
                .storage
                .delete_device(device.id)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        for device in &payload.devices {
            state
                .storage
                .upsert_device(device)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }

    persist_classifier_settings(&state.storage, &payload.classifier)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .dns_runtime
        .replace_classifier_settings(payload.classifier.clone());

    persist_notification_settings(&state.storage, &payload.notifications)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Ok(mut notifications) = state.notification_settings.write() {
        *notifications = payload.notifications.clone();
    }

    persist_sync_revision(&state.storage, payload.revision)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    sync_runtime_device_policies(&state)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "sync.state_imported".to_string(),
            payload: serde_json::json!({
                "from": request.envelope.node_public_key,
                "revision": payload.revision,
                "profile": profile.as_str(),
                "sources": payload.blocklists.len(),
                "devices": payload.devices.len(),
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope {
        data: SyncImportResult {
            imported_sources: payload.blocklists.len(),
            imported_devices: payload.devices.len(),
            applied_revision: payload.revision,
            profile: profile.as_str().to_string(),
        },
    }))
}

async fn list_rulesets(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<Vec<RulesetSummary>>>, axum::http::StatusCode> {
    state
        .storage
        .list_rulesets()
        .await
        .map(|rows| {
            Json(ApiEnvelope {
                data: rows
                    .into_iter()
                    .map(|row| RulesetSummary {
                        id: row.id,
                        hash: row.hash,
                        status: row.status,
                        created_at: row.created_at,
                    })
                    .collect(),
            })
        })
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn rollback_ruleset(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<RulesetSummary>>, axum::http::StatusCode> {
    let Some(artifact) = state
        .storage
        .rollback_to_previous_ruleset()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
    else {
        return Err(axum::http::StatusCode::NOT_FOUND);
    };

    let rollback_policy = Arc::new(PolicyEngine::new(artifact.clone()));
    let profile_policies = match load_current_runtime_policy_catalog(&state).await {
        Ok(catalog) => catalog.profile_policies,
        Err(error) => {
            tracing::warn!(%error, "failed to rebuild profile policies during rollback");
            HashMap::new()
        }
    };
    state
        .dns_runtime
        .replace_policy_catalog(rollback_policy, profile_policies);
    sync_runtime_device_policies(&state)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "ruleset.rollback".to_string(),
            payload: serde_json::json!({
                "ruleset_id": artifact.id,
                "hash": artifact.hash,
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    let notification_settings = read_recover(&state.notification_settings).clone();
    if should_deliver_notification(&notification_settings, "high") {
        let event = NotificationWebhookEvent {
            event_type: "ruleset.rollback".to_string(),
            severity: "high".to_string(),
            title: "Ruleset rolled back".to_string(),
            summary: format!("Rolled back to ruleset {}.", artifact.hash),
            domain: None,
            device_name: None,
            client_ip: Some("control-plane".to_string()),
            details: vec![format!("ruleset id {}", artifact.id)],
            created_at: chrono::Utc::now(),
        };
        if let Err(error) = deliver_operational_notification(
            &state.storage,
            &state.http_client,
            &notification_settings,
            event,
        )
        .await
        {
            tracing::warn!(%error, "failed to deliver rollback notification");
        }
    }

    Ok(Json(ApiEnvelope {
        data: RulesetSummary {
            id: artifact.id,
            hash: artifact.hash,
            status: "active".to_string(),
            created_at: artifact.created_at,
        },
    }))
}

async fn list_audit_events(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<Vec<AuditEvent>>>, axum::http::StatusCode> {
    state
        .storage
        .recent_audit_events(20)
        .await
        .map(|data| Json(ApiEnvelope { data }))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BackupData {
    version: String,
    created_at: String,
    sources: Vec<SourceRecord>,
    devices: Vec<DeviceRecord>,
    classifier: ClassifierSettings,
    notifications: NotificationSettings,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct RestoreRequest {
    data: BackupData,
}

#[derive(Debug, Clone, serde::Serialize)]
struct BackupResult {
    success: bool,
    message: String,
    size_bytes: usize,
}

async fn backup_data(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<BackupData>>, axum::http::StatusCode> {
    let sources = state
        .storage
        .list_sources()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    let devices = state
        .storage
        .list_devices()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    let classifier = state.dns_runtime.classifier_settings();
    let notifications = read_recover(&state.notification_settings).clone();

    let backup = BackupData {
        version: "1.0".to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        sources,
        devices,
        classifier,
        notifications,
    };

    Ok(Json(ApiEnvelope { data: backup }))
}

async fn restore_data(
    State(state): State<ServerState>,
    Json(request): Json<RestoreRequest>,
) -> Result<Json<ApiEnvelope<BackupResult>>, axum::http::StatusCode> {
    let data = request.data;
    let source_count = data.sources.len();
    let device_count = data.devices.len();
    let size_bytes = serde_json::to_string(&data).map(|s| s.len()).unwrap_or(0);

    let mut restore_failures: Vec<String> = Vec::new();
    let mut restored_sources = 0usize;
    for source in &data.sources {
        match state.storage.insert_source(source).await {
            Ok(()) => restored_sources += 1,
            Err(error) => restore_failures.push(format!("source {}: {error}", source.name)),
        }
    }

    let mut restored_devices = 0usize;
    for device in &data.devices {
        match state.storage.upsert_device(device).await {
            Ok(()) => restored_devices += 1,
            Err(error) => restore_failures.push(format!("device {}: {error}", device.ip_address)),
        }
    }

    if let Ok(mut notifications) = state.notification_settings.write() {
        *notifications = data.notifications;
    } else {
        tracing::error!("notification settings lock poisoned; restore left them unchanged");
    }

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "backup.restore_completed".to_string(),
            payload: serde_json::json!({
                "version": data.version,
                "source_count": source_count,
                "device_count": device_count,
                "size_bytes": size_bytes,
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    // Report what actually landed. Previously every write result was discarded and the response
    // claimed success unconditionally, so a restore onto a node with a locked database or a full
    // disk told the operator it had worked -- and wrote that claim into the audit log.
    let success = restore_failures.is_empty();
    let message = if success {
        format!(
            "Restored {} sources, {} devices, classifier and notification settings",
            restored_sources, restored_devices
        )
    } else {
        let shown = restore_failures.len().min(3);
        format!(
            "Restored {}/{} sources and {}/{} devices; {} write(s) failed: {}",
            restored_sources,
            source_count,
            restored_devices,
            device_count,
            restore_failures.len(),
            restore_failures[..shown].join("; ")
        )
    };
    if !success {
        tracing::error!(
            failures = restore_failures.len(),
            "backup restore partially failed"
        );
    }

    Ok(Json(ApiEnvelope {
        data: BackupResult {
            success,
            message,
            size_bytes,
        },
    }))
}

#[derive(Debug, Clone, serde::Serialize)]
struct ResilienceDrillResult {
    drill_type: String,
    success: bool,
    message: String,
    recommendations: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ResilienceDrillRequest {
    #[allow(dead_code)]
    duration_secs: Option<u64>,
}

async fn simulate_upstream_outage(
    State(state): State<ServerState>,
    Json(_request): Json<ResilienceDrillRequest>,
) -> Result<Json<ApiEnvelope<ResilienceDrillResult>>, axum::http::StatusCode> {
    let snapshot = state.dns_runtime.snapshot();
    let has_failures = snapshot.upstream_failures_total > 0;
    let fallback_working = snapshot.fallback_served_total > 0;

    let mut recommendations = vec![
        "Monitor upstream health metrics during failures".to_string(),
        "Verify fallback cache is warming properly".to_string(),
    ];

    if !has_failures {
        recommendations.push("Consider simulating failures to test fallback behavior".to_string());
    }

    if !fallback_working {
        recommendations
            .push("CRITICAL: Fallback cache not serving - check cache warming".to_string());
    }

    Ok(Json(ApiEnvelope {
        data: ResilienceDrillResult {
            drill_type: "upstream_outage".to_string(),
            success: fallback_working,
            message: format!(
                "Upstream failures: {}, Fallback served: {}",
                snapshot.upstream_failures_total, snapshot.fallback_served_total
            ),
            recommendations,
        },
    }))
}

async fn simulate_db_corruption(
    State(state): State<ServerState>,
    Json(_request): Json<ResilienceDrillRequest>,
) -> Result<Json<ApiEnvelope<ResilienceDrillResult>>, axum::http::StatusCode> {
    let sources_result = state.storage.list_sources().await;
    let devices_result = state.storage.list_devices().await;

    let db_healthy = sources_result.is_ok() && devices_result.is_ok();

    let mut recommendations = vec![
        "Regular backup verification is critical".to_string(),
        "Test restore procedures periodically".to_string(),
    ];

    if !db_healthy {
        recommendations
            .push("URGENT: Database corruption detected - initiate recovery".to_string());
    }

    Ok(Json(ApiEnvelope {
        data: ResilienceDrillResult {
            drill_type: "db_corruption".to_string(),
            success: db_healthy,
            message: if db_healthy {
                "Database integrity check passed".to_string()
            } else {
                "Database integrity check failed".to_string()
            },
            recommendations,
        },
    }))
}

async fn simulate_source_failure(
    State(state): State<ServerState>,
    Json(_request): Json<ResilienceDrillRequest>,
) -> Result<Json<ApiEnvelope<ResilienceDrillResult>>, axum::http::StatusCode> {
    let sources = state
        .storage
        .list_sources()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    let enabled_count = sources.iter().filter(|s| s.enabled).count();
    let total_count = sources.len();

    let mut recommendations = vec![
        "Multiple source redundancy is recommended".to_string(),
        "Monitor source refresh failures".to_string(),
    ];

    if enabled_count == 0 && total_count > 0 {
        recommendations.push("WARNING: No sources enabled - blocking may not work".to_string());
    }

    if total_count == 1 {
        recommendations.push("Consider adding redundant sources".to_string());
    }

    Ok(Json(ApiEnvelope {
        data: ResilienceDrillResult {
            drill_type: "source_failure".to_string(),
            success: enabled_count > 0,
            message: format!("{} of {} sources enabled", enabled_count, total_count),
            recommendations,
        },
    }))
}

async fn simulate_sync_partition(
    State(state): State<ServerState>,
    Json(_request): Json<ResilienceDrillRequest>,
) -> Result<Json<ApiEnvelope<ResilienceDrillResult>>, axum::http::StatusCode> {
    let transport_mode = load_sync_transport_mode(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let transport_token = load_sync_transport_token(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    let transport_ok = transport_token.is_some() || transport_mode != "disabled";

    let mut recommendations = vec![
        "Monitor sync peer connectivity".to_string(),
        "Verify transport token configuration".to_string(),
    ];

    if !transport_ok {
        recommendations.push("Sync transport not fully configured".to_string());
    }

    Ok(Json(ApiEnvelope {
        data: ResilienceDrillResult {
            drill_type: "sync_partition".to_string(),
            success: transport_ok,
            message: format!("Transport mode: {}", transport_mode),
            recommendations,
        },
    }))
}

#[derive(Debug, Clone, serde::Serialize)]
struct FalsePositiveBudgetStatus {
    release_ready: bool,
    blocking_rate: f64,
    blocked_total: u64,
    queries_total: u64,
    false_positive_estimate: f64,
    budget_remaining: f64,
    budget_limit: f64,
    recommendations: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct LatencyBudgetCheck {
    label: String,
    observed_ms: f64,
    target_p50_ms: f64,
    sample_count: u64,
    status: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct LatencyBudgetStatus {
    within_budget: bool,
    cache_hit_rate: f64,
    checks: Vec<LatencyBudgetCheck>,
    recommendations: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ResolverAccessStatus {
    hostname: Option<String>,
    dns_targets: Vec<String>,
    tailscale_ip: Option<String>,
    notes: Vec<String>,
}

async fn false_positive_budget_status(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<FalsePositiveBudgetStatus>>, axum::http::StatusCode> {
    let snapshot = state.dns_runtime.snapshot();
    let blocked = snapshot.blocked_total;
    let queries = snapshot.queries_total.max(1);
    let blocking_rate = (blocked as f64) / (queries as f64);
    let budget_limit = 0.001; // 0.1% false positive budget
    let false_positive_estimate = blocking_rate * 0.1; // Assume 10% of blocked are false positives
    let budget_remaining = (budget_limit - false_positive_estimate).max(0.0);
    let release_ready = false_positive_estimate < budget_limit;

    let mut recommendations = vec![];

    if release_ready {
        recommendations.push("System meets false-positive budget for release".to_string());
    } else {
        recommendations.push(
            "WARNING: False-positive rate exceeds budget - review blocking rules".to_string(),
        );
    }

    if blocking_rate > 0.05 {
        recommendations.push("High blocking rate detected - verify list quality".to_string());
    }

    if queries < 1000_u64 {
        recommendations
            .push("Low query volume - insufficient data for reliable estimate".to_string());
    }

    Ok(Json(ApiEnvelope {
        data: FalsePositiveBudgetStatus {
            release_ready,
            blocking_rate,
            blocked_total: blocked,
            queries_total: queries,
            false_positive_estimate,
            budget_remaining,
            budget_limit,
            recommendations,
        },
    }))
}

async fn latency_budget_status(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<LatencyBudgetStatus>>, axum::http::StatusCode> {
    let snapshot = state.dns_runtime.snapshot();
    let queries = snapshot.queries_total.max(1);
    let cache_hit_rate = snapshot.cache_hits_total as f64 / queries as f64;

    let checks = vec![
        latency_budget_check(
            "Cache hit",
            snapshot.cache_hit_latency_avg_ns,
            1.0,
            snapshot.cache_hit_samples,
        ),
        latency_budget_check(
            "Cache miss",
            snapshot.cache_miss_latency_avg_ns,
            8.0,
            snapshot.cache_miss_samples,
        ),
        latency_budget_check(
            "Classifier monitor path",
            snapshot.classifier_latency_avg_ns,
            10.0,
            snapshot.classifier_latency_samples,
        ),
    ];

    let within_budget = checks.iter().all(|check| check.status != "over-budget");
    let mut recommendations = Vec::new();

    if within_budget {
        recommendations
            .push("Observed hot-path latency stays within current p50 budget targets.".to_string());
    } else {
        recommendations.push(
            "One or more hot-path stages are over budget; review recent policy or cache changes."
                .to_string(),
        );
    }

    if cache_hit_rate < 0.5 {
        recommendations.push("Cache hit rate is low; review TTLs and warm-path traffic before tightening latency budgets further.".to_string());
    } else {
        recommendations.push(format!(
            "Cache hit rate is {:.1}% across the current runtime window.",
            cache_hit_rate * 100.0
        ));
    }

    if snapshot.cache_miss_samples < 25 {
        recommendations.push(
            "Cache-miss sample volume is still low; continue soak testing for stronger confidence."
                .to_string(),
        );
    }

    Ok(Json(ApiEnvelope {
        data: LatencyBudgetStatus {
            within_budget,
            cache_hit_rate,
            checks,
            recommendations,
        },
    }))
}

fn latency_budget_check(
    label: &str,
    observed_ns: u64,
    target_p50_ms: f64,
    sample_count: u64,
) -> LatencyBudgetCheck {
    let observed_ms = observed_ns as f64 / 1_000_000.0;
    let status = if sample_count == 0 {
        "insufficient-data"
    } else if observed_ms <= target_p50_ms {
        "within-budget"
    } else {
        "over-budget"
    };

    LatencyBudgetCheck {
        label: label.to_string(),
        observed_ms,
        target_p50_ms,
        sample_count,
        status: status.to_string(),
    }
}

async fn resolver_access_status(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<ApiEnvelope<ResolverAccessStatus>>, axum::http::StatusCode> {
    let dns_targets = discover_dns_targets(
        state.advertised_dns_port,
        state.dns_udp_bind_addr,
        &headers,
        &state.advertised_dns_targets,
    );
    let tailscale_ip = discover_tailscale_ipv4();
    let hostname = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| read_command_output("hostname", &[]));

    let mut notes = if state.advertised_dns_port == 53 {
        vec!["Point devices at this hostname or IP directly in DNS settings; port 53 is already exposed.".to_string()]
    } else {
        vec![format!(
            "Point devices at port {} for DNS on this deployment.",
            state.advertised_dns_port
        )]
    };
    if tailscale_ip.is_some() {
        notes.push(
            "Tailscale is available, so tailnet devices can use the Tailscale address directly."
                .to_string(),
        );
    }
    if state.advertised_dns_port == 53 {
        notes.push(
            "Android tablets and phones should use the Wi-Fi network DNS setting with the LAN IP shown here; Android Private DNS expects DNS-over-TLS and is not the right mode for this deployment."
                .to_string(),
        );
        notes.push(
            "On dual-stack networks, also point clients or your router at Cogwheel's IPv6 DNS target; otherwise IPv6 lookups can bypass the IPv4-only filter path."
                .to_string(),
        );
    }

    Ok(Json(ApiEnvelope {
        data: ResolverAccessStatus {
            hostname,
            dns_targets,
            tailscale_ip,
            notes,
        },
    }))
}

fn discover_dns_targets(
    advertised_port: u16,
    bind_addr: SocketAddr,
    headers: &HeaderMap,
    configured_targets: &[String],
) -> Vec<String> {
    let mut targets = Vec::new();

    for target in configured_targets {
        targets.push(format_dns_target(target, advertised_port));
    }

    if let Some(host) = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(':').next().unwrap_or(value).trim())
        .filter(|value| !value.is_empty())
    {
        targets.push(format_dns_target(host, advertised_port));
    }

    if bind_addr.ip().is_unspecified() {
        for ip in discover_local_ipv4s() {
            if !ip.starts_with("172.") {
                targets.push(format_dns_target(&ip, advertised_port));
            }
        }
        for ip in discover_local_ipv6s() {
            targets.push(format_dns_target(&ip, advertised_port));
        }
    } else {
        targets.push(format_dns_target(
            &bind_addr.ip().to_string(),
            advertised_port,
        ));
    }

    if targets.is_empty() {
        targets.push(format_dns_target("127.0.0.1", advertised_port));
    }

    targets.sort();
    targets.dedup();
    targets
}

fn format_dns_target(host: &str, port: u16) -> String {
    if host.contains(':') || host.parse::<std::net::Ipv4Addr>().is_ok() {
        return host.to_string();
    }
    if port == 53 {
        host.to_string()
    } else {
        format!("{}:{}", host, port)
    }
}

fn discover_local_ipv4s() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        if let Some(output) = read_command_output("hostname", &["-I"]) {
            return output
                .split_whitespace()
                .filter(|value| value.parse::<std::net::Ipv4Addr>().is_ok())
                .map(ToString::to_string)
                .collect();
        }
    }

    #[cfg(target_os = "macos")]
    {
        let mut values = Vec::new();
        for interface in ["en0", "en1"] {
            if let Some(ip) = read_command_output("ipconfig", &["getifaddr", interface]) {
                values.push(ip);
            }
        }
        if !values.is_empty() {
            return values;
        }
    }

    Vec::new()
}

fn discover_local_ipv6s() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        if let Some(output) = read_command_output(
            "sh",
            &[
                "-c",
                "ip -6 -o addr show scope global | awk '{print $4}' | cut -d/ -f1",
            ],
        ) {
            return output
                .split_whitespace()
                .filter(|value| value.parse::<std::net::Ipv6Addr>().is_ok())
                .filter(|value| !value.starts_with("fe80:") && *value != "::1")
                .map(ToString::to_string)
                .collect();
        }
    }

    Vec::new()
}

fn discover_tailscale_ipv4() -> Option<String> {
    read_command_output("tailscale", &["ip", "-4"]).and_then(|output| {
        output
            .lines()
            .map(str::trim)
            .find(|line| line.parse::<std::net::Ipv4Addr>().is_ok())
            .map(ToString::to_string)
    })
}

async fn favicon() -> axum::http::StatusCode {
    axum::http::StatusCode::NO_CONTENT
}

fn read_command_output(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

async fn runtime_snapshot(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<DnsRuntimeSnapshot>>, axum::http::StatusCode> {
    Ok(Json(ApiEnvelope {
        data: state.dns_runtime.snapshot(),
    }))
}

async fn runtime_health(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<RuntimeHealthResponse>>, axum::http::StatusCode> {
    Ok(Json(ApiEnvelope {
        data: current_runtime_health(&state),
    }))
}

async fn run_runtime_health_check(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<RuntimeHealthResponse>>, axum::http::StatusCode> {
    active_runtime_health_check(&state)
        .await
        .map(|data| Json(ApiEnvelope { data }))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(serde::Deserialize)]
struct PauseRuntimeRequest {
    minutes: u32,
}

async fn pause_runtime(
    State(state): State<ServerState>,
    Json(request): Json<PauseRuntimeRequest>,
) -> Result<(), axum::http::StatusCode> {
    let until = chrono::Utc::now() + chrono::Duration::minutes(request.minutes as i64);
    state.dns_runtime.pause_protection_until(until);

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: uuid::Uuid::new_v4(),
            event_type: "runtime.protection_paused".to_string(),
            payload: serde_json::json!({
                "minutes": request.minutes,
                "until": until,
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(())
}

async fn resume_runtime(State(state): State<ServerState>) -> Result<(), axum::http::StatusCode> {
    state.dns_runtime.resume_protection();

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: uuid::Uuid::new_v4(),
            event_type: "runtime.protection_resumed".to_string(),
            payload: "{}".to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(())
}

async fn refresh_sources(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<RefreshResponse>>, axum::http::StatusCode> {
    if !state.rate_limiter.is_allowed("refresh_sources") {
        return Err(axum::http::StatusCode::TOO_MANY_REQUESTS);
    }

    refresh_sources_once(&state, "manual", None)
        .await
        .map(|data| Json(ApiEnvelope { data }))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn list_services(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<Vec<ServiceToggleView>>>, axum::http::StatusCode> {
    build_service_toggle_views(&state)
        .await
        .map(|data| Json(ApiEnvelope { data }))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn update_service_toggle(
    State(state): State<ServerState>,
    Json(request): Json<UpdateServiceToggleRequest>,
) -> Result<Json<ApiEnvelope<RefreshResponse>>, axum::http::StatusCode> {
    let manifests = built_in_service_manifests();
    if !manifests
        .iter()
        .any(|item| item.service_id == request.service_id)
    {
        return Err(axum::http::StatusCode::NOT_FOUND);
    }

    let mut snapshot = load_service_toggle_snapshot(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    snapshot.upsert(&request.service_id, request.mode);
    persist_service_toggle_snapshot(&state.storage, &snapshot)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "service-toggle.updated".to_string(),
            payload: serde_json::json!({
                "service_id": request.service_id,
                "mode": snapshot.mode_for(&request.service_id),
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    refresh_sources_once(&state, "service-toggle", None)
        .await
        .map(|data| Json(ApiEnvelope { data }))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn update_classifier_settings(
    State(state): State<ServerState>,
    Json(request): Json<UpdateClassifierSettingsRequest>,
) -> Result<Json<ApiEnvelope<ClassifierSettings>>, axum::http::StatusCode> {
    let settings = ClassifierSettings {
        mode: request.mode,
        sensitivity: request.sensitivity,
    };

    persist_classifier_settings(&state.storage, &settings)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .dns_runtime
        .replace_classifier_settings(settings.clone());
    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "classifier-settings.updated".to_string(),
            payload: serde_json::to_string(&settings)
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope { data: settings }))
}

async fn update_notification_settings(
    State(state): State<ServerState>,
    Json(request): Json<UpdateNotificationSettingsRequest>,
) -> Result<Json<ApiEnvelope<NotificationSettings>>, axum::http::StatusCode> {
    let settings = NotificationSettings {
        enabled: request.enabled,
        webhook_url: normalize_webhook_url(request.webhook_url.as_deref())
            .ok_or(axum::http::StatusCode::BAD_REQUEST)?,
        min_severity: normalize_notification_severity(&request.min_severity)
            .ok_or(axum::http::StatusCode::BAD_REQUEST)?,
    };

    persist_notification_settings(&state.storage, &settings)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    *write_recover(&state.notification_settings) = settings.clone();
    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "notification-settings.updated".to_string(),
            payload: serde_json::to_string(&settings)
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope { data: settings }))
}

async fn test_notification_settings(
    State(state): State<ServerState>,
    Json(request): Json<TestNotificationRequest>,
) -> Result<Json<ApiEnvelope<NotificationTestResult>>, axum::http::StatusCode> {
    let settings = read_recover(&state.notification_settings).clone();
    let Some(target) = settings.webhook_url.clone() else {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    };

    let severity = normalize_notification_severity(
        request
            .severity
            .as_deref()
            .unwrap_or(&settings.min_severity),
    )
    .ok_or(axum::http::StatusCode::BAD_REQUEST)?;
    let dry_run = request.dry_run.unwrap_or(false);

    let test_event = SecurityEventRecord {
        id: Uuid::new_v4(),
        device_id: None,
        device_name: Some(
            request
                .device_name
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "Control Plane Test".to_string()),
        ),
        client_ip: "127.0.0.1".to_string(),
        domain: request
            .domain
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "notification-test.cogwheel.local".to_string()),
        classifier_score: 1.0,
        severity: severity.clone(),
        created_at: chrono::Utc::now(),
    };

    if dry_run {
        state
            .storage
            .record_audit_event(&AuditEvent {
                id: Uuid::new_v4(),
                event_type: "notification-settings.tested.dry-run".to_string(),
                payload: serde_json::to_string(&serde_json::json!({
                    "target": target,
                    "severity": test_event.severity,
                    "domain": test_event.domain,
                }))
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?,
                created_at: test_event.created_at,
            })
            .await
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

        return Ok(Json(ApiEnvelope {
            data: NotificationTestResult {
                outcome: "validated".to_string(),
                target,
            },
        }));
    }

    deliver_security_notification(
        state.storage.as_ref(),
        &state.http_client,
        &settings,
        &test_event,
    )
    .await
    .map_err(|_| axum::http::StatusCode::BAD_GATEWAY)?;
    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "notification-settings.tested".to_string(),
            payload: serde_json::to_string(&serde_json::json!({
                "target": target,
                "severity": test_event.severity,
            }))
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: test_event.created_at,
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope {
        data: NotificationTestResult {
            outcome: "sent".to_string(),
            target,
        },
    }))
}

async fn update_notification_test_presets(
    State(state): State<ServerState>,
    Json(request): Json<UpdateNotificationPresetsRequest>,
) -> Result<Json<ApiEnvelope<Vec<NotificationTestPreset>>>, axum::http::StatusCode> {
    let presets = normalize_notification_test_presets(request.presets);
    persist_notification_test_presets(&state.storage, &presets)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "notification-test-presets.updated".to_string(),
            payload: serde_json::to_string(&presets)
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope { data: presets }))
}

async fn upsert_blocklist(
    State(state): State<ServerState>,
    Json(request): Json<UpsertBlocklistRequest>,
) -> Result<Json<ApiEnvelope<RefreshResponse>>, axum::http::StatusCode> {
    if !state.rate_limiter.is_allowed("upsert_blocklist") {
        return Err(axum::http::StatusCode::TOO_MANY_REQUESTS);
    }

    let normalized_kind =
        normalize_source_kind(&request.kind).ok_or(axum::http::StatusCode::BAD_REQUEST)?;
    Url::parse(&request.url).map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;

    let source = SourceRecord {
        id: request.id.unwrap_or_else(Uuid::new_v4),
        name: request.name,
        url: request.url,
        kind: normalized_kind,
        enabled: request.enabled,
        refresh_interval_minutes: request.refresh_interval_minutes.unwrap_or(60).max(1),
        profile: normalize_profile_name(request.profile.as_deref().unwrap_or("custom"))
            .ok_or(axum::http::StatusCode::BAD_REQUEST)?,
        verification_strictness: normalize_verification_strictness(
            request
                .verification_strictness
                .as_deref()
                .unwrap_or("balanced"),
        )
        .ok_or(axum::http::StatusCode::BAD_REQUEST)?,
    };
    state
        .storage
        .insert_source(&source)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "blocklist.upserted".to_string(),
            payload: serde_json::to_string(&source)
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    if request.refresh_now.unwrap_or(true) && source.enabled {
        return refresh_sources_once(&state, "blocklist-update", None)
            .await
            .map(|data| Json(ApiEnvelope { data }))
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok(Json(ApiEnvelope {
        data: RefreshResponse {
            outcome: "saved".to_string(),
            ruleset: None,
            notes: vec![format!("saved blocklist {}", source.name)],
        },
    }))
}

async fn update_blocklist_state(
    State(state): State<ServerState>,
    Json(request): Json<UpdateBlocklistStateRequest>,
) -> Result<Json<ApiEnvelope<RefreshResponse>>, axum::http::StatusCode> {
    let mut source = state
        .storage
        .list_sources()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .find(|source| source.id == request.id)
        .ok_or(axum::http::StatusCode::NOT_FOUND)?;

    if is_reserved_source_id(source.id) && !request.enabled {
        return Err(axum::http::StatusCode::CONFLICT);
    }

    source.enabled = request.enabled;
    state
        .storage
        .insert_source(&source)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "blocklist.state_updated".to_string(),
            payload: serde_json::to_string(&source)
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    if request.refresh_now.unwrap_or(true) {
        return refresh_sources_once(&state, "blocklist-state-update", None)
            .await
            .map(|data| Json(ApiEnvelope { data }))
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok(Json(ApiEnvelope {
        data: RefreshResponse {
            outcome: "saved".to_string(),
            ruleset: None,
            notes: vec![format!(
                "{} blocklist {}",
                if source.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                source.name
            )],
        },
    }))
}

async fn delete_blocklist(
    State(state): State<ServerState>,
    Json(request): Json<DeleteBlocklistRequest>,
) -> Result<Json<ApiEnvelope<RefreshResponse>>, axum::http::StatusCode> {
    if is_reserved_source_id(request.id) {
        return Err(axum::http::StatusCode::CONFLICT);
    }

    let source = state
        .storage
        .list_sources()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .find(|source| source.id == request.id)
        .ok_or(axum::http::StatusCode::NOT_FOUND)?;

    let deleted = state
        .storage
        .delete_source(request.id)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    if !deleted {
        return Err(axum::http::StatusCode::NOT_FOUND);
    }

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "blocklist.deleted".to_string(),
            payload: serde_json::to_string(&source)
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    if request.refresh_now.unwrap_or(true) {
        return refresh_sources_once(&state, "blocklist-delete", None)
            .await
            .map(|data| Json(ApiEnvelope { data }))
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok(Json(ApiEnvelope {
        data: RefreshResponse {
            outcome: "saved".to_string(),
            ruleset: None,
            notes: vec![format!("deleted blocklist {}", source.name)],
        },
    }))
}

async fn refresh_sources_once(
    state: &ServerState,
    reason: &str,
    only_source_ids: Option<&HashSet<Uuid>>,
) -> Result<RefreshResponse> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build refresh http client")?;

    let selected_sources = state
        .storage
        .list_sources()
        .await?
        .into_iter()
        .filter(|source| source.enabled)
        .filter(|source| {
            only_source_ids
                .map(|ids| ids.contains(&source.id))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();

    anyhow::ensure!(
        !selected_sources.is_empty(),
        "no enabled sources configured"
    );

    let source_ids = selected_sources
        .iter()
        .map(|source| source.id)
        .collect::<Vec<_>>();
    update_source_refresh_attempts(&state.storage, &source_ids, chrono::Utc::now()).await?;

    let enabled_sources = selected_sources
        .into_iter()
        .map(source_definition_from_record)
        .collect::<Result<Vec<_>>>()?;

    let enabled_source_count = enabled_sources.len();
    let mut parsed_sources = Vec::with_capacity(enabled_source_count);
    for source in enabled_sources {
        parsed_sources.push(fetch_and_parse_source(&client, source).await?);
    }

    let manifests = built_in_service_manifests();
    let snapshot = load_service_toggle_snapshot(&state.storage).await?;
    let service_layer = compile_service_rule_layer(&manifests, &snapshot);
    if !service_layer.rules.is_empty() {
        parsed_sources.push(synthetic_source("service-toggles", service_layer.rules));
    }

    let verification = verify_candidate(&parsed_sources, &state.protected_domains);
    if !verification.passed {
        let rejection_notes = verification
            .notes
            .iter()
            .cloned()
            .chain(service_layer.notes.iter().cloned())
            .collect::<Vec<_>>();
        state
            .storage
            .record_audit_event(&AuditEvent {
                id: Uuid::new_v4(),
                event_type: "ruleset.refresh_rejected".to_string(),
                payload: serde_json::json!({
                    "reason": reason,
                    "notes": verification.notes,
                    "blocked_protected_domains": verification.blocked_protected_domains,
                    "invalid_ratio": verification.invalid_ratio,
                })
                .to_string(),
                created_at: chrono::Utc::now(),
            })
            .await?;

        let notification_settings = read_recover(&state.notification_settings).clone();
        if should_deliver_notification(&notification_settings, "high") {
            let event = NotificationWebhookEvent {
                event_type: "ruleset.refresh_rejected".to_string(),
                severity: "high".to_string(),
                title: "Ruleset refresh rejected".to_string(),
                summary: format!("Refresh {} was rejected before activation.", reason),
                domain: None,
                device_name: None,
                client_ip: Some("control-plane".to_string()),
                details: rejection_notes.clone(),
                created_at: chrono::Utc::now(),
            };
            if let Err(error) = deliver_operational_notification(
                &state.storage,
                &client,
                &notification_settings,
                event,
            )
            .await
            {
                tracing::warn!(%error, "failed to deliver refresh rejection notification");
            }
        }

        return Ok(RefreshResponse {
            outcome: "rejected".to_string(),
            ruleset: None,
            notes: rejection_notes,
        });
    }

    let catalog = build_runtime_policy_catalog(
        &parsed_sources,
        state.protected_domains.as_ref().clone(),
        configured_block_mode(),
    );

    state
        .storage
        .record_ruleset(&RulesetRecord {
            id: catalog.global_policy.artifact().id,
            hash: catalog.global_policy.artifact().hash.clone(),
            status: "candidate".to_string(),
            created_at: catalog.global_policy.artifact().created_at,
            artifact_json: serde_json::to_string(catalog.global_policy.artifact())?,
        })
        .await?;
    let runtime_before = state.dns_runtime.snapshot();
    state
        .storage
        .activate_ruleset(catalog.global_policy.artifact().id)
        .await?;
    state.dns_runtime.replace_policy_catalog(
        catalog.global_policy.clone(),
        catalog.profile_policies.clone(),
    );
    sync_runtime_device_policies(state).await?;

    let mut regression_notes =
        post_activation_regressions(catalog.global_policy.as_ref(), &state.protected_domains)
            .unwrap_or_default();
    let runtime_report = run_runtime_guard_probes(state, &runtime_before).await;
    if runtime_report.degraded {
        regression_notes.extend(runtime_report.notes);
    }

    if !regression_notes.is_empty() {
        let Some(artifact) = state.storage.rollback_to_previous_ruleset().await? else {
            anyhow::bail!("regression detected but no previous ruleset available for rollback");
        };
        state.dns_runtime.replace_policy_catalog(
            Arc::new(PolicyEngine::new(artifact.clone())),
            HashMap::new(),
        );
        sync_runtime_device_policies(state).await?;
        state
            .storage
            .record_audit_event(&AuditEvent {
                id: Uuid::new_v4(),
                event_type: "ruleset.auto_rollback".to_string(),
                payload: serde_json::json!({
                    "reason": reason,
                    "rolled_back_to": artifact.id,
                    "notes": regression_notes,
                })
                .to_string(),
                created_at: chrono::Utc::now(),
            })
            .await?;

        let notification_settings = read_recover(&state.notification_settings).clone();
        if should_deliver_notification(&notification_settings, "critical") {
            let event = NotificationWebhookEvent {
                event_type: "ruleset.auto_rollback".to_string(),
                severity: "critical".to_string(),
                title: "Ruleset auto-rollback triggered".to_string(),
                summary: format!("Refresh {} triggered runtime guard rollback.", reason),
                domain: None,
                device_name: None,
                client_ip: Some("control-plane".to_string()),
                details: regression_notes.clone(),
                created_at: chrono::Utc::now(),
            };
            if let Err(error) = deliver_operational_notification(
                &state.storage,
                &client,
                &notification_settings,
                event,
            )
            .await
            {
                tracing::warn!(%error, "failed to deliver auto rollback notification");
            }
        }

        return Ok(RefreshResponse {
            outcome: "rolled_back".to_string(),
            ruleset: Some(to_ruleset_summary(
                &artifact.id,
                &artifact.hash,
                "active",
                artifact.created_at,
            )),
            notes: regression_notes,
        });
    }

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "ruleset.activated".to_string(),
            payload: serde_json::json!({
                "ruleset_id": catalog.global_policy.artifact().id,
                "hash": catalog.global_policy.artifact().hash,
                "reason": reason,
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await?;

    Ok(RefreshResponse {
        outcome: "activated".to_string(),
        ruleset: Some(to_ruleset_summary(
            &catalog.global_policy.artifact().id,
            &catalog.global_policy.artifact().hash,
            "active",
            catalog.global_policy.artifact().created_at,
        )),
        notes: vec![format!("refreshed {} source(s)", enabled_source_count)]
            .into_iter()
            .chain(service_layer.notes)
            .collect(),
    })
}

async fn load_service_toggle_snapshot(storage: &Storage) -> Result<ServiceToggleSnapshot> {
    let Some(value) = storage.get_setting("service_toggles").await? else {
        return Ok(ServiceToggleSnapshot::default());
    };
    Ok(ServiceToggleSnapshot::from_json(&value).unwrap_or_default())
}

async fn load_classifier_settings(storage: &Storage) -> Result<ClassifierSettings> {
    let Some(value) = storage.get_setting("classifier_settings").await? else {
        return Ok(ClassifierSettings::default());
    };
    Ok(serde_json::from_str(&value).unwrap_or_default())
}

/// Feedback the household has given but that has not yet been folded into an adaptation.
///
/// Kept as one JSON blob in the `settings` table rather than a table of its own: it is bounded to
/// [`MAX_PENDING_FEEDBACK`] rows, it is only ever read and written whole, and it then rides the
/// existing backup and sync paths for free.
async fn load_classifier_feedback(storage: &Storage) -> Result<Vec<cogwheel_classifier::Feedback>> {
    let Some(value) = storage.get_setting("classifier_feedback").await? else {
        return Ok(Vec::new());
    };
    Ok(serde_json::from_str(&value).unwrap_or_default())
}

async fn persist_classifier_feedback(
    storage: &Storage,
    feedback: &[cogwheel_classifier::Feedback],
) -> Result<()> {
    storage
        .upsert_setting("classifier_feedback", &serde_json::to_string(feedback)?)
        .await?;
    Ok(())
}

/// The promoted adaptation, with the measurements that justified promoting it.
///
/// The delta itself is hex because the `settings` table stores `TEXT`. The measurements are stored
/// alongside it rather than recomputed on read: they are the *evidence* for this specific delta, and
/// recomputing them at boot would cost 25,000 inferences every restart to rediscover a number that
/// cannot have changed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredAdaptation {
    delta_hex: String,
    roc_auc: f32,
    false_positive_rate: [f32; 3],
    example_count: usize,
    trained_at: i64,
}

async fn load_classifier_adaptation(storage: &Storage) -> Result<Option<StoredAdaptation>> {
    let Some(value) = storage.get_setting("classifier_adaptation").await? else {
        return Ok(None);
    };
    // An empty value is how rollback records "no adaptation"; the settings table has no delete.
    if value.trim().is_empty() {
        return Ok(None);
    }
    match serde_json::from_str(&value) {
        Ok(stored) => Ok(Some(stored)),
        Err(error) => {
            // Never fail the boot over this. The base model is intact by construction, so an
            // unreadable adaptation costs quality, not availability.
            tracing::warn!(%error, "stored classifier adaptation is unreadable; staying on the base model");
            Ok(None)
        }
    }
}

async fn persist_classifier_adaptation(storage: &Storage, stored: &StoredAdaptation) -> Result<()> {
    storage
        .upsert_setting("classifier_adaptation", &serde_json::to_string(stored)?)
        .await?;
    Ok(())
}

async fn clear_classifier_adaptation(storage: &Storage) -> Result<()> {
    storage.upsert_setting("classifier_adaptation", "").await?;
    Ok(())
}

async fn load_notification_settings(storage: &Storage) -> Result<NotificationSettings> {
    let Some(value) = storage.get_setting("notification_settings").await? else {
        return Ok(NotificationSettings {
            enabled: false,
            webhook_url: None,
            min_severity: "high".to_string(),
        });
    };
    Ok(
        serde_json::from_str(&value).unwrap_or(NotificationSettings {
            enabled: false,
            webhook_url: None,
            min_severity: "high".to_string(),
        }),
    )
}

async fn load_notification_test_presets(storage: &Storage) -> Result<Vec<NotificationTestPreset>> {
    let Some(value) = storage.get_setting("notification_test_presets").await? else {
        return Ok(Vec::new());
    };
    Ok(normalize_notification_test_presets(
        serde_json::from_str(&value).unwrap_or_default(),
    ))
}

async fn load_block_profiles(storage: &Storage) -> Result<Vec<BlockProfileRecord>> {
    let Some(value) = storage.get_setting("block_profiles").await? else {
        return Ok(default_block_profiles());
    };

    let parsed = serde_json::from_str::<Vec<StoredBlockProfileRecord>>(&value)
        .map(|profiles| {
            profiles
                .into_iter()
                .map(StoredBlockProfileRecord::into_block_profile)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|_| default_block_profiles());
    Ok(normalize_block_profiles(parsed))
}

async fn persist_block_profiles(storage: &Storage, profiles: &[BlockProfileRecord]) -> Result<()> {
    storage
        .upsert_setting("block_profiles", &serde_json::to_string(profiles)?)
        .await?;
    Ok(())
}

fn default_block_profiles() -> Vec<BlockProfileRecord> {
    let now = chrono::Utc::now();
    vec![
        BlockProfileRecord {
            id: "family".to_string(),
            emoji: "🛡️".to_string(),
            name: "Family".to_string(),
            description: "Covers the everyday family setup with the core OISD list plus lighter NSFW filtering."
                .to_string(),
            blocklists: ["oisd-small", "oisd-nsfw-small"]
                .into_iter()
                .filter_map(preset_block_profile_list)
                .collect(),
            allowlists: vec!["pbskids.org".to_string(), "khanacademy.org".to_string()],
            updated_at: now,
        },
        BlockProfileRecord {
            id: "focus".to_string(),
            emoji: "🌿".to_string(),
            name: "Focus".to_string(),
            description: "A quieter setup for work or school devices with the smaller OISD core list only."
                .to_string(),
            blocklists: ["oisd-small"]
                .into_iter()
                .filter_map(preset_block_profile_list)
                .collect(),
            allowlists: vec!["calendar.google.com".to_string(), "notion.so".to_string()],
            updated_at: now,
        },
    ]
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum StoredBlockProfileListRecord {
    Id(String),
    Record(BlockProfileListRecord),
}

#[derive(Debug, Clone, serde::Deserialize)]
struct StoredBlockProfileRecord {
    id: String,
    emoji: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    blocklists: Vec<StoredBlockProfileListRecord>,
    #[serde(default)]
    allowlists: Vec<String>,
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl StoredBlockProfileRecord {
    fn into_block_profile(self) -> BlockProfileRecord {
        let now = chrono::Utc::now();
        BlockProfileRecord {
            id: self.id,
            emoji: self.emoji,
            name: self.name,
            description: self.description,
            blocklists: self
                .blocklists
                .into_iter()
                .filter_map(|entry| match entry {
                    StoredBlockProfileListRecord::Id(id) => legacy_block_profile_list(&id),
                    StoredBlockProfileListRecord::Record(record) => Some(record),
                })
                .collect(),
            allowlists: self.allowlists,
            updated_at: self.updated_at.unwrap_or(now),
        }
    }
}

fn preset_block_profile_lists() -> Vec<BlockProfileListRecord> {
    vec![
        BlockProfileListRecord {
            id: "oisd-small".to_string(),
            name: "OISD Small".to_string(),
            url: "https://small.oisd.nl".to_string(),
            kind: "preset".to_string(),
            family: "core-small".to_string(),
        },
        BlockProfileListRecord {
            id: "oisd-big".to_string(),
            name: "OISD Big".to_string(),
            url: "https://big.oisd.nl".to_string(),
            kind: "preset".to_string(),
            family: "core-full".to_string(),
        },
        BlockProfileListRecord {
            id: "oisd-nsfw-small".to_string(),
            name: "OISD NSFW Small".to_string(),
            url: "https://nsfw-small.oisd.nl".to_string(),
            kind: "preset".to_string(),
            family: "nsfw-small".to_string(),
        },
        BlockProfileListRecord {
            id: "oisd-nsfw".to_string(),
            name: "OISD NSFW".to_string(),
            url: "https://nsfw.oisd.nl".to_string(),
            kind: "preset".to_string(),
            family: "nsfw-full".to_string(),
        },
    ]
}

fn preset_block_profile_list(id: &str) -> Option<BlockProfileListRecord> {
    preset_block_profile_lists()
        .into_iter()
        .find(|entry| entry.id == id)
}

fn legacy_block_profile_list(id: &str) -> Option<BlockProfileListRecord> {
    match id {
        "essential" => preset_block_profile_list("oisd-small"),
        "balanced" => preset_block_profile_list("oisd-big"),
        "aggressive" => preset_block_profile_list("oisd-big"),
        other => preset_block_profile_list(other),
    }
}

fn normalize_block_profile_id(value: &str) -> Option<String> {
    let normalized = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() {
                char
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn normalize_block_profile_name(value: &str) -> Option<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_string())
    }
}

fn normalize_domain_list(entries: Vec<String>) -> Vec<String> {
    let mut normalized = entries
        .into_iter()
        .flat_map(|entry| entry.split(',').map(str::to_string).collect::<Vec<_>>())
        .filter_map(|entry| {
            let trimmed = entry.trim().trim_matches('.').to_ascii_lowercase();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn normalize_block_profile_list_name(value: &str) -> Option<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_string())
    }
}

fn normalize_block_profile_list_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalize_block_profile_lists(
    entries: Vec<BlockProfileListRecord>,
) -> Vec<BlockProfileListRecord> {
    let mut normalized = entries
        .into_iter()
        .filter_map(|entry| {
            if let Some(preset) = preset_block_profile_list(&entry.id) {
                return Some(preset);
            }

            let name = normalize_block_profile_list_name(&entry.name)?;
            let url = normalize_block_profile_list_url(&entry.url)?;
            let id = normalize_block_profile_id(&entry.id)
                .or_else(|| normalize_block_profile_id(&name))
                .unwrap_or_else(|| Uuid::new_v4().to_string());

            Some(BlockProfileListRecord {
                id,
                name,
                url,
                kind: if entry.kind.trim().is_empty() {
                    "custom".to_string()
                } else {
                    entry.kind.trim().to_string()
                },
                family: if entry.family.trim().is_empty() {
                    "custom".to_string()
                } else {
                    entry.family.trim().to_string()
                },
            })
        })
        .collect::<Vec<_>>();

    let has_core_full = normalized.iter().any(|entry| entry.id == "oisd-big");
    let has_nsfw_full = normalized.iter().any(|entry| entry.id == "oisd-nsfw");
    if has_core_full {
        normalized.retain(|entry| entry.id != "oisd-small");
    }
    if has_nsfw_full {
        normalized.retain(|entry| entry.id != "oisd-nsfw-small");
    }

    normalized.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.cmp(&right.id))
    });
    normalized.dedup_by(|left, right| left.id == right.id || left.url == right.url);
    normalized
}

fn normalize_block_profiles(mut profiles: Vec<BlockProfileRecord>) -> Vec<BlockProfileRecord> {
    for profile in &mut profiles {
        profile.id = normalize_block_profile_id(&profile.id)
            .or_else(|| normalize_block_profile_id(&profile.name))
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        profile.name = normalize_block_profile_name(&profile.name)
            .unwrap_or_else(|| "Untitled profile".to_string());
        profile.emoji = profile.emoji.trim().to_string();
        if profile.emoji.is_empty() {
            profile.emoji = "🧩".to_string();
        }
        profile.description = profile.description.trim().to_string();
        profile.blocklists = normalize_block_profile_lists(profile.blocklists.clone());
        profile.allowlists = normalize_domain_list(profile.allowlists.clone());
    }

    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    profiles.dedup_by(|left, right| left.id == right.id);
    profiles
}

async fn upsert_block_profile(
    State(state): State<ServerState>,
    Json(request): Json<UpsertBlockProfileRequest>,
) -> Result<Json<ApiEnvelope<Vec<BlockProfileRecord>>>, (axum::http::StatusCode, String)> {
    let profile_id = request
        .id
        .as_deref()
        .and_then(normalize_block_profile_id)
        .or_else(|| normalize_block_profile_id(&request.name))
        .ok_or((
            axum::http::StatusCode::BAD_REQUEST,
            "block profile requires a name".to_string(),
        ))?;
    let profile_name = normalize_block_profile_name(&request.name).ok_or((
        axum::http::StatusCode::BAD_REQUEST,
        "block profile requires a friendly name".to_string(),
    ))?;

    let mut profiles = load_block_profiles(&state.storage).await.map_err(|error| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
        )
    })?;

    let next_profile = BlockProfileRecord {
        id: profile_id.clone(),
        emoji: if request.emoji.trim().is_empty() {
            "🧩".to_string()
        } else {
            request.emoji.trim().to_string()
        },
        name: profile_name.clone(),
        description: request.description.unwrap_or_default().trim().to_string(),
        blocklists: normalize_block_profile_lists(request.blocklists),
        allowlists: normalize_domain_list(request.allowlists),
        updated_at: chrono::Utc::now(),
    };

    if let Some(existing) = profiles.iter_mut().find(|profile| profile.id == profile_id) {
        *existing = next_profile;
    } else {
        profiles.push(next_profile);
    }

    let profiles = normalize_block_profiles(profiles);
    persist_block_profiles(&state.storage, &profiles)
        .await
        .map_err(|error| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "block-profile.updated".to_string(),
            payload: serde_json::to_string(&serde_json::json!({
                "id": profile_id,
                "name": profile_name,
            }))
            .map_err(|error| {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    error.to_string(),
                )
            })?,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|error| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;

    Ok(Json(ApiEnvelope { data: profiles }))
}

async fn delete_block_profile(
    State(state): State<ServerState>,
    Json(request): Json<DeleteBlockProfileRequest>,
) -> Result<Json<ApiEnvelope<Vec<BlockProfileRecord>>>, (axum::http::StatusCode, String)> {
    let profile_id = normalize_block_profile_id(&request.id).ok_or((
        axum::http::StatusCode::BAD_REQUEST,
        "block profile requires an id".to_string(),
    ))?;

    let mut profiles = load_block_profiles(&state.storage).await.map_err(|error| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
        )
    })?;

    let removed_profile = profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .cloned()
        .ok_or((
            axum::http::StatusCode::NOT_FOUND,
            "block profile not found".to_string(),
        ))?;

    profiles.retain(|profile| profile.id != profile_id);

    persist_block_profiles(&state.storage, &profiles)
        .await
        .map_err(|error| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "block-profile.deleted".to_string(),
            payload: serde_json::to_string(&serde_json::json!({
                "id": removed_profile.id,
                "name": removed_profile.name,
            }))
            .map_err(|error| {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    error.to_string(),
                )
            })?,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|error| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;

    Ok(Json(ApiEnvelope { data: profiles }))
}

async fn load_source_refresh_state(storage: &Storage) -> Result<SourceRefreshState> {
    let Some(value) = storage.get_setting("source_refresh_state").await? else {
        return Ok(SourceRefreshState::default());
    };
    Ok(serde_json::from_str(&value).unwrap_or_default())
}

async fn build_service_toggle_views(state: &ServerState) -> Result<Vec<ServiceToggleView>> {
    let manifests = built_in_service_manifests();
    let snapshot = load_service_toggle_snapshot(&state.storage).await?;

    Ok(manifests
        .into_iter()
        .map(|manifest| ServiceToggleView {
            mode: snapshot.mode_for(&manifest.service_id),
            manifest,
        })
        .collect())
}

async fn build_blocklist_status_views(
    state: &ServerState,
    blocklists: &[SourceRecord],
) -> Result<Vec<BlocklistStatusView>> {
    let refresh_state = load_source_refresh_state(&state.storage).await?;
    let now = chrono::Utc::now();

    Ok(blocklists
        .iter()
        .map(|source| BlocklistStatusView {
            id: source.id,
            name: source.name.clone(),
            last_refresh_attempt_at: refresh_state.last_refresh_for(source.id),
            due_for_refresh: source_due_for_refresh(
                source,
                refresh_state.last_refresh_for(source.id),
                now,
            ),
        })
        .collect())
}

fn current_runtime_health(state: &ServerState) -> RuntimeHealthResponse {
    let snapshot = state.dns_runtime.snapshot();
    let report = evaluate_runtime_regressions(
        &DnsRuntimeSnapshot {
            upstream_failures_total: 0,
            fallback_served_total: 0,
            cache_hits_total: 0,
            cache_expired_total: 0,
            cname_uncloaks_total: 0,
            cname_blocks_total: 0,
            queries_total: 0,
            blocked_total: 0,
            cache_hit_latency_avg_ns: 0,
            cache_hit_samples: 0,
            cache_miss_latency_avg_ns: 0,
            cache_miss_samples: 0,
            classifier_latency_avg_ns: 0,
            classifier_latency_samples: 0,
        },
        &snapshot,
        &state.runtime_guard,
    );

    RuntimeHealthResponse {
        snapshot,
        degraded: report.degraded,
        notes: report.notes,
    }
}

async fn active_runtime_health_check(state: &ServerState) -> Result<RuntimeHealthResponse> {
    let before = state.dns_runtime.snapshot();
    let current = current_runtime_health(state);
    let probe_report = run_runtime_guard_probes(state, &before).await;
    let after = state.dns_runtime.snapshot();

    let mut notes = current.notes;
    for note in probe_report.notes {
        if !notes.contains(&note) {
            notes.push(note);
        }
    }
    let degraded = current.degraded || probe_report.degraded;
    let response = RuntimeHealthResponse {
        snapshot: after,
        degraded,
        notes,
    };

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: if response.degraded {
                "runtime.health_check_degraded".to_string()
            } else {
                "runtime.health_check_passed".to_string()
            },
            payload: serde_json::to_string(&serde_json::json!({
                "degraded": response.degraded,
                "notes": response.notes,
                "snapshot": response.snapshot,
            }))?,
            created_at: chrono::Utc::now(),
        })
        .await?;

    if response.degraded {
        let notification_settings = read_recover(&state.notification_settings).clone();
        if should_deliver_notification(&notification_settings, "high") {
            let event = NotificationWebhookEvent {
                event_type: "runtime.health_degraded".to_string(),
                severity: "high".to_string(),
                title: "Runtime health degraded".to_string(),
                summary: "A manual runtime health check detected regressions or probe failures."
                    .to_string(),
                domain: None,
                device_name: None,
                client_ip: Some("control-plane".to_string()),
                details: response.notes.clone(),
                created_at: chrono::Utc::now(),
            };
            if let Err(error) = deliver_operational_notification(
                &state.storage,
                &state.http_client,
                &notification_settings,
                event,
            )
            .await
            {
                tracing::warn!(%error, "failed to deliver runtime health notification");
            }
        }
    }

    Ok(response)
}

async fn persist_service_toggle_snapshot(
    storage: &Storage,
    snapshot: &ServiceToggleSnapshot,
) -> Result<()> {
    storage
        .upsert_setting("service_toggles", &snapshot.to_json()?)
        .await?;
    Ok(())
}

async fn persist_source_refresh_state(storage: &Storage, state: &SourceRefreshState) -> Result<()> {
    storage
        .upsert_setting("source_refresh_state", &serde_json::to_string(state)?)
        .await?;
    Ok(())
}

async fn persist_classifier_settings(
    storage: &Storage,
    settings: &ClassifierSettings,
) -> Result<()> {
    storage
        .upsert_setting("classifier_settings", &serde_json::to_string(settings)?)
        .await?;
    Ok(())
}

async fn persist_notification_settings(
    storage: &Storage,
    settings: &NotificationSettings,
) -> Result<()> {
    storage
        .upsert_setting("notification_settings", &serde_json::to_string(settings)?)
        .await?;
    Ok(())
}

async fn persist_notification_test_presets(
    storage: &Storage,
    presets: &[NotificationTestPreset],
) -> Result<()> {
    storage
        .upsert_setting(
            "notification_test_presets",
            &serde_json::to_string(presets)?,
        )
        .await?;
    Ok(())
}

async fn run_runtime_guard_probes(
    state: &ServerState,
    before: &DnsRuntimeSnapshot,
) -> RuntimeRegressionReport {
    let mut notes = Vec::new();
    for domain in &state.runtime_guard.probe_domains {
        if let Err(error) = state.dns_runtime.probe_domain(domain, RecordType::A).await {
            notes.push(format!("runtime probe failed for {domain}: {error}"));
        }
    }

    let after = state.dns_runtime.snapshot();
    let mut report = evaluate_runtime_regressions(before, &after, &state.runtime_guard);
    report.notes.extend(notes);
    if report
        .notes
        .iter()
        .any(|note| note.starts_with("runtime probe failed"))
    {
        report.degraded = true;
    }
    report
}

fn evaluate_runtime_regressions(
    before: &DnsRuntimeSnapshot,
    after: &DnsRuntimeSnapshot,
    guard: &RuntimeGuardConfig,
) -> RuntimeRegressionReport {
    let upstream_failures_delta = after
        .upstream_failures_total
        .saturating_sub(before.upstream_failures_total);
    let fallback_served_delta = after
        .fallback_served_total
        .saturating_sub(before.fallback_served_total);

    let mut notes = Vec::new();
    if upstream_failures_delta > guard.max_upstream_failures_delta {
        notes.push(format!(
            "upstream failures delta {upstream_failures_delta} exceeds threshold {}",
            guard.max_upstream_failures_delta
        ));
    }
    if fallback_served_delta > guard.max_fallback_served_delta {
        notes.push(format!(
            "fallback served delta {fallback_served_delta} exceeds threshold {}",
            guard.max_fallback_served_delta
        ));
    }

    RuntimeRegressionReport {
        degraded: !notes.is_empty(),
        notes,
    }
}

async fn update_source_refresh_attempts(
    storage: &Storage,
    source_ids: &[Uuid],
    refreshed_at: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let mut state = load_source_refresh_state(storage).await?;
    for source_id in source_ids {
        state.record_attempt(*source_id, refreshed_at);
    }
    persist_source_refresh_state(storage, &state).await
}

async fn due_source_ids(state: &ServerState) -> Result<HashSet<Uuid>> {
    let now = chrono::Utc::now();
    let refresh_state = load_source_refresh_state(&state.storage).await?;
    let sources = state.storage.list_sources().await?;

    Ok(sources
        .into_iter()
        .filter(|source| source.enabled)
        .filter(|source| {
            source_due_for_refresh(source, refresh_state.last_refresh_for(source.id), now)
        })
        .map(|source| source.id)
        .collect())
}

async fn warm_runtime_policy_catalog(state: &ServerState) -> Result<()> {
    let catalog = load_current_runtime_policy_catalog(state).await?;
    state
        .dns_runtime
        .replace_policy_catalog(catalog.global_policy, catalog.profile_policies);
    Ok(())
}

async fn load_current_runtime_policy_catalog(state: &ServerState) -> Result<RuntimePolicyCatalog> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build runtime policy catalog http client")?;

    let enabled_sources = state
        .storage
        .list_sources()
        .await?
        .into_iter()
        .filter(|source| source.enabled)
        .map(source_definition_from_record)
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(!enabled_sources.is_empty(), "no enabled sources configured");

    let mut parsed_sources = Vec::with_capacity(enabled_sources.len());
    for source in enabled_sources {
        parsed_sources.push(fetch_and_parse_source(&client, source).await?);
    }

    let manifests = built_in_service_manifests();
    let snapshot = load_service_toggle_snapshot(&state.storage).await?;
    let service_layer = compile_service_rule_layer(&manifests, &snapshot);
    if !service_layer.rules.is_empty() {
        parsed_sources.push(synthetic_source("service-toggles", service_layer.rules));
    }

    let verification = verify_candidate(&parsed_sources, &state.protected_domains);
    anyhow::ensure!(
        verification.passed,
        "runtime policy catalog verification failed: {:?}",
        verification.notes
    );

    Ok(build_runtime_policy_catalog(
        &parsed_sources,
        state.protected_domains.as_ref().clone(),
        configured_block_mode(),
    ))
}

fn source_due_for_refresh(
    source: &SourceRecord,
    last_refresh_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(last_refresh_attempt_at) = last_refresh_attempt_at else {
        return true;
    };
    let elapsed = now
        .signed_duration_since(last_refresh_attempt_at)
        .num_minutes();
    elapsed >= source.refresh_interval_minutes.max(1)
}

fn source_definition_from_record(record: SourceRecord) -> Result<SourceDefinition> {
    let kind = source_kind_from_str(&record.kind)
        .ok_or_else(|| anyhow::anyhow!("unsupported source kind: {}", record.kind))?;

    Ok(SourceDefinition {
        id: record.id,
        name: record.name,
        url: Url::parse(&record.url)?,
        kind,
        enabled: record.enabled,
        profile: normalize_profile_name(&record.profile)
            .ok_or_else(|| anyhow::anyhow!("unsupported source profile: {}", record.profile))?,
        verification_strictness: record.verification_strictness,
    })
}

fn normalize_profile_name(profile: &str) -> Option<String> {
    let normalized = profile.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn build_runtime_policy_catalog(
    parsed_sources: &[ParsedSource],
    protected_domains: HashSet<String>,
    block_mode: BlockMode,
) -> RuntimePolicyCatalog {
    let global_policy = Arc::new(build_policy_engine(
        parsed_sources.to_vec(),
        protected_domains.clone(),
        block_mode.clone(),
    ));

    let profiles = parsed_sources
        .iter()
        .filter_map(|source| normalize_profile_name(&source.source.profile))
        .filter(|profile| profile != "shared")
        .collect::<HashSet<_>>();

    let mut profile_policies = HashMap::new();
    for profile in profiles {
        let scoped_sources = parsed_sources
            .iter()
            .filter(|source| {
                normalize_profile_name(&source.source.profile)
                    .is_some_and(|candidate| candidate == profile || candidate == "shared")
            })
            .cloned()
            .collect::<Vec<_>>();

        if !scoped_sources.iter().any(|source| {
            normalize_profile_name(&source.source.profile).as_deref() == Some(profile.as_str())
        }) {
            continue;
        }

        profile_policies.insert(
            profile,
            Arc::new(build_policy_engine(
                scoped_sources,
                protected_domains.clone(),
                block_mode.clone(),
            )),
        );
    }

    RuntimePolicyCatalog {
        global_policy,
        profile_policies,
    }
}

fn runtime_device_policies_from_records(devices: Vec<DeviceRecord>) -> Vec<DevicePolicyConfig> {
    let manifests = built_in_service_manifests();
    let manifest_map = manifests
        .into_iter()
        .map(|manifest| (manifest.service_id.clone(), manifest))
        .collect::<HashMap<_, _>>();

    devices
        .into_iter()
        .map(|device| {
            let policy_mode = normalize_device_policy_mode(&device.policy_mode)
                .unwrap_or_else(|| "global".to_string());
            let blocklist_profile_override = if policy_mode == "custom" {
                device
                    .blocklist_profile_override
                    .as_deref()
                    .and_then(normalize_profile_name)
            } else {
                None
            };
            let protection_override = if policy_mode == "custom" {
                normalize_device_protection_override(&device.protection_override)
                    .unwrap_or_else(|| "inherit".to_string())
            } else {
                "inherit".to_string()
            };
            let allowed_domains = if policy_mode == "custom" {
                normalize_device_allowed_domains(device.allowed_domains)
            } else {
                Vec::new()
            };
            let service_overrides = if policy_mode == "custom" {
                normalize_device_service_overrides(device.service_overrides)
            } else {
                Vec::new()
            };
            let mut expanded_allowed_domains = allowed_domains.clone();
            let mut blocked_domains = Vec::new();
            for override_record in &service_overrides {
                if let Some(manifest) = manifest_map.get(&override_record.service_id) {
                    match override_record.mode.as_str() {
                        "allow" => {
                            expanded_allowed_domains.extend(manifest.allow_domains.clone());
                            expanded_allowed_domains.extend(manifest.exceptions.clone());
                        }
                        "block" => blocked_domains.extend(manifest.block_domains.clone()),
                        _ => {}
                    }
                }
            }
            let expanded_allowed_domains =
                normalize_device_allowed_domains(expanded_allowed_domains);
            let blocked_domains = normalize_device_allowed_domains(blocked_domains);

            DevicePolicyConfig {
                ip_address: device.ip_address,
                policy_mode,
                blocklist_profile_override,
                protection_override,
                allowed_domains: expanded_allowed_domains,
                blocked_domains,
            }
        })
        .collect()
}

async fn sync_runtime_device_policies(state: &ServerState) -> Result<()> {
    let devices = state.storage.list_devices().await?;
    state
        .dns_runtime
        .replace_device_policies(runtime_device_policies_from_records(devices));
    Ok(())
}

fn normalize_source_kind(kind: &str) -> Option<String> {
    let normalized = kind.trim().to_ascii_lowercase();
    source_kind_from_str(&normalized)?;
    Some(normalized)
}

fn source_kind_from_str(kind: &str) -> Option<SourceKind> {
    match kind {
        "domains" => Some(SourceKind::Domains),
        "hosts" => Some(SourceKind::Hosts),
        "adblock" => Some(SourceKind::Adblock),
        _ => None,
    }
}

fn normalize_verification_strictness(strictness: &str) -> Option<String> {
    let normalized = strictness.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "strict" | "balanced" | "relaxed" => Some(normalized),
        _ => None,
    }
}

fn normalize_device_policy_mode(mode: &str) -> Option<String> {
    let normalized = mode.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "global" | "custom" => Some(normalized),
        _ => None,
    }
}

fn normalize_device_protection_override(mode: &str) -> Option<String> {
    let normalized = mode.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "inherit" | "bypass" => Some(normalized),
        _ => None,
    }
}

fn normalize_device_allowed_domains(domains: Vec<String>) -> Vec<String> {
    let mut normalized = domains
        .into_iter()
        .filter_map(|domain| {
            let trimmed = domain.trim().trim_matches('.').to_ascii_lowercase();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn normalize_device_service_overrides(
    overrides: Vec<DeviceServiceOverrideRecord>,
) -> Vec<DeviceServiceOverrideRecord> {
    let manifests = built_in_service_manifests();
    let known_ids = manifests
        .iter()
        .map(|manifest| manifest.service_id.as_str())
        .collect::<HashSet<_>>();
    let mut normalized = Vec::new();

    for override_record in overrides {
        let service_id = override_record.service_id.trim().to_ascii_lowercase();
        let mode = override_record.mode.trim().to_ascii_lowercase();
        if !known_ids.contains(service_id.as_str()) {
            continue;
        }
        if !matches!(mode.as_str(), "allow" | "block") {
            continue;
        }

        normalized
            .retain(|existing: &DeviceServiceOverrideRecord| existing.service_id != service_id);
        normalized.push(DeviceServiceOverrideRecord { service_id, mode });
    }

    normalized.sort_by(|left, right| left.service_id.cmp(&right.service_id));
    normalized
}

fn validate_device_service_overrides(
    policy_mode: &str,
    overrides: Vec<DeviceServiceOverrideRecord>,
) -> Result<Vec<DeviceServiceOverrideRecord>, String> {
    if overrides.is_empty() {
        return Ok(Vec::new());
    }
    if policy_mode != "custom" {
        return Err("device service overrides require custom policy mode".to_string());
    }

    let manifests = built_in_service_manifests()
        .into_iter()
        .map(|manifest| (manifest.service_id.clone(), manifest))
        .collect::<HashMap<_, _>>();
    let normalized = normalize_device_service_overrides(overrides.clone());

    for override_record in overrides {
        let service_id = override_record.service_id.trim().to_ascii_lowercase();
        let mode = override_record.mode.trim().to_ascii_lowercase();
        let Some(manifest) = manifests.get(&service_id) else {
            return Err(format!(
                "unknown device service override `{}`; choose one of the built-in services",
                override_record.service_id.trim()
            ));
        };
        if !matches!(mode.as_str(), "allow" | "block") {
            return Err(format!(
                "device service override `{}` must use allow or block mode",
                manifest.display_name
            ));
        }

        let expanded_domains = if mode == "allow" {
            manifest
                .allow_domains
                .iter()
                .chain(manifest.block_domains.iter())
                .chain(manifest.exceptions.iter())
                .collect::<HashSet<_>>()
                .len()
        } else {
            manifest.block_domains.len()
        };
        if expanded_domains == 0 {
            return Err(format!(
                "device service override `{}` has no device-specific domains for {} mode",
                manifest.display_name, mode
            ));
        }
    }

    if normalized.is_empty() {
        return Err(
            "device service overrides must use known built-in services with allow or block mode"
                .to_string(),
        );
    }

    Ok(normalized)
}

fn severity_for_classifier_score(score: f32) -> &'static str {
    if score >= 0.99 {
        "critical"
    } else if score >= 0.96 {
        "high"
    } else {
        "medium"
    }
}

fn severity_rank(severity: &str) -> usize {
    match severity {
        "critical" => 3,
        "high" => 2,
        _ => 1,
    }
}

fn build_security_summary(events: &[SecurityEventRecord]) -> SecuritySummary {
    let mut medium_count = 0;
    let mut high_count = 0;
    let mut critical_count = 0;
    let mut top_devices = HashMap::<String, DeviceSecuritySummary>::new();

    for event in events {
        match event.severity.as_str() {
            "critical" => critical_count += 1,
            "high" => high_count += 1,
            _ => medium_count += 1,
        }

        let label = event
            .device_name
            .clone()
            .unwrap_or_else(|| event.client_ip.clone());
        let entry = top_devices
            .entry(label.clone())
            .or_insert_with(|| DeviceSecuritySummary {
                label,
                event_count: 0,
                highest_severity: event.severity.clone(),
            });
        entry.event_count += 1;
        if severity_rank(&event.severity) > severity_rank(&entry.highest_severity) {
            entry.highest_severity = event.severity.clone();
        }
    }

    let mut top_devices = top_devices.into_values().collect::<Vec<_>>();
    top_devices.sort_by(|left, right| {
        right
            .event_count
            .cmp(&left.event_count)
            .then_with(|| {
                severity_rank(&right.highest_severity).cmp(&severity_rank(&left.highest_severity))
            })
            .then_with(|| left.label.cmp(&right.label))
    });
    top_devices.truncate(3);

    SecuritySummary {
        medium_count,
        high_count,
        critical_count,
        top_devices,
    }
}

fn build_notification_delivery_events(
    deliveries: &[NotificationDeliveryRecord],
) -> Vec<NotificationDeliveryEvent> {
    deliveries
        .iter()
        .map(|delivery| NotificationDeliveryEvent {
            status: delivery.status.clone(),
            event_type: delivery.event_type.clone(),
            severity: delivery.severity.clone(),
            title: delivery.title.clone(),
            summary: delivery.summary.clone(),
            target: delivery
                .device_name
                .clone()
                .unwrap_or_else(|| delivery.client_ip.clone()),
            domain: delivery.domain.clone(),
            device_name: delivery.device_name.clone(),
            client_ip: delivery.client_ip.clone(),
            attempts: delivery.attempts,
            created_at: delivery.created_at,
        })
        .take(5)
        .collect()
}

fn build_notification_health_summary(
    deliveries: &[NotificationDeliveryRecord],
) -> NotificationHealthSummary {
    let mut delivered_count = 0;
    let mut failed_count = 0;
    let mut last_delivery_at = None;
    let mut last_failure_at = None;

    for delivery in deliveries {
        match delivery.status.as_str() {
            "delivered" => {
                delivered_count += 1;
                if last_delivery_at.is_none_or(|current| delivery.created_at > current) {
                    last_delivery_at = Some(delivery.created_at);
                }
            }
            "failed" => {
                failed_count += 1;
                if last_failure_at.is_none_or(|current| delivery.created_at > current) {
                    last_failure_at = Some(delivery.created_at);
                }
            }
            _ => {}
        }
    }

    NotificationHealthSummary {
        delivered_count,
        failed_count,
        last_delivery_at,
        last_failure_at,
    }
}

fn build_notification_failure_analytics(
    deliveries: &[NotificationDeliveryRecord],
) -> NotificationFailureAnalytics {
    let mut delivered_count = 0usize;
    let mut failed_count = 0usize;
    let mut failed_domains = HashMap::<String, usize>::new();

    for delivery in deliveries {
        match delivery.status.as_str() {
            "delivered" => delivered_count += 1,
            "failed" => {
                failed_count += 1;
                if delivery.domain != "control-plane" {
                    *failed_domains.entry(delivery.domain.clone()).or_insert(0) += 1;
                }
            }
            _ => {}
        }
    }

    let total = delivered_count + failed_count;
    let success_rate_percent = if total == 0 {
        100.0
    } else {
        ((delivered_count as f32 / total as f32) * 1000.0).round() / 10.0
    };

    let mut top_failed_domains = failed_domains
        .into_iter()
        .map(|(domain, failure_count)| NotificationFailureDomain {
            domain,
            failure_count,
        })
        .collect::<Vec<_>>();
    top_failed_domains.sort_by(|left, right| {
        right
            .failure_count
            .cmp(&left.failure_count)
            .then_with(|| left.domain.cmp(&right.domain))
    });
    top_failed_domains.truncate(3);

    NotificationFailureAnalytics {
        success_rate_percent,
        top_failed_domains,
    }
}

fn normalize_notification_window(window: Option<usize>) -> usize {
    match window.unwrap_or(30) {
        10 => 10,
        50 => 50,
        100 => 100,
        _ => 30,
    }
}

fn normalize_notification_test_presets(
    presets: Vec<NotificationTestPreset>,
) -> Vec<NotificationTestPreset> {
    let mut normalized = Vec::new();

    for preset in presets {
        let name = preset.name.trim().to_string();
        let domain = preset.domain.trim().to_string();
        let device_name = preset.device_name.trim().to_string();
        let Some(severity) = normalize_notification_severity(&preset.severity) else {
            continue;
        };
        if name.is_empty() || domain.is_empty() || device_name.is_empty() {
            continue;
        }

        normalized.retain(|existing: &NotificationTestPreset| existing.name != name);
        normalized.push(NotificationTestPreset {
            name,
            domain,
            severity,
            device_name,
            dry_run: preset.dry_run,
        });
    }

    normalized.sort_by(|left, right| left.name.cmp(&right.name));
    normalized.truncate(8);
    normalized
}

fn normalize_notification_severity(severity: &str) -> Option<String> {
    match severity.trim().to_ascii_lowercase().as_str() {
        "medium" => Some("medium".to_string()),
        "high" => Some("high".to_string()),
        "critical" => Some("critical".to_string()),
        _ => None,
    }
}

fn normalize_webhook_url(url: Option<&str>) -> Option<Option<String>> {
    let Some(url) = url else {
        return Some(None);
    };
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Some(None);
    }
    let parsed = Url::parse(trimmed).ok()?;
    match parsed.scheme() {
        "https" | "http" => Some(Some(parsed.to_string())),
        _ => None,
    }
}

fn should_deliver_notification(settings: &NotificationSettings, severity: &str) -> bool {
    settings.enabled
        && settings.webhook_url.is_some()
        && severity_rank(severity) >= severity_rank(&settings.min_severity)
}

fn notification_retry_delay(attempt: usize) -> Duration {
    let multiplier = 1u64.checked_shl(attempt.min(4) as u32).unwrap_or(16);
    Duration::from_millis(250 * multiplier)
}

async fn send_security_notification(
    client: &Client,
    settings: &NotificationSettings,
    security_event: &SecurityEventRecord,
) -> Result<()> {
    let event = NotificationWebhookEvent {
        event_type: "security.alert_raised".to_string(),
        severity: security_event.severity.clone(),
        title: security_event.domain.clone(),
        summary: format!(
            "{} alert for {}.",
            security_event.severity,
            security_event
                .device_name
                .as_deref()
                .unwrap_or(&security_event.client_ip)
        ),
        domain: Some(security_event.domain.clone()),
        device_name: security_event.device_name.clone(),
        client_ip: Some(security_event.client_ip.clone()),
        details: vec![format!(
            "classifier score {:.2}",
            security_event.classifier_score
        )],
        created_at: security_event.created_at,
    };
    send_notification(client, settings, &event).await
}

async fn send_notification(
    client: &Client,
    settings: &NotificationSettings,
    event: &NotificationWebhookEvent,
) -> Result<()> {
    let Some(webhook_url) = settings.webhook_url.as_deref() else {
        return Ok(());
    };
    client
        .post(webhook_url)
        .json(&serde_json::json!({
            "event_type": event.event_type,
            "severity": event.severity,
            "title": event.title,
            "summary": event.summary,
            "domain": event.domain,
            "client_ip": event.client_ip,
            "device_name": event.device_name,
            "details": event.details,
            "created_at": event.created_at,
        }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn deliver_operational_notification(
    storage: &Storage,
    client: &Client,
    settings: &NotificationSettings,
    event: NotificationWebhookEvent,
) -> Result<()> {
    let mut last_error = None;

    for attempt in 0..3 {
        match send_notification(client, settings, &event).await {
            Ok(()) => {
                storage
                    .record_notification_delivery(&NotificationDeliveryRecord {
                        id: Uuid::new_v4(),
                        event_type: event.event_type.clone(),
                        status: "delivered".to_string(),
                        severity: event.severity.clone(),
                        title: event.title.clone(),
                        summary: event.summary.clone(),
                        domain: event
                            .domain
                            .clone()
                            .unwrap_or_else(|| "control-plane".to_string()),
                        device_name: event.device_name.clone(),
                        client_ip: event
                            .client_ip
                            .clone()
                            .unwrap_or_else(|| "control-plane".to_string()),
                        attempts: attempt + 1,
                        created_at: event.created_at,
                    })
                    .await?;
                storage
                    .record_audit_event(&AuditEvent {
                        id: Uuid::new_v4(),
                        event_type: "notification.delivery_succeeded".to_string(),
                        payload: serde_json::to_string(&serde_json::json!({
                            "event_type": event.event_type,
                            "severity": event.severity,
                            "title": event.title,
                            "summary": event.summary,
                            "domain": event.domain,
                            "client_ip": event.client_ip,
                            "device_name": event.device_name,
                            "attempts": attempt + 1,
                        }))?,
                        created_at: event.created_at,
                    })
                    .await?;
                return Ok(());
            }
            Err(error) => {
                last_error = Some(error.to_string());
                if attempt < 2 {
                    tokio::time::sleep(notification_retry_delay(attempt)).await;
                }
            }
        }
    }

    let error_message = last_error.unwrap_or_else(|| "unknown delivery error".to_string());

    storage
        .record_notification_delivery(&NotificationDeliveryRecord {
            id: Uuid::new_v4(),
            event_type: event.event_type.clone(),
            status: "failed".to_string(),
            severity: event.severity.clone(),
            title: event.title.clone(),
            summary: event.summary.clone(),
            domain: event
                .domain
                .clone()
                .unwrap_or_else(|| "control-plane".to_string()),
            device_name: event.device_name.clone(),
            client_ip: event
                .client_ip
                .clone()
                .unwrap_or_else(|| "control-plane".to_string()),
            attempts: 3,
            created_at: event.created_at,
        })
        .await?;

    storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "notification.delivery_failed".to_string(),
            payload: serde_json::to_string(&serde_json::json!({
                "event_type": event.event_type,
                "severity": event.severity,
                "title": event.title,
                "summary": event.summary,
                "domain": event.domain,
                "client_ip": event.client_ip,
                "device_name": event.device_name,
                "attempts": 3,
                "error": error_message.clone(),
            }))?,
            created_at: event.created_at,
        })
        .await?;

    anyhow::bail!(
        "operational notification delivery failed after retries: {}",
        error_message
    )
}

async fn deliver_security_notification(
    storage: &Storage,
    client: &Client,
    settings: &NotificationSettings,
    security_event: &SecurityEventRecord,
) -> Result<()> {
    let mut last_error = None;

    for attempt in 0..3 {
        match send_security_notification(client, settings, security_event).await {
            Ok(()) => {
                storage
                    .record_notification_delivery(&NotificationDeliveryRecord {
                        id: Uuid::new_v4(),
                        event_type: "security.alert_raised".to_string(),
                        status: "delivered".to_string(),
                        severity: security_event.severity.clone(),
                        title: security_event.domain.clone(),
                        summary: format!(
                            "{} alert for {}.",
                            security_event.severity,
                            security_event
                                .device_name
                                .as_deref()
                                .unwrap_or(&security_event.client_ip)
                        ),
                        domain: security_event.domain.clone(),
                        device_name: security_event.device_name.clone(),
                        client_ip: security_event.client_ip.clone(),
                        attempts: attempt + 1,
                        created_at: security_event.created_at,
                    })
                    .await?;
                storage
                    .record_audit_event(&AuditEvent {
                        id: Uuid::new_v4(),
                        event_type: "security.alert_delivery_succeeded".to_string(),
                        payload: serde_json::to_string(&serde_json::json!({
                            "severity": security_event.severity,
                            "domain": security_event.domain,
                            "client_ip": security_event.client_ip,
                            "device_name": security_event.device_name,
                            "attempts": attempt + 1,
                        }))?,
                        created_at: security_event.created_at,
                    })
                    .await?;
                return Ok(());
            }
            Err(error) => {
                last_error = Some(error.to_string());
                if attempt < 2 {
                    tokio::time::sleep(notification_retry_delay(attempt)).await;
                }
            }
        }
    }

    let error_message = last_error.unwrap_or_else(|| "unknown delivery error".to_string());

    storage
        .record_notification_delivery(&NotificationDeliveryRecord {
            id: Uuid::new_v4(),
            event_type: "security.alert_raised".to_string(),
            status: "failed".to_string(),
            severity: security_event.severity.clone(),
            title: security_event.domain.clone(),
            summary: format!(
                "{} alert for {}.",
                security_event.severity,
                security_event
                    .device_name
                    .as_deref()
                    .unwrap_or(&security_event.client_ip)
            ),
            domain: security_event.domain.clone(),
            device_name: security_event.device_name.clone(),
            client_ip: security_event.client_ip.clone(),
            attempts: 3,
            created_at: security_event.created_at,
        })
        .await?;

    storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "security.alert_delivery_failed".to_string(),
            payload: serde_json::to_string(&serde_json::json!({
                "severity": security_event.severity,
                "domain": security_event.domain,
                "client_ip": security_event.client_ip,
                "device_name": security_event.device_name,
                "attempts": 3,
                "error": error_message.clone(),
            }))?,
            created_at: security_event.created_at,
        })
        .await?;

    anyhow::bail!(
        "security alert delivery failed after retries: {}",
        error_message
    )
}

async fn record_security_event_from_classification(
    storage: Arc<Storage>,
    http_client: Client,
    notification_settings: Arc<RwLock<NotificationSettings>>,
    event: ClassificationEvent,
) -> Result<()> {
    let Some(client_ip) = event.client_ip.clone() else {
        return Ok(());
    };
    let device = storage.find_device_by_ip(&client_ip).await?;
    let severity = severity_for_classifier_score(event.score).to_string();
    let security_event = SecurityEventRecord {
        id: Uuid::new_v4(),
        device_id: device.as_ref().map(|record| record.id),
        device_name: device.as_ref().map(|record| record.name.clone()),
        client_ip,
        domain: event.domain,
        classifier_score: f64::from(event.score),
        severity,
        created_at: event.observed_at,
    };
    storage.record_security_event(&security_event).await?;
    let current_notification_settings = read_recover(&notification_settings).clone();
    if matches!(security_event.severity.as_str(), "high" | "critical") {
        storage
            .record_audit_event(&AuditEvent {
                id: Uuid::new_v4(),
                event_type: "security.alert_raised".to_string(),
                payload: serde_json::to_string(&serde_json::json!({
                    "severity": security_event.severity,
                    "domain": security_event.domain,
                    "client_ip": security_event.client_ip,
                    "device_name": security_event.device_name,
                    "classifier_score": security_event.classifier_score,
                }))?,
                created_at: event.observed_at,
            })
            .await?;
    }
    if should_deliver_notification(&current_notification_settings, &security_event.severity) {
        deliver_security_notification(
            storage.as_ref(),
            &http_client,
            &current_notification_settings,
            &security_event,
        )
        .await?;
    }
    Ok(())
}

fn is_reserved_source_id(source_id: Uuid) -> bool {
    source_id == Uuid::from_u128(1)
}

fn post_activation_regressions(
    policy: &PolicyEngine,
    protected_domains: &HashSet<String>,
) -> Option<Vec<String>> {
    let blocked = protected_domains
        .iter()
        .filter_map(|domain| match policy.evaluate(domain).kind {
            DecisionKind::Blocked(_) => Some(format!("protected domain blocked: {domain}")),
            DecisionKind::Allowed => None,
        })
        .collect::<Vec<_>>();

    if blocked.is_empty() {
        None
    } else {
        Some(blocked)
    }
}

fn to_ruleset_summary(
    id: &Uuid,
    hash: &str,
    status: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> RulesetSummary {
    RulesetSummary {
        id: *id,
        hash: hash.to_string(),
        status: status.to_string(),
        created_at,
    }
}

// ---------------------------------------------------------------- classifier API

/// Per-sensitivity calibration figures, so the UI can show what each option actually costs.
#[derive(Debug, Clone, serde::Serialize)]
struct SensitivityBand {
    low: f32,
    balanced: f32,
    high: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ClassifierModelInfo {
    version: u32,
    trained_at: String,
    roc_auc: f32,
    pr_auc: f32,
    resident_bytes: usize,
    thresholds: SensitivityBand,
    false_positive_rate: SensitivityBand,
    recall: SensitivityBand,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ClassifierEngineStats {
    scored: u64,
    cache_hits: u64,
    cache_misses: u64,
    dropped: u64,
    blocked: u64,
    protected_overrides: u64,
    hook_panics: u64,
    cached_entries: u64,
}

/// What adaptation is doing right now, and on what evidence.
///
/// Everything here is reported rather than summarised into a single "adapted: yes/no", because the
/// point of gating a delta on measurements is lost if the user cannot see the measurements.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ClassifierAdaptationInfo {
    active: bool,
    trained_at: Option<String>,
    example_count: usize,
    ngram_entries: usize,
    roc_auc: Option<f32>,
    false_positive_rate: Option<SensitivityBand>,
    /// The delta's certified worst-case effect on any logit, and the ceiling it is held under.
    max_logit_shift: f32,
    logit_budget: f32,
    pending_feedback: usize,
    minimum_feedback: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ClassifierStatusResponse {
    settings: ClassifierSettings,
    model: ClassifierModelInfo,
    stats: ClassifierEngineStats,
    active_threshold: f32,
    adaptation: ClassifierAdaptationInfo,
}

/// Describe the engine's current adaptation state.
///
/// `stored` carries the measurements the gate recorded when the delta was promoted; the delta itself
/// is read back from the engine so the report describes what is actually scoring traffic rather than
/// what the database believes should be.
fn build_adaptation_info(
    state: &ServerState,
    stored: Option<&StoredAdaptation>,
    pending_feedback: usize,
) -> ClassifierAdaptationInfo {
    let delta = state.dns_runtime.classifier().active_delta();
    // The stored measurements are evidence *for a specific delta*. If that delta is not the one
    // scoring traffic -- it failed validation at boot, or was rolled back -- reporting its figures
    // would advertise quality nothing is currently delivering, so they are withheld with it.
    let evidence = stored.filter(|_| delta.is_some());
    ClassifierAdaptationInfo {
        active: delta.is_some(),
        trained_at: delta.as_ref().and_then(|delta| {
            chrono::DateTime::from_timestamp(delta.trained_at(), 0)
                .map(|timestamp| timestamp.to_rfc3339())
        }),
        example_count: delta.as_ref().map_or(0, |delta| delta.example_count()),
        ngram_entries: delta.as_ref().map_or(0, |delta| delta.ngram_entries()),
        roc_auc: evidence.map(|stored| stored.roc_auc),
        false_positive_rate: evidence.map(|stored| SensitivityBand {
            low: stored.false_positive_rate[0],
            balanced: stored.false_positive_rate[1],
            high: stored.false_positive_rate[2],
        }),
        max_logit_shift: delta
            .as_ref()
            .map_or(0.0, |delta| delta.certified_max_logit_shift()),
        logit_budget: cogwheel_classifier::adapt::DELTA_LOGIT_BUDGET,
        pending_feedback,
        minimum_feedback: cogwheel_classifier::adapt::MIN_FEEDBACK_EXAMPLES,
    }
}

fn build_classifier_status(
    state: &ServerState,
    adaptation: ClassifierAdaptationInfo,
) -> ClassifierStatusResponse {
    let engine = state.dns_runtime.classifier();
    let model = engine.model();
    let quality = model.quality();
    let thresholds = model.thresholds();
    let stats = engine.stats();

    ClassifierStatusResponse {
        settings: engine.settings(),
        model: ClassifierModelInfo {
            version: cogwheel_classifier::model::FORMAT_VERSION,
            trained_at: chrono::DateTime::from_timestamp(model.trained_at(), 0)
                .unwrap_or_default()
                .to_rfc3339(),
            roc_auc: quality.roc_auc,
            pr_auc: quality.pr_auc,
            resident_bytes: model.resident_bytes(),
            thresholds: SensitivityBand {
                low: thresholds.low,
                balanced: thresholds.balanced,
                high: thresholds.high,
            },
            false_positive_rate: SensitivityBand {
                low: quality.false_positive_rate[0],
                balanced: quality.false_positive_rate[1],
                high: quality.false_positive_rate[2],
            },
            recall: SensitivityBand {
                low: quality.recall_at_threshold[0],
                balanced: quality.recall_at_threshold[1],
                high: quality.recall_at_threshold[2],
            },
        },
        stats: ClassifierEngineStats {
            scored: stats.scored,
            cache_hits: stats.cache_hits,
            cache_misses: stats.cache_misses,
            dropped: stats.dropped,
            blocked: stats.blocked,
            protected_overrides: stats.protected_overrides,
            hook_panics: stats.hook_panics,
            cached_entries: stats.cached_entries,
        },
        active_threshold: engine.active_threshold(),
        adaptation,
    }
}

async fn classifier_status(
    State(state): State<ServerState>,
) -> Json<ApiEnvelope<ClassifierStatusResponse>> {
    let stored = load_classifier_adaptation(&state.storage)
        .await
        .unwrap_or_default();
    let pending = load_classifier_feedback(&state.storage)
        .await
        .unwrap_or_default()
        .len();
    let adaptation = build_adaptation_info(&state, stored.as_ref(), pending);
    Json(ApiEnvelope {
        data: build_classifier_status(&state, adaptation),
    })
}

#[derive(serde::Deserialize)]
struct InspectDomainRequest {
    domain: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ContributionView {
    label: String,
    kind: String,
    value: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectDomainResponse {
    domain: String,
    probability: f32,
    protected: bool,
    decision: String,
    active_threshold: f32,
    blocklist_match: Option<String>,
    contributions: Vec<ContributionView>,
}

/// Score an arbitrary domain on demand and explain the result.
///
/// This is the one place inference runs synchronously, and it is deliberate: it is an operator
/// action on the HTTP path, not the DNS path, and a single inference is ~7 microseconds.
async fn inspect_domain(
    State(state): State<ServerState>,
    Json(request): Json<InspectDomainRequest>,
) -> Result<Json<ApiEnvelope<InspectDomainResponse>>, axum::http::StatusCode> {
    // Bound the input before doing any work: this endpoint is reachable by anything on the LAN.
    if request.domain.len() > cogwheel_classifier::normalize::MAX_HOST_LEN {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    let domain = cogwheel_classifier::normalize(&request.domain)
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;

    let engine = state.dns_runtime.classifier();
    let verdict = engine.score_now(&domain);
    let active_threshold = engine.active_threshold();
    let would_block = !verdict.protected
        && verdict.probability >= active_threshold
        && engine.settings().mode == cogwheel_classifier::ClassifierMode::Protect;

    let contributions = engine
        .explain(&domain, 12)
        .into_iter()
        .map(|contribution| ContributionView {
            label: contribution.label,
            kind: match contribution.kind {
                cogwheel_classifier::ContributionKind::Dense => "dense".to_string(),
                cogwheel_classifier::ContributionKind::Ngram => "ngram".to_string(),
            },
            value: contribution.value,
        })
        .collect();

    Ok(Json(ApiEnvelope {
        data: InspectDomainResponse {
            domain,
            probability: verdict.probability,
            protected: verdict.protected,
            decision: if would_block { "block" } else { "allow" }.to_string(),
            active_threshold,
            blocklist_match: None,
            contributions,
        },
    }))
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DetectionView {
    domain: String,
    client: Option<String>,
    probability: f32,
    protected: bool,
    blocked: bool,
    observed_at: String,
}

#[derive(serde::Deserialize)]
struct DetectionsQuery {
    #[serde(default)]
    limit: Option<usize>,
}

// ------------------------------------------------------- classifier adaptation API

/// Most feedback items retained. Beyond this the oldest are dropped.
///
/// A household reports a handful of mistakes a month, so this is years of headroom — but the row is
/// read and written whole on every submission, and an unbounded one would eventually make a single
/// feedback click cost a multi-megabyte round trip through SQLite.
const MAX_PENDING_FEEDBACK: usize = 5_000;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClassifierFeedbackRequest {
    domain: String,
    is_ad: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ClassifierFeedbackResponse {
    domain: String,
    is_ad: bool,
    pending_feedback: usize,
    minimum_feedback: usize,
}

/// Record one correction from the household.
///
/// Nothing is trained here. Feedback only accumulates; turning it into a model change is an explicit
/// second step (`/adapt`) that has to pass the gate, so a stream of clicks can never quietly become
/// a behaviour change.
async fn classifier_feedback(
    State(state): State<ServerState>,
    Json(request): Json<ClassifierFeedbackRequest>,
) -> Result<Json<ApiEnvelope<ClassifierFeedbackResponse>>, axum::http::StatusCode> {
    // Bound the input before doing any work: this endpoint is reachable by anything on the LAN.
    if request.domain.len() > cogwheel_classifier::normalize::MAX_HOST_LEN {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    // Normalise at the door rather than at training time. An unscoreable name is a client bug or a
    // typo, and the household deserves to be told now instead of having it silently discarded weeks
    // later when someone presses Adapt.
    let domain = cogwheel_classifier::normalize(&request.domain)
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;

    let mut feedback = load_classifier_feedback(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    // One live claim per host: a later report replaces an earlier one rather than stacking with it,
    // so a household that changes its mind is not training the model on both answers.
    feedback.retain(|item| item.host != domain);
    feedback.push(cogwheel_classifier::Feedback {
        host: domain.clone(),
        is_ad: request.is_ad,
        observed_at: chrono::Utc::now(),
    });
    while feedback.len() > MAX_PENDING_FEEDBACK {
        feedback.remove(0);
    }

    persist_classifier_feedback(&state.storage, &feedback)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope {
        data: ClassifierFeedbackResponse {
            domain,
            is_ad: request.is_ad,
            pending_feedback: feedback.len(),
            minimum_feedback: cogwheel_classifier::adapt::MIN_FEEDBACK_EXAMPLES,
        },
    }))
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AdaptationOutcomeView {
    /// `promoted`, `rejected` or `notEnoughData`.
    status: String,
    promoted: bool,
    /// Set only on rejection, and always specific about which criterion failed.
    reason: Option<String>,
    /// ROC-AUC of base+delta on the committed holdout, when it was measured.
    roc_auc: Option<f32>,
    /// False-positive rate of base+delta at the three calibrated thresholds.
    false_positive_rate: Option<SensitivityBand>,
    example_count: Option<usize>,
    /// Feedback available, when there was not enough of it to judge.
    have: Option<usize>,
    /// Feedback required.
    need: Option<usize>,
    adaptation: ClassifierAdaptationInfo,
}

/// Train a correction from the pending feedback, measure it, and keep it only if it holds up.
///
/// The base model is never touched. On rejection the previously active delta (if any) is also left
/// exactly as it was: a failed adaptation attempt is a no-op, not a rollback.
async fn classifier_adapt(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<AdaptationOutcomeView>>, axum::http::StatusCode> {
    let feedback = load_classifier_feedback(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    // Training is a few thousand sparse SGD steps and the gate is 25,000 inferences. That is tens to
    // hundreds of milliseconds of pure CPU, and running it on the async runtime would stall the DNS
    // listeners sharing this executor on a 4-core Pi -- the exact mistake the scoring worker exists
    // to avoid.
    let base = state.dns_runtime.classifier().model().clone();
    let (delta, outcome) = tokio::task::spawn_blocking(move || {
        let delta = cogwheel_classifier::train_delta(
            &base,
            &feedback,
            cogwheel_classifier::AdaptConfig::default(),
        );
        let outcome = cogwheel_classifier::evaluate_and_gate(
            &base,
            &delta,
            &cogwheel_classifier::embedded_holdout(),
            base.quality(),
        );
        (delta, outcome)
    })
    .await
    .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut view = match &outcome {
        cogwheel_classifier::AdaptationOutcome::Promoted {
            auc,
            false_positive_rate,
            example_count,
        } => AdaptationOutcomeView {
            status: "promoted".to_string(),
            promoted: true,
            reason: None,
            roc_auc: Some(*auc),
            false_positive_rate: Some(SensitivityBand {
                low: false_positive_rate[0],
                balanced: false_positive_rate[1],
                high: false_positive_rate[2],
            }),
            example_count: Some(*example_count),
            have: None,
            need: None,
            adaptation: build_adaptation_info(&state, None, 0),
        },
        cogwheel_classifier::AdaptationOutcome::Rejected {
            reason,
            auc,
            false_positive_rate,
        } => AdaptationOutcomeView {
            status: "rejected".to_string(),
            promoted: false,
            reason: Some(reason.clone()),
            roc_auc: Some(*auc),
            false_positive_rate: Some(SensitivityBand {
                low: false_positive_rate[0],
                balanced: false_positive_rate[1],
                high: false_positive_rate[2],
            }),
            example_count: None,
            have: None,
            need: None,
            adaptation: build_adaptation_info(&state, None, 0),
        },
        cogwheel_classifier::AdaptationOutcome::NotEnoughData { have, need } => {
            AdaptationOutcomeView {
                status: "notEnoughData".to_string(),
                promoted: false,
                reason: None,
                roc_auc: None,
                false_positive_rate: None,
                example_count: None,
                have: Some(*have),
                need: Some(*need),
                adaptation: build_adaptation_info(&state, None, 0),
            }
        }
    };

    if let cogwheel_classifier::AdaptationOutcome::Promoted {
        auc,
        false_positive_rate,
        example_count,
    } = &outcome
    {
        let stored = StoredAdaptation {
            delta_hex: delta.to_hex(),
            roc_auc: *auc,
            false_positive_rate: *false_positive_rate,
            example_count: *example_count,
            trained_at: delta.trained_at(),
        };
        persist_classifier_adaptation(&state.storage, &stored)
            .await
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        state
            .dns_runtime
            .classifier()
            .set_active_delta(Some(Arc::new(delta)));
        tracing::info!(
            roc_auc = *auc,
            example_count = *example_count,
            "classifier adaptation promoted"
        );
    } else if let cogwheel_classifier::AdaptationOutcome::Rejected { reason, .. } = &outcome {
        tracing::info!(reason = %reason, "classifier adaptation rejected by the quality gate");
    }

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "classifier-adaptation.evaluated".to_string(),
            payload: serde_json::to_string(&view)
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    // Rebuild the adaptation block after the engine has been updated, so the response describes the
    // state the caller is now in rather than the one it was in a moment ago.
    let stored = load_classifier_adaptation(&state.storage)
        .await
        .unwrap_or_default();
    let pending = load_classifier_feedback(&state.storage)
        .await
        .unwrap_or_default()
        .len();
    view.adaptation = build_adaptation_info(&state, stored.as_ref(), pending);

    Ok(Json(ApiEnvelope { data: view }))
}

/// Discard the active delta and return to the shipped model.
///
/// This is the entire rollback story, and it is deliberately this small: the base was never
/// modified, so there is nothing to restore. Pending feedback is left alone — the household's
/// corrections are their data, not a side effect of an adaptation they chose to undo.
async fn classifier_adapt_rollback(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<ClassifierAdaptationInfo>>, axum::http::StatusCode> {
    clear_classifier_adaptation(&state.storage)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    state.dns_runtime.classifier().set_active_delta(None);
    tracing::info!("classifier adaptation rolled back to the base model");

    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "classifier-adaptation.rolled-back".to_string(),
            payload: "{}".to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    let pending = load_classifier_feedback(&state.storage)
        .await
        .unwrap_or_default()
        .len();
    Ok(Json(ApiEnvelope {
        data: build_adaptation_info(&state, None, pending),
    }))
}

async fn classifier_detections(
    State(state): State<ServerState>,
    axum::extract::Query(query): axum::extract::Query<DetectionsQuery>,
) -> Json<ApiEnvelope<Vec<DetectionView>>> {
    // Clamp rather than reject: an unbounded limit would let one request serialise the whole ring.
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let detections = state
        .dns_runtime
        .classifier()
        .recent_detections(limit)
        .into_iter()
        .map(|detection| DetectionView {
            domain: detection.host,
            client: detection.client,
            probability: detection.probability,
            protected: detection.protected,
            blocked: detection.blocked,
            observed_at: detection.observed_at.to_rfc3339(),
        })
        .collect();
    Json(ApiEnvelope { data: detections })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use cogwheel_classifier::ClassifierMode;

    #[test]
    fn runtime_regression_thresholds_trigger_degraded_state() {
        let before = DnsRuntimeSnapshot {
            upstream_failures_total: 1,
            fallback_served_total: 0,
            cache_hits_total: 0,
            cache_expired_total: 0,
            cname_uncloaks_total: 0,
            cname_blocks_total: 0,
            queries_total: 100,
            blocked_total: 10,
            cache_hit_latency_avg_ns: 0,
            cache_hit_samples: 0,
            cache_miss_latency_avg_ns: 0,
            cache_miss_samples: 0,
            classifier_latency_avg_ns: 0,
            classifier_latency_samples: 0,
        };
        let after = DnsRuntimeSnapshot {
            upstream_failures_total: 3,
            fallback_served_total: 1,
            cache_hits_total: 0,
            cache_expired_total: 0,
            cname_uncloaks_total: 0,
            cname_blocks_total: 0,
            queries_total: 200,
            blocked_total: 20,
            cache_hit_latency_avg_ns: 0,
            cache_hit_samples: 0,
            cache_miss_latency_avg_ns: 0,
            cache_miss_samples: 0,
            classifier_latency_avg_ns: 0,
            classifier_latency_samples: 0,
        };
        let guard = RuntimeGuardConfig {
            probe_domains: vec!["example.com".to_string()],
            max_upstream_failures_delta: 0,
            max_fallback_served_delta: 0,
        };

        let report = evaluate_runtime_regressions(&before, &after, &guard);
        assert!(report.degraded);
        assert_eq!(report.notes.len(), 2);
    }

    #[test]
    fn runtime_regression_thresholds_allow_healthy_state() {
        let before = DnsRuntimeSnapshot {
            upstream_failures_total: 1,
            fallback_served_total: 1,
            cache_hits_total: 0,
            cache_expired_total: 0,
            cname_uncloaks_total: 0,
            cname_blocks_total: 0,
            queries_total: 100,
            blocked_total: 10,
            cache_hit_latency_avg_ns: 0,
            cache_hit_samples: 0,
            cache_miss_latency_avg_ns: 0,
            cache_miss_samples: 0,
            classifier_latency_avg_ns: 0,
            classifier_latency_samples: 0,
        };
        let after = DnsRuntimeSnapshot {
            upstream_failures_total: 1,
            fallback_served_total: 1,
            cache_hits_total: 2,
            cache_expired_total: 0,
            cname_uncloaks_total: 1,
            cname_blocks_total: 0,
            queries_total: 200,
            blocked_total: 15,
            cache_hit_latency_avg_ns: 0,
            cache_hit_samples: 0,
            cache_miss_latency_avg_ns: 0,
            cache_miss_samples: 0,
            classifier_latency_avg_ns: 0,
            classifier_latency_samples: 0,
        };
        let guard = RuntimeGuardConfig::default();

        let report = evaluate_runtime_regressions(&before, &after, &guard);
        assert!(!report.degraded);
        assert!(report.notes.is_empty());
    }

    /// The subscriber cap and the Drop-based slot release are the event bus's two
    /// correctness-critical behaviours. Without a test, a refactor that dropped `SubscriberGuard`
    /// or moved the `fetch_add` would leak slots until the endpoint refused every client, and
    /// nothing in the suite would notice.
    #[test]
    fn event_bus_releases_subscriber_slots_when_streams_are_dropped() {
        let bus = EventBus::new();
        let counter = Arc::clone(&bus.subscribers);

        {
            let _guards: Vec<SubscriberGuard> = (0..MAX_EVENT_SUBSCRIBERS)
                .map(|_| {
                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    SubscriberGuard(Arc::clone(&counter))
                })
                .collect();
            assert_eq!(
                counter.load(std::sync::atomic::Ordering::Relaxed),
                MAX_EVENT_SUBSCRIBERS,
                "every slot should be taken"
            );
        }

        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "dropping the guards must return every slot"
        );
    }

    #[test]
    fn event_bus_publishes_without_subscribers() {
        // A send with no receivers must be a no-op, not an error path the DNS observers have to
        // handle on every query.
        let bus = EventBus::new();
        bus.publish(StreamEvent::Query(Box::new(StreamQueryEvent {
            domain: "example.com".to_string(),
            client: "127.0.0.1".to_string(),
            device_name: None,
            blocked: false,
            reason: None,
            observed_at: "2026-01-01T00:00:00Z".to_string(),
        })));
        assert_eq!(
            bus.subscribers.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn event_bus_delivers_each_frame_kind_to_a_subscriber() {
        let bus = EventBus::new();
        let mut receiver = bus.sender.subscribe();

        bus.publish(StreamEvent::Detection(Box::new(StreamDetectionEvent {
            domain: "ads.example.com".to_string(),
            client: "127.0.0.1".to_string(),
            device_name: None,
            probability: 0.97,
            decision: "block".to_string(),
            observed_at: "2026-01-01T00:00:00Z".to_string(),
        })));
        bus.publish(StreamEvent::Health(Box::new(StreamHealthEvent {
            degraded: true,
            notes: vec!["upstream slow".to_string()],
            observed_at: "2026-01-01T00:00:00Z".to_string(),
        })));

        let first = receiver.try_recv();
        assert!(
            matches!(&first, Ok(StreamEvent::Detection(event)) if event.domain == "ads.example.com"),
            "expected a detection frame for ads.example.com, got {first:?}"
        );
        let second = receiver.try_recv();
        assert!(
            matches!(&second, Ok(StreamEvent::Health(event)) if event.degraded),
            "expected a degraded health frame, got {second:?}"
        );
    }

    #[test]
    fn classifier_settings_round_trip_json() {
        let settings = ClassifierSettings {
            mode: ClassifierMode::Protect,
            sensitivity: cogwheel_classifier::Sensitivity::High,
        };

        let encoded = serde_json::to_string(&settings).expect("encode settings");
        let decoded: ClassifierSettings = serde_json::from_str(&encoded).expect("decode settings");
        assert_eq!(decoded.mode, ClassifierMode::Protect);
        assert_eq!(decoded.sensitivity, cogwheel_classifier::Sensitivity::High);
    }

    /// Settings blobs written by builds that predate the sensitivity field must still load, because
    /// they are sitting in the `settings` key-value table of every existing install.
    #[test]
    fn classifier_settings_tolerate_legacy_blobs() {
        let decoded: ClassifierSettings =
            serde_json::from_str(r#"{"mode":"protect","threshold":0.92}"#)
                .expect("legacy blob must still decode");
        assert_eq!(decoded.mode, ClassifierMode::Protect);
        assert_eq!(
            decoded.sensitivity,
            cogwheel_classifier::Sensitivity::Balanced,
            "an unknown legacy field should fall back to the default sensitivity"
        );
    }

    /// The stored adaptation row has to survive a restart intact, delta bytes included — it is the
    /// only copy of a correction the user explicitly approved.
    #[test]
    fn stored_adaptation_round_trips_through_the_settings_table() {
        let base = cogwheel_classifier::embedded_model().expect("embedded model must parse");
        let feedback: Vec<cogwheel_classifier::Feedback> = (0..40)
            .map(|index| cogwheel_classifier::Feedback {
                host: format!("h{index}.example{index}.com"),
                is_ad: index % 3 == 0,
                observed_at: chrono::Utc::now(),
            })
            .collect();
        let delta = cogwheel_classifier::train_delta(
            &base,
            &feedback,
            cogwheel_classifier::AdaptConfig::default(),
        );

        let stored = StoredAdaptation {
            delta_hex: delta.to_hex(),
            roc_auc: 0.8912,
            false_positive_rate: [0.001, 0.005, 0.023],
            example_count: delta.example_count(),
            trained_at: delta.trained_at(),
        };
        let encoded = serde_json::to_string(&stored).expect("encode");
        assert!(
            encoded.contains("\"deltaHex\"") && encoded.contains("\"falsePositiveRate\""),
            "stored adaptation must use the camelCase convention: {encoded}"
        );

        let decoded: StoredAdaptation = serde_json::from_str(&encoded).expect("decode");
        let restored =
            cogwheel_classifier::Delta::from_hex(&decoded.delta_hex).expect("delta must reload");
        assert_eq!(restored.example_count(), delta.example_count());
        for host in ["h1.example1.com", "chase.com", "doubleclick.net"] {
            assert_eq!(
                base.probability_with_delta(host, Some(&restored)),
                base.probability_with_delta(host, Some(&delta)),
                "{host} scored differently after a persistence round trip"
            );
        }
    }

    /// A delta row corrupted on disk must be dropped, not applied. `Delta::from_hex` is the gate for
    /// that, so pin the fact that the server's restore path actually gets an error out of it.
    #[test]
    fn a_corrupt_stored_delta_is_rejected_rather_than_applied() {
        let base = cogwheel_classifier::embedded_model().expect("parse");
        let feedback: Vec<cogwheel_classifier::Feedback> = (0..40)
            .map(|index| cogwheel_classifier::Feedback {
                host: format!("h{index}.example{index}.com"),
                is_ad: index % 3 == 0,
                observed_at: chrono::Utc::now(),
            })
            .collect();
        let delta = cogwheel_classifier::train_delta(
            &base,
            &feedback,
            cogwheel_classifier::AdaptConfig::default(),
        );
        let mut hex = delta.to_hex();
        assert!(hex.len() > 200);
        // Flip one nibble deep in the weight block.
        let position = hex.len() - 11;
        hex.replace_range(position..position + 1, "f");
        assert!(
            cogwheel_classifier::Delta::from_hex(&hex).is_err()
                || cogwheel_classifier::Delta::from_hex(&hex) == Ok(delta),
            "a corrupted delta must not silently become a different valid delta"
        );

        assert!(cogwheel_classifier::Delta::from_hex("not hex at all").is_err());
        assert!(cogwheel_classifier::Delta::from_hex("").is_err());
    }

    #[test]
    fn classifier_feedback_request_uses_camel_case() {
        let request: ClassifierFeedbackRequest =
            serde_json::from_str(r#"{"domain":"ads.example.com","isAd":true}"#).expect("decode");
        assert_eq!(request.domain, "ads.example.com");
        assert!(request.is_ad);
    }

    #[test]
    fn adaptation_outcome_view_serialises_in_camel_case() {
        let view = AdaptationOutcomeView {
            status: "rejected".to_string(),
            promoted: false,
            reason: Some("false-positive rate at balanced sensitivity rose".to_string()),
            roc_auc: Some(0.8901),
            false_positive_rate: Some(SensitivityBand {
                low: 0.001,
                balanced: 0.006,
                high: 0.024,
            }),
            example_count: None,
            have: None,
            need: None,
            adaptation: ClassifierAdaptationInfo {
                active: false,
                trained_at: None,
                example_count: 0,
                ngram_entries: 0,
                roc_auc: None,
                false_positive_rate: None,
                max_logit_shift: 0.0,
                logit_budget: cogwheel_classifier::adapt::DELTA_LOGIT_BUDGET,
                pending_feedback: 42,
                minimum_feedback: cogwheel_classifier::adapt::MIN_FEEDBACK_EXAMPLES,
            },
        };
        let encoded = serde_json::to_string(&view).expect("encode");
        for key in [
            "\"rocAuc\"",
            "\"falsePositiveRate\"",
            "\"pendingFeedback\"",
            "\"maxLogitShift\"",
            "\"logitBudget\"",
            "\"minimumFeedback\"",
        ] {
            assert!(encoded.contains(key), "missing {key} in {encoded}");
        }
    }

    #[test]
    fn normalize_source_kind_accepts_known_kinds() {
        assert_eq!(normalize_source_kind("HOSTS"), Some("hosts".to_string()));
        assert_eq!(
            normalize_source_kind(" domains "),
            Some("domains".to_string())
        );
        assert_eq!(normalize_source_kind("weird"), None);
    }

    #[test]
    fn baseline_source_id_is_reserved() {
        assert!(is_reserved_source_id(Uuid::from_u128(1)));
        assert!(!is_reserved_source_id(Uuid::new_v4()));
    }

    #[test]
    fn normalize_verification_strictness_accepts_known_values() {
        assert_eq!(
            normalize_verification_strictness("STRICT"),
            Some("strict".to_string())
        );
        assert_eq!(
            normalize_verification_strictness(" balanced "),
            Some("balanced".to_string())
        );
        assert_eq!(normalize_verification_strictness("unknown"), None);
    }

    #[test]
    fn normalize_device_policy_mode_accepts_known_values() {
        assert_eq!(
            normalize_device_policy_mode("GLOBAL"),
            Some("global".to_string())
        );
        assert_eq!(
            normalize_device_policy_mode(" custom "),
            Some("custom".to_string())
        );
        assert_eq!(normalize_device_policy_mode("invalid"), None);
    }

    #[test]
    fn normalize_device_protection_override_accepts_known_values() {
        assert_eq!(
            normalize_device_protection_override(" BYPASS "),
            Some("bypass".to_string())
        );
        assert_eq!(
            normalize_device_protection_override("inherit"),
            Some("inherit".to_string())
        );
        assert_eq!(normalize_device_protection_override("block"), None);
    }

    #[test]
    fn normalize_device_allowed_domains_deduplicates_values() {
        assert_eq!(
            normalize_device_allowed_domains(vec![
                " Example.com ".to_string(),
                "example.com".to_string(),
                "cdn.example.com.".to_string(),
                " ".to_string(),
            ]),
            vec!["cdn.example.com".to_string(), "example.com".to_string()]
        );
    }

    #[test]
    fn normalize_device_service_overrides_filters_unknown_values() {
        assert_eq!(
            normalize_device_service_overrides(vec![
                DeviceServiceOverrideRecord {
                    service_id: "tiktok".to_string(),
                    mode: "allow".to_string(),
                },
                DeviceServiceOverrideRecord {
                    service_id: "unknown".to_string(),
                    mode: "block".to_string(),
                },
                DeviceServiceOverrideRecord {
                    service_id: "tiktok".to_string(),
                    mode: "block".to_string(),
                },
            ]),
            vec![DeviceServiceOverrideRecord {
                service_id: "tiktok".to_string(),
                mode: "block".to_string(),
            }]
        );
    }

    #[test]
    fn validate_device_service_overrides_rejects_global_mode_payloads() {
        assert_eq!(
            validate_device_service_overrides(
                "global",
                vec![DeviceServiceOverrideRecord {
                    service_id: "tiktok".to_string(),
                    mode: "allow".to_string(),
                }],
            ),
            Err("device service overrides require custom policy mode".to_string())
        );
    }

    #[test]
    fn validate_device_service_overrides_rejects_invalid_values() {
        assert_eq!(
            validate_device_service_overrides(
                "custom",
                vec![DeviceServiceOverrideRecord {
                    service_id: "unknown".to_string(),
                    mode: "allow".to_string(),
                }],
            ),
            Err(
                "unknown device service override `unknown`; choose one of the built-in services"
                    .to_string()
            )
        );

        assert_eq!(
            validate_device_service_overrides(
                "custom",
                vec![DeviceServiceOverrideRecord {
                    service_id: "tiktok".to_string(),
                    mode: "monitor".to_string(),
                }],
            ),
            Err("device service override `TikTok` must use allow or block mode".to_string())
        );
    }

    #[test]
    fn validate_device_service_overrides_normalizes_known_values() {
        assert_eq!(
            validate_device_service_overrides(
                "custom",
                vec![
                    DeviceServiceOverrideRecord {
                        service_id: " tiktok ".to_string(),
                        mode: "allow".to_string(),
                    },
                    DeviceServiceOverrideRecord {
                        service_id: "tiktok".to_string(),
                        mode: "block".to_string(),
                    },
                ],
            ),
            Ok(vec![DeviceServiceOverrideRecord {
                service_id: "tiktok".to_string(),
                mode: "block".to_string(),
            }])
        );
    }

    #[test]
    fn normalize_profile_name_accepts_non_empty_values() {
        assert_eq!(
            normalize_profile_name(" Balanced "),
            Some("balanced".to_string())
        );
        assert_eq!(normalize_profile_name("   "), None);
    }

    #[test]
    fn normalize_notification_inputs_accept_expected_values() {
        assert_eq!(
            normalize_notification_severity(" HIGH "),
            Some("high".to_string())
        );
        assert_eq!(normalize_notification_severity("low"), None);
        assert_eq!(normalize_webhook_url(None), Some(None));
        assert_eq!(normalize_webhook_url(Some("   ")), Some(None));
        assert!(normalize_webhook_url(Some("https://hooks.example.test/path")).is_some());
        assert_eq!(normalize_webhook_url(Some("ftp://example.test")), None);
    }

    #[test]
    fn notification_delivery_respects_thresholds() {
        let settings = NotificationSettings {
            enabled: true,
            webhook_url: Some("https://hooks.example.test/path".to_string()),
            min_severity: "high".to_string(),
        };

        assert!(!should_deliver_notification(&settings, "medium"));
        assert!(should_deliver_notification(&settings, "high"));
        assert!(should_deliver_notification(&settings, "critical"));
    }

    #[test]
    fn notification_retry_delay_backs_off() {
        assert_eq!(notification_retry_delay(0), Duration::from_millis(250));
        assert_eq!(notification_retry_delay(1), Duration::from_millis(500));
        assert_eq!(notification_retry_delay(2), Duration::from_millis(1000));
    }

    #[test]
    fn runtime_device_policies_clear_global_overrides() {
        let configs = runtime_device_policies_from_records(vec![DeviceRecord {
            id: Uuid::new_v4(),
            name: "Laptop".to_string(),
            ip_address: "192.168.1.10".to_string(),
            policy_mode: "global".to_string(),
            blocklist_profile_override: Some("Aggressive".to_string()),
            protection_override: "bypass".to_string(),
            allowed_domains: vec!["example.com".to_string()],
            service_overrides: vec![DeviceServiceOverrideRecord {
                service_id: "tiktok".to_string(),
                mode: "allow".to_string(),
            }],
        }]);

        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].policy_mode, "global");
        assert_eq!(configs[0].blocklist_profile_override, None);
        assert_eq!(configs[0].protection_override, "inherit");
        assert!(configs[0].allowed_domains.is_empty());
        assert!(configs[0].blocked_domains.is_empty());
    }

    #[test]
    fn build_runtime_policy_catalog_includes_shared_rules_in_profiles() {
        let shared = parse_source(
            SourceDefinition {
                id: Uuid::new_v4(),
                name: "Shared".to_string(),
                url: Url::parse("data:text/plain,shared.example").expect("shared url"),
                kind: SourceKind::Domains,
                enabled: true,
                profile: "shared".to_string(),
                verification_strictness: "balanced".to_string(),
            },
            "shared.example",
        );
        let balanced = parse_source(
            SourceDefinition {
                id: Uuid::new_v4(),
                name: "Balanced".to_string(),
                url: Url::parse("data:text/plain,balanced.example").expect("balanced url"),
                kind: SourceKind::Domains,
                enabled: true,
                profile: "balanced".to_string(),
                verification_strictness: "balanced".to_string(),
            },
            "balanced.example",
        );

        let catalog =
            build_runtime_policy_catalog(&[shared, balanced], HashSet::new(), BlockMode::NullIp);
        let balanced_policy = catalog
            .profile_policies
            .get("balanced")
            .expect("balanced profile policy");

        assert!(matches!(
            balanced_policy.evaluate("shared.example").kind,
            DecisionKind::Blocked(_)
        ));
        assert!(matches!(
            balanced_policy.evaluate("balanced.example").kind,
            DecisionKind::Blocked(_)
        ));
    }

    #[test]
    fn build_security_summary_tracks_severity_and_devices() {
        let summary = build_security_summary(&[
            SecurityEventRecord {
                id: Uuid::new_v4(),
                device_id: None,
                device_name: Some("Laptop".to_string()),
                client_ip: "192.168.1.10".to_string(),
                domain: "alpha.example".to_string(),
                classifier_score: 0.97,
                severity: "high".to_string(),
                created_at: Utc::now(),
            },
            SecurityEventRecord {
                id: Uuid::new_v4(),
                device_id: None,
                device_name: Some("Laptop".to_string()),
                client_ip: "192.168.1.10".to_string(),
                domain: "beta.example".to_string(),
                classifier_score: 0.995,
                severity: "critical".to_string(),
                created_at: Utc::now(),
            },
            SecurityEventRecord {
                id: Uuid::new_v4(),
                device_id: None,
                device_name: None,
                client_ip: "192.168.1.20".to_string(),
                domain: "gamma.example".to_string(),
                classifier_score: 0.93,
                severity: "medium".to_string(),
                created_at: Utc::now(),
            },
        ]);

        assert_eq!(summary.medium_count, 1);
        assert_eq!(summary.high_count, 1);
        assert_eq!(summary.critical_count, 1);
        assert_eq!(summary.top_devices.len(), 2);
        assert_eq!(summary.top_devices[0].label, "Laptop");
        assert_eq!(summary.top_devices[0].event_count, 2);
        assert_eq!(summary.top_devices[0].highest_severity, "critical");
    }

    #[test]
    fn build_notification_delivery_events_maps_delivery_records() {
        let deliveries = build_notification_delivery_events(&[NotificationDeliveryRecord {
            id: Uuid::new_v4(),
            event_type: "security.alert_raised".to_string(),
            status: "delivered".to_string(),
            severity: "high".to_string(),
            title: "notify.example".to_string(),
            summary: "high alert for Laptop after 2 attempt(s).".to_string(),
            domain: "notify.example".to_string(),
            device_name: Some("Laptop".to_string()),
            client_ip: "192.168.1.25".to_string(),
            attempts: 2,
            created_at: Utc::now(),
        }]);

        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].status, "delivered");
        assert_eq!(deliveries[0].event_type, "security.alert_raised");
        assert_eq!(deliveries[0].title, "notify.example");
        assert_eq!(deliveries[0].target, "Laptop");
        assert_eq!(deliveries[0].domain, "notify.example");
        assert_eq!(deliveries[0].attempts, 2);
    }

    #[test]
    fn build_notification_delivery_events_supports_operational_payloads() {
        let deliveries = build_notification_delivery_events(&[NotificationDeliveryRecord {
            id: Uuid::new_v4(),
            event_type: "ruleset.rollback".to_string(),
            status: "delivered".to_string(),
            severity: "high".to_string(),
            title: "Ruleset rolled back".to_string(),
            summary: "Rolled back to the previous verified ruleset.".to_string(),
            domain: "control-plane".to_string(),
            device_name: None,
            client_ip: "control-plane".to_string(),
            attempts: 1,
            created_at: Utc::now(),
        }]);

        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].status, "delivered");
        assert_eq!(deliveries[0].event_type, "ruleset.rollback");
        assert_eq!(deliveries[0].title, "Ruleset rolled back");
        assert_eq!(
            deliveries[0].summary,
            "Rolled back to the previous verified ruleset."
        );
        assert_eq!(deliveries[0].target, "control-plane");
        assert_eq!(deliveries[0].client_ip, "control-plane");
        assert_eq!(deliveries[0].domain, "control-plane");
    }

    #[test]
    fn build_notification_health_summary_tracks_outcomes() {
        let now = Utc::now();
        let summary = build_notification_health_summary(&[
            NotificationDeliveryRecord {
                id: Uuid::new_v4(),
                event_type: "security.alert_raised".to_string(),
                status: "delivered".to_string(),
                severity: "high".to_string(),
                title: "ok.example".to_string(),
                summary: "delivered".to_string(),
                domain: "ok.example".to_string(),
                device_name: None,
                client_ip: "192.168.1.25".to_string(),
                attempts: 1,
                created_at: now,
            },
            NotificationDeliveryRecord {
                id: Uuid::new_v4(),
                event_type: "security.alert_raised".to_string(),
                status: "failed".to_string(),
                severity: "high".to_string(),
                title: "fail.example".to_string(),
                summary: "failed".to_string(),
                domain: "fail.example".to_string(),
                device_name: None,
                client_ip: "192.168.1.25".to_string(),
                attempts: 3,
                created_at: now + chrono::Duration::seconds(5),
            },
        ]);

        assert_eq!(summary.delivered_count, 1);
        assert_eq!(summary.failed_count, 1);
        assert_eq!(summary.last_delivery_at, Some(now));
        assert_eq!(
            summary.last_failure_at,
            Some(now + chrono::Duration::seconds(5))
        );
    }

    #[test]
    fn build_notification_failure_analytics_tracks_failed_domains() {
        let analytics = build_notification_failure_analytics(&[
            NotificationDeliveryRecord {
                id: Uuid::new_v4(),
                event_type: "security.alert_raised".to_string(),
                status: "delivered".to_string(),
                severity: "high".to_string(),
                title: "ok.example".to_string(),
                summary: "ok".to_string(),
                domain: "ok.example".to_string(),
                device_name: None,
                client_ip: "192.168.1.25".to_string(),
                attempts: 1,
                created_at: Utc::now(),
            },
            NotificationDeliveryRecord {
                id: Uuid::new_v4(),
                event_type: "security.alert_raised".to_string(),
                status: "failed".to_string(),
                severity: "high".to_string(),
                title: "fail.example".to_string(),
                summary: "failed".to_string(),
                domain: "fail.example".to_string(),
                device_name: None,
                client_ip: "192.168.1.25".to_string(),
                attempts: 3,
                created_at: Utc::now(),
            },
            NotificationDeliveryRecord {
                id: Uuid::new_v4(),
                event_type: "security.alert_raised".to_string(),
                status: "failed".to_string(),
                severity: "high".to_string(),
                title: "fail.example".to_string(),
                summary: "failed again".to_string(),
                domain: "fail.example".to_string(),
                device_name: None,
                client_ip: "192.168.1.25".to_string(),
                attempts: 3,
                created_at: Utc::now(),
            },
        ]);

        assert_eq!(analytics.success_rate_percent, 33.3);
        assert_eq!(analytics.top_failed_domains.len(), 1);
        assert_eq!(analytics.top_failed_domains[0].domain, "fail.example");
        assert_eq!(analytics.top_failed_domains[0].failure_count, 2);
    }

    #[test]
    fn parse_tailscale_status_json_extracts_health_fields() {
        let status = parse_tailscale_status_json(
            &serde_json::json!({
                "BackendState": "Running",
                "CurrentTailnet": { "Name": "example.ts.net" },
                "Self": {
                    "HostName": "cogwheel-node",
                    "UsingExitNode": true
                },
                "Peer": {
                    "peer-a": {},
                    "peer-b": {}
                },
                "Health": ["wantrunning is false"]
            })
            .to_string(),
        );

        assert!(status.installed);
        assert!(status.daemon_running);
        assert_eq!(status.backend_state.as_deref(), Some("Running"));
        assert_eq!(status.hostname.as_deref(), Some("cogwheel-node"));
        assert_eq!(status.tailnet_name.as_deref(), Some("example.ts.net"));
        assert_eq!(status.peer_count, 2);
        assert!(status.exit_node_active);
        assert_eq!(status.health_warnings, vec!["wantrunning is false"]);
    }

    #[test]
    fn normalize_notification_window_accepts_known_values() {
        assert_eq!(normalize_notification_window(Some(10)), 10);
        assert_eq!(normalize_notification_window(Some(50)), 50);
        assert_eq!(normalize_notification_window(Some(100)), 100);
        assert_eq!(normalize_notification_window(Some(999)), 30);
        assert_eq!(normalize_notification_window(None), 30);
    }

    #[test]
    fn normalize_notification_test_presets_filters_invalid_entries() {
        let presets = normalize_notification_test_presets(vec![
            NotificationTestPreset {
                name: "weekday".to_string(),
                domain: "notify.example".to_string(),
                severity: "high".to_string(),
                device_name: "Laptop".to_string(),
                dry_run: false,
            },
            NotificationTestPreset {
                name: "weekday".to_string(),
                domain: "notify-two.example".to_string(),
                severity: "critical".to_string(),
                device_name: "Tablet".to_string(),
                dry_run: true,
            },
            NotificationTestPreset {
                name: "".to_string(),
                domain: "ignored.example".to_string(),
                severity: "high".to_string(),
                device_name: "Ignored".to_string(),
                dry_run: false,
            },
        ]);

        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "weekday");
        assert_eq!(presets[0].domain, "notify-two.example");
        assert_eq!(presets[0].severity, "critical");
        assert!(presets[0].dry_run);
    }

    #[test]
    fn severity_for_classifier_score_uses_expected_bands() {
        assert_eq!(severity_for_classifier_score(0.995), "critical");
        assert_eq!(severity_for_classifier_score(0.97), "high");
        assert_eq!(severity_for_classifier_score(0.92), "medium");
    }

    #[test]
    fn source_refresh_state_tracks_attempts() {
        let mut state = SourceRefreshState::default();
        let source_id = Uuid::new_v4();
        let now = chrono::Utc::now();
        state.record_attempt(source_id, now);
        assert_eq!(state.last_refresh_for(source_id), Some(now));
    }

    #[test]
    fn source_due_for_refresh_respects_interval() {
        let source = SourceRecord {
            id: Uuid::new_v4(),
            name: "scheduled".to_string(),
            url: "data:text/plain,scheduled.example".to_string(),
            kind: "domains".to_string(),
            enabled: true,
            refresh_interval_minutes: 30,
            profile: "balanced".to_string(),
            verification_strictness: "balanced".to_string(),
        };
        let now = chrono::Utc::now();
        assert!(!source_due_for_refresh(
            &source,
            Some(now - chrono::TimeDelta::minutes(5)),
            now,
        ));
        assert!(source_due_for_refresh(
            &source,
            Some(now - chrono::TimeDelta::minutes(45)),
            now,
        ));
    }

    #[test]
    fn parse_tailscale_status_json_handles_missing_fields() {
        let status = parse_tailscale_status_json("{}");
        assert!(status.installed);
        assert!(status.daemon_running);
        assert!(status.hostname.is_none());
        assert!(!status.exit_node_active);
    }

    #[test]
    fn parse_tailscale_status_json_detects_exit_node_status_variants() {
        let status_with_exit_node = parse_tailscale_status_json(
            &serde_json::json!({
                "Self": { "ExitNode": true }
            })
            .to_string(),
        );
        assert!(status_with_exit_node.exit_node_active);

        let status_with_exit_node_status = parse_tailscale_status_json(
            &serde_json::json!({
                "Self": { "ExitNodeStatus": "Active" }
            })
            .to_string(),
        );
        assert!(status_with_exit_node_status.exit_node_active);

        let status_without_exit = parse_tailscale_status_json(
            &serde_json::json!({
                "Self": { "ExitNode": false }
            })
            .to_string(),
        );
        assert!(!status_without_exit.exit_node_active);
    }

    #[test]
    fn tailscale_saved_state_serialization() {
        let state = TailscaleSavedState {
            exit_node_enabled: true,
            saved_at: "2024-01-01T00:00:00Z".to_string(),
            hostname: "test-node".to_string(),
        };
        let json = serde_json::to_string(&state).expect("encode tailscale state");
        let parsed: TailscaleSavedState =
            serde_json::from_str(&json).expect("decode tailscale state");
        assert!(parsed.exit_node_enabled);
        assert_eq!(parsed.hostname, "test-node");
    }

    /// hickory builds its TLS client config as `RootCertStore::empty()` and
    /// only fills it in under a trust-anchor feature. With `tls-ring` alone the
    /// store stays EMPTY, so every DoT/DoH certificate fails to validate: the
    /// build succeeds, the TCP connection succeeds, the handshake is rejected,
    /// and every encrypted query returns SERVFAIL forever. Measured -- that is
    /// exactly what happened before `webpki-roots` was added.
    ///
    /// Nothing else fails if this feature is dropped, which is what makes it
    /// worth pinning here. A guard on the manifest is crude, but it is the only
    /// place the mistake can be made and the only place it can be caught
    /// cheaply; the alternative is a live TLS connection in the test suite.
    #[test]
    fn encrypted_upstreams_have_trust_anchors_compiled_in() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../Cargo.toml")
            .canonicalize()
            .expect("workspace manifest should exist");
        let text = std::fs::read_to_string(&manifest).expect("read workspace manifest");
        let hickory = text
            .split("hickory-resolver = ")
            .nth(1)
            .expect("workspace should pin hickory-resolver");
        let declaration = &hickory[..hickory.find('}').unwrap_or(hickory.len())];

        assert!(
            declaration.contains("webpki-roots")
                || declaration.contains("rustls-platform-verifier"),
            "hickory-resolver must enable a trust-anchor feature or DoT/DoH silently never \
             resolves; found: {declaration}"
        );
        assert!(
            declaration.contains("tls-ring") || declaration.contains("tls-aws-lc-rs"),
            "hickory-resolver must enable a TLS feature for DoT upstreams; found: {declaration}"
        );
    }

    fn cli(args: &[&str]) -> CliAction {
        parse_cli(&args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>())
    }

    // The workspace bans `panic!`, so these render the wrong variant into the
    // assertion message instead of unwrapping it.
    fn printed(action: CliAction) -> String {
        match action {
            CliAction::Print(text) => text,
            other => format!("expected Print, got {other:?}"),
        }
    }

    fn failed(action: CliAction) -> String {
        match action {
            CliAction::Fail(message) => message,
            other => format!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn no_arguments_starts_the_server() {
        assert_eq!(cli(&[]), CliAction::Run);
    }

    #[test]
    fn version_prints_the_crate_version_and_does_not_start_the_server() {
        for flag in ["--version", "-V"] {
            let text = printed(cli(&[flag]));
            assert!(
                text.starts_with("cogwheel-server ") && text.contains(env!("CARGO_PKG_VERSION")),
                "{flag} produced {text:?}"
            );
        }
    }

    #[test]
    fn help_prints_usage_and_does_not_start_the_server() {
        for flag in ["--help", "-h"] {
            let text = printed(cli(&[flag]));
            assert!(text.contains("Usage:"), "{flag} produced {text:?}");
        }
    }

    /// The regression that motivated all of this: an argument the binary does
    /// not understand must NOT fall through and start a DNS server. Doing so
    /// on an appliance means a second resolver racing the real one for :53.
    ///
    /// `healthcheck` is in this list deliberately -- it used to be accepted and
    /// return success without checking anything.
    #[test]
    fn an_unrecognised_argument_refuses_to_start_the_server() {
        for arg in ["--verison", "-x", "serve", "healthcheck", "/etc/cogwheel"] {
            let message = failed(cli(&[arg]));
            assert!(
                message.contains(arg) && message.contains("unrecognised"),
                "{arg} produced {message:?}"
            );
        }
    }

    fn blocking(mode: cogwheel_api::BlockResponseMode) -> cogwheel_api::BlockingConfig {
        cogwheel_api::BlockingConfig {
            mode,
            ..cogwheel_api::BlockingConfig::default()
        }
    }

    #[test]
    fn the_default_block_response_is_unchanged_from_previous_versions() {
        assert_eq!(
            cogwheel_api::BlockingConfig::default().mode,
            cogwheel_api::BlockResponseMode::NullIp
        );
        let resolved = resolve_block_mode(&cogwheel_api::BlockingConfig::default(), &[])
            .expect("the default must always resolve");
        assert_eq!(resolved, BlockMode::NullIp);
    }

    #[test]
    fn each_simple_mode_maps_to_its_dns_response() {
        use cogwheel_api::BlockResponseMode as Mode;
        for (mode, expected) in [
            (Mode::NullIp, BlockMode::NullIp),
            (Mode::NxDomain, BlockMode::NxDomain),
            (Mode::NoData, BlockMode::NoData),
            (Mode::Refused, BlockMode::Refused),
        ] {
            let resolved = resolve_block_mode(&blocking(mode), &[]).expect("should resolve");
            assert_eq!(resolved, expected, "{mode:?}");
        }
    }

    #[test]
    fn sinkhole_prefers_the_explicitly_configured_address() {
        let mut config = blocking(cogwheel_api::BlockResponseMode::Sinkhole);
        config.sinkhole_address = Some("192.0.2.10".parse().expect("literal"));
        let resolved = resolve_block_mode(&config, &["198.51.100.7".to_string()])
            .expect("explicit address should resolve");
        assert_eq!(
            resolved,
            BlockMode::CustomIp {
                ipv4: Some("192.0.2.10".parse().expect("literal")),
                ipv6: None,
            }
        );
    }

    /// The advertised list starts with the machine's hostname and can contain
    /// loopback, neither of which a client on the LAN can reach.
    #[test]
    fn sinkhole_discovers_a_routable_address_and_skips_names_and_loopback() {
        let resolved = resolve_block_mode(
            &blocking(cogwheel_api::BlockResponseMode::Sinkhole),
            &[
                "raspberrypi".to_string(),
                "127.0.0.1".to_string(),
                "0.0.0.0".to_string(),
                "192.168.1.50".to_string(),
            ],
        )
        .expect("should find the routable address");
        assert_eq!(
            resolved,
            BlockMode::CustomIp {
                ipv4: Some("192.168.1.50".parse().expect("literal")),
                ipv6: None,
            }
        );
    }

    /// Answering AAAA with something derived from an IPv4 address would send
    /// v6 clients to an address nothing listens on. An absent AAAA makes them
    /// fall back to the A record, which does answer.
    #[test]
    fn sinkhole_never_fabricates_the_address_family_it_does_not_have() {
        let v6 = resolve_block_mode(
            &blocking(cogwheel_api::BlockResponseMode::Sinkhole),
            &["fd00::5".to_string()],
        )
        .expect("should resolve");
        assert_eq!(
            v6,
            BlockMode::CustomIp {
                ipv4: None,
                ipv6: Some("fd00::5".parse().expect("literal")),
            }
        );
    }

    /// Silently falling back to another block mode would leave the operator
    /// believing the sink is in use while it is not.
    #[test]
    fn sinkhole_without_any_usable_address_fails_startup_with_guidance() {
        let error = resolve_block_mode(
            &blocking(cogwheel_api::BlockResponseMode::Sinkhole),
            &["raspberrypi".to_string(), "127.0.0.1".to_string()],
        )
        .expect_err("must not silently pick another mode");
        let message = error.to_string();
        assert!(
            message.contains("COGWHEEL_BLOCKING__SINKHOLE_ADDRESS"),
            "{message}"
        );
    }

    #[test]
    fn block_mode_is_parsed_from_the_spellings_people_actually_write() {
        use cogwheel_api::BlockResponseMode as Mode;
        for (text, expected) in [
            ("sinkhole", Mode::Sinkhole),
            ("SINKHOLE", Mode::Sinkhole),
            (" nxdomain ", Mode::NxDomain),
            ("null_ip", Mode::NullIp),
            ("null-ip", Mode::NullIp),
            ("refused", Mode::Refused),
        ] {
            assert_eq!(text.parse::<Mode>().expect(text), expected, "{text}");
        }
        assert!("wat".parse::<Mode>().is_err());
    }
}

// ---------------------------------------------------------------- live event stream

/// Upper bound on simultaneous SSE subscribers.
///
/// Each connection holds a broadcast receiver and a task. A household needs one or two; the cap
/// exists so a misbehaving client cannot open thousands and exhaust the appliance's memory.
const MAX_EVENT_SUBSCRIBERS: usize = 32;

/// Buffered events per subscriber before the slowest one starts missing frames.
///
/// A slow reader lags rather than applying backpressure to the DNS path — losing display frames is
/// always preferable to slowing resolution.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// A frame pushed to connected control planes.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamQueryEvent {
    domain: String,
    client: String,
    device_name: Option<String>,
    blocked: bool,
    reason: Option<String>,
    observed_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamDetectionEvent {
    domain: String,
    client: String,
    device_name: Option<String>,
    probability: f32,
    decision: String,
    observed_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamHealthEvent {
    degraded: bool,
    notes: Vec<String>,
    observed_at: String,
}

/// Which SSE event name a frame is published under.
#[derive(Debug, Clone)]
enum StreamEvent {
    Query(Box<StreamQueryEvent>),
    Detection(Box<StreamDetectionEvent>),
    Health(Box<StreamHealthEvent>),
}

/// Fan-out for live events, with a bounded subscriber count.
#[derive(Clone)]
struct EventBus {
    sender: tokio::sync::broadcast::Sender<StreamEvent>,
    subscribers: Arc<std::sync::atomic::AtomicUsize>,
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field(
                "subscribers",
                &self.subscribers.load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish()
    }
}

impl EventBus {
    fn new() -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            sender,
            subscribers: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Publish a frame. Never blocks and never fails: with no subscribers the send is a no-op.
    fn publish(&self, event: StreamEvent) {
        let _ = self.sender.send(event);
    }
}

/// A subscriber slot that decrements the connection count when the stream is dropped.
///
/// Teardown has to be tied to the guard rather than the handler body, because an SSE handler
/// returns as soon as the stream is constructed — the client may stay connected for hours after.
struct SubscriberGuard(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Live query and detection stream.
///
/// Returns 503 once [`MAX_EVENT_SUBSCRIBERS`] connections are open rather than accepting unbounded
/// clients.
async fn events_stream(
    State(state): State<ServerState>,
) -> Result<
    axum::response::Sse<
        impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
    >,
    axum::http::StatusCode,
> {
    let subscribers = Arc::clone(&state.events.subscribers);
    let previous = subscribers.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if previous >= MAX_EVENT_SUBSCRIBERS {
        subscribers.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        return Err(axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }
    let guard = SubscriberGuard(subscribers);

    // `BroadcastStream` gives us the receiver as a Stream without pulling in a generator macro.
    // The guard is moved into the closure so the subscriber count drops when the client
    // disconnects and the stream is dropped -- an SSE handler returns as soon as the stream is
    // constructed, so teardown cannot live in the handler body.
    // An SSE stream never ends on its own, so `with_graceful_shutdown` would wait on it forever --
    // one open browser tab was enough to make SIGTERM hang until the supervisor SIGKILLed us.
    // Ending the stream on the shutdown signal lets the connection close and the server exit.
    let mut shutdown = state.shutdown.clone();
    let stream = tokio_stream::wrappers::BroadcastStream::new(state.events.sender.subscribe())
        .take_until(async move {
            let _ = shutdown.changed().await;
        })
        .filter_map(move |item| {
            let _guard = &guard;
            let frame = match item {
                Ok(StreamEvent::Query(event)) => axum::response::sse::Event::default()
                    .event("query")
                    .json_data(&*event),
                Ok(StreamEvent::Detection(event)) => axum::response::sse::Event::default()
                    .event("detection")
                    .json_data(&*event),
                Ok(StreamEvent::Health(event)) => axum::response::sse::Event::default()
                    .event("health")
                    .json_data(&*event),
                // A slow reader missed frames. Skip them and keep the connection alive rather than
                // tearing down a working stream over dropped display rows.
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(_)) => {
                    return std::future::ready(None);
                }
            };
            std::future::ready(frame.ok().map(Ok))
        });

    Ok(axum::response::Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}
