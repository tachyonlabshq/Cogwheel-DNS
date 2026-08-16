/**
 * The complete Cogwheel control-plane HTTP contract.
 *
 * Field naming is deliberately inconsistent because the wire is: every legacy
 * `/api/v1` handler serialises Rust structs verbatim (snake_case fields,
 * PascalCase enums), while the rewritten classifier endpoints emit camelCase.
 * Renaming either side here would only hide the seam, so the types mirror the
 * wire exactly and the UI layer does the translating.
 */

const API_BASE =
  import.meta.env.VITE_COGWHEEL_API_BASE ??
  (typeof window !== "undefined" ? window.location.origin : "http://127.0.0.1:8080");

/** Thrown for every non-2xx response so callers can branch on status. */
export class ApiError extends Error {
  readonly status: number;
  readonly path: string;

  constructor(message: string, status: number, path: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.path = path;
  }
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return "Unknown error";
}

type RequestOptions = { signal?: AbortSignal };

async function request(path: string, init: RequestInit, options?: RequestOptions): Promise<Response> {
  let response: Response;
  try {
    response = await fetch(`${API_BASE}${path}`, {
      ...init,
      signal: options?.signal,
      headers: {
        "Content-Type": "application/json",
        // Marks the call as programmatic so the SPA fallback cannot be mistaken
        // for a navigation and answered with index.html.
        "X-Requested-With": "XMLHttpRequest",
        ...(init.headers ?? {}),
      },
    });
  } catch (cause) {
    if (cause instanceof DOMException && cause.name === "AbortError") throw cause;
    throw new ApiError("The control plane is unreachable.", 0, path);
  }

  if (!response.ok) {
    // Style-A handlers answer with an empty body, so fall back to the status line.
    const detail = (await response.text().catch(() => "")).trim();
    throw new ApiError(detail || `${response.status} ${response.statusText}`, response.status, path);
  }

  return response;
}

/** Every JSON handler wraps its payload in `{ data: T }`. */
async function fetchJson<T>(path: string, init: RequestInit = {}, options?: RequestOptions): Promise<T> {
  const response = await request(path, init, options);
  const payload = (await response.json()) as { data: T };
  return payload.data;
}

/** `pause`/`resume` return HTTP 200 with an empty body, breaking the envelope. */
async function fetchVoid(path: string, init: RequestInit = {}, options?: RequestOptions): Promise<void> {
  await request(path, init, options);
}

const post = (body?: unknown): RequestInit => ({
  method: "POST",
  ...(body === undefined ? {} : { body: JSON.stringify(body) }),
});

/* ------------------------------------------------------------------------- */
/* Legacy `/api/v1` types — snake_case, matching the Rust serialisation.       */
/* ------------------------------------------------------------------------- */

export type DnsRuntimeSnapshot = {
  upstream_failures_total: number;
  fallback_served_total: number;
  cache_hits_total: number;
  cname_uncloaks_total: number;
  cname_blocks_total: number;
  queries_total: number;
  blocked_total: number;
  cache_hit_latency_avg_ns: number;
  cache_hit_samples: number;
  cache_miss_latency_avg_ns: number;
  cache_miss_samples: number;
  classifier_latency_avg_ns: number;
  classifier_latency_samples: number;
};

export type RuntimeHealth = {
  snapshot: DnsRuntimeSnapshot;
  degraded: boolean;
  notes: string[];
};

export type RulesetSummary = {
  id: string;
  hash: string;
  status: string;
  created_at: string;
};

export type AuditEvent = {
  id: string;
  event_type: string;
  /** JSON, double-encoded as a string on the wire. */
  payload: string;
  created_at: string;
};

export type SourceRecord = {
  id: string;
  name: string;
  url: string;
  kind: string;
  enabled: boolean;
  refresh_interval_minutes: number;
  profile: string;
  verification_strictness: string;
};

export type BlocklistStatus = {
  id: string;
  name: string;
  last_refresh_attempt_at: string | null;
  due_for_refresh: boolean;
};

