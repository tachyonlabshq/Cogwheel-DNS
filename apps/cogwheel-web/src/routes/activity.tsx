import React from "react";
import { ActivityIcon, PauseIcon, PlayIcon, RadioIcon, ShieldAlertIcon, Trash2Icon } from "lucide-react";
import { api, type SecurityEventRecord } from "@/lib/api";
import { severityLabel, severityTone } from "@/lib/derive";
import { formatCount, formatProbability, formatRelative, formatTime } from "@/lib/format";
import { ACTIVITY_BUFFER_LIMIT } from "@/lib/constants";
import { useAsync } from "@/hooks/use-async";
import { useEventStream, type ActivityRow } from "@/hooks/use-event-stream";
import { useCogwheel } from "@/data/context";
import { Button } from "@/components/ui/button";
import { PageHeader, PageSections, PageShell } from "@/components/app/page";
import { SectionCard } from "@/components/app/section-card";
import { DataTable, type Column } from "@/components/app/data-table";
import { SelectField } from "@/components/app/select-field";
import { TextField } from "@/components/app/text-field";
import { EmptyState, NoticeBanner } from "@/components/app/states";
import { StatusIndicator, StatusPill } from "@/components/app/status-indicator";
import { useDomainInspector } from "@/components/app/inspector-context";

type VerdictFilter = "all" | "blocked" | "allowed" | "detections";

const STREAM_TONE = {
  open: { tone: "good" as const, label: "Live", detail: "Receiving events as they happen." },
  connecting: { tone: "idle" as const, label: "Connecting", detail: "Opening the event stream…" },
  reconnecting: {
    tone: "warn" as const,
    label: "Reconnecting",
    detail: "The stream dropped. Retrying with a growing delay.",
  },
  paused: { tone: "idle" as const, label: "Paused", detail: "New rows are buffered until you resume." },
};

