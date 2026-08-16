import React from "react";
import { ChartNoAxesColumnIcon, HistoryIcon, MailWarningIcon, ShieldAlertIcon } from "lucide-react";
import {
  api,
  type FalsePositiveBudgetStatus,
  type NotificationDeliveryEvent,
  type RulesetSummary,
} from "@/lib/api";
import { blockedRatio, severityLabel, severityTone } from "@/lib/derive";
import { formatCompact, formatCount, formatDateTime, formatPercent, shortHash } from "@/lib/format";
import { useAsync } from "@/hooks/use-async";
import { useCogwheel } from "@/data/context";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { PageHeader, PageSections, PageShell } from "@/components/app/page";
import { SectionCard } from "@/components/app/section-card";
import { StatTile } from "@/components/app/stat-tile";
import { DataTable, type Column } from "@/components/app/data-table";
import { RankBars } from "@/components/app/metric-sparkline";
import { StatusPill } from "@/components/app/status-indicator";
import { ConfirmDialog } from "@/components/app/confirm-dialog";
import { AsyncRegion, EmptyState, LoadingSkeleton, NoticeBanner } from "@/components/app/states";
import { useDomainInspector } from "@/components/app/inspector-context";

export function InsightsScreen() {
  const { data, phase, error, busy, mutate, reload } = useCogwheel();
  const { inspect } = useDomainInspector();
  const [confirmRollback, setConfirmRollback] = React.useState(false);

  const loading = phase === "loading";

  const rulesets = useAsync<RulesetSummary[]>("rulesets", (signal) => api.rulesets({ signal }));
  const budget = useAsync<FalsePositiveBudgetStatus>("false-positive-budget", (signal) =>
    api.falsePositiveBudget({ signal }),
  );

  const dashboard = data.dashboard;
  const insights = dashboard.domain_insights;
  const summary = dashboard.security_summary;
  const analytics = dashboard.notification_failure_analytics;

  const rulesetColumns: Column<RulesetSummary>[] = [
    {
      key: "hash",
      header: "Ruleset",
      render: (row) => <span className="font-mono text-xs">{shortHash(row.hash, 16)}</span>,
    },
    {
      key: "status",
      header: "Status",
      render: (row) =>
        row.status === "active" ? (
          <StatusPill label="Active" tone="good" />
        ) : row.status === "previous" ? (
          <StatusPill label="Previous" tone="idle" />
        ) : (
          <Badge variant="outline">{row.status}</Badge>
        ),
      sortValue: (row) => row.status,
    },
    {
      key: "created",
      header: "Built",
      align: "end",
      render: (row) => (
        <span className="text-muted-foreground text-xs">{formatDateTime(row.created_at)}</span>
      ),
      sortValue: (row) => row.created_at,
    },
  ];

  const deliveryColumns: Column<NotificationDeliveryEvent>[] = [
    {
      key: "status",
      header: "Status",
      render: (row) =>
        row.status === "delivered" ? (
          <StatusPill label="Delivered" tone="good" />
        ) : (
          <StatusPill label="Failed" tone="bad" />
        ),
      sortValue: (row) => row.status,
    },
    { key: "title", header: "Alert", render: (row) => row.title },
    {
      key: "domain",
      header: "Domain",
      hideOnStack: true,
      render: (row) => <span className="font-mono text-xs">{row.domain}</span>,
    },
    { key: "target", header: "Target", hideOnStack: true, render: (row) => row.target },
    {
      key: "attempts",
      header: "Attempts",
      align: "end",
      render: (row) => <span className="tabular">{formatCount(row.attempts)}</span>,
      sortValue: (row) => row.attempts,
    },
    {
      key: "created",
      header: "When",
      align: "end",
      hideOnStack: true,
      render: (row) => (
        <span className="text-muted-foreground text-xs">{formatDateTime(row.created_at)}</span>
      ),
      sortValue: (row) => row.created_at,
    },
  ];

  return (
    <PageShell>
      <PageHeader
        actions={
          <Button onClick={() => void reload()} variant="outline">
            Reload
          </Button>
        }
        description="What the appliance has actually seen, and how the ruleset got to where it is."
        title="Insights"
      />

      <PageSections>
        {/* A zeroed tile during the first poll reads as "nothing has happened",
            which is a different claim from "we have not been told yet". */}
        {loading ? (
          <LoadingSkeleton rows={4} variant="cards" />
        ) : (
          <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
            <StatTile
              hint="Rolling window held in memory; lost on restart"
              label="Observed queries"
              value={formatCompact(insights.observed_queries)}
            />
            <StatTile
              delta={`${formatPercent(blockedRatio(dashboard), 2)} of all traffic`}
              label="Blocked"
              value={formatCompact(dashboard.runtime_health.snapshot.blocked_total)}
            />
            <StatTile
              delta={`${formatCount(summary.medium_count)} medium · ${formatCount(summary.high_count)} high`}
              label="Critical events"
              tone={summary.critical_count > 0 ? "bad" : "neutral"}
              toneLabel={summary.critical_count > 0 ? "Attention" : undefined}
              value={formatCount(summary.critical_count)}
            />
            <StatTile
              hint="Webhook deliveries that succeeded"
              label="Alert success rate"
              tone={analytics.success_rate_percent < 100 ? "warn" : "good"}
              toneLabel={analytics.success_rate_percent < 100 ? "Degraded" : "Healthy"}
              value={`${analytics.success_rate_percent.toFixed(1)}%`}
            />
          </div>
        )}

        <div className="grid gap-4 xl:grid-cols-2">
          <SectionCard description="Busiest destinations in the rolling window." title="Top queried domains">
            <AsyncRegion
              empty={
                <EmptyState
                  description="Nothing has been resolved through Cogwheel yet in the current window."
                  icon={ChartNoAxesColumnIcon}
                  title="No traffic recorded"
                />
              }
              error={error}
              errorTitle="Could not load queried domains"
              isEmpty={insights.top_queried_domains.length === 0}
              loading={loading}
              onRetry={() => void reload()}
              skeletonRows={5}
            >
              <>
                <RankBars
                  ariaLabel="Top queried domains by query count"
                  data={insights.top_queried_domains.map((entry) => ({
                    label: entry.domain,
                    value: entry.count,
                  }))}
                />
                <ul className="mt-3 space-y-1">
                  {insights.top_queried_domains.map((entry) => (
                    <li className="flex items-center justify-between gap-3" key={entry.domain}>
                      <button
                        className="truncate font-mono text-foreground text-xs hover:underline"
                        onClick={() => inspect(entry.domain)}
                        type="button"
                      >
                        {entry.domain}
                      </button>
                      <span className="tabular text-muted-foreground text-xs">
                        {formatCount(entry.count)}
                      </span>
                    </li>
                  ))}
                </ul>
              </>
            </AsyncRegion>
          </SectionCard>

          <SectionCard description="Where filtering engages most." title="Top blocked domains">
            <AsyncRegion
              empty={
                <EmptyState
                  description="No domain has been blocked in the current window."
                  icon={ShieldAlertIcon}
                  title="Nothing blocked yet"
                />
              }
              error={error}
              errorTitle="Could not load blocked domains"
              isEmpty={insights.top_blocked_domains.length === 0}
              loading={loading}
              onRetry={() => void reload()}
              skeletonRows={5}
            >
              <>
                <RankBars
                  ariaLabel="Top blocked domains by block count"
                  data={insights.top_blocked_domains.map((entry) => ({
                    label: entry.domain,
                    value: entry.count,
                  }))}
                  tone="blocked"
                />
                <ul className="mt-3 space-y-1">
                  {insights.top_blocked_domains.map((entry) => (
                    <li className="flex items-center justify-between gap-3" key={entry.domain}>
                      <button
                        className="truncate font-mono text-foreground text-xs hover:underline"
                        onClick={() => inspect(entry.domain)}
                        type="button"
                      >
                        {entry.domain}
                      </button>
                      <span className="tabular text-muted-foreground text-xs">
                        {formatCount(entry.count)}
                      </span>
                    </li>
                  ))}
                </ul>
              </>
            </AsyncRegion>
          </SectionCard>
        </div>

        <SectionCard
          description="Devices generating the most classifier-flagged events."
          title="Noisiest devices"
        >
          <AsyncRegion
            empty={
              <EmptyState
                description="No device has triggered a risky-domain event yet."
                icon={ShieldAlertIcon}
                title="No flagged devices"
              />
            }
            error={error}
            errorTitle="Could not load flagged devices"
            isEmpty={summary.top_devices.length === 0}
            loading={loading}
            onRetry={() => void reload()}
            skeletonRows={3}
          >
            <ul className="space-y-2">
              {summary.top_devices.map((device) => (
                <li
                  className="flex items-center justify-between gap-3 rounded-lg border border-border px-3 py-2"
                  key={device.label}
                >
                  <span className="min-w-0 truncate text-foreground text-sm">{device.label}</span>
                  <span className="flex shrink-0 items-center gap-2">
                    <span className="tabular text-muted-foreground text-xs">
                      {formatCount(device.event_count)} events
                    </span>
                    <StatusPill
                      label={severityLabel(device.highest_severity)}
                      tone={severityTone(device.highest_severity)}
                    />
                  </span>
                </li>
              ))}
            </ul>
          </AsyncRegion>
        </SectionCard>

        <SectionCard
          description="An estimate, not a measurement: the appliance assumes 10% of blocks are false positives because nothing in the system measures them. Treat it as a smoke alarm, not a metric."
          title="False-positive budget"
        >
          {budget.loading && !budget.data ? (
            <p className="text-muted-foreground text-sm">Loading…</p>
          ) : budget.error && !budget.data ? (
            <NoticeBanner
              actions={
                <Button onClick={budget.reload} size="sm" variant="outline">
                  Retry
                </Button>
              }
              detail={budget.error}
              title="Could not load the budget"
              tone="warn"
            />
          ) : budget.data ? (
            <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
              <StatTile
                label="Release ready"
                tone={budget.data.release_ready ? "good" : "warn"}
                toneLabel={budget.data.release_ready ? "Yes" : "No"}
                value={budget.data.release_ready ? "Within budget" : "Over budget"}
              />
              <StatTile label="Blocking rate" value={formatPercent(budget.data.blocking_rate, 3)} />
              <StatTile
                hint="blocking_rate × 0.1, hardcoded server-side"
                label="Estimated FP rate"
                value={formatPercent(budget.data.false_positive_estimate, 4)}
              />
              <StatTile
                hint={`Limit ${formatPercent(budget.data.budget_limit, 3)}`}
                label="Budget remaining"
                value={formatPercent(budget.data.budget_remaining, 4)}
              />
            </div>
          ) : null}

          {budget.data && budget.data.recommendations.length > 0 ? (
            <ul className="mt-4 space-y-1">
              {budget.data.recommendations.map((line) => (
                <li className="text-muted-foreground text-sm" key={line}>
                  {line}
                </li>
              ))}
            </ul>
          ) : null}
        </SectionCard>

        <SectionCard
          actions={
            <>
              <Button onClick={rulesets.reload} size="sm" variant="outline">
                Refresh
              </Button>
              <Button
                isLoading={busy === "rollback-ruleset"}
                onClick={() => setConfirmRollback(true)}
                size="sm"
                variant="outline"
              >
                <HistoryIcon aria-hidden />
                Roll back
              </Button>
            </>
          }
          description="Every ruleset this appliance has built. The list is unbounded server-side, so it grows with each refresh."
          title="Ruleset history"
        >
          <DataTable
            columns={rulesetColumns}
            empty={{
              icon: HistoryIcon,
              title: "No rulesets recorded",
              description: "A ruleset is recorded each time sources are refreshed and verified.",
            }}
            error={rulesets.error}
            loading={rulesets.loading}
            onRetry={rulesets.reload}
            rowKey={(row) => row.id}
            rows={rulesets.data ?? []}
          />
        </SectionCard>

        <SectionCard
          description="Recent webhook alert deliveries and the domains that failed most often."
          title="Alert delivery"
        >
          {analytics.top_failed_domains.length > 0 ? (
            <ul className="mb-4 flex flex-wrap gap-1.5">
              {analytics.top_failed_domains.map((entry) => (
                <li key={entry.domain}>
                  <Badge variant="outline">
                    {entry.domain} · {formatCount(entry.failure_count)} failures
                  </Badge>
                </li>
              ))}
            </ul>
          ) : null}

          <DataTable
            columns={deliveryColumns}
            empty={{
              icon: MailWarningIcon,
              title: "No deliveries recorded",
              description:
                "Alert deliveries appear here once outbound notifications are enabled and an event fires.",
            }}
            error={error}
            loading={loading}
            onRetry={() => void reload()}
            rowKey={(row) => `${row.created_at}-${row.domain}-${row.status}`}
            rows={dashboard.recent_notification_deliveries}
          />
        </SectionCard>
      </PageSections>

      <ConfirmDialog
        confirmLabel="Roll back ruleset"
        consequence="The previous ruleset is reactivated and the policy catalog is rebuilt, which re-fetches every enabled source over HTTP."
        description={`The active ruleset ${
          dashboard.active_ruleset ? shortHash(dashboard.active_ruleset.hash) : "(none)"
        } will be replaced by the previously active one.`}
        destructive
        onConfirm={async () => {
          await mutate({
            key: "rollback-ruleset",
            action: () => api.rollbackRuleset(),
            successTitle: "Rollback completed",
            successDetail: (result) => `Restored ruleset ${shortHash(result.hash)}.`,
            failureTitle: "Could not roll back",
          });
          rulesets.reload();
        }}
        onOpenChange={setConfirmRollback}
        open={confirmRollback}
        title="Roll back to the previous ruleset?"
      />
    </PageShell>
  );
}