export type ServiceMode = "Inherit" | "Allow" | "Block";

export type ServiceManifest = {
  service_id: string;
  display_name: string;
  category: string;
  risk_notes: string;
  allow_domains: string[];
  block_domains: string[];
  exceptions: string[];
};

export type ServiceToggle = {
  manifest: ServiceManifest;
  mode: ServiceMode;
};

export type DeviceServiceOverride = {
  service_id: string;
  mode: "allow" | "block";
};

export type DeviceRecord = {
  id: string;
  name: string;
  ip_address: string;
  policy_mode: "global" | "custom";
  blocklist_profile_override: string | null;
  protection_override: "inherit" | "bypass";
  allowed_domains: string[];
  service_overrides: DeviceServiceOverride[];
};

export type BlockProfileListRecord = {
  id: string;
  name: string;
  url: string;
  kind: string;
  family: string;
};

export type BlockProfileRecord = {
  id: string;
  emoji: string;
  name: string;
  description: string;
  blocklists: BlockProfileListRecord[];
  allowlists: string[];
  updated_at: string;
};

export type SecurityEventRecord = {
  id: string;
  device_id: string | null;
  device_name: string | null;
  client_ip: string;
  domain: string;
  classifier_score: number;
  severity: string;
  created_at: string;
};

export type DeviceSecuritySummary = {
  label: string;
  event_count: number;
  highest_severity: string;
};

export type SecuritySummary = {
  medium_count: number;
  high_count: number;
  critical_count: number;
  top_devices: DeviceSecuritySummary[];
};

export type DomainInsightEntry = { domain: string; count: number };

export type DomainInsights = {
  top_queried_domains: DomainInsightEntry[];
  top_blocked_domains: DomainInsightEntry[];
  observed_queries: number;
};

export type NotificationSeverity = "medium" | "high" | "critical";

export type NotificationSettings = {
  enabled: boolean;
  webhook_url: string | null;
  min_severity: NotificationSeverity;
};

export type NotificationDeliveryEvent = {
  status: string;
  event_type: string;
  severity: string;
  title: string;
  summary: string;
  target: string;
  domain: string;
  device_name: string | null;
  client_ip: string;
  attempts: number;
  created_at: string;
};

export type NotificationHealthSummary = {
  delivered_count: number;
  failed_count: number;
  last_delivery_at: string | null;
  last_failure_at: string | null;
};

export type NotificationFailureDomain = { domain: string; failure_count: number };

export type NotificationFailureAnalytics = {
  success_rate_percent: number;
  top_failed_domains: NotificationFailureDomain[];
};

export type NotificationTestResult = { outcome: string; target: string };

export type NotificationTestRequest = {
  domain?: string;
  severity?: NotificationSeverity;
  device_name?: string;
  dry_run?: boolean;
};

export type NotificationTestPreset = {
  name: string;
  domain: string;
  severity: NotificationSeverity;
  device_name: string;
  dry_run: boolean;
};

export type DashboardSummary = {
  protection_status: string;
  protection_paused_until: string | null;
  active_ruleset: RulesetSummary | null;
  source_count: number;
  enabled_source_count: number;
  service_toggle_count: number;
  device_count: number;
  runtime_health: RuntimeHealth;
  latest_audit_events: AuditEvent[];
  recent_security_events: SecurityEventRecord[];
  recent_notification_deliveries: NotificationDeliveryEvent[];
  notification_health: NotificationHealthSummary;
  notification_failure_analytics: NotificationFailureAnalytics;
  security_summary: SecuritySummary;
  domain_insights: DomainInsights;
};

export type LegacyClassifierSettings = {
  mode: "Off" | "Monitor" | "Protect";
  threshold: number;
};

export type RuntimeGuardConfig = {
  probe_domains: string[];
  max_upstream_failures_delta: number;
  max_fallback_served_delta: number;
};

