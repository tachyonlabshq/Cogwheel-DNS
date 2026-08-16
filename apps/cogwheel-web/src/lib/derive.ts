import type { DashboardSummary, DeviceRecord, ServiceManifest } from "@/lib/api";
import type { Tone } from "@/components/app/status-indicator";

export type ProtectionState = {
  tone: Tone;
  label: string;
  detail: string;
  paused: boolean;
};

/**
 * `protection_status` is derived server-side as Paused / Needs Attention /
 * Protected. Anything else (including the "Loading" placeholder) is treated as
 * unknown rather than guessed at.
 */
export function protectionState(dashboard: DashboardSummary, offline: boolean): ProtectionState {
  if (offline) {
    return {
      tone: "bad",
      label: "Unreachable",
      detail: "The control plane did not answer. Filtering may still be running on the appliance.",
      paused: false,
    };
  }

  switch (dashboard.protection_status) {
    case "Paused":
      return {
        tone: "warn",
        label: "Paused",
        detail: "Blocking and classification are suspended until the snooze expires.",
        paused: true,
      };
    case "Needs Attention":
      return {
        tone: "bad",
        label: "Needs attention",
        detail: dashboard.runtime_health.notes[0] ?? "The runtime guard reported a regression.",
        paused: false,
      };
    case "Protected":
      return {
        tone: "good",
        label: "Protected",
        detail: "Filtering and classification are active.",
        paused: false,
      };
    default:
      return {
        tone: "idle",
        label: "Unknown",
        detail: "Waiting for the first response from the control plane.",
        paused: false,
      };
  }
}

export function severityTone(severity: string): Tone {
  switch (severity.toLowerCase()) {
    case "critical":
    case "high":
      return "bad";
    case "medium":
      return "warn";
    default:
      return "idle";
  }
}

export function severityLabel(severity: string): string {
  if (!severity) return "Unknown";
  return severity.charAt(0).toUpperCase() + severity.slice(1);
}

/** Seconds left on the pause window, or 0 when protection is not paused. */
export function pauseSecondsRemaining(dashboard: DashboardSummary, now = Date.now()): number {
  if (!dashboard.protection_paused_until) return 0;
  const until = new Date(dashboard.protection_paused_until).getTime();
  if (Number.isNaN(until)) return 0;
  return Math.max(0, Math.round((until - now) / 1000));
}

export function blockedRatio(dashboard: DashboardSummary): number {
  const { queries_total, blocked_total } = dashboard.runtime_health.snapshot;
  if (queries_total <= 0) return 0;
  return blocked_total / queries_total;
}

/**
 * Mirrors the server's `validate_device_service_overrides`: an allow rule spans
 * every domain the manifest knows about, a block rule only the blocked set.
 */
export function serviceOverrideDomains(manifest: ServiceManifest, mode: "allow" | "block"): string[] {
  const domains =
    mode === "allow"
      ? [...manifest.allow_domains, ...manifest.block_domains, ...manifest.exceptions]
      : [...manifest.block_domains];
  return [...new Set(domains)];
}

/** The server forces these to their defaults whenever the mode is not custom. */
export function normaliseDeviceInput<T extends { policy_mode?: DeviceRecord["policy_mode"] }>(
  input: T,
): T & {
  blocklist_profile_override: string | null;
  protection_override: DeviceRecord["protection_override"];
  allowed_domains: string[];
  service_overrides: DeviceRecord["service_overrides"];
} {
  const record = input as T & {
    blocklist_profile_override?: string | null;
    protection_override?: DeviceRecord["protection_override"];
    allowed_domains?: string[];
    service_overrides?: DeviceRecord["service_overrides"];
  };

  if (record.policy_mode !== "custom") {
    return {
      ...record,
      blocklist_profile_override: null,
      protection_override: "inherit",
      allowed_domains: [],
      service_overrides: [],
    };
  }

  return {
    ...record,
    blocklist_profile_override: record.blocklist_profile_override ?? null,
    protection_override: record.protection_override ?? "inherit",
    allowed_domains: record.allowed_domains ?? [],
    service_overrides: record.service_overrides ?? [],
  };
}

export function splitDomainList(value: string): string[] {
  return value
    .split(",")
    .map((entry) => entry.trim().toLowerCase())
    .filter(Boolean);
}

export function slugify(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

/** Bare-minimum shape check so the inspector does not round-trip obvious junk. */
export function looksLikeDomain(value: string): boolean {
  const trimmed = value.trim().toLowerCase();
  if (!trimmed || trimmed.length > 253) return false;
  return /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/.test(trimmed);
}
