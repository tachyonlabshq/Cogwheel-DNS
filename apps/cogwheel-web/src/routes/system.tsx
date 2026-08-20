import React from "react";
import {
  ActivityIcon,
  DownloadIcon,
  FlaskConicalIcon,
  GaugeIcon,
  ScrollTextIcon,
  ShareIcon,
  StethoscopeIcon,
  UsersIcon,
} from "lucide-react";
import { useSearchParams } from "react-router-dom";
import {
  api,
  type AuditEvent,
  type BackupData,
  type ConfigVersionStatus,
  type LatencyBudgetCheck,
  type LoadTestResult,
  type ResilienceDrill,
  type ResilienceDrillResult,
  type SyncEnvelope,
  type SyncPeerStatus,
} from "@/lib/api";
import {
  AUDIT_FILTERS,
  matchesAuditFilter,
  recoveryActions,
  summarizeAuditEvent,
  type AuditFilterId,
} from "@/lib/audit";
import {
  formatCount,
  formatDateTime,
  formatMs,
  formatNanosAsMs,
  formatPercent,
  formatRelative,
  shortHash,
} from "@/lib/format";
import { cn } from "@/lib/utils";
import { notify } from "@/lib/toast";
import { useAsync } from "@/hooks/use-async";
import { useCogwheel } from "@/data/context";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Textarea } from "@/components/ui/textarea";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { PageHeader, PageShell } from "@/components/app/page";
import { SectionCard } from "@/components/app/section-card";
import { StatTile } from "@/components/app/stat-tile";
import { DataTable, type Column } from "@/components/app/data-table";
import { SelectField } from "@/components/app/select-field";
import { TextField } from "@/components/app/text-field";
import { FieldRow, FormField } from "@/components/app/form-field";
import { StatusIndicator, StatusPill } from "@/components/app/status-indicator";
import { ConfirmDialog } from "@/components/app/confirm-dialog";
import { AsyncRegion, ErrorState, NoticeBanner } from "@/components/app/states";

const TABS = ["diagnostics", "sync", "backup", "audit"] as const;
type TabId = (typeof TABS)[number];

export function SystemScreen() {
  const [params, setParams] = useSearchParams();
  const tab = (params.get("tab") ?? "diagnostics") as TabId;

  const setTab = (next: string) =>
    setParams(
      (current) => {
        const merged = new URLSearchParams(current);
        merged.set("tab", next);
        return merged;
      },
      { replace: true },
    );

  return (
    <PageShell>
      <PageHeader
        description="Diagnostics, replication, backups and the audit trail."
        title="System"
      />

      <Tabs
        onValueChange={(details) => setTab(details.value)}
        value={TABS.includes(tab) ? tab : "diagnostics"}
      >
        <TabsList className="mb-6">
          <TabsTrigger value="diagnostics">Diagnostics</TabsTrigger>
          <TabsTrigger value="sync">Sync</TabsTrigger>
          <TabsTrigger value="backup">Backup</TabsTrigger>
          <TabsTrigger value="audit">Audit trail</TabsTrigger>
        </TabsList>

        <TabsContent value="diagnostics">
          <DiagnosticsPane />
        </TabsContent>
        <TabsContent value="sync">
          <SyncPane />
        </TabsContent>
        <TabsContent value="backup">
          <BackupPane />
        </TabsContent>
        <TabsContent value="audit">
          <AuditPane onFilterNotifications={() => setTab("audit")} />
        </TabsContent>
      </Tabs>
    </PageShell>
  );
}

/* -------------------------------------------------------------------------- */

