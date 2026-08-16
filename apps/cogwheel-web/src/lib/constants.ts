import type {
  BlockProfileListRecord,
  BlockProfileRecord,
  ClassifierSensitivity,
  DashboardSummary,
  FederatedLearningSettings,
  LatencyBudgetStatus,
  ResolverAccessStatus,
  SettingsSummary,
  SyncNodeStatus,
  TailscaleDnsCheckResult,
  TailscaleStatus,
  ThreatIntelSettings,
} from "@/lib/api";

/**
 * Neutral defaults so screens can render structure before the first response
 * lands, instead of null-checking every field at every use site.
 */

export const emptyRuntimeSnapshot = {
  upstream_failures_total: 0,
  fallback_served_total: 0,
  cache_hits_total: 0,
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
};

export const emptyDashboard: DashboardSummary = {
  protection_status: "Loading",
  protection_paused_until: null,
  active_ruleset: null,
  source_count: 0,
  enabled_source_count: 0,
  service_toggle_count: 0,
  device_count: 0,
  runtime_health: { snapshot: emptyRuntimeSnapshot, degraded: false, notes: [] },
  latest_audit_events: [],
  recent_security_events: [],
  recent_notification_deliveries: [],
  notification_health: {
    delivered_count: 0,
    failed_count: 0,
    last_delivery_at: null,
    last_failure_at: null,
  },
  notification_failure_analytics: { success_rate_percent: 100, top_failed_domains: [] },
  security_summary: { medium_count: 0, high_count: 0, critical_count: 0, top_devices: [] },
  domain_insights: { top_queried_domains: [], top_blocked_domains: [], observed_queries: 0 },
};

export const emptySettings: SettingsSummary = {
  blocklists: [],
  blocklist_statuses: [],
  block_profiles: [],
  devices: [],
  services: [],
  classifier: { mode: "Monitor", threshold: 0.92 },
  notifications: { enabled: false, webhook_url: null, min_severity: "high" },
  notification_test_presets: [],
  runtime_guard: { probe_domains: [], max_upstream_failures_delta: 0, max_fallback_served_delta: 0 },
};

export const emptySyncStatus: SyncNodeStatus = {
  local_node_public_key: "",
  profile: "full",
  revision: 0,
  transport_mode: "opportunistic",
  transport_token_configured: false,
  replay_cache_entries: 0,
  peers: [],
};

export const emptyTailscaleStatus: TailscaleStatus = {
  installed: false,
  daemon_running: false,
  backend_state: null,
  hostname: null,
  tailnet_name: null,
  peer_count: 0,
  exit_node_active: false,
  version: null,
  health_warnings: [],
  last_error: null,
};

export const emptyTailscaleDnsCheck: TailscaleDnsCheckResult = {
  configured: false,
  message: "",
  local_dns_server: null,
  suggestions: [],
};

export const emptyThreatIntel: ThreatIntelSettings = { providers: [], recommendations: [] };

export const emptyFederatedLearning: FederatedLearningSettings = {
  enabled: false,
  coordinator_url: null,
  node_id: "local-node",
  round_interval_hours: 24,
  last_round_at: null,
  last_model_version: null,
  privacy_mode: "model-updates-only",
  raw_log_export_enabled: false,
  recommendations: [],
};

export const emptyLatencyBudget: LatencyBudgetStatus = {
  within_budget: true,
  cache_hit_rate: 0,
  checks: [],
  recommendations: [],
};

export const emptyResolverAccess: ResolverAccessStatus = {
  hostname: null,
  dns_targets: [],
  tailscale_ip: null,
  notes: [],
};

export const emptyBlockProfileDraft: BlockProfileRecord = {
  id: "",
  emoji: "",
  name: "",
  description: "",
  blocklists: [],
  allowlists: [],
  updated_at: new Date(0).toISOString(),
};

/**
 * The four presets the backend canonicalises against
 * (`normalize_block_profile_lists`). Core and NSFW families are mutually
 * exclusive: picking the big list drops the small one and vice versa.
 */
export const oisdProfileOptions: BlockProfileListRecord[] = [
  { id: "oisd-small", name: "OISD Small", url: "https://small.oisd.nl", kind: "preset", family: "core-small" },
  { id: "oisd-big", name: "OISD Big", url: "https://big.oisd.nl", kind: "preset", family: "core-full" },
  {
    id: "oisd-nsfw-small",
    name: "OISD NSFW Small",
    url: "https://nsfw-small.oisd.nl",
    kind: "preset",
    family: "nsfw-small",
  },
  { id: "oisd-nsfw", name: "OISD NSFW", url: "https://nsfw.oisd.nl", kind: "preset", family: "nsfw-full" },
];

export const MUTUALLY_EXCLUSIVE_PRESETS: Record<string, string> = {
  "oisd-big": "oisd-small",
  "oisd-small": "oisd-big",
  "oisd-nsfw": "oisd-nsfw-small",
  "oisd-nsfw-small": "oisd-nsfw",
};

export const CACHE_KEYS = {
  dashboard: "cogwheel_dashboard_cache",
  settings: "cogwheel_settings_cache",
  syncStatus: "cogwheel_sync_status_cache",
  tailscale: "cogwheel_tailscale_cache",
  tailscaleDns: "cogwheel_tailscale_dns_cache",
  threatIntel: "cogwheel_threat_intel_cache",
  federatedLearning: "cogwheel_federated_learning_cache",
  latencyBudget: "cogwheel_latency_budget_cache",
  resolverAccess: "cogwheel_resolver_access_cache",
  classifier: "cogwheel_classifier_cache",
} as const;

export const THEME_STORAGE_KEY = "cogwheel-theme";

/** Poll cadence for the shared control-plane snapshot. */
export const REFRESH_INTERVAL_MS = 5_000;

/** Fixed by the backend: `normalize_notification_window` accepts only 10/50/100. */
export const NOTIFICATION_WINDOW = 30;
export const NOTIFICATION_HISTORY_WINDOW = 10;

/** Rows kept in the live activity buffer before the oldest are dropped. */
export const ACTIVITY_BUFFER_LIMIT = 500;

export const SNOOZE_OPTIONS = [5, 15, 60] as const;

export const SENSITIVITY_ORDER: ClassifierSensitivity[] = ["low", "balanced", "high"];

export const SENSITIVITY_LABEL: Record<ClassifierSensitivity, string> = {
  low: "Low",
  balanced: "Balanced",
  high: "High",
};

export const SENSITIVITY_BLURB: Record<ClassifierSensitivity, string> = {
  low: "Blocks only the clearest ad domains. Fewest surprises.",
  balanced: "The default trade-off between coverage and mistakes.",
  high: "Catches more ad domains and mis-blocks more legitimate ones.",
};

export const CLASSIFIER_MODE_BLURB = {
  off: "The model is not consulted. Only blocklists and service rules apply.",
  monitor: "Verdicts are recorded and shown here, but nothing is blocked by the model.",
  protect: "Domains scoring above the active threshold are blocked.",
} as const;