export type SettingsSummary = {
  blocklists: SourceRecord[];
  blocklist_statuses: BlocklistStatus[];
  block_profiles: BlockProfileRecord[];
  devices: DeviceRecord[];
  services: ServiceToggle[];
  classifier: LegacyClassifierSettings;
  notifications: NotificationSettings;
  notification_test_presets: NotificationTestPreset[];
  runtime_guard: RuntimeGuardConfig;
};

export type SyncPeerStatus = {
  node_public_key: string;
  imports: number;
  last_import_at: string;
  last_revision: number;
  profile: string;
};

export type SyncNodeStatus = {
  local_node_public_key: string;
  profile: string;
  revision: number;
  transport_mode: string;
  transport_token_configured: boolean;
  replay_cache_entries: number;
  peers: SyncPeerStatus[];
};

export type SyncProfileView = { profile: string };
export type SyncTransportView = { mode: string; token_configured: boolean };

export type TailscaleStatus = {
  installed: boolean;
  daemon_running: boolean;
  backend_state: string | null;
  hostname: string | null;
  tailnet_name: string | null;
  peer_count: number;
  exit_node_active: boolean;
  version: string | null;
  health_warnings: string[];
  last_error: string | null;
};

export type TailscaleExitNodeResult = { success: boolean; message: string };
export type TailscaleRollbackResult = {
  success: boolean;
  message: string;
  previous_state: boolean | null;
};
export type TailscaleDnsCheckResult = {
  configured: boolean;
  message: string;
  local_dns_server: string | null;
  suggestions: string[];
};

export type LoadTestResult = {
  success: boolean;
  queries_sent: number;
  queries_succeeded: number;
  queries_failed: number;
  avg_latency_ms: number;
  p95_latency_ms: number;
  p99_latency_ms: number;
  /** Echoed back from the request; the server does not measure it. */
  cache_hit_ratio: number;
  throughput_qps: number;
  errors: string[];
};

export type FalsePositiveBudgetStatus = {
  release_ready: boolean;
  blocking_rate: number;
  blocked_total: number;
  queries_total: number;
  false_positive_estimate: number;
  budget_remaining: number;
  budget_limit: number;
  recommendations: string[];
};

export type LatencyBudgetCheck = {
  label: string;
  observed_ms: number;
  target_p50_ms: number;
  sample_count: number;
  status: string;
};

export type LatencyBudgetStatus = {
  within_budget: boolean;
  cache_hit_rate: number;
  checks: LatencyBudgetCheck[];
  recommendations: string[];
};

export type ConfigVersionStatus = {
  schema_version: number;
  config_version: number;
  cogwheel_version: string;
  migration_count: number;
  upgrade_available: boolean;
  recommendations: string[];
};

export type ThreatIntelProviderConfig = {
  id: string;
  display_name: string;
  enabled: boolean;
  feed_url: string | null;
  api_key_configured: boolean;
  update_interval_minutes: number;
  last_sync_at: string | null;
  last_error: string | null;
  capabilities: string[];
};

export type ThreatIntelSettings = {
  providers: ThreatIntelProviderConfig[];
  recommendations: string[];
};

export type FederatedLearningSettings = {
  enabled: boolean;
  coordinator_url: string | null;
  node_id: string;
  round_interval_hours: number;
  last_round_at: string | null;
  last_model_version: string | null;
  privacy_mode: string;
  raw_log_export_enabled: boolean;
  recommendations: string[];
};

export type ResolverAccessStatus = {
  hostname: string | null;
  dns_targets: string[];
  tailscale_ip: string | null;
  notes: string[];
};

export type RefreshResponse = {
  outcome: string;
  ruleset?: RulesetSummary | null;
  notes: string[];
};

export type BackupData = {
  version: string;
  created_at: string;
  sources: SourceRecord[];
  devices: DeviceRecord[];
  classifier: LegacyClassifierSettings;
  notifications: NotificationSettings;
};

export type BackupResult = { success: boolean; message: string; size_bytes: number };

/** Signed replication envelope. The payload is base64url of a SyncStatePayloadV1. */
export type SyncEnvelope = {
  node_public_key: string;
  timestamp: string;
  nonce: string;
  payload_b64: string;
  signature_b64: string;
};

