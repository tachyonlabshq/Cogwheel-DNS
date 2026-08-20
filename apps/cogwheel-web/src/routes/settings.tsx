import React from "react";
import { useSearchParams } from "react-router-dom";
import { PlusIcon, SendIcon, Trash2Icon } from "lucide-react";
import {
  api,
  type NotificationSeverity,
  type NotificationTestPreset,
  type ThreatIntelProviderConfig,
} from "@/lib/api";
import { formatCount, formatDateTime, formatRelative } from "@/lib/format";
import { useCogwheel } from "@/data/context";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { PageHeader, PageShell } from "@/components/app/page";
import { SectionCard } from "@/components/app/section-card";
import { DataTable, type Column } from "@/components/app/data-table";
import { SelectField } from "@/components/app/select-field";
import { TextField } from "@/components/app/text-field";
import { FieldRow, FormField } from "@/components/app/form-field";
import { StatusIndicator, StatusPill } from "@/components/app/status-indicator";
import { AsyncRegion, EmptyState, NoticeBanner } from "@/components/app/states";
import { ConfirmDialog } from "@/components/app/confirm-dialog";

const TABS = ["alerts", "integrations", "guard"] as const;
type TabId = (typeof TABS)[number];

export function SettingsScreen() {
  const [params, setParams] = useSearchParams();
  const tab = (params.get("tab") ?? "alerts") as TabId;

  return (
    <PageShell>
      <PageHeader
        description="Everything the appliance persists that is not a blocking decision."
        title="Settings"
      />

      <Tabs
        onValueChange={(details) =>
          setParams(
            (current) => {
              const next = new URLSearchParams(current);
              next.set("tab", details.value);
              return next;
            },
            { replace: true },
          )
        }
        value={TABS.includes(tab) ? tab : "alerts"}
      >
        <TabsList className="mb-6">
          <TabsTrigger value="alerts">Alerts</TabsTrigger>
          <TabsTrigger value="integrations">Integrations</TabsTrigger>
          <TabsTrigger value="guard">Runtime guard</TabsTrigger>
        </TabsList>

        <TabsContent value="alerts">
          <AlertsPane />
        </TabsContent>
        <TabsContent value="integrations">
          <IntegrationsPane />
        </TabsContent>
        <TabsContent value="guard">
          <GuardPane />
        </TabsContent>
      </Tabs>
    </PageShell>
  );
}

/* -------------------------------------------------------------------------- */