function DiagnosticsPane() {
  const { data, phase, error, busy, mutate, reload } = useCogwheel();
  const health = data.dashboard.runtime_health;
  const snapshot = health.snapshot;
  const latency = data.latencyBudget;
  const tailscale = data.tailscale;

  const version = useAsync<ConfigVersionStatus>("config-version", (signal) =>
    api.configVersion({ signal }),
  );

  const [durationSecs, setDurationSecs] = React.useState("5");
  const [qps, setQps] = React.useState("50");
  const [cacheRatio, setCacheRatio] = React.useState("0.8");
  const [loadTest, setLoadTest] = React.useState<LoadTestResult | null>(null);
  const [confirmLoadTest, setConfirmLoadTest] = React.useState(false);
  const [drill, setDrill] = React.useState<ResilienceDrill>("upstream-outage");
  const [drillResult, setDrillResult] =
    React.useState<ResilienceDrillResult | null>(null);
  const [confirmExitNode, setConfirmExitNode] = React.useState(false);
  const [confirmTailscaleRollback, setConfirmTailscaleRollback] =
    React.useState(false);

  const latencyColumns: Column<LatencyBudgetCheck>[] = [
    { key: "label", header: "Path", render: (row) => row.label },
    {
      key: "target",
      header: "Target p50",
      align: "end",
      hideOnStack: true,
      render: (row) => (
        <span className="tabular">{formatMs(row.target_p50_ms)}</span>
      ),
    },
    {
      key: "observed",
      header: "Observed mean",
      align: "end",
      render: (row) => (
        <span className="tabular">{formatMs(row.observed_ms)}</span>
      ),
      sortValue: (row) => row.observed_ms,
    },
    {
      key: "samples",
      header: "Samples",
      align: "end",
      hideOnStack: true,
      render: (row) => (
        <span className="tabular">{formatCount(row.sample_count)}</span>
      ),
      sortValue: (row) => row.sample_count,
    },
    {
      key: "status",
      header: "Status",
      align: "end",
      render: (row) =>
        row.status === "within-budget" ? (
          <StatusPill label="Within budget" tone="good" />
        ) : row.status === "over-budget" ? (
          <StatusPill label="Over budget" tone="bad" />
        ) : (
          <StatusPill label="Not enough data" tone="idle" />
        ),
      sortValue: (row) => row.status,
    },
  ];

  return (
    <div className="flex flex-col gap-6">
      <SectionCard
        actions={
          <Button
            isLoading={busy === "runtime-health-check"}
            onClick={() =>
              void mutate({
                key: "runtime-health-check",
                action: () => api.runtimeHealthCheck(),
                successTitle: (report) =>
                  report.degraded ? "Runtime degraded" : "Runtime healthy",
                successDetail: (report) =>
                  report.notes[0] ??
                  "Runtime guard probes completed without regressions.",
                failureTitle: "Health check failed",
              })
            }
            size="sm"
            variant="outline"
          >
            <StethoscopeIcon aria-hidden />
            Run health check
          </Button>
        }
        description="Counters reported by the DNS runtime, plus an on-demand probe of the configured guard domains."
        title="Runtime health"
      >
        {/* Guarded: before the first poll resolves, `runtime_health` is the all-zero default, so
            rendering it unguarded stated "Healthy" in green next to zeroed counters — asserting
            the resolver was fine when nothing had been measured yet. */}
        <AsyncRegion
          empty={null}
          error={error}
          isEmpty={false}
          loading={phase === "loading"}
          onRetry={reload}
          skeleton="cards"
          skeletonRows={4}
        >
          <StatusIndicator
            className="mb-4"
            description={
              health.notes[0] ?? "No regressions reported by the runtime guard."
            }
            label={health.degraded ? "Degraded" : "Healthy"}
            showIcon
            tone={health.degraded ? "bad" : "good"}
          />

          <div className="grid gap-6 sm:grid-cols-2 xl:grid-cols-4">
            <StatTile
              label="Queries"
              value={formatCount(snapshot.queries_total)}
            />
            <StatTile
              label="Blocked"
              value={formatCount(snapshot.blocked_total)}
            />
            <StatTile
              label="Upstream failures"
              tone={snapshot.upstream_failures_total > 0 ? "warn" : "neutral"}
              toneLabel={
                snapshot.upstream_failures_total > 0 ? "Seen" : undefined
              }
              value={formatCount(snapshot.upstream_failures_total)}
            />
            <StatTile
              label="Fallback served"
              value={formatCount(snapshot.fallback_served_total)}
            />
            <StatTile
              label="Cache hits"
              value={formatCount(snapshot.cache_hits_total)}
            />
            <StatTile
              hint="A tracker can hide behind an alias on the site's own domain. Uncloaking follows the alias to the real destination so the filter judges where the request actually goes."
              label="CNAME uncloaks"
              value={formatCount(snapshot.cname_uncloaks_total)}
            />
            <StatTile
              hint="Queries blocked only after following that alias. The hostname asked for was on no list; the address it pointed at was."
              label="CNAME blocks"
              value={formatCount(snapshot.cname_blocks_total)}
            />
            <StatTile
              delta={`${formatCount(snapshot.classifier_latency_samples)} samples`}
              label="Classifier latency"
              value={formatNanosAsMs(snapshot.classifier_latency_avg_ns)}
            />
          </div>

          {health.notes.length > 0 ? (
            <ul className="mt-4 space-y-1">
              {health.notes.map((note) => (
                <li className="text-muted-foreground text-sm" key={note}>
                  {note}
                </li>
              ))}
            </ul>
          ) : null}
        </AsyncRegion>
      </SectionCard>

      <SectionCard
        actions={
          <Badge variant="outline">
            Cache hit rate {formatPercent(latency.cache_hit_rate, 1)}
          </Badge>
        }
        description="Targets are hardcoded on the appliance and `observed` is a mean, not a true p50, despite the column name coming from the API."
        title="Latency budget"
      >
        {!latency.within_budget ? (
          <NoticeBanner
            className="mb-4"
            detail={
              latency.recommendations[0] ??
              "At least one path is over its target."
            }
            title="Outside budget"
            tone="warn"
          />
        ) : null}

        <DataTable
          columns={latencyColumns}
          stackBelow="xl"
          empty={{
            icon: GaugeIcon,
            title: "No latency samples yet",
            description:
              "Checks appear once the resolver has answered enough queries to average.",
          }}
          error={error}
          loading={phase === "loading"}
          onRetry={() => void reload()}
          rowKey={(row) => row.label}
          rows={latency.checks}
        />

        {latency.recommendations.length > 0 ? (
          <ul className="mt-4 space-y-1">
            {latency.recommendations.map((line) => (
              <li className="text-muted-foreground text-sm" key={line}>
                {line}
              </li>
            ))}
          </ul>
        ) : null}
      </SectionCard>

      <SectionCard
        description="Runs real recursive DNS traffic against the configured upstreams. It is not a simulation and it will show up in your upstream's logs."
        footer={
          <Button
            isLoading={busy === "load-test"}
            onClick={() => setConfirmLoadTest(true)}
            variant="outline"
          >
            <FlaskConicalIcon aria-hidden />
            Run load test
          </Button>
        }
        title="Load test"
      >
        <div className="grid gap-6 sm:grid-cols-3">
          <TextField
            hint="Unbounded server-side; keep it short."
            inputMode="numeric"
            label="Duration (seconds)"
            onChange={setDurationSecs}
            value={durationSecs}
          />
          <TextField
            inputMode="numeric"
            label="Queries per second"
            onChange={setQps}
            value={qps}
          />
          <TextField
            hint="Steers which domains are picked; the response echoes this value rather than measuring it."
            inputMode="decimal"
            label="Cache hit ratio"
            onChange={setCacheRatio}
            value={cacheRatio}
          />
        </div>

        {loadTest ? (
          <div className="mt-4 grid gap-6 sm:grid-cols-2 xl:grid-cols-4">
            <StatTile
              label="Sent"
              tone={loadTest.success ? "good" : "bad"}
              toneLabel={loadTest.success ? "No failures" : "Failures"}
              value={formatCount(loadTest.queries_sent)}
            />
            <StatTile
              delta={`${formatCount(loadTest.queries_failed)} failed`}
              label="Succeeded"
              value={formatCount(loadTest.queries_succeeded)}
            />
            <StatTile
              delta={`p95 ${formatMs(loadTest.p95_latency_ms)} · p99 ${formatMs(loadTest.p99_latency_ms)}`}
              label="Average latency"
              value={formatMs(loadTest.avg_latency_ms)}
            />
            <StatTile
              label="Throughput"
              value={`${loadTest.throughput_qps.toFixed(1)} qps`}
            />
          </div>
        ) : null}

        {loadTest && loadTest.errors.length > 0 ? (
          <ul className="mt-4 space-y-1">
            {loadTest.errors.map((line) => (
              <li
                className="font-mono text-destructive-foreground text-xs"
                key={line}
              >
                {line}
              </li>
            ))}
          </ul>
        ) : null}
      </SectionCard>

      <SectionCard
        description="These endpoints are named “simulate” but inject nothing. They read current counters and return advice, so treat them as status reports."
        footer={
          <Button
            isLoading={busy === "resilience-drill"}
            onClick={() =>
              void mutate({
                key: "resilience-drill",
                action: () => api.runResilienceDrill(drill),
                successTitle: "Drill completed",
                successDetail: (result) => result.message,
                failureTitle: "Drill failed",
                after: "none",
              }).then((result) => {
                if (result) setDrillResult(result);
                return result;
              })
            }
            variant="outline"
          >
            Run drill
          </Button>
        }
        title="Resilience drills"
      >
        <SelectField
          label="Drill"
          onChange={(value) => setDrill(value as ResilienceDrill)}
          options={[
            { value: "upstream-outage", label: "Upstream outage readiness" },
            { value: "db-corruption", label: "Database readability" },
            { value: "source-failure", label: "Source availability" },
            { value: "sync-partition", label: "Sync partition readiness" },
          ]}
          value={drill}
        />

        {drillResult ? (
          <div className="mt-4 rounded-xl border border-border p-4">
            <StatusIndicator
              description={drillResult.message}
              label={drillResult.success ? "Ready" : "Not ready"}
              showIcon
              tone={drillResult.success ? "good" : "warn"}
            />
            {drillResult.recommendations.length > 0 ? (
              <ul className="mt-3 space-y-1">
                {drillResult.recommendations.map((line) => (
                  <li className="text-muted-foreground text-sm" key={line}>
                    {line}
                  </li>
                ))}
              </ul>
            ) : null}
          </div>
        ) : null}
      </SectionCard>

      <SectionCard
        actions={
          <Badge variant={tailscale.installed ? "outline" : "secondary"}>
            {tailscale.exit_node_active
              ? "Exit node advertised"
              : tailscale.installed
                ? "Installed"
                : "Not installed"}
          </Badge>
        }
        description="Tailscale integration for reaching this resolver off-LAN."
        footer={
          <>
            <Button
              disabled={!tailscale.installed || !tailscale.daemon_running}
              isLoading={busy === "tailscale-exit-node"}
              onClick={() => setConfirmExitNode(true)}
              variant="outline"
            >
              {tailscale.exit_node_active
                ? "Stop advertising exit node"
                : "Advertise as exit node"}
            </Button>
            <Button
              isLoading={busy === "tailscale-rollback"}
              onClick={() => setConfirmTailscaleRollback(true)}
              variant="ghost"
            >
              Roll back
            </Button>
          </>
        }
        title="Tailscale"
      >
        <dl className="grid gap-6 sm:grid-cols-2 xl:grid-cols-4">
          <SummaryTile label="Host" value={tailscale.hostname ?? "Unknown"} />
          <SummaryTile
            label="Tailnet"
            value={tailscale.tailnet_name ?? "Unknown"}
          />
          <SummaryTile
            label="Peers"
            value={formatCount(tailscale.peer_count)}
          />
          <SummaryTile label="Version" value={tailscale.version ?? "Unknown"} />
        </dl>

        {data.tailscaleDns.suggestions.length > 0 ? (
          <NoticeBanner
            className="mt-4"
            detail={data.tailscaleDns.suggestions.join(" ")}
            title={data.tailscaleDns.message || "Tailscale DNS check"}
            tone="neutral"
          />
        ) : null}

        {tailscale.last_error ? (
          <p className="mt-3 text-destructive-foreground text-sm">
            {tailscale.last_error}
          </p>
        ) : null}

        {tailscale.health_warnings.length > 0 ? (
          <ul className="mt-3 space-y-1">
            {tailscale.health_warnings.map((warning) => (
              <li className="text-muted-foreground text-sm" key={warning}>
                {warning}
              </li>
            ))}
          </ul>
        ) : null}
      </SectionCard>

      <SectionCard description="Schema and build information." title="Version">
        {version.error && !version.data ? (
          <ErrorState
            detail={version.error}
            onRetry={version.reload}
            title="Could not read the version"
          />
        ) : version.data ? (
          <>
            <dl className="grid gap-6 sm:grid-cols-2 xl:grid-cols-4">
              <SummaryTile
                label="Cogwheel"
                value={version.data.cogwheel_version}
              />
              <SummaryTile
                label="Schema version"
                value={formatCount(version.data.schema_version)}
              />
              <SummaryTile
                label="Config version"
                value={formatCount(version.data.config_version)}
              />
              <SummaryTile
                label="Upgrade available"
                value={version.data.upgrade_available ? "Yes" : "No"}
              />
            </dl>
            {version.data.recommendations.length > 0 ? (
              <ul className="mt-4 space-y-1">
                {version.data.recommendations.map((line) => (
                  <li className="text-muted-foreground text-sm" key={line}>
                    {line}
                  </li>
                ))}
              </ul>
            ) : null}
          </>
        ) : (
          <p className="text-muted-foreground text-sm">Loading…</p>
        )}
      </SectionCard>

      <ConfirmDialog
        confirmLabel="Run load test"
        consequence="Real recursive queries are issued to the configured upstreams for the whole duration, and a worker thread is occupied while it runs."
        description={`${qps} queries per second for ${durationSecs} seconds against this resolver's real upstreams.`}
        onConfirm={async () => {
          const result = await mutate({
            key: "load-test",
            action: () =>
              api.runLoadTest(
                Number.parseInt(durationSecs, 10) || 5,
                Number.parseInt(qps, 10) || 50,
                Number.parseFloat(cacheRatio) || 0.8,
              ),
            successTitle: "Load test finished",
            successDetail: (report) =>
              `${formatCount(report.queries_succeeded)} of ${formatCount(report.queries_sent)} queries succeeded.`,
            failureTitle: "Load test failed",
            after: "none",
          });
          if (result) setLoadTest(result);
        }}
        onOpenChange={setConfirmLoadTest}
        open={confirmLoadTest}
        title="Run a load test against live upstreams?"
      />

      <ConfirmDialog
        confirmLabel={
          tailscale.exit_node_active
            ? "Stop advertising"
            : "Advertise exit node"
        }
        consequence="This runs `tailscale up` on the appliance and reconfigures host networking. The previous value is saved so it can be rolled back."
        description={`Exit-node advertising for ${tailscale.hostname ?? "this node"} will be turned ${
          tailscale.exit_node_active ? "off" : "on"
        }.`}
        destructive
        onConfirm={async () => {
          await mutate({
            key: "tailscale-exit-node",
            action: () => api.tailscaleExitNode(!tailscale.exit_node_active),
            successTitle: tailscale.exit_node_active
              ? "Exit node disabled"
              : "Exit node enabled",
            successDetail: (result) => result.message,
            failureTitle: "Could not change exit-node state",
          });
        }}
        onOpenChange={setConfirmExitNode}
        open={confirmExitNode}
        title="Reconfigure Tailscale networking?"
      />

      <ConfirmDialog
        confirmLabel="Roll back exit-node settings"
        consequence="This runs `tailscale up` on the appliance and reconfigures host networking, exactly like advertising the exit node does. Whatever the appliance saved as the previous value is what gets restored; there is no preview of it."
        description={`Exit-node advertising for ${
          tailscale.hostname ?? "this node"
        } will be reset to the value saved before it was last changed.`}
        destructive
        onConfirm={async () => {
          await mutate({
            key: "tailscale-rollback",
            action: () => api.tailscaleRollback(),
            successTitle: "Exit node rolled back",
            successDetail: (result) => result.message,
            failureTitle: "Could not roll back",
          });
        }}
        onOpenChange={setConfirmTailscaleRollback}
        open={confirmTailscaleRollback}
        title="Roll back Tailscale networking?"
      />
    </div>
  );
}