export function ActivityScreen() {
  const { data } = useCogwheel();
  const { inspect } = useDomainInspector();
  const [paused, setPaused] = React.useState(false);
  const [verdict, setVerdict] = React.useState<VerdictFilter>("all");
  const [device, setDevice] = React.useState("all");
  const [search, setSearch] = React.useState("");

  const stream = useEventStream(paused);
  const events = useAsync<SecurityEventRecord[]>("security-events", (signal) => api.securityEvents({ signal }));

  const clients = React.useMemo(() => {
    const seen = new Map<string, string>();
    for (const device of data.settings.devices) seen.set(device.ip_address, device.name);
    for (const row of stream.rows) {
      if (!seen.has(row.client)) seen.set(row.client, row.deviceName ?? row.client);
    }
    return [...seen.entries()];
  }, [data.settings.devices, stream.rows]);

  const filtered = React.useMemo(() => {
    const needle = search.trim().toLowerCase();
    return stream.rows.filter((row) => {
      if (device !== "all" && row.client !== device) return false;
      if (verdict === "detections" && row.kind !== "detection") return false;
      if (verdict === "blocked" && !isBlocked(row)) return false;
      if (verdict === "allowed" && isBlocked(row)) return false;
      if (needle && !row.domain.toLowerCase().includes(needle)) return false;
      return true;
    });
  }, [device, search, stream.rows, verdict]);

  const streamColumns: Column<ActivityRow>[] = [
    {
      key: "time",
      header: "Time",
      render: (row) => <span className="tabular text-muted-foreground text-xs">{formatTime(row.observedAt)}</span>,
      sortValue: (row) => row.observedAt,
    },
    {
      key: "domain",
      header: "Domain",
      render: (row) => <span className="font-mono text-xs">{row.domain}</span>,
    },
    {
      key: "client",
      header: "Device",
      render: (row) => row.deviceName ?? row.client,
      hideOnStack: false,
    },
    {
      key: "verdict",
      header: "Verdict",
      render: (row) =>
        isBlocked(row) ? (
          <StatusPill label="Blocked" tone="bad" />
        ) : (
          <StatusPill label="Allowed" tone="good" />
        ),
    },
    {
      key: "detail",
      header: "Detail",
      align: "end",
      hideOnStack: true,
      render: (row) =>
        row.kind === "detection" ? (
          <span className="tabular text-xs">score {formatProbability(row.probability)}</span>
        ) : (
          <span className="text-muted-foreground text-xs">{row.reason ?? "—"}</span>
        ),
    },
  ];

  const eventColumns: Column<SecurityEventRecord>[] = [
    {
      key: "time",
      header: "When",
      render: (row) => (
        <span className="text-muted-foreground text-xs">{formatRelative(row.created_at)}</span>
      ),
      sortValue: (row) => row.created_at,
    },
    { key: "domain", header: "Domain", render: (row) => <span className="font-mono text-xs">{row.domain}</span> },
    { key: "device", header: "Device", render: (row) => row.device_name ?? "Unassigned device" },
    {
      key: "client",
      header: "Client IP",
      hideOnStack: true,
      render: (row) => <span className="font-mono text-xs">{row.client_ip}</span>,
    },
    {
      key: "score",
      header: "Score",
      align: "end",
      hideOnStack: true,
      render: (row) => <span className="tabular">{formatProbability(row.classifier_score)}</span>,
      sortValue: (row) => row.classifier_score,
    },
    {
      key: "severity",
      header: "Severity",
      align: "end",
      render: (row) => <StatusPill label={severityLabel(row.severity)} tone={severityTone(row.severity)} />,
    },
  ];

  const status = STREAM_TONE[stream.status];

  return (
    <PageShell>
      <PageHeader
        actions={
          <>
            <Button onClick={() => setPaused((current) => !current)} variant={paused ? "default" : "outline"}>
              {paused ? <PlayIcon aria-hidden /> : <PauseIcon aria-hidden />}
              {paused ? "Resume stream" : "Pause stream"}
            </Button>
            <Button disabled={stream.rows.length === 0} onClick={stream.clear} variant="outline">
              <Trash2Icon aria-hidden />
              Clear
            </Button>
          </>
        }
        description="Every query the resolver answers, streamed live. The buffer holds the most recent 500 rows."
        title="Activity"
      />

      <PageSections>
        {stream.error && stream.status === "reconnecting" ? (
          <NoticeBanner
            detail={`${stream.error} Recent security events below are still available over the regular API.`}
            title="Live stream unavailable"
            tone="warn"
          />
        ) : null}

        <SectionCard
          actions={<StatusIndicator description={status.detail} label={status.label} tone={status.tone} />}
          description={`Showing ${formatCount(filtered.length)} of ${formatCount(stream.rows.length)} buffered rows (cap ${ACTIVITY_BUFFER_LIMIT}).`}
          title="Live query stream"
        >
          <div className="mb-4 grid gap-3 sm:grid-cols-3">
            <TextField
              label="Domain contains"
              onChange={setSearch}
              placeholder="Filter by domain"
              searchTarget
              value={search}
            />
            <SelectField
              label="Device"
              onChange={setDevice}
              options={[
                { value: "all", label: "All devices" },
                ...clients.map(([ip, name]) => ({
                  value: ip,
                  label: name === ip ? ip : `${name} (${ip})`,
                })),
              ]}
              value={device}
            />
            <SelectField
              label="Verdict"
              onChange={(next) => setVerdict(next as VerdictFilter)}
              options={[
                { value: "all", label: "All verdicts" },
                { value: "blocked", label: "Blocked only" },
                { value: "allowed", label: "Allowed only" },
                { value: "detections", label: "Classifier detections" },
              ]}
              value={verdict}
            />
          </div>

          {paused && stream.pendingCount > 0 ? (
            <NoticeBanner
              actions={
                <Button onClick={() => setPaused(false)} size="sm" variant="outline">
                  Resume and merge
                </Button>
              }
              className="mb-4"
              detail="They will be merged into the list when you resume."
              title={`${formatCount(stream.pendingCount)} row(s) arrived while paused`}
              tone="neutral"
            />
          ) : null}

          {/* Announced politely so a screen reader is told about new rows
              without the list stealing focus mid-read. */}
          <div aria-label="Live query stream" aria-live="polite" role="log">
            {stream.rows.length === 0 && stream.status !== "reconnecting" ? (
              <EmptyState
                description="Queries appear the moment a device resolves through Cogwheel. If nothing arrives, check the connection instructions on Overview."
                icon={RadioIcon}
                title="Waiting for the first query"
              />
            ) : (
              <DataTable
                columns={streamColumns}
                empty={{
                  icon: ActivityIcon,
                  title: "No rows match these filters",
                  description: "Widen the domain filter, device or verdict to see buffered traffic.",
                }}
                onRowClick={(row) => inspect(row.domain)}
                rowActionLabel={(row) => `Inspect ${row.domain}`}
                rowKey={(row) => row.id}
                rows={filtered}
              />
            )}
          </div>
        </SectionCard>

        <SectionCard
          actions={
            <Button onClick={events.reload} size="sm" variant="outline">
              Refresh
            </Button>
          }
          description="The 20 most recent classifier-flagged events, persisted by the appliance."
          title="Recent risky events"
        >
          <DataTable
            columns={eventColumns}
            empty={{
              icon: ShieldAlertIcon,
              title: "No risky events recorded",
              description: "The classifier has not flagged any domain above the alerting threshold yet.",
            }}
            error={events.error}
            loading={events.loading}
            onRetry={events.reload}
            onRowClick={(row) => inspect(row.domain)}
            rowActionLabel={(row) => `Inspect ${row.domain}`}
            rowKey={(row) => row.id}
            rows={events.data ?? []}
          />
        </SectionCard>
      </PageSections>
    </PageShell>
  );
}

function isBlocked(row: ActivityRow): boolean {
  return row.kind === "query" ? row.blocked : row.decision === "block";
}