function AlertsPane() {
  const { data, phase, error, busy, mutate, reload } = useCogwheel();
  const notifications = data.settings.notifications;
  const health = data.dashboard.notification_health;

  const [enabled, setEnabled] = React.useState(notifications.enabled);
  const [webhook, setWebhook] = React.useState(notifications.webhook_url ?? "");
  const [severity, setSeverity] = React.useState<NotificationSeverity>(notifications.min_severity);

  const [testDomain, setTestDomain] = React.useState("notification-test.cogwheel.local");
  const [testDevice, setTestDevice] = React.useState("Control Plane Test");
  const [testSeverity, setTestSeverity] = React.useState<NotificationSeverity>(notifications.min_severity);
  const [dryRun, setDryRun] = React.useState(true);

  const [presets, setPresets] = React.useState<NotificationTestPreset[]>(
    data.settings.notification_test_presets,
  );
  const [presetName, setPresetName] = React.useState("");
  // Deleting a preset writes the shortened list straight to the appliance and
  // there is no undo, so it goes through the house guard like every other
  // server-side deletion.
  const [pendingDeletePreset, setPendingDeletePreset] = React.useState<NotificationTestPreset | null>(
    null,
  );

  // The form is a draft over server state; re-sync whenever the server's copy
  // changes underneath us (another operator, or a sync import).
  React.useEffect(() => {
    setEnabled(notifications.enabled);
    setWebhook(notifications.webhook_url ?? "");
    setSeverity(notifications.min_severity);
  }, [notifications.enabled, notifications.min_severity, notifications.webhook_url]);

  React.useEffect(() => {
    setPresets(data.settings.notification_test_presets);
  }, [data.settings.notification_test_presets]);

  const presetColumns: Column<NotificationTestPreset>[] = [
    { key: "name", header: "Name", render: (row) => row.name },
    {
      key: "domain",
      header: "Domain",
      render: (row) => <span className="font-mono text-xs">{row.domain}</span>,
    },
    { key: "device", header: "Device", hideOnStack: true, render: (row) => row.device_name },
    { key: "severity", header: "Severity", render: (row) => <Badge variant="outline">{row.severity}</Badge> },
    {
      key: "dry",
      header: "Mode",
      render: (row) => <Badge variant="secondary">{row.dry_run ? "Dry run" : "Live send"}</Badge>,
    },
    {
      key: "actions",
      header: "Actions",
      align: "end",
      render: (row) => (
        <span className="flex justify-end gap-1.5">
          <Button
            onClick={() => {
              setTestDomain(row.domain);
              setTestDevice(row.device_name);
              setTestSeverity(row.severity);
              setDryRun(row.dry_run);
            }}
            size="sm"
            variant="outline"
          >
            Load
          </Button>
          <Button
            aria-label={`Delete preset ${row.name}`}
            onClick={() => setPendingDeletePreset(row)}
            size="icon-sm"
            title={`Delete preset ${row.name}`}
            variant="ghost"
          >
            <Trash2Icon aria-hidden />
          </Button>
        </span>
      ),
    },
  ];

  function savePresets(next: NotificationTestPreset[]) {
    return mutate({
      key: "notification-presets",
      action: () => api.updateNotificationTestPresets(next),
      successTitle: "Test presets saved",
      successDetail: `${next.length} preset(s) stored. The appliance keeps at most 8.`,
      failureTitle: "Could not save test presets",
      after: "full",
    });
  }

  return (
    <div className="flex flex-col gap-6">
      <SectionCard
        actions={
          notifications.enabled ? (
            <Badge variant="outline">Webhook · {notifications.min_severity}+</Badge>
          ) : (
            <Badge variant="secondary">Disabled</Badge>
          )
        }
        description="Cogwheel POSTs a JSON alert to a webhook when a risky-domain event meets the minimum severity."
        footer={
          <Button
            isLoading={busy === "notifications-save"}
            onClick={() =>
              void mutate({
                key: "notifications-save",
                action: () =>
                  api.updateNotifications({
                    enabled,
                    webhook_url: webhook.trim() || null,
                    min_severity: severity,
                  }),
                successTitle: "Notifications updated",
                successDetail: enabled
                  ? "Webhook delivery is configured."
                  : "Outbound alert delivery is disabled.",
                failureTitle: "Could not update notifications",
              })
            }
          >
            Save alert settings
          </Button>
        }
        title="Alert delivery"
      >
        <div className="space-y-4">
          <FormField
            hint="The URL is stored in cleartext on the appliance and is returned by the settings and backup endpoints."
            label="Enable outbound alert notifications"
            orientation="horizontal"
          >
            <Switch
              checked={enabled}
              onCheckedChange={(details) => setEnabled(details.checked)}
            />
          </FormField>

          <FieldRow>
            <TextField
              label="Webhook URL"
              onChange={setWebhook}
              placeholder="https://hooks.example.com/cogwheel"
              value={webhook}
            />
            <SelectField
              label="Minimum severity"
              onChange={(value) => setSeverity(value as NotificationSeverity)}
              options={[
                { value: "medium", label: "Medium and above" },
                { value: "high", label: "High and above" },
                { value: "critical", label: "Critical only" },
              ]}
              value={severity}
            />
          </FieldRow>

          <div className="grid gap-3 sm:grid-cols-2">
            <StatusIndicator
              description={
                health.last_delivery_at ? `Last ${formatRelative(health.last_delivery_at)}` : "None yet"
              }
              label={`${formatCount(health.delivered_count)} delivered`}
              tone={health.delivered_count > 0 ? "good" : "idle"}
            />
            <StatusIndicator
              description={
                health.last_failure_at ? `Last ${formatRelative(health.last_failure_at)}` : "None recorded"
              }
              label={`${formatCount(health.failed_count)} failed`}
              tone={health.failed_count > 0 ? "bad" : "idle"}
            />
          </div>
        </div>
      </SectionCard>

      <SectionCard
        description="Send a synthetic alert to confirm the endpoint works. A dry run validates the target without making an outbound request."
        footer={
          <>
            <Button
              disabled={!webhook.trim()}
              isLoading={busy === "notifications-test"}
              onClick={() =>
                void mutate({
                  key: "notifications-test",
                  action: () =>
                    api.testNotifications({
                      domain: testDomain,
                      device_name: testDevice,
                      severity: testSeverity,
                      dry_run: dryRun,
                    }),
                  successTitle: dryRun ? "Webhook validated" : "Test notification sent",
                  successDetail: (result) =>
                    dryRun
                      ? `Validated ${result.target} without sending a live request.`
                      : `Delivered to ${result.target} and added to recent history.`,
                  failureTitle: "Test notification failed",
                })
              }
            >
              <SendIcon aria-hidden />
              {dryRun ? "Validate webhook" : "Send test alert"}
            </Button>
            <Button
              disabled={!presetName.trim()}
              onClick={() => {
                const next = [
                  ...presets.filter((preset) => preset.name !== presetName.trim()),
                  {
                    name: presetName.trim(),
                    domain: testDomain,
                    device_name: testDevice,
                    severity: testSeverity,
                    dry_run: dryRun,
                  },
                ];
                setPresetName("");
                void savePresets(next);
              }}
              variant="outline"
            >
              <PlusIcon aria-hidden />
              Save as preset
            </Button>
          </>
        }
        title="Test a delivery"
      >
        <div className="space-y-4">
          {!webhook.trim() ? (
            <NoticeBanner
              detail="Set and save a webhook URL above before sending a test."
              title="No webhook configured"
              tone="warn"
            />
          ) : null}

          <FieldRow>
            <TextField label="Test domain" onChange={setTestDomain} value={testDomain} />
            <TextField label="Device name" onChange={setTestDevice} value={testDevice} />
          </FieldRow>

          <FieldRow>
            <SelectField
              label="Severity"
              onChange={(value) => setTestSeverity(value as NotificationSeverity)}
              options={[
                { value: "medium", label: "Medium" },
                { value: "high", label: "High" },
                { value: "critical", label: "Critical" },
              ]}
              value={testSeverity}
            />
            <FormField
              hint="Dry runs never contact the endpoint. Turn this off to make a real outbound request."
              label="Dry run"
              orientation="horizontal"
            >
              <Switch checked={dryRun} onCheckedChange={(details) => setDryRun(details.checked)} />
            </FormField>
          </FieldRow>

          <TextField
            hint="Presets are normalised server-side: blank fields are dropped, duplicates replaced, and the list truncated to 8."
            label="Preset name"
            onChange={setPresetName}
            placeholder="Nightly smoke test"
            value={presetName}
          />
        </div>
      </SectionCard>

      <SectionCard description="Saved test configurations." title="Test presets">
        <DataTable
          columns={presetColumns}
          empty={{
            icon: SendIcon,
            title: "No presets saved",
            description: "Fill in a test above and save it as a preset to re-run it later.",
          }}
          error={error}
          loading={phase === "loading"}
          onRetry={() => void reload()}
          rowKey={(row) => row.name}
          rows={presets}
        />
      </SectionCard>

      <ConfirmDialog
        confirmLabel="Delete preset"
        consequence="The remaining presets are written back to the appliance immediately. There is no undo."
        description={`"${pendingDeletePreset?.name ?? ""}" (${
          pendingDeletePreset?.domain ?? ""
        } as ${pendingDeletePreset?.device_name ?? ""}) will be removed from the saved test presets.`}
        destructive
        onConfirm={async () => {
          if (!pendingDeletePreset) return;
          const target = pendingDeletePreset;
          await savePresets(presets.filter((preset) => preset.name !== target.name));
        }}
        onOpenChange={(open) => {
          if (!open) setPendingDeletePreset(null);
        }}
        open={pendingDeletePreset !== null}
        title={`Delete preset ${pendingDeletePreset?.name ?? ""}?`}
      />
    </div>
  );
}