/* -------------------------------------------------------------------------- */

function SyncPane() {
  const { data, phase, error, busy, mutate, reload } = useCogwheel();
  const sync = data.syncStatus;

  const [profile, setProfile] = React.useState(sync.profile);
  const [transport, setTransport] = React.useState(sync.transport_mode);
  const [token, setToken] = React.useState("");
  const [confirmTransport, setConfirmTransport] = React.useState(false);
  const [envelopeText, setEnvelopeText] = React.useState("");
  const [confirmImport, setConfirmImport] = React.useState(false);

  React.useEffect(() => {
    setProfile(sync.profile);
    setTransport(sync.transport_mode);
  }, [sync.profile, sync.transport_mode]);

  const parsedEnvelope = React.useMemo<SyncEnvelope | null>(() => {
    if (!envelopeText.trim()) return null;
    try {
      const value: unknown = JSON.parse(envelopeText);
      if (
        value &&
        typeof value === "object" &&
        "payload_b64" in value &&
        "signature_b64" in value &&
        "node_public_key" in value
      ) {
        return value as SyncEnvelope;
      }
      return null;
    } catch {
      return null;
    }
  }, [envelopeText]);

  const exportState = async () => {
    const envelope = await mutate({
      key: "sync-export",
      action: () => api.exportSyncState(),
      successTitle: "Sync state exported",
      successDetail: "The signed envelope was downloaded.",
      failureTitle: "Could not export sync state",
      after: "none",
    });
    if (!envelope) return;
    // The endpoint returns JSON, so the file is assembled client-side.
    const blob = new Blob([JSON.stringify(envelope, null, 2)], {
      type: "application/json",
    });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `cogwheel-sync-${new Date().toISOString().slice(0, 10)}.json`;
    anchor.click();
    URL.revokeObjectURL(url);
  };

  const peerColumns: Column<SyncPeerStatus>[] = [
    {
      key: "key",
      header: "Node key",
      render: (row) => (
        <span className="font-mono text-xs">
          {row.node_public_key.slice(0, 16)}…
        </span>
      ),
    },
    {
      key: "profile",
      header: "Profile",
      render: (row) => <Badge variant="outline">{row.profile}</Badge>,
    },
    {
      key: "imports",
      header: "Imports",
      align: "end",
      render: (row) => (
        <span className="tabular">{formatCount(row.imports)}</span>
      ),
      sortValue: (row) => row.imports,
    },
    {
      key: "revision",
      header: "Revision",
      align: "end",
      hideOnStack: true,
      render: (row) => (
        <span className="tabular">{formatCount(row.last_revision)}</span>
      ),
      sortValue: (row) => row.last_revision,
    },
    {
      key: "last",
      header: "Last import",
      align: "end",
      render: (row) => (
        <span className="text-muted-foreground text-xs">
          {formatDateTime(row.last_import_at)}
        </span>
      ),
      sortValue: (row) => row.last_import_at,
    },
  ];

  return (
    <div className="flex flex-col gap-6">
      <SectionCard
        description="This node's identity and replication state."
        title="Node"
      >
        <dl className="grid gap-6 sm:grid-cols-2 xl:grid-cols-4">
          <SummaryTile label="Profile" value={sync.profile} />
          <SummaryTile label="Revision" value={formatCount(sync.revision)} />
          <SummaryTile label="Transport" value={sync.transport_mode} />
          <SummaryTile
            label="Bearer token"
            value={sync.transport_token_configured ? "Configured" : "Not set"}
          />
        </dl>
        <p className="mt-3 break-all font-mono text-muted-foreground text-xs">
          {sync.local_node_public_key || "Node key not reported"}
        </p>
      </SectionCard>

      <SectionCard
        description="Full replication copies sources and devices; settings-only skips them; a read-only follower never exports."
        footer={
          <Button
            isLoading={busy === "sync-profile-save"}
            onClick={() =>
              void mutate({
                key: "sync-profile-save",
                action: () => api.updateSyncProfile(profile),
                successTitle: "Sync profile updated",
                successDetail: `Node sync profile is now ${profile}.`,
                failureTitle: "Could not update sync profile",
              })
            }
            variant="secondary"
          >
            Save profile
          </Button>
        }
        title="Replication profile"
      >
        <SelectField
          hint="Unrecognised values are silently coerced to full replication by the appliance."
          label="Profile"
          onChange={setProfile}
          options={[
            { value: "full", label: "Full replication" },
            { value: "settings-only", label: "Settings only" },
            { value: "read-only-follower", label: "Read-only follower" },
          ]}
          value={profile}
        />
      </SectionCard>

      <SectionCard
        description="The transport policy guards the sync endpoints — including the one that changes it."
        footer={
          <Button
            isLoading={busy === "sync-transport-save"}
            onClick={() => setConfirmTransport(true)}
            variant="secondary"
          >
            Save transport
          </Button>
        }
        title="Transport"
      >
        <div className="space-y-4">
          <NoticeBanner
            detail="Selecting “HTTPS required” behind a proxy that does not send x-forwarded-proto: https makes every sync endpoint permanently unreachable over HTTP. Recovery means editing SQLite on the appliance by hand."
            title="Lockout hazard"
            tone="bad"
          />
          <FieldRow>
            <SelectField
              label="Transport mode"
              onChange={setTransport}
              options={[
                { value: "opportunistic", label: "Opportunistic" },
                { value: "https-required", label: "HTTPS required" },
              ]}
              value={transport}
            />
            <TextField
              hint={
                sync.transport_token_configured
                  ? "Set a new token, or leave blank to keep the current one."
                  : "Optional shared bearer token. Stored in cleartext on the appliance."
              }
              label="Bearer token"
              onChange={setToken}
              type="password"
              value={token}
            />
          </FieldRow>
        </div>
      </SectionCard>

      <SectionCard
        description="Exports a signed, replayable snapshot of this node's state for another Cogwheel appliance to import. It carries the notification webhook URL in cleartext."
        footer={
          <Button
            isLoading={busy === "sync-export"}
            onClick={() => void exportState()}
            variant="outline"
          >
            <ShareIcon aria-hidden />
            Export signed envelope
          </Button>
        }
        title="Export state"
      >
        <p className="text-muted-foreground text-sm">
          A read-only follower cannot export. The exported revision is not
          written back on this build, so repeated exports emit the same revision
          number.
        </p>
      </SectionCard>

      <SectionCard
        description="Applies another node's signed envelope to this one."
        footer={
          <Button
            disabled={!parsedEnvelope}
            isLoading={busy === "sync-import"}
            onClick={() => setConfirmImport(true)}
            variant="destructive"
          >
            Import envelope
          </Button>
        }
        title="Import state"
      >
        <div className="space-y-3">
          <NoticeBanner
            detail="Under the full replication profile the appliance deletes every existing source and device before inserting the payload's, and it does so without a transaction. A failure part-way leaves this node with an empty or half-populated configuration."
            title="Importing is destructive"
            tone="bad"
          />
          <FormField
            error={
              envelopeText.trim() && !parsedEnvelope
                ? "That is not a valid signed sync envelope."
                : undefined
            }
            label="Signed envelope JSON"
          >
            <Textarea
              className="min-h-32 font-mono text-xs"
              onChange={(event) => setEnvelopeText(event.target.value)}
              placeholder='{"node_public_key":"…","payload_b64":"…","signature_b64":"…"}'
              value={envelopeText}
            />
          </FormField>
        </div>
      </SectionCard>

      <SectionCard
        description="Reconstructed from the last 200 audit events — there is no peer table, so older peers drop off silently."
        title="Peers"
      >
        <DataTable
          columns={peerColumns}
          empty={{
            icon: UsersIcon,
            title: "No peers seen",
            description:
              "Peers appear after this node imports state from another Cogwheel appliance.",
          }}
          error={error}
          loading={phase === "loading"}
          onRetry={() => void reload()}
          rowKey={(row) => row.node_public_key}
          rows={sync.peers}
        />
      </SectionCard>

      <ConfirmDialog
        confirmLabel={`Set transport to ${transport}`}
        consequence={
          transport === "https-required"
            ? "If this appliance is not behind a proxy that sets x-forwarded-proto: https, every sync endpoint — including the one that undoes this — becomes unreachable."
            : "Sync endpoints will accept plain HTTP as well as HTTPS."
        }
        description={`Transport mode for sync on this node becomes "${transport}"${
          token.trim() ? ", and the bearer token is replaced" : ""
        }.`}
        destructive={transport === "https-required"}
        onConfirm={async () => {
          await mutate({
            key: "sync-transport-save",
            action: () =>
              api.updateSyncTransport(transport, token.trim() || undefined),
            successTitle: "Sync transport updated",
            successDetail: `Transport mode is now ${transport}.`,
            failureTitle: "Could not update sync transport",
          });
          setToken("");
        }}
        onOpenChange={setConfirmTransport}
        open={confirmTransport}
        title="Change the sync transport policy?"
      />

      <ConfirmDialog
        confirmLabel="Import and overwrite"
        consequence="Any source or device on this appliance that is absent from the envelope is deleted. There is no undo, and the operation is not transactional."
        description={`State signed by node ${
          parsedEnvelope
            ? `${parsedEnvelope.node_public_key.slice(0, 16)}…`
            : ""
        } will replace this node's sources and devices.`}
        destructive
        onConfirm={async () => {
          if (!parsedEnvelope) return;
          await mutate({
            key: "sync-import",
            action: () => api.importSyncState(parsedEnvelope),
            successTitle: "Sync state imported",
            successDetail: (result) =>
              `${result.imported_sources} source(s) and ${result.imported_devices} device(s) applied at revision ${result.applied_revision}.`,
            failureTitle: "Could not import sync state",
          });
          setEnvelopeText("");
        }}
        onOpenChange={setConfirmImport}
        open={confirmImport}
        title="Import this node's state?"
      />
    </div>
  );
}

