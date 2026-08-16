import type { AuditEvent, DashboardSummary } from "@/lib/api";

/** `payload` arrives as a JSON string, so it is double-encoded on the wire. */
export function parseAuditPayload(event: AuditEvent): Record<string, unknown> {
  try {
    const parsed: unknown = JSON.parse(event.payload);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>;
    }
  } catch {
    /* Malformed payloads are common enough that they must not break the list. */
  }
  return {};
}

function stringifyAuditValue(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (Array.isArray(value)) return value.map(stringifyAuditValue).filter(Boolean).join(", ");
  if (typeof value === "object") {
    return Object.entries(value as Record<string, unknown>)
      .map(([key, nested]) => `${key}: ${stringifyAuditValue(nested)}`)
      .join(", ");
  }
  return "";
}

function firstNote(payload: Record<string, unknown>): string | null {
  const notes = payload.notes;
  if (Array.isArray(notes) && notes.length > 0) return stringifyAuditValue(notes[0]);
  return null;
}

export type AuditSummary = { title: string; detail: string; category: string };

export const AUDIT_FILTERS = [
  { id: "all", label: "All events" },
  { id: "runtime", label: "Runtime" },
  { id: "notifications", label: "Notifications" },
  { id: "devices", label: "Devices" },
  { id: "rulesets", label: "Rulesets" },
] as const;

export type AuditFilterId = (typeof AUDIT_FILTERS)[number]["id"];

export function matchesAuditFilter(event: AuditEvent, filter: AuditFilterId): boolean {
  switch (filter) {
    case "runtime":
      return event.event_type.startsWith("runtime.");
    case "notifications":
      return event.event_type.startsWith("notification.") || event.event_type.startsWith("security.alert");
    case "devices":
      return event.event_type.startsWith("device.");
    case "rulesets":
      return event.event_type.startsWith("ruleset.");
    default:
      return true;
  }
}

/**
 * Turns a raw audit row into something an operator can read. The special cases
 * are carried over verbatim from the previous UI so no event type silently
 * regresses to the generic fallback.
 */
export function summarizeAuditEvent(event: AuditEvent): AuditSummary {
  const payload = parseAuditPayload(event);
  const category = event.event_type.split(".")[0] ?? event.event_type;

  const generic = (): AuditSummary => {
    const [key, value] = Object.entries(payload)[0] ?? [];
    const rendered = key ? `${key}: ${stringifyAuditValue(value)}` : "";
    return {
      title: event.event_type,
      detail: rendered || "No structured payload details recorded.",
      category,
    };
  };

  if (event.event_type === "ruleset.rollback") {
    const hash = typeof payload.hash === "string" ? payload.hash : "";
    return {
      title: "Ruleset rollback completed",
      detail: hash
        ? `Recovered ruleset ${hash.slice(0, 12)} after an operator-triggered rollback.`
        : "Recovered the previous ruleset after an operator-triggered rollback.",
      category,
    };
  }

  if (event.event_type === "ruleset.auto_rollback") {
    return {
      title: "Automatic rollback triggered",
      detail: firstNote(payload) ?? "The runtime guard rejected the new ruleset and restored the previous one.",
      category,
    };
  }

  if (event.event_type === "ruleset.refresh_rejected") {
    return {
      title: "Ruleset refresh rejected",
      detail: firstNote(payload) ?? "The candidate ruleset failed verification and was not activated.",
      category,
    };
  }

  if (event.event_type.startsWith("notification.delivery_") || event.event_type.startsWith("security.alert_delivery_")) {
    const title =
      (typeof payload.title === "string" && payload.title) ||
      (typeof payload.domain === "string" && payload.domain) ||
      "Notification delivery";
    const severity = typeof payload.severity === "string" ? payload.severity : "unknown";
    const target =
      (typeof payload.client_ip === "string" && payload.client_ip) ||
      (typeof payload.device_name === "string" && payload.device_name) ||
      "control-plane";
    return {
      title,
      detail:
        (typeof payload.summary === "string" && payload.summary) || `${severity} delivery to ${target}.`,
      category,
    };
  }

  if (event.event_type.startsWith("runtime.health_check_")) {
    const degraded = event.event_type.endsWith("degraded");
    return {
      title: degraded ? "Runtime health check found a regression" : "Runtime health check passed",
      detail: firstNote(payload) ?? "Runtime guard probes completed without regressions.",
      category,
    };
  }

  if (event.event_type === "device.upserted") {
    const name = typeof payload.name === "string" ? payload.name : "device";
    const mode = typeof payload.policy_mode === "string" ? payload.policy_mode : "global";
    const ip = typeof payload.ip_address === "string" ? payload.ip_address : "unknown address";
    return { title: `Updated device ${name}`, detail: `Policy mode ${mode} for ${ip}.`, category };
  }

  return generic();
}