/* -------------------------------------------------------------------------- */

function IntegrationsPane() {
  const { data, phase, error, busy, mutate, patch, reload } = useCogwheel();
  const threatIntel = data.threatIntel;
  const federated = data.federatedLearning;

  const [coordinator, setCoordinator] = React.useState(federated.coordinator_url ?? "");
  const [roundHours, setRoundHours] = React.useState(String(federated.round_interval_hours));
  const [federatedEnabled, setFederatedEnabled] = React.useState(federated.enabled);

  React.useEffect(() => {
    setCoordinator(federated.coordinator_url ?? "");
    setRoundHours(String(federated.round_interval_hours));
    setFederatedEnabled(federated.enabled);
  }, [federated.coordinator_url, federated.enabled, federated.round_interval_hours]);

  const updateProvider = (id: string, changes: Partial<ThreatIntelProviderConfig>) => {
    patch({
      threatIntel: {
        ...threatIntel,
        providers: threatIntel.providers.map((provider) =>
          provider.id === id ? { ...provider, ...changes } : provider,
        ),
      },
    });
  };

  return (
    <div className="flex flex-col gap-6">
      <NoticeBanner
        detail="Neither subsystem below is wired into DNS resolution on this build. Their settings are held in memory only and are lost when the appliance restarts."
        title="Threat intelligence and federated learning are not active"
        tone="warn"
      />

      <SectionCard
        actions={
          <Badge variant="outline">
            {formatCount(threatIntel.providers.filter((provider) => provider.enabled).length)} enabled
          </Badge>
        }
        description="Feed endpoints the appliance would consult. Nothing fetches them yet."
        title="Threat intelligence"
      >
        <AsyncRegion
          empty={
            <EmptyState
              description="The appliance did not report any threat intelligence providers."
              icon={Trash2Icon}
              title="No providers configured"
            />
          }
          error={error}
          errorTitle="Could not load threat intelligence providers"
          isEmpty={threatIntel.providers.length === 0}
          loading={phase === "loading"}
          onRetry={() => void reload()}
          skeletonRows={3}
        >
          <ul className="flex flex-col gap-3">
            {threatIntel.providers.map((provider) => (
              <li className="rounded-xl border border-border p-4" key={provider.id}>
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0">
                    <p className="font-medium text-foreground text-sm">{provider.display_name}</p>
                    <p className="text-muted-foreground text-xs">{provider.capabilities.join(" • ")}</p>
                  </div>
                  <FormField label="Enabled" orientation="horizontal">
                    <Switch
                      checked={provider.enabled}
                      onCheckedChange={(details) => updateProvider(provider.id, { enabled: details.checked })}
                    />
                  </FormField>
                </div>

                <div className="mt-3 grid gap-3 sm:grid-cols-2">
                  <TextField
                    label="Feed URL"
                    onChange={(value) => updateProvider(provider.id, { feed_url: value || null })}
                    placeholder="https://feed.example.com/dns"
                    value={provider.feed_url ?? ""}
                  />
                  <TextField
                    hint="Clamped to a five-minute minimum server-side."
                    inputMode="numeric"
                    label="Interval (minutes)"
                    onChange={(value) =>
                      updateProvider(provider.id, {
                        update_interval_minutes: Number.parseInt(value, 10) || 0,
                      })
                    }
                    value={String(provider.update_interval_minutes)}
                  />
                </div>

                <div className="mt-3 flex flex-wrap items-center gap-3">
                  <StatusPill
                    label={provider.api_key_configured ? "API key set" : "No API key"}
                    tone={provider.api_key_configured ? "good" : "idle"}
                  />
                  {provider.last_sync_at ? (
                    <span className="text-muted-foreground text-xs">
                      Last sync {formatDateTime(provider.last_sync_at)}
                    </span>
                  ) : (
                    <span className="text-muted-foreground text-xs">Never synced</span>
                  )}
                  {provider.last_error ? (
                    <span className="text-destructive-foreground text-xs">{provider.last_error}</span>
                  ) : null}
                  <Button
                    className="ms-auto"
                    isLoading={busy === `threat-intel-${provider.id}`}
                    onClick={() =>
                      void mutate({
                        key: `threat-intel-${provider.id}`,
                        action: () =>
                          api.updateThreatIntelProvider(
                            provider.id,
                            provider.enabled,
                            provider.feed_url,
                            provider.update_interval_minutes,
                          ),
                        successTitle: "Provider saved",
                        successDetail: `${provider.display_name} updated in memory. It will reset on restart.`,
                        failureTitle: "Could not save provider",
                      })
                    }
                    size="sm"
                    variant="outline"
                  >
                    Save
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        </AsyncRegion>

        {threatIntel.recommendations.length > 0 ? (
          <ul className="mt-4 space-y-1">
            {threatIntel.recommendations.map((line) => (
              <li className="text-muted-foreground text-sm" key={line}>
                {line}
              </li>
            ))}
          </ul>
        ) : null}
      </SectionCard>

      <SectionCard
        actions={
          <Badge variant={federated.enabled ? "outline" : "secondary"}>
            {federated.enabled ? federated.privacy_mode : "Disabled"}
          </Badge>
        }
        description="Round scheduling for a coordinator that this build never contacts."
        footer={
          <Button
            isLoading={busy === "federated-learning-save"}
            onClick={() =>
              void mutate({
                key: "federated-learning-save",
                action: () =>
                  api.updateFederatedLearning(
                    federatedEnabled,
                    coordinator.trim() || null,
                    Number.parseInt(roundHours, 10) || 24,
                  ),
                successTitle: "Federated learning updated",
                successDetail: federatedEnabled
                  ? "Settings stored in memory until the appliance restarts."
                  : "Federated learning is disabled.",
                failureTitle: "Could not update federated learning",
              })
            }
          >
            Save
          </Button>
        }
        title="Federated learning"
      >
        <div className="space-y-4">
          <FormField label="Enable federated rounds" orientation="horizontal">
            <Switch
              checked={federatedEnabled}
              onCheckedChange={(details) => setFederatedEnabled(details.checked)}
            />
          </FormField>

          <FieldRow>
            <TextField
              label="Coordinator URL"
              onChange={setCoordinator}
              placeholder="https://coordinator.example.com"
              value={coordinator}
            />
            <TextField
              hint="Clamped to at least one hour server-side."
              inputMode="numeric"
              label="Round interval (hours)"
              onChange={setRoundHours}
              value={roundHours}
            />
          </FieldRow>

          <dl className="grid gap-6 sm:grid-cols-3">
            <SummaryTile label="Node ID" value={federated.node_id} />
            <SummaryTile label="Privacy mode" value={federated.privacy_mode} />
            <SummaryTile
              label="Raw log export"
              value={federated.raw_log_export_enabled ? "Enabled" : "Forced off"}
            />
          </dl>
        </div>
      </SectionCard>
    </div>
  );
}

/* -------------------------------------------------------------------------- */

function GuardPane() {
  const { data } = useCogwheel();
  const guard = data.settings.runtime_guard;

  return (
    <div className="flex flex-col gap-6">
      <SectionCard
        description="The runtime guard decides when the dashboard reports “Needs attention” and when a new ruleset is auto-rolled-back. It is configured by environment variables on the appliance; there is no endpoint to change it from here."
        title="Runtime guard (read-only)"
      >
        <dl className="grid gap-6 sm:grid-cols-3">
          <SummaryTile
            label="Max upstream failure delta"
            value={formatCount(guard.max_upstream_failures_delta)}
          />
          <SummaryTile
            label="Max fallback served delta"
            value={formatCount(guard.max_fallback_served_delta)}
          />
          <SummaryTile label="Probe domains" value={formatCount(guard.probe_domains.length)} />
        </dl>

        {guard.probe_domains.length > 0 ? (
          <ul className="mt-4 flex flex-wrap gap-1.5">
            {guard.probe_domains.map((domain) => (
              <li key={domain}>
                <Badge variant="outline">{domain}</Badge>
              </li>
            ))}
          </ul>
        ) : (
          <p className="mt-4 text-muted-foreground text-sm">
            No probe domains are configured, so the active health check verifies counters only.
          </p>
        )}

        <NoticeBanner
          className="mt-4"
          detail="The health comparison uses an all-zero baseline, so with both deltas at 0 the dashboard latches to “Needs attention” after the first upstream failure and never recovers on its own. Setting a non-zero delta on the appliance avoids that."
          title="Known behaviour worth knowing"
          tone="warn"
        />
      </SectionCard>

      <SectionCard
        description="Where the legacy classifier settings used to live."
        title="Classifier"
      >
        <p className="text-muted-foreground text-sm">
          Mode and sensitivity moved to their own screen, alongside the model's measured performance and
          the domain inspector.
        </p>
      </SectionCard>
    </div>
  );
}

function SummaryTile({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg border border-border px-3 py-2">
      <dt className="text-muted-foreground text-xs">{label}</dt>
      <dd className="mt-0.5 truncate text-foreground text-sm">{value}</dd>
    </div>
  );
}
