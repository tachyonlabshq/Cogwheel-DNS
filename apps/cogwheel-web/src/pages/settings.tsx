import { useEffect, useMemo, useState } from "react";
import { useCogwheel } from "@/contexts/cogwheel-context";
import {
  api,
  type AuditEvent,
  type SettingsSummary,
  type ServiceToggle,
} from "@/lib/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardDescription, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { ListRow } from "@/components/shared";

type SettingsView = "basic" | "advanced";

export default function SettingsPage() {
  const {
    dashboard,
    settings,
    syncStatus,
    tailscaleStatus,
    tailscaleDnsCheck,
    threatIntelSettings,
    setThreatIntelSettings,
    federatedLearningSettings,
    setFederatedLearningSettings,
    latencyBudget,
    busyAction,
    setBusyAction,
    pushToast,
    load,
    handleRefreshSources,
    handleRollbackRuleset,
    handleRuntimeHealthCheck,
  } = useCogwheel();

  const [settingsView, setSettingsView] = useState<SettingsView>("basic");
  const [classifierThreshold, setClassifierThreshold] = useState("0.92");
  const [notificationEnabled, setNotificationEnabled] = useState(false);
  const [notificationWebhookUrl, setNotificationWebhookUrl] = useState("");
  const [notificationMinSeverity, setNotificationMinSeverity] = useState<
    "medium" | "high" | "critical"
  >("high");
  const [notificationTestDomain] = useState(
    "notification-test.cogwheel.local",
  );
  const [notificationTestSeverity, setNotificationTestSeverity] = useState<
    "medium" | "high" | "critical"
  >("high");
  const [notificationTestDeviceName] = useState("Control Plane Test");
  const [notificationDryRun] = useState(false);
  const [serviceSearch] = useState("");
  const [auditEventFilter, setAuditEventFilter] = useState<
    "all" | "runtime" | "notifications" | "devices" | "rulesets"
  >("all");
  const [showServicesView, setShowServicesView] = useState(false);
  const [syncProfileDraft, setSyncProfileDraft] = useState("full");
  const [syncTransportModeDraft, setSyncTransportModeDraft] =
    useState("opportunistic");
  const [syncTransportTokenDraft, setSyncTransportTokenDraft] = useState("");

  const [blocklistName, setBlocklistName] = useState("");
  const [blocklistUrl, setBlocklistUrl] = useState("");
  const [blocklistProfile, setBlocklistProfile] = useState("custom");
  const [blocklistStrictness, setBlocklistStrictness] = useState<
    "strict" | "balanced" | "relaxed"
  >("balanced");
  const [blocklistInterval, setBlocklistInterval] = useState("60");

  // Sync local state from context
  useEffect(() => {
    setClassifierThreshold(settings.classifier.threshold.toFixed(2));
  }, [settings.classifier.threshold]);

  useEffect(() => {
    setNotificationEnabled(settings.notifications.enabled);
    setNotificationWebhookUrl(settings.notifications.webhook_url ?? "");
    setNotificationMinSeverity(settings.notifications.min_severity);
    setNotificationTestSeverity(settings.notifications.min_severity);
  }, [settings.notifications]);

  useEffect(() => {
    setSyncProfileDraft(syncStatus.profile);
    setSyncTransportModeDraft(syncStatus.transport_mode);
    setSyncTransportTokenDraft("");
  }, [syncStatus.profile, syncStatus.transport_mode]);

  const filteredServices = useMemo(
    () =>
      settings.services.filter((service) => {
        const query = serviceSearch.trim().toLowerCase();
        if (!query) return true;
        return `${service.manifest.display_name} ${service.manifest.category} ${service.manifest.risk_notes}`
          .toLowerCase()
          .includes(query);
      }),
    [serviceSearch, settings.services],
  );

  const filteredAuditEvents = useMemo(
    () =>
      dashboard.latest_audit_events.filter((event) => {
        if (auditEventFilter === "all") return true;
        if (auditEventFilter === "notifications")
          return (
            event.event_type.startsWith("notification.") ||
            event.event_type.startsWith("security.alert")
          );
        if (auditEventFilter === "runtime")
          return event.event_type.startsWith("runtime.");
        if (auditEventFilter === "devices")
          return event.event_type.startsWith("device.");
        if (auditEventFilter === "rulesets")
          return event.event_type.startsWith("ruleset.");
        return true;
      }),
    [auditEventFilter, dashboard.latest_audit_events],
  );

  const recoveryActions = useMemo(() => {
    const actions: Array<{
      title: string;
      detail: string;
      steps: string[];
      actionLabel: string;
      actionKey:
        | "runtime-health-check"
        | "notifications"
        | "refresh-sources"
        | "rollback-ruleset";
      disabled?: boolean;
    }> = [];

    if (dashboard.runtime_health.degraded) {
      actions.push({
        title: "Check runtime health again",
        detail:
          dashboard.runtime_health.notes[0] ??
          "Probe the runtime again to confirm whether the issue is still active.",
        steps: [
          "Run an active health check to refresh probe results.",
          "If probes still fail, compare the runtime notes with the most recent ruleset change.",
          "Roll back if the degraded state appeared after a fresh source update.",
        ],
        actionLabel:
          busyAction === "runtime-health-check"
            ? "Checking..."
            : "Run health check",
        actionKey: "runtime-health-check",
        disabled: busyAction === "runtime-health-check",
      });
    }

    if (dashboard.notification_health.failed_count > 0) {
      actions.push({
        title: "Review notification delivery",
        detail:
          "Open recent notification events and look for repeated delivery failures before the next alert is missed.",
        steps: [
          "Filter recent delivery history down to failed events.",
          "Check whether the failures are security alerts or control-plane recovery events.",
          "Fix the webhook target before relying on the next health or risky-domain alert.",
        ],
        actionLabel: "Show notifications",
        actionKey: "notifications",
      });
    }

    if (!dashboard.active_ruleset) {
      actions.push({
        title: "Refresh sources now",
        detail:
          "The resolver does not have an active ruleset yet, so request a fresh source refresh from the control plane.",
        steps: [
          "Refresh sources to build a fresh candidate ruleset.",
          "Confirm the active ruleset hash appears in the dashboard summary.",
          "Re-run a runtime health check once the new ruleset is active.",
        ],
        actionLabel:
          busyAction === "refresh-sources"
            ? "Refreshing..."
            : "Refresh sources",
        actionKey: "refresh-sources",
        disabled: busyAction === "refresh-sources",
      });
    }

    if (dashboard.active_ruleset && dashboard.runtime_health.degraded) {
      actions.push({
        title: "Roll back to the previous ruleset",
        detail:
          "If the degraded state appeared after a recent change, roll back to the last known-good policy set.",
        steps: [
          "Roll back to the previous verified ruleset.",
          "Watch the notification history for rollback delivery events.",
          "Run the health check again to confirm the runtime recovered.",
        ],
        actionLabel:
          busyAction === "rollback-ruleset" ? "Rolling back..." : "Roll back",
        actionKey: "rollback-ruleset",
        disabled: busyAction === "rollback-ruleset",
      });
    }

    if (actions.length === 0) {
      actions.push({
        title: "System looks steady",
        detail:
          "No immediate recovery flow is needed right now. Use refresh or device editing when you are ready to make the next change.",
        steps: [
          "Keep sources fresh before the next policy edit.",
          "Use the checklist to finish any incomplete setup items.",
          "Review recent audit events after each meaningful control-plane change.",
        ],
        actionLabel:
          busyAction === "refresh-sources"
            ? "Refreshing..."
            : "Refresh sources",
        actionKey: "refresh-sources",
        disabled: busyAction === "refresh-sources",
      });
    }

    return actions.slice(0, 3);
  }, [
    busyAction,
    dashboard.active_ruleset,
    dashboard.notification_health.failed_count,
    dashboard.runtime_health.degraded,
    dashboard.runtime_health.notes,
  ]);

  // --- Handlers ---

  async function handleSyncProfileSave() {
    setBusyAction("sync-profile-save");
    try {
      await api.updateSyncProfile(syncProfileDraft);
      pushToast(
        "Sync profile updated",
        `Node sync profile is now ${syncProfileDraft}.`,
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Sync profile update failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleSyncTransportSave() {
    setBusyAction("sync-transport-save");
    try {
      await api.updateSyncTransport(
        syncTransportModeDraft,
        syncTransportTokenDraft,
      );
      pushToast(
        "Sync transport updated",
        `Transport mode is now ${syncTransportModeDraft}.`,
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Sync transport update failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleClassifierUpdate(
    mode: SettingsSummary["classifier"]["mode"],
  ) {
    setBusyAction(`classifier-mode-${mode}`);
    try {
      await api.updateClassifier(
        mode,
        Number.parseFloat(classifierThreshold) ||
          settings.classifier.threshold,
      );
      pushToast("Classifier updated", `Mode switched to ${mode}.`, "success");
      await load();
    } catch (mutationError) {
      pushToast(
        "Classifier update failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleClassifierThresholdSave() {
    setBusyAction("classifier-threshold");
    try {
      const threshold =
        Number.parseFloat(classifierThreshold) ||
        settings.classifier.threshold;
      await api.updateClassifier(settings.classifier.mode, threshold);
      pushToast(
        "Threshold saved",
        `Classifier threshold is now ${threshold.toFixed(2)}.`,
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Threshold update failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleNotificationSave() {
    setBusyAction("notifications-save");
    try {
      await api.updateNotifications({
        enabled: notificationEnabled,
        webhook_url: notificationWebhookUrl || null,
        min_severity: notificationMinSeverity,
      });
      pushToast(
        "Notifications updated",
        notificationEnabled
          ? "Webhook delivery is configured."
          : "Webhook delivery is disabled.",
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Notification update failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleNotificationTest() {
    setBusyAction("notifications-test");
    try {
      const result = await api.testNotifications({
        domain: notificationTestDomain,
        severity: notificationTestSeverity,
        device_name: notificationTestDeviceName,
        dry_run: notificationDryRun,
      });
      pushToast(
        notificationDryRun ? "Webhook validated" : "Test notification sent",
        notificationDryRun
          ? `Validated ${result.target} without sending a live request.`
          : `Delivered to ${result.target} and added to recent history.`,
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Test notification failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleTailscaleExitNodeToggle() {
    const newState = !tailscaleStatus.exit_node_active;
    setBusyAction("tailscale-exit-node");
    try {
      const result = await api.tailscaleExitNode(newState);
      pushToast(
        newState ? "Exit node enabled" : "Exit node disabled",
        result.message,
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Exit node toggle failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleTailscaleRollback() {
    setBusyAction("tailscale-rollback");
    try {
      const result = await api.tailscaleRollback();
      pushToast("Exit node rolled back", result.message, "success");
      await load();
    } catch (mutationError) {
      pushToast(
        "Rollback failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleThreatIntelProviderSave(providerId: string) {
    const provider = threatIntelSettings.providers.find(
      (item) => item.id === providerId,
    );
    if (!provider) {
      pushToast(
        "Provider missing",
        "The selected provider could not be found.",
        "error",
      );
      return;
    }

    setBusyAction(`threat-intel-${providerId}`);
    try {
      const next = await api.updateThreatIntelProvider(
        provider.id,
        provider.enabled,
        provider.feed_url,
        provider.update_interval_minutes,
      );
      setThreatIntelSettings(next);
      pushToast(
        "Threat intel updated",
        `${provider.display_name} settings saved.`,
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Threat intel update failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleFederatedLearningSave() {
    setBusyAction("federated-learning-save");
    try {
      const next = await api.updateFederatedLearningStatus(
        federatedLearningSettings.enabled,
        federatedLearningSettings.coordinator_url,
        federatedLearningSettings.round_interval_hours,
      );
      setFederatedLearningSettings(next);
      pushToast(
        "Federated learning updated",
        next.enabled
          ? "Coordinator settings are active with model-updates-only privacy."
          : "Federated learning is disabled.",
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Federated learning update failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleServiceUpdate(
    serviceId: string,
    mode: ServiceToggle["mode"],
  ) {
    setBusyAction(`service-${serviceId}`);
    try {
      await api.updateService(serviceId, mode);
      pushToast("Service updated", `Service mode set to ${mode}.`, "success");
      await load();
    } catch (mutationError) {
      pushToast(
        "Service update failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleBlocklistCreate() {
    setBusyAction("create-blocklist");
    try {
      await api.upsertBlocklist({
        name: blocklistName,
        url: blocklistUrl,
        kind: "domains",
        enabled: true,
        refresh_interval_minutes:
          Number.parseInt(blocklistInterval, 10) || 60,
        profile: blocklistProfile,
        verification_strictness: blocklistStrictness,
      });
      setBlocklistName("");
      setBlocklistUrl("");
      setBlocklistProfile("custom");
      setBlocklistStrictness("balanced");
      setBlocklistInterval("60");
      pushToast(
        "Blocklist added",
        "The source was saved and refreshed.",
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Blocklist add failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleBlocklistToggle(id: string, enabled: boolean) {
    setBusyAction(`blocklist-toggle-${id}`);
    try {
      await api.setBlocklistEnabled(id, enabled);
      pushToast(
        enabled ? "Blocklist enabled" : "Blocklist disabled",
        "Ruleset refresh requested.",
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Blocklist update failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  return (
    <section className="grid gap-6">
      <Card className="overflow-hidden p-0">
        <div className="border-b border-border px-6 py-5">
          <CardTitle>Settings</CardTitle>
          <CardDescription className="mt-1">
            Start with the few settings most homes actually change, then open
            advanced controls only when you need them.
          </CardDescription>
          <div className="mt-5 inline-flex rounded-xl border border-border bg-muted/40 p-1">
            <Button
              variant={settingsView === "basic" ? "default" : "ghost"}
              size="sm"
              className="rounded-lg"
              onClick={() => setSettingsView("basic")}
            >
              Everyday
            </Button>
            <Button
              variant={settingsView === "advanced" ? "default" : "ghost"}
              size="sm"
              className="rounded-lg"
              onClick={() => setSettingsView("advanced")}
            >
              Advanced
            </Button>
          </div>
          <div className="mt-3 text-sm text-muted-foreground">
            {settingsView === "advanced"
              ? "Advanced mode includes sync, Tailscale, classifier tuning, operator feeds, and audit history."
              : "Everyday mode keeps the page focused on alerts, blocklists, and common services."}
          </div>
        </div>
        {settingsView === "advanced" ? (
          <div className="mt-5 grid gap-4 px-6 pb-6 lg:grid-cols-2">
            <div className="rounded-2xl border border-border bg-muted/20 p-4 text-sm">
              <div className="font-medium">Sync and replication</div>
              <div className="mt-2 grid gap-2 text-muted-foreground">
                <div>
                  Profile:{" "}
                  <span className="font-medium text-foreground">
                    {syncStatus.profile}
                  </span>
                </div>
                <div>
                  Revision:{" "}
                  <span className="font-medium text-foreground">
                    {syncStatus.revision}
                  </span>
                </div>
                <div>
                  Peers:{" "}
                  <span className="font-medium text-foreground">
                    {syncStatus.peers.length}
                  </span>
                </div>
              </div>
              <div className="mt-4 grid gap-3 xl:grid-cols-[1fr_auto]">
                <select
                  className="h-10 rounded-xl border border-input bg-background px-4 text-sm"
                  value={syncProfileDraft}
                  onChange={(event) =>
                    setSyncProfileDraft(event.target.value)
                  }
                >
                  <option value="full">Full replication</option>
                  <option value="settings-only">Settings only</option>
                  <option value="read-only-follower">
                    Read-only follower
                  </option>
                </select>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => void handleSyncProfileSave()}
                  disabled={busyAction === "sync-profile-save"}
                >
                  Save profile
                </Button>
              </div>
              <div className="mt-3 grid gap-3 xl:grid-cols-[180px_minmax(0,1fr)_auto]">
                <select
                  className="h-10 rounded-xl border border-input bg-background px-4 text-sm"
                  value={syncTransportModeDraft}
                  onChange={(event) =>
                    setSyncTransportModeDraft(event.target.value)
                  }
                >
                  <option value="opportunistic">Opportunistic</option>
                  <option value="https-required">HTTPS required</option>
                </select>
                <Input
                  value={syncTransportTokenDraft}
                  onChange={(event) =>
                    setSyncTransportTokenDraft(event.target.value)
                  }
                  placeholder={
                    syncStatus.transport_token_configured
                      ? "Set new token or leave blank to clear"
                      : "Optional bearer token"
                  }
                />
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => void handleSyncTransportSave()}
                  disabled={busyAction === "sync-transport-save"}
                >
                  Save transport
                </Button>
              </div>
            </div>
            <div className="rounded-2xl border border-border bg-muted/20 p-4 text-sm">
              <div className="flex items-center justify-between gap-3">
                <div className="font-medium">Tailscale</div>
                <Badge>
                  {tailscaleStatus.exit_node_active
                    ? "Exit node advertised"
                    : tailscaleStatus.installed
                      ? "Installed"
                      : "Not installed"}
                </Badge>
              </div>
              <div className="mt-2 grid gap-2 text-muted-foreground">
                <div>
                  Host:{" "}
                  <span className="font-medium text-foreground">
                    {tailscaleStatus.hostname ?? "-"}
                  </span>
                </div>
                <div>
                  Tailnet:{" "}
                  <span className="font-medium text-foreground">
                    {tailscaleStatus.tailnet_name ?? "-"}
                  </span>
                </div>
                <div>
                  Peers:{" "}
                  <span className="font-medium text-foreground">
                    {tailscaleStatus.peer_count}
                  </span>
                </div>
              </div>
              <div className="mt-4 flex flex-wrap gap-2">
                <Button
                  variant={
                    tailscaleStatus.exit_node_active ? "ghost" : "secondary"
                  }
                  size="sm"
                  onClick={() => void handleTailscaleExitNodeToggle()}
                  disabled={busyAction === "tailscale-exit-node"}
                >
                  {busyAction === "tailscale-exit-node"
                    ? "Updating..."
                    : tailscaleStatus.exit_node_active
                      ? "Disable exit-node filtering"
                      : "Enable exit-node filtering"}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => void handleTailscaleRollback()}
                  disabled={busyAction === "tailscale-rollback"}
                >
                  {busyAction === "tailscale-rollback"
                    ? "Rolling back..."
                    : "Roll back"}
                </Button>
              </div>
              {tailscaleDnsCheck.suggestions.length > 0 ? (
                <div className="mt-3 rounded-lg bg-primary/10 px-3 py-2 text-xs text-primary">
                  {tailscaleDnsCheck.message}
                </div>
              ) : null}
              <div className="mt-3 text-xs text-muted-foreground">
                When enabled, Cogwheel advertises this machine as a Tailscale
                exit node and keeps DNS on the local filter path for exit-node
                traffic only.
              </div>
            </div>
          </div>
        ) : null}
        {settingsView === "advanced" ? (
          <div className="mx-6 mb-6 rounded-2xl border border-border bg-muted/20 p-4">
            <div className="flex items-center justify-between gap-3">
              <div>
                <div className="font-medium">Latency budgets</div>
                <div className="text-sm text-muted-foreground">
                  Tracks the DNS hot path against the documented p50 budgets for
                  cache hits, cache misses, and classifier work.
                </div>
              </div>
              <Badge>
                {latencyBudget.within_budget
                  ? "Within budget"
                  : "Needs attention"}
              </Badge>
            </div>
            <div className="mt-4 grid gap-3 lg:grid-cols-[0.9fr_1.1fr]">
              <div className="rounded-2xl border border-border bg-background p-4 text-sm">
                <div className="text-muted-foreground">
                  Current cache hit rate
                </div>
                <div className="mt-1 text-2xl font-semibold text-foreground">
                  {(latencyBudget.cache_hit_rate * 100).toFixed(1)}%
                </div>
                <div className="mt-2 text-xs text-muted-foreground">
                  Higher cache hit rates usually keep household traffic under
                  the fastest path budget.
                </div>
              </div>
              <div className="grid gap-3 sm:grid-cols-3">
                {latencyBudget.checks.map((check) => (
                  <div
                    key={check.label}
                    className="rounded-2xl border border-border bg-background p-4 text-sm"
                  >
                    <div className="flex items-center justify-between gap-2">
                      <div className="font-medium text-foreground">
                        {check.label}
                      </div>
                      <Badge>{check.status}</Badge>
                    </div>
                    <div className="mt-3 text-lg font-semibold text-foreground">
                      {check.observed_ms.toFixed(3)} ms
                    </div>
                    <div className="mt-1 text-xs text-muted-foreground">
                      Target p50 {check.target_p50_ms.toFixed(1)} ms
                    </div>
                    <div className="mt-1 text-xs text-muted-foreground">
                      Samples: {check.sample_count}
                    </div>
                  </div>
                ))}
              </div>
            </div>
            {latencyBudget.recommendations.length > 0 ? (
              <div className="mt-4 rounded-2xl border border-dashed border-border/70 bg-background/80 p-4 text-sm text-muted-foreground">
                {latencyBudget.recommendations.join(" ")}
              </div>
            ) : null}
          </div>
        ) : null}
      </Card>

      <section className="grid gap-6 lg:grid-cols-[1.15fr_0.85fr]">
        <Card id="settings-page-core" className="overflow-hidden p-0">
          <div className="border-b border-border px-6 py-5">
            <CardTitle>Policy and notifications</CardTitle>
            <CardDescription className="mt-1">
              {settingsView === "advanced"
                ? "Core controls for alerts, classifier behavior, and optional intelligence features."
                : "Simple household controls for alerts and the few behaviors you are likely to change often."}
            </CardDescription>
          </div>
          <div className="space-y-5 px-6 py-6">
            {settingsView === "advanced" ? (
              <>
                <section className="space-y-3">
                  <div className="flex items-center justify-between">
                    <div>
                      <div className="font-medium">Classifier mode</div>
                      <div className="text-sm text-muted-foreground">
                        Persisted directly in the backend control plane.
                      </div>
                    </div>
                    <Badge>{settings.classifier.mode}</Badge>
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {(["Off", "Monitor", "Protect"] as const).map((mode) => (
                      <Button
                        key={mode}
                        variant={
                          settings.classifier.mode === mode
                            ? "primary"
                            : "secondary"
                        }
                        size="sm"
                        onClick={() => void handleClassifierUpdate(mode)}
                        disabled={busyAction === `classifier-mode-${mode}`}
                      >
                        {mode}
                      </Button>
                    ))}
                  </div>
                  <div className="grid gap-3 sm:grid-cols-[1fr_auto]">
                    <Input
                      value={classifierThreshold}
                      onChange={(event) =>
                        setClassifierThreshold(event.target.value)
                      }
                      placeholder="0.92"
                    />
                    <Button
                      variant="secondary"
                      onClick={() => void handleClassifierThresholdSave()}
                      disabled={busyAction === "classifier-threshold"}
                    >
                      Save threshold
                    </Button>
                  </div>
                </section>

                <Separator />
              </>
            ) : null}

            <section className="space-y-3">
              <div className="flex items-center justify-between gap-3">
                <div>
                  <div className="font-medium">Alert delivery</div>
                  <div className="text-sm text-muted-foreground">
                    Send high-severity security alerts to an external webhook.
                  </div>
                </div>
                <Badge>
                  {notificationEnabled
                    ? `Webhook ${notificationMinSeverity}+`
                    : "Disabled"}
                </Badge>
              </div>
              <label className="flex items-center gap-3 rounded-2xl border border-border bg-muted/20 px-4 py-3 text-sm">
                <input
                  type="checkbox"
                  checked={notificationEnabled}
                  onChange={(event) =>
                    setNotificationEnabled(event.target.checked)
                  }
                />
                Enable outbound alert notifications
              </label>
              <div className="grid gap-3 xl:grid-cols-[minmax(0,1fr)_170px_auto]">
                <Input
                  value={notificationWebhookUrl}
                  onChange={(event) =>
                    setNotificationWebhookUrl(event.target.value)
                  }
                  placeholder="https://hooks.example.com/cogwheel"
                />
                <select
                  className="h-11 rounded-xl border border-input bg-background px-4 text-sm"
                  value={notificationMinSeverity}
                  onChange={(event) =>
                    setNotificationMinSeverity(
                      event.target.value as "medium" | "high" | "critical",
                    )
                  }
                >
                  <option value="medium">Medium+</option>
                  <option value="high">High+</option>
                  <option value="critical">Critical only</option>
                </select>
                <div className="flex gap-2">
                  <Button
                    variant="secondary"
                    onClick={() => void handleNotificationSave()}
                    disabled={busyAction === "notifications-save"}
                  >
                    Save alerts
                  </Button>
                  <Button
                    variant="ghost"
                    onClick={() => void handleNotificationTest()}
                    disabled={
                      busyAction === "notifications-test" ||
                      !notificationWebhookUrl
                    }
                  >
                    Send test
                  </Button>
                </div>
              </div>
            </section>

            {settingsView === "advanced" ? <Separator /> : null}

            {settingsView === "advanced" ? (
              <>
                <section className="space-y-4">
                  <div className="flex items-center justify-between gap-3">
                    <div>
                      <div className="font-medium">
                        Optional intelligence feeds
                      </div>
                      <div className="text-sm text-muted-foreground">
                        Keep enrichment providers off the DNS hot path and
                        enable them only when needed.
                      </div>
                    </div>
                    <Badge>
                      {
                        threatIntelSettings.providers.filter(
                          (provider) => provider.enabled,
                        ).length
                      }{" "}
                      enabled
                    </Badge>
                  </div>
                  <div className="grid gap-3">
                    {threatIntelSettings.providers.map((provider) => (
                      <ListRow
                        key={provider.id}
                        tone="muted"
                        title={provider.display_name}
                        detail={provider.capabilities.join(" \u2022 ")}
                        right={
                          <Badge
                            className={
                              provider.enabled
                                ? "bg-primary/10 text-primary"
                                : "bg-muted text-muted-foreground"
                            }
                          >
                            {provider.enabled ? "Enabled" : "Disabled"}
                          </Badge>
                        }
                        footer={
                          <div className="mt-3 grid gap-3 xl:grid-cols-[minmax(0,1fr)_180px_auto]">
                            <Input
                              value={provider.feed_url ?? ""}
                              onChange={(event) =>
                                setThreatIntelSettings((current) => ({
                                  ...current,
                                  providers: current.providers.map((item) =>
                                    item.id === provider.id
                                      ? {
                                          ...item,
                                          feed_url:
                                            event.target.value || null,
                                        }
                                      : item,
                                  ),
                                }))
                              }
                              placeholder="https://feed.example.invalid/dns"
                            />
                            <Input
                              value={String(
                                provider.update_interval_minutes,
                              )}
                              onChange={(event) => {
                                const nextValue = Number.parseInt(
                                  event.target.value,
                                  10,
                                );
                                setThreatIntelSettings((current) => ({
                                  ...current,
                                  providers: current.providers.map((item) =>
                                    item.id === provider.id
                                      ? {
                                          ...item,
                                          update_interval_minutes:
                                            Number.isNaN(nextValue)
                                              ? item.update_interval_minutes
                                              : nextValue,
                                        }
                                      : item,
                                  ),
                                }));
                              }}
                              placeholder="60"
                            />
                            <div className="flex justify-end">
                              <Button
                                variant="secondary"
                                size="sm"
                                onClick={() =>
                                  void handleThreatIntelProviderSave(
                                    provider.id,
                                  )
                                }
                                disabled={
                                  busyAction ===
                                  `threat-intel-${provider.id}`
                                }
                              >
                                {busyAction ===
                                `threat-intel-${provider.id}`
                                  ? "Saving..."
                                  : "Save"}
                              </Button>
                            </div>
                          </div>
                        }
                      />
                    ))}
                  </div>
                </section>

                <Separator />
              </>
            ) : null}

            {settingsView === "advanced" ? (
              <section className="space-y-4">
                <div className="flex items-center justify-between gap-3">
                  <div>
                    <div className="font-medium">Federated learning</div>
                    <div className="text-sm text-muted-foreground">
                      Share model updates only. Raw logs stay local.
                    </div>
                  </div>
                  <Badge>
                    {federatedLearningSettings.enabled
                      ? federatedLearningSettings.privacy_mode
                      : "Disabled"}
                  </Badge>
                </div>
                <label className="flex items-center gap-3 rounded-2xl border border-border bg-muted/20 px-4 py-3 text-sm">
                  <input
                    type="checkbox"
                    checked={federatedLearningSettings.enabled}
                    onChange={(event) =>
                      setFederatedLearningSettings((current) => ({
                        ...current,
                        enabled: event.target.checked,
                      }))
                    }
                  />
                  Enable federated learning coordinator sync
                </label>
                <div className="grid gap-3 xl:grid-cols-[minmax(0,1fr)_180px_auto]">
                  <Input
                    value={federatedLearningSettings.coordinator_url ?? ""}
                    onChange={(event) =>
                      setFederatedLearningSettings((current) => ({
                        ...current,
                        coordinator_url: event.target.value || null,
                      }))
                    }
                    placeholder="https://coordinator.example.invalid"
                  />
                  <Input
                    value={String(
                      federatedLearningSettings.round_interval_hours,
                    )}
                    onChange={(event) => {
                      const nextValue = Number.parseInt(
                        event.target.value,
                        10,
                      );
                      setFederatedLearningSettings((current) => ({
                        ...current,
                        round_interval_hours: Number.isNaN(nextValue)
                          ? current.round_interval_hours
                          : nextValue,
                      }));
                    }}
                    placeholder="24"
                  />
                  <Button
                    variant="secondary"
                    onClick={() => void handleFederatedLearningSave()}
                    disabled={busyAction === "federated-learning-save"}
                  >
                    {busyAction === "federated-learning-save"
                      ? "Saving..."
                      : "Save"}
                  </Button>
                </div>
              </section>
            ) : null}
          </div>
        </Card>

        <div className="grid gap-6">
          <Card id="settings-page-blocklists" className="overflow-hidden p-0">
            <div className="border-b border-border px-6 py-5">
              <CardTitle>Sources and services</CardTitle>
              <CardDescription className="mt-1">
                {settingsView === "advanced"
                  ? "Manage imported blocklists and common-service toggles without crowding the overview."
                  : "The core household settings live here: list sources, profile sources, and a few service toggles."}
              </CardDescription>
            </div>
            <div className="grid gap-4 px-6 py-6">
              <div className="rounded-2xl border border-border bg-muted/20 p-4 text-sm">
                <div className="font-medium">Add blocklist</div>
                <div className="mt-3 grid gap-3">
                  <Input
                    value={blocklistName}
                    onChange={(event) =>
                      setBlocklistName(event.target.value)
                    }
                    placeholder="Human-readable name"
                  />
                  <Input
                    value={blocklistUrl}
                    onChange={(event) =>
                      setBlocklistUrl(event.target.value)
                    }
                    placeholder="Source URL or data: URL"
                  />
                  <div className="grid gap-3 xl:grid-cols-3">
                    <select
                      className="h-11 rounded-xl border border-input bg-background px-4 text-sm"
                      value={blocklistProfile}
                      onChange={(event) =>
                        setBlocklistProfile(event.target.value)
                      }
                    >
                      <option value="custom">Custom</option>
                      <option value="essential">Essential</option>
                      <option value="balanced">Balanced</option>
                      <option value="aggressive">Aggressive</option>
                    </select>
                    <select
                      className="h-11 rounded-xl border border-input bg-background px-4 text-sm"
                      value={blocklistStrictness}
                      onChange={(event) =>
                        setBlocklistStrictness(
                          event.target.value as
                            | "strict"
                            | "balanced"
                            | "relaxed",
                        )
                      }
                    >
                      <option value="strict">Strict</option>
                      <option value="balanced">Balanced</option>
                      <option value="relaxed">Relaxed</option>
                    </select>
                    <Input
                      value={blocklistInterval}
                      onChange={(event) =>
                        setBlocklistInterval(event.target.value)
                      }
                      placeholder="Refresh minutes"
                    />
                  </div>
                  <div className="flex justify-end">
                    <Button
                      onClick={() => void handleBlocklistCreate()}
                      disabled={
                        !blocklistName ||
                        !blocklistUrl ||
                        busyAction === "create-blocklist"
                      }
                    >
                      Add blocklist
                    </Button>
                  </div>
                </div>
              </div>
              {settings.blocklists.map((source) => (
                <ListRow
                  key={source.id}
                  title={source.name}
                  detail={`${source.profile} \u2022 ${source.refresh_interval_minutes}m`}
                  right={
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() =>
                        void handleBlocklistToggle(
                          source.id,
                          !source.enabled,
                        )
                      }
                      disabled={
                        busyAction === `blocklist-toggle-${source.id}`
                      }
                    >
                      {source.enabled ? "Disable" : "Enable"}
                    </Button>
                  }
                />
              ))}
              <Card
                id="services"
                className="border border-border bg-muted/20 p-5 shadow-none"
              >
                <CardTitle>Services</CardTitle>
                <CardDescription className="mt-1">
                  Optional curated allow/block toggles for common apps.
                </CardDescription>
                <div className="mt-4 grid gap-3">
                  {filteredServices
                    .slice(
                      0,
                      showServicesView
                        ? filteredServices.length
                        : 3,
                    )
                    .map((service) => (
                      <ListRow
                        key={service.manifest.service_id}
                        title={service.manifest.display_name}
                        detail={service.manifest.risk_notes}
                        right={<Badge>{service.mode}</Badge>}
                        footer={
                          <div className="mt-3 flex flex-wrap gap-2">
                            {(
                              ["Inherit", "Allow", "Block"] as const
                            ).map((mode) => (
                              <Button
                                key={mode}
                                variant={
                                  service.mode === mode
                                    ? "primary"
                                    : "secondary"
                                }
                                size="sm"
                                onClick={() =>
                                  void handleServiceUpdate(
                                    service.manifest.service_id,
                                    mode,
                                  )
                                }
                                disabled={
                                  busyAction ===
                                  `service-${service.manifest.service_id}`
                                }
                              >
                                {mode}
                              </Button>
                            ))}
                          </div>
                        }
                      />
                    ))}
                  {!showServicesView ? (
                    <div className="flex justify-end">
                      <Button
                        variant="ghost"
                        onClick={() => setShowServicesView(true)}
                      >
                        Show all services
                      </Button>
                    </div>
                  ) : null}
                </div>
              </Card>
            </div>
          </Card>

          {settingsView === "advanced" ? (
            <Card className="overflow-hidden p-0">
              <div className="border-b border-border px-6 py-5">
                <CardTitle>Recovery and operator feed</CardTitle>
                <CardDescription className="mt-1">
                  Use guided recovery, audit history, and runtime notes without
                  cluttering the household overview.
                </CardDescription>
              </div>
              <div className="space-y-5 px-6 py-6">
                <section className="space-y-3">
                  <div className="font-medium">Guided recovery</div>
                  <div className="grid gap-3">
                    {recoveryActions.map((item) => (
                      <ListRow
                        key={item.title}
                        tone="muted"
                        title={item.title}
                        detail={item.detail}
                        footer={
                          <div className="mt-3">
                            <Button
                              variant="secondary"
                              size="sm"
                              onClick={() => {
                                if (
                                  item.actionKey ===
                                  "runtime-health-check"
                                ) {
                                  void handleRuntimeHealthCheck();
                                  return;
                                }
                                if (
                                  item.actionKey === "notifications"
                                ) {
                                  setAuditEventFilter("notifications");
                                  return;
                                }
                                if (
                                  item.actionKey === "rollback-ruleset"
                                ) {
                                  void handleRollbackRuleset();
                                  return;
                                }
                                void handleRefreshSources();
                              }}
                              disabled={item.disabled}
                            >
                              {item.actionLabel}
                            </Button>
                          </div>
                        }
                      />
                    ))}
                  </div>
                </section>
                <Separator />
                <section className="space-y-3">
                  <div className="flex flex-col gap-3 md:flex-row md:items-center md:justify-between">
                    <div>
                      <div className="font-medium">Recent audit events</div>
                      <div className="text-sm text-muted-foreground">
                        Filter the operator feed to focus on the control-plane
                        changes you are investigating.
                      </div>
                    </div>
                    <div className="flex flex-wrap gap-2">
                      {(
                        [
                          ["all", "All events"],
                          ["runtime", "Runtime"],
                          ["notifications", "Notifications"],
                          ["devices", "Devices"],
                          ["rulesets", "Rulesets"],
                        ] as const
                      ).map(([value, label]) => (
                        <Button
                          key={value}
                          variant={
                            auditEventFilter === value
                              ? "primary"
                              : "ghost"
                          }
                          size="sm"
                          onClick={() =>
                            setAuditEventFilter(
                              value as
                                | "all"
                                | "runtime"
                                | "notifications"
                                | "devices"
                                | "rulesets",
                            )
                          }
                        >
                          {label}
                        </Button>
                      ))}
                    </div>
                  </div>
                  <div className="grid gap-3">
                    {filteredAuditEvents.slice(0, 8).map((event) => {
                      const summary = summarizeAuditEvent(event);
                      return (
                        <ListRow
                          key={event.id}
                          tone="muted"
                          title={summary.title}
                          detail={summary.detail}
                          meta={
                            <div className="mt-1 text-xs text-muted-foreground">
                              {event.event_type}
                            </div>
                          }
                          right={<Badge>{summary.category}</Badge>}
                        />
                      );
                    })}
                  </div>
                </section>
              </div>
            </Card>
          ) : null}
        </div>
      </section>
    </section>
  );
}

// ---------------------------------------------------------------------------
// Audit event helpers (moved from App.tsx)
// ---------------------------------------------------------------------------

function summarizeAuditEvent(event: AuditEvent) {
  const payload = parseAuditPayload(event.payload);
  const category = event.event_type.split(".")[0] ?? "system";

  if (event.event_type === "ruleset.rollback") {
    return {
      category,
      title: "Ruleset rollback completed",
      detail: `Recovered ruleset ${String(payload.hash ?? "unknown").slice(0, 12)} after an operator-triggered rollback.`,
    };
  }

  if (event.event_type === "ruleset.auto_rollback") {
    return {
      category,
      title: "Automatic rollback triggered",
      detail: String(
        firstPayloadItem(payload.notes) ??
          "Runtime guard restored the previous verified ruleset.",
      ),
    };
  }

  if (event.event_type === "ruleset.refresh_rejected") {
    return {
      category,
      title: "Ruleset refresh rejected",
      detail: String(
        firstPayloadItem(payload.notes) ??
          "Verification blocked the candidate ruleset before activation.",
      ),
    };
  }

  if (
    event.event_type.startsWith("notification.delivery_") ||
    event.event_type.startsWith("security.alert_delivery_")
  ) {
    return {
      category,
      title: String(
        payload.title ?? payload.domain ?? "Notification delivery",
      ),
      detail: String(
        payload.summary ??
          `${payload.severity ?? "unknown"} delivery to ${payload.client_ip ?? payload.device_name ?? "control-plane"}.`,
      ),
    };
  }

  if (event.event_type.startsWith("runtime.health_check_")) {
    return {
      category,
      title: event.event_type.endsWith("degraded")
        ? "Runtime health degraded"
        : "Runtime health check passed",
      detail: String(
        firstPayloadItem(payload.notes) ??
          "Manual runtime health check completed.",
      ),
    };
  }

  if (event.event_type === "device.upserted") {
    return {
      category,
      title: `Updated device ${String(payload.name ?? "unnamed device")}`,
      detail: `Policy mode ${String(payload.policy_mode ?? "unknown")} for ${String(payload.ip_address ?? "unknown IP")}.`,
    };
  }

  const [firstKey, firstValue] = Object.entries(payload)[0] ?? [];
  return {
    category,
    title: event.event_type,
    detail: firstKey
      ? `${firstKey}: ${stringifyAuditValue(firstValue)}`
      : "No structured payload details recorded.",
  };
}

function parseAuditPayload(payload: string): Record<string, unknown> {
  try {
    const parsed = JSON.parse(payload) as unknown;
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : {};
  } catch {
    return {};
  }
}

function firstPayloadItem(value: unknown) {
  return Array.isArray(value) && value.length > 0 ? value[0] : undefined;
}

function stringifyAuditValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean")
    return String(value);
  if (Array.isArray(value) && value.length > 0)
    return stringifyAuditValue(value[0]);
  if (value && typeof value === "object") {
    const [firstKey, firstValue] = Object.entries(value)[0] ?? [];
    return firstKey
      ? `${firstKey}: ${stringifyAuditValue(firstValue)}`
      : "details available";
  }
  return "details available";
}