/* -------------------------------------------------------------------------- */

function BackupPane() {
  const { busy, mutate } = useCogwheel();
  const backup = useAsync<BackupData>("backup", (signal) =>
    api.backup({ signal }),
  );
  const [restoreText, setRestoreText] = React.useState("");
  const [confirmRestore, setConfirmRestore] = React.useState(false);

  const download = () => {
    if (!backup.data) return;
    // The API returns JSON rather than a file, so the download is assembled here.
    const blob = new Blob([JSON.stringify(backup.data, null, 2)], {
      type: "application/json",
    });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `cogwheel-backup-${new Date().toISOString().slice(0, 10)}.json`;
    anchor.click();
    URL.revokeObjectURL(url);
    notify.success(
      "Backup downloaded",
      "The file contains the webhook URL in cleartext — store it safely.",
    );
  };

  const parsed = React.useMemo<BackupData | null>(() => {
    if (!restoreText.trim()) return null;
    try {
      const value: unknown = JSON.parse(restoreText);
      if (
        value &&
        typeof value === "object" &&
        "version" in value &&
        "sources" in value
      ) {
        return value as BackupData;
      }
      return null;
    } catch {
      return null;
    }
  }, [restoreText]);

  return (
    <div className="flex flex-col gap-6">
      <NoticeBanner
        detail="A backup contains sources, devices, classifier settings and notification settings — including the webhook URL in cleartext. It omits block profiles, service toggles, sync settings, rulesets and audit history."
        title="What a backup does and does not contain"
        tone="neutral"
      />

      <SectionCard
        actions={
          <Button onClick={backup.reload} size="sm" variant="outline">
            Refresh
          </Button>
        }
        description="A point-in-time copy of the appliance's core configuration."
        footer={
          <Button disabled={!backup.data} onClick={download}>
            <DownloadIcon aria-hidden />
            Download JSON
          </Button>
        }
        title="Export"
      >
        {backup.loading && !backup.data ? (
          <p className="text-muted-foreground text-sm">Loading…</p>
        ) : backup.error && !backup.data ? (
          <ErrorState
            detail={backup.error}
            onRetry={backup.reload}
            title="Could not read the backup"
          />
        ) : backup.data ? (
          <dl className="grid gap-6 sm:grid-cols-2 xl:grid-cols-4">
            <SummaryTile label="Format" value={backup.data.version} />
            <SummaryTile
              label="Created"
              value={formatDateTime(backup.data.created_at)}
            />
            <SummaryTile
              label="Sources"
              value={formatCount(backup.data.sources.length)}
            />
            <SummaryTile
              label="Devices"
              value={formatCount(backup.data.devices.length)}
            />
          </dl>
        ) : null}
      </SectionCard>

      <SectionCard
        description="Paste a previously exported backup. The appliance merges it in rather than replacing what is there."
        footer={
          <Button
            disabled={!parsed}
            isLoading={busy === "backup-restore"}
            onClick={() => setConfirmRestore(true)}
            variant="destructive"
          >
            Restore
          </Button>
        }
        title="Restore"
      >
        <div className="space-y-3">
          <NoticeBanner
            detail="Restore is additive: existing sources and devices absent from the file survive. The appliance also drops the classifier block and never persists the notification block, so those two do not survive a restart."
            title="Restore is not a true replace"
            tone="warn"
          />
          <FormField
            error={
              restoreText.trim() && !parsed
                ? "That is not a valid Cogwheel backup document."
                : undefined
            }
            label="Backup JSON"
          >
            <Textarea
              className="min-h-40 font-mono text-xs"
              onChange={(event) => setRestoreText(event.target.value)}
              placeholder='{"version":"1.0", …}'
              value={restoreText}
            />
          </FormField>
        </div>
      </SectionCard>

      <ConfirmDialog
        confirmLabel="Restore backup"
        consequence="Sources and devices in the file are inserted or replaced by id. Per-record failures are swallowed server-side, so verify the lists afterwards."
        description={`${formatCount(parsed?.sources.length ?? 0)} source(s) and ${formatCount(
          parsed?.devices.length ?? 0,
        )} device(s) from backup "${parsed?.version ?? ""}" (created ${formatDateTime(
          parsed?.created_at,
        )}) will be written to this appliance.`}
        destructive
        onConfirm={async () => {
          if (!parsed) return;
          await mutate({
            key: "backup-restore",
            action: () => api.restoreBackup(parsed),
            successTitle: "Restore completed",
            successDetail: (result) => result.message,
            failureTitle: "Restore failed",
          });
          setRestoreText("");
        }}
        onOpenChange={setConfirmRestore}
        open={confirmRestore}
        title="Restore this backup?"
      />
    </div>
  );
}