export type RecoveryAction = {
  id: string;
  title: string;
  detail: string;
  /** The longer walkthrough. The old UI computed these and never rendered them. */
  steps: string[];
  actionLabel: string;
  busyKey: string;
  kind: "health-check" | "filter-notifications" | "refresh-sources" | "rollback-ruleset";
};

/**
 * Rules engine behind "Guided recovery", in the same priority order the old
 * Settings tab used. At most three actions, with a steady-state fallback.
 */
export function recoveryActions(dashboard: DashboardSummary): RecoveryAction[] {
  const actions: RecoveryAction[] = [];

  if (dashboard.runtime_health.degraded) {
    actions.push({
      id: "health",
      title: "Check runtime health again",
      detail: dashboard.runtime_health.notes[0] ?? "The runtime guard flagged a regression.",
      steps: [
        "Re-run the guard probes against the configured probe domains.",
        "Compare upstream failure and fallback counters against the configured deltas.",
        "If the probes pass, the degraded flag clears on the next dashboard poll.",
      ],
      actionLabel: "Run health check",
      busyKey: "runtime-health-check",
      kind: "health-check",
    });
  }

  if (dashboard.notification_health.failed_count > 0) {
    actions.push({
      id: "notifications",
      title: "Review notification delivery",
      detail: `${dashboard.notification_health.failed_count} delivery attempt(s) failed.`,
      steps: [
        "Filter the audit trail to notification events.",
        "Check the webhook URL is reachable from the appliance.",
        "Send a test notification once the endpoint is confirmed.",
      ],
      actionLabel: "Show notification events",
      busyKey: "audit-filter-notifications",
      kind: "filter-notifications",
    });
  }

  if (!dashboard.active_ruleset) {
    actions.push({
      id: "sources",
      title: "Refresh sources now",
      detail: "No ruleset is currently active, so nothing is being blocked.",
      steps: [
        "Fetch every enabled blocklist source.",
        "Verify the candidate ruleset before activating it.",
        "Activate the ruleset and hot-swap the policy engine.",
      ],
      actionLabel: "Refresh sources",
      busyKey: "refresh-sources",
      kind: "refresh-sources",
    });
  }

  if (dashboard.active_ruleset && dashboard.runtime_health.degraded) {
    actions.push({
      id: "rollback",
      title: "Roll back to the previous ruleset",
      detail: "The active ruleset coincides with a degraded runtime.",
      steps: [
        "Reactivate the ruleset that was previously marked active.",
        "Rebuild the profile policy catalog from the enabled sources.",
        "Resync device policies and invalidate the DNS cache.",
      ],
      actionLabel: "Roll back ruleset",
      busyKey: "rollback-ruleset",
      kind: "rollback-ruleset",
    });
  }

  if (actions.length === 0) {
    return [
      {
        id: "steady",
        title: "System looks steady",
        detail: "No degraded signals. A manual source refresh is still available.",
        steps: [
          "Runtime guard reports no regression.",
          "Notification deliveries are succeeding.",
          "A ruleset is active and serving traffic.",
        ],
        actionLabel: "Refresh sources",
        busyKey: "refresh-sources",
        kind: "refresh-sources",
      },
    ];
  }

  return actions.slice(0, 3);
}