export type SyncImportResult = {
  imported_sources: number;
  imported_devices: number;
  applied_revision: number;
  profile: string;
};

export type ResilienceDrillResult = {
  drill_type: string;
  success: boolean;
  message: string;
  recommendations: string[];
};

export type ResilienceDrill =
  | "upstream-outage"
  | "db-corruption"
  | "source-failure"
  | "sync-partition";

/* ------------------------------------------------------------------------- */
/* Rewritten classifier contract — camelCase (04-design-system.md §5).         */
/* ------------------------------------------------------------------------- */

export type ClassifierMode = "off" | "monitor" | "protect";
export type ClassifierSensitivity = "low" | "balanced" | "high";

export type SensitivityTriple = Record<ClassifierSensitivity, number>;

/**
 * What on-device adaptation is doing right now, and on what evidence.
 *
 * The shipped model is never rewritten. A "delta" is a bounded additive correction
 * stored beside it, so `active: false` means scoring is bit-identical to the model
 * as it left the factory. The measurements are only reported while the delta they
 * were measured on is the one actually scoring traffic.
 */
export type ClassifierAdaptation = {
  active: boolean;
  /** RFC3339, or null when no correction is active. */
  trainedAt: string | null;
  /** Feedback items the active correction was trained on. */
  exampleCount: number;
  /** Hashed character-run entries the correction carries. */
  ngramEntries: number;
  /** ROC-AUC of base+correction on the committed holdout; null when nothing is active. */
  rocAuc: number | null;
  falsePositiveRate: SensitivityTriple | null;
  /** Certified worst-case effect of the correction on any score, in logits. */
  maxLogitShift: number;
  /** The ceiling `maxLogitShift` is held under. */
  logitBudget: number;
  /** Reports stored but not yet turned into a correction. */
  pendingFeedback: number;
  /** Reports required before adaptation can be judged at all. */
  minimumFeedback: number;
};

export type ClassifierFeedbackResult = {
  /** The normalised host the appliance stored. */
  domain: string;
  isAd: boolean;
  pendingFeedback: number;
  minimumFeedback: number;
};

/**
 * The result of training a correction and putting it in front of the promotion gate.
 *
 * `rejected` is a healthy outcome, not a failure: the gate measured the correction
 * against the committed holdout and refused to install something that scored worse.
 * Nothing changes on the appliance in that case.
 */
export type AdaptationOutcome = {
  status: "promoted" | "rejected" | "notEnoughData";
  promoted: boolean;
  /** Set only on rejection; names the exact criterion that failed. Render verbatim. */
  reason: string | null;
  rocAuc: number | null;
  falsePositiveRate: SensitivityTriple | null;
  /** Set only on promotion. */
  exampleCount: number | null;
  /** Both set only when there was too little feedback to judge. */
  have: number | null;
  need: number | null;
  /** The adaptation state the caller is now in, after the outcome was applied. */
  adaptation: ClassifierAdaptation;
};

export type ClassifierStatus = {
  settings: { mode: ClassifierMode; sensitivity: ClassifierSensitivity };
  model: {
    version: number;
    trainedAt: string;
    rocAuc: number;
    prAuc: number;
    residentBytes: number;
    thresholds: SensitivityTriple;
    /** Measured on the held-out split, expressed as a fraction (0.00099 = 0.099%). */
    falsePositiveRate: SensitivityTriple;
    recall: SensitivityTriple;
  };
  stats: {
    scored: number;
    cacheHits: number;
    cacheMisses: number;
    dropped: number;
    blocked: number;
    protectedOverrides: number;
    hookPanics: number;
    cachedEntries: number;
  };
  activeThreshold: number;
  adaptation: ClassifierAdaptation;
};

export type InspectionContribution = {
  label: string;
  kind: "dense" | "ngram";
  /** Signed contribution to the score: positive pushes toward "ad domain". */
  value: number;
};