/* -------------------------------------------------------------------------- */

function AuditPane({
  onFilterNotifications,
}: {
  onFilterNotifications: () => void;
}) {
  const { data, busy, mutate } = useCogwheel();
  const [filter, setFilter] = React.useState<AuditFilterId>("all");
  const [expanded, setExpanded] = React.useState<string | null>(null);
  const [confirmRollback, setConfirmRollback] = React.useState(false);

  const events = useAsync<AuditEvent[]>("audit-events", (signal) =>
    api.auditEvents({ signal }),
  );
  const rows = React.useMemo(
    () =>
      (events.data ?? []).filter((event) => matchesAuditFilter(event, filter)),
    [events.data, filter],
  );

  const actions = recoveryActions(data.dashboard);

  const runAction = (kind: (typeof actions)[number]["kind"]) => {
    switch (kind) {
      case "health-check":
        return mutate({
          key: "runtime-health-check",
          action: () => api.runtimeHealthCheck(),
          successTitle: (report) =>
            report.degraded ? "Runtime degraded" : "Runtime healthy",
          successDetail: (report) =>
            report.notes[0] ?? "Guard probes completed without regressions.",
          failureTitle: "Health check failed",
        });
      case "refresh-sources":
        return mutate({
          key: "refresh-sources",
          action: () => api.refreshSources(),
          successTitle: "Sources refreshed",
          successDetail: (result) =>
            result.notes[0] ?? `Outcome: ${result.outcome}.`,
          failureTitle: "Could not refresh sources",
        });
      case "rollback-ruleset":
        // Guarded exactly like the identical action on Insights, which names the
        // ruleset being replaced before it swaps the appliance's policy out.
        setConfirmRollback(true);
        return Promise.resolve(null);
      case "filter-notifications":
        setFilter("notifications");
        onFilterNotifications();
        return Promise.resolve(null);
      default:
        return Promise.resolve(null);
    }
  };

  const columns: Column<AuditEvent>[] = [
    {
      key: "event",
      header: "Event",
      className: "whitespace-normal",
      render: (row) => {
        const summary = summarizeAuditEvent(row);
        return (
          <span>
            <span className="block font-medium text-foreground text-sm">
              {summary.title}
            </span>
            <span className="block text-muted-foreground text-xs">
              {summary.detail}
            </span>
          </span>
        );
      },
    },
    {
      key: "type",
      header: "Type",
      hideOnStack: true,
      render: (row) => (
        <span className="font-mono text-muted-foreground text-xs">
          {row.event_type}
        </span>
      ),
      sortValue: (row) => row.event_type,
    },
    {
      key: "category",
      header: "Category",
      hideOnStack: true,
      render: (row) => (
        <Badge variant="outline">{row.event_type.split(".")[0]}</Badge>
      ),
    },
    {
      key: "when",
      header: "When",
      align: "end",
      render: (row) => (
        <span className="text-muted-foreground text-xs">
          {formatRelative(row.created_at)}
        </span>
      ),
      sortValue: (row) => row.created_at,
    },
    {
      key: "payload",
      header: "Payload",
      align: "end",
      hideOnStack: true,
      render: (row) => (
        <Button
          onClick={(event) => {
            event.stopPropagation();
            setExpanded((current) => (current === row.id ? null : row.id));
          }}
          size="sm"
          variant="ghost"
        >
          {expanded === row.id ? "Hide" : "Show"}
        </Button>
      ),
    },
  ];

  const expandedEvent = rows.find((row) => row.id === expanded);

  return (
    <div className="flex flex-col gap-6">
      <SectionCard
        description="Suggested next steps, computed from the current dashboard state."
        title="Guided recovery"
      >
        <ul className="grid gap-6 xl:grid-cols-3">
          {actions.map((action) => (
            <li
              className="flex flex-col rounded-xl border border-border p-4"
              key={action.id}
            >
              <p className="font-medium text-foreground text-sm">
                {action.title}
              </p>
              <p className="mt-1 flex-1 text-muted-foreground text-sm">
                {action.detail}
              </p>
              <details className="mt-2">
                <summary className="cursor-pointer text-muted-foreground text-xs hover:text-foreground">
                  What this does
                </summary>
                <ol className="mt-1.5 list-decimal space-y-1 ps-4 text-muted-foreground text-xs">
                  {action.steps.map((step) => (
                    <li key={step}>{step}</li>
                  ))}
                </ol>
              </details>
              <Button
                className="mt-3"
                isLoading={busy === action.busyKey}
                onClick={() => void runAction(action.kind)}
                size="sm"
                variant="outline"
              >
                {action.actionLabel}
              </Button>
            </li>
          ))}
        </ul>
      </SectionCard>

      <SectionCard
        actions={
          <Button onClick={events.reload} size="sm" variant="outline">
            Refresh
          </Button>
        }
        description="The 20 most recent audit events. The appliance offers no pagination beyond that."
        title="Audit trail"
      >
        <div className="mb-4 flex flex-wrap gap-1.5">
          {AUDIT_FILTERS.map((option) => (
            <Button
              className={cn(option.id === filter && "pointer-events-none")}
              key={option.id}
              onClick={() => setFilter(option.id)}
              pill
              size="sm"
              variant={option.id === filter ? "default" : "outline"}
            >
              {option.label}
            </Button>
          ))}
        </div>

        <DataTable
          columns={columns}
          empty={{
            icon: filter === "all" ? ScrollTextIcon : ActivityIcon,
            title:
              filter === "all"
                ? "No audit events recorded"
                : "No events match this filter",
            description:
              filter === "all"
                ? "Audit entries are written whenever configuration changes or the runtime guard runs."
                : "Choose a different category to see the rest of the trail.",
          }}
          error={events.error}
          loading={events.loading}
          onRetry={events.reload}
          rowKey={(row) => row.id}
          rows={rows}
        />

        {expandedEvent ? (
          <pre className="mt-4 overflow-x-auto rounded-lg bg-muted p-3 font-mono text-xs">
            {formatPayload(expandedEvent.payload)}
          </pre>
        ) : null}
      </SectionCard>

      <ConfirmDialog
        confirmLabel="Roll back ruleset"
        consequence="The previous ruleset is reactivated and the policy catalog is rebuilt, which re-fetches every enabled source over HTTP."
        description={`The active ruleset ${
          data.dashboard.active_ruleset
            ? shortHash(data.dashboard.active_ruleset.hash)
            : "(none)"
        } will be replaced by the previously active one.`}
        destructive
        onConfirm={async () => {
          await mutate({
            key: "rollback-ruleset",
            action: () => api.rollbackRuleset(),
            successTitle: "Rollback completed",
            successDetail: (result) =>
              `Restored ruleset ${shortHash(result.hash)}.`,
            failureTitle: "Could not roll back",
          });
        }}
        onOpenChange={setConfirmRollback}
        open={confirmRollback}
        title="Roll back to the previous ruleset?"
      />
    </div>
  );
}

function formatPayload(payload: string): string {
  try {
    return JSON.stringify(JSON.parse(payload), null, 2);
  } catch {
    return payload || "(empty payload)";
  }
}

function SummaryTile({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg border border-border px-3 py-2">
      <dt className="text-muted-foreground text-xs">{label}</dt>
      <dd className="mt-0.5 truncate text-foreground text-sm">{value}</dd>
    </div>
  );
}