export type Inspection = {
  domain: string;
  probability: number;
  protected: boolean;
  decision: "allow" | "block";
  activeThreshold: number;
  blocklistMatch: string | null;
  contributions: InspectionContribution[];
};

export type ClassifierDetection = {
  domain: string;
  probability: number;
  decision: "allow" | "block";
  protected: boolean;
  observedAt: string;
  client: string;
};

/* ------------------------------------------------------------------------- */
/* Server-sent events (`GET /api/v1/events/stream`).                          */
/* ------------------------------------------------------------------------- */

export type StreamQueryEvent = {
  domain: string;
  client: string;
  deviceName?: string | null;
  blocked: boolean;
  reason?: string | null;
  observedAt: string;
  latencyMs?: number | null;
};

export type StreamDetectionEvent = {
  domain: string;
  client: string;
  deviceName?: string | null;
  probability: number;
  decision: "allow" | "block";
  observedAt: string;
};

export type StreamHealthEvent = {
  degraded: boolean;
  notes?: string[];
  observedAt?: string;
};

export const eventsStreamUrl = `${API_BASE}/api/v1/events/stream`;

/* ------------------------------------------------------------------------- */
/* Client                                                                     */
/* ------------------------------------------------------------------------- */

export const api = {
  /* --- Read: overview ---------------------------------------------------- */
  dashboard: (notificationWindow?: number, notificationHistoryWindow?: number, options?: RequestOptions) => {
    const params = new URLSearchParams();
    if (notificationWindow !== undefined) params.set("notification_window", String(notificationWindow));
    if (notificationHistoryWindow !== undefined)
      params.set("notification_history_window", String(notificationHistoryWindow));
    const query = params.toString();
    return fetchJson<DashboardSummary>(`/api/v1/dashboard${query ? `?${query}` : ""}`, {}, options);
  },
  settings: (options?: RequestOptions) => fetchJson<SettingsSummary>("/api/v1/settings", {}, options),
  runtimeSnapshot: (options?: RequestOptions) =>
    fetchJson<DnsRuntimeSnapshot>("/api/v1/runtime", {}, options),
  runtimeHealth: (options?: RequestOptions) => fetchJson<RuntimeHealth>("/api/v1/runtime/health", {}, options),
  resolverAccess: (options?: RequestOptions) =>
    fetchJson<ResolverAccessStatus>("/api/v1/resolver-access", {}, options),
  securityEvents: (options?: RequestOptions) =>
    fetchJson<SecurityEventRecord[]>("/api/v1/security-events", {}, options),
  auditEvents: (options?: RequestOptions) => fetchJson<AuditEvent[]>("/api/v1/audit-events", {}, options),
  devices: (options?: RequestOptions) => fetchJson<DeviceRecord[]>("/api/v1/devices", {}, options),
  sources: (options?: RequestOptions) => fetchJson<SourceRecord[]>("/api/v1/sources", {}, options),
  services: (options?: RequestOptions) => fetchJson<ServiceToggle[]>("/api/v1/services", {}, options),
  rulesets: (options?: RequestOptions) => fetchJson<RulesetSummary[]>("/api/v1/rulesets", {}, options),
  latencyBudget: (options?: RequestOptions) =>
    fetchJson<LatencyBudgetStatus>("/api/v1/latency-budget", {}, options),
  falsePositiveBudget: (options?: RequestOptions) =>
    fetchJson<FalsePositiveBudgetStatus>("/api/v1/false-positive-budget", {}, options),
  configVersion: (options?: RequestOptions) =>
    fetchJson<ConfigVersionStatus>("/api/v1/config/version", {}, options),
  backup: (options?: RequestOptions) => fetchJson<BackupData>("/api/v1/backup", {}, options),

  /* --- Read: sync / tailscale -------------------------------------------- */
  syncStatus: (options?: RequestOptions) => fetchJson<SyncNodeStatus>("/api/v1/sync/status", {}, options),
  syncProfile: (options?: RequestOptions) => fetchJson<SyncProfileView>("/api/v1/sync/profile", {}, options),
  syncTransport: (options?: RequestOptions) =>
    fetchJson<SyncTransportView>("/api/v1/sync/transport", {}, options),
  tailscaleStatus: (options?: RequestOptions) =>
    fetchJson<TailscaleStatus>("/api/v1/tailscale/status", {}, options),
  tailscaleDnsCheck: (options?: RequestOptions) =>
    fetchJson<TailscaleDnsCheckResult>("/api/v1/tailscale/dns-check", {}, options),

  /* --- Read: integrations ------------------------------------------------ */
  threatIntelProviders: (options?: RequestOptions) =>
    fetchJson<ThreatIntelSettings>("/api/v1/threat-intel/providers", {}, options),
  federatedLearningStatus: (options?: RequestOptions) =>
    fetchJson<FederatedLearningSettings>("/api/v1/federated-learning/status", {}, options),

  /* --- Classifier (rewritten contract) ----------------------------------- */
  classifier: (options?: RequestOptions) => fetchJson<ClassifierStatus>("/api/v1/classifier", {}, options),
  updateClassifier: (mode: ClassifierMode, sensitivity: ClassifierSensitivity) =>
    fetchJson<ClassifierStatus>("/api/v1/classifier/settings", post({ mode, sensitivity })),
  inspectDomain: (domain: string, options?: RequestOptions) =>
    fetchJson<Inspection>("/api/v1/classifier/inspect", post({ domain }), options),
  classifierDetections: (limit = 50, options?: RequestOptions) =>
    fetchJson<ClassifierDetection[]>(`/api/v1/classifier/detections?limit=${limit}`, {}, options),

  /* --- Classifier adaptation ---------------------------------------------- */
  /** Stores one correction. Nothing is trained and no score moves until `adaptClassifier`. */
  classifierFeedback: (domain: string, isAd: boolean) =>
    fetchJson<ClassifierFeedbackResult>("/api/v1/classifier/feedback", post({ domain, isAd })),
  /** Trains a correction from stored feedback and installs it only if it clears the gate. */
  adaptClassifier: () => fetchJson<AdaptationOutcome>("/api/v1/classifier/adapt", post()),
  /** Discards the active correction. The base model was never modified, so this is all rollback is. */
  rollbackClassifierAdaptation: () =>
    fetchJson<ClassifierAdaptation>("/api/v1/classifier/adapt/rollback", post()),

  /* --- Runtime ----------------------------------------------------------- */
  pauseRuntime: (minutes: number) => fetchVoid("/api/v1/runtime/pause", post({ minutes })),
  resumeRuntime: () => fetchVoid("/api/v1/runtime/resume", post()),
  runtimeHealthCheck: () => fetchJson<RuntimeHealth>("/api/v1/runtime/health/check", post()),

  /* --- Sources / blocklists ---------------------------------------------- */
  refreshSources: () => fetchJson<RefreshResponse>("/api/v1/sources/refresh", post()),
  upsertBlocklist: (input: {
    id?: string;
    name: string;
    url: string;
    kind: string;
    enabled: boolean;
    refresh_interval_minutes?: number;
    profile?: string;
    verification_strictness?: string;
  }) => fetchJson<RefreshResponse>("/api/v1/settings/blocklists", post({ ...input, refresh_now: true })),
  setBlocklistEnabled: (id: string, enabled: boolean) =>
    fetchJson<RefreshResponse>("/api/v1/settings/blocklists/state", post({ id, enabled, refresh_now: true })),
  deleteBlocklist: (id: string) =>
    fetchJson<RefreshResponse>("/api/v1/settings/blocklists/delete", post({ id, refresh_now: true })),

  /* --- Services ---------------------------------------------------------- */
  updateService: (service_id: string, mode: ServiceMode) =>
    fetchJson<RefreshResponse>("/api/v1/services/toggles", post({ service_id, mode })),

  /* --- Block profiles ----------------------------------------------------- */
  upsertBlockProfile: (input: {
    id?: string;
    emoji: string;
    name: string;
    description?: string;
    blocklists: BlockProfileListRecord[];
    allowlists: string[];
  }) => fetchJson<BlockProfileRecord[]>("/api/v1/settings/block-profiles", post(input)),
  deleteBlockProfile: (id: string) =>
    fetchJson<BlockProfileRecord[]>("/api/v1/settings/block-profiles/delete", post({ id })),

  /* --- Devices ------------------------------------------------------------ */
  upsertDevice: (input: {
    id?: string;
    name: string;
    ip_address: string;
    policy_mode?: DeviceRecord["policy_mode"];
    blocklist_profile_override?: string | null;
    protection_override?: DeviceRecord["protection_override"];
    allowed_domains?: string[];
    service_overrides?: DeviceServiceOverride[];
  }) => fetchJson<DeviceRecord>("/api/v1/devices", post(input)),

  /* --- Notifications ------------------------------------------------------ */
  updateNotifications: (input: NotificationSettings) =>
    fetchJson<NotificationSettings>("/api/v1/settings/notifications", post(input)),
  testNotifications: (input: NotificationTestRequest = {}) =>
    fetchJson<NotificationTestResult>("/api/v1/settings/notifications/test", post(input)),
  updateNotificationTestPresets: (presets: NotificationTestPreset[]) =>
    fetchJson<NotificationTestPreset[]>("/api/v1/settings/notifications/presets", post({ presets })),

  /* --- Rulesets ----------------------------------------------------------- */
  rollbackRuleset: () => fetchJson<RulesetSummary>("/api/v1/rulesets/rollback", post()),

  /* --- Sync --------------------------------------------------------------- */
  exportSyncState: (profile?: string, options?: RequestOptions) =>
    fetchJson<SyncEnvelope>(
      `/api/v1/sync/export${profile ? `?profile=${encodeURIComponent(profile)}` : ""}`,
      {},
      options,
    ),
  importSyncState: (envelope: SyncEnvelope) =>
    fetchJson<SyncImportResult>("/api/v1/sync/import", post({ envelope })),
  updateSyncProfile: (profile: string) =>
    fetchJson<SyncProfileView>("/api/v1/sync/profile", post({ profile })),
  updateSyncTransport: (mode: string, token?: string) =>
    fetchJson<SyncTransportView>("/api/v1/sync/transport", post({ mode, token })),

  /* --- Tailscale ---------------------------------------------------------- */
  tailscaleExitNode: (enabled: boolean) =>
    fetchJson<TailscaleExitNodeResult>("/api/v1/tailscale/exit-node", post({ enabled })),
  tailscaleRollback: () => fetchJson<TailscaleRollbackResult>("/api/v1/tailscale/rollback", post()),

  /* --- Integrations ------------------------------------------------------- */
  updateThreatIntelProvider: (
    id: string,
    enabled: boolean,
    feed_url: string | null,
    update_interval_minutes: number,
  ) =>
    fetchJson<ThreatIntelSettings>(
      "/api/v1/threat-intel/providers",
      post({ id, enabled, feed_url, update_interval_minutes }),
    ),
  updateFederatedLearning: (
    enabled: boolean,
    coordinator_url: string | null,
    round_interval_hours: number,
  ) =>
    fetchJson<FederatedLearningSettings>(
      "/api/v1/federated-learning/status",
      post({ enabled, coordinator_url, round_interval_hours }),
    ),

  /* --- Diagnostics -------------------------------------------------------- */
  restoreBackup: (data: BackupData) =>
    fetchJson<BackupResult>("/api/v1/backup/restore", post({ data })),
  runResilienceDrill: (drill: ResilienceDrill) =>
    fetchJson<ResilienceDrillResult>(`/api/v1/resilience/${drill}`, post({})),
  runLoadTest: (duration_secs: number, qps: number, cache_hit_ratio: number) =>
    fetchJson<LoadTestResult>("/api/v1/load-test", post({ duration_secs, qps, cache_hit_ratio })),
};
