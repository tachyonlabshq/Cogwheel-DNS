import React from "react";
import { BrainCircuitIcon, InfoIcon, SearchIcon, ShieldCheckIcon } from "lucide-react";
import {
  api,
  type ClassifierDetection,
  type ClassifierMode,
  type ClassifierSensitivity,
  type ClassifierStatus,
} from "@/lib/api";
import {
  CLASSIFIER_MODE_BLURB,
  SENSITIVITY_BLURB,
  SENSITIVITY_LABEL,
  SENSITIVITY_ORDER,
} from "@/lib/constants";
import { looksLikeDomain } from "@/lib/derive";
import {
  formatBytes,
  formatCompact,
  formatCount,
  formatDateTime,
  formatPercent,
  formatProbability,
} from "@/lib/format";
import { cn } from "@/lib/utils";
import { useAsync } from "@/hooks/use-async";
import { useCogwheel } from "@/data/context";
import { Button } from "@/components/ui/button";
import { PageHeader, PageSections, PageShell } from "@/components/app/page";
import { SectionCard } from "@/components/app/section-card";
import { StatTile } from "@/components/app/stat-tile";
import { DataTable, type Column } from "@/components/app/data-table";
import { TextField } from "@/components/app/text-field";
import { StatusPill } from "@/components/app/status-indicator";
import { ErrorState, LoadingSkeleton, NoticeBanner } from "@/components/app/states";
import { useDomainInspector } from "@/components/app/inspector-context";

const MODES: ClassifierMode[] = ["off", "monitor", "protect"];

export function ClassifierScreen() {
  const { data, phase, error, busy, mutate, reload } = useCogwheel();
  const { inspect } = useDomainInspector();
  const [domain, setDomain] = React.useState("");

  const status = data.classifier;
  const detections = useAsync<ClassifierDetection[]>("classifier-detections", (signal) =>
    api.classifierDetections(50, { signal }),
  );

  const detectionColumns: Column<ClassifierDetection>[] = [
    {
      key: "domain",
      header: "Domain",
      render: (row) => <span className="font-mono text-xs">{row.domain}</span>,
      sortValue: (row) => row.domain,
    },
    { key: "client", header: "Client", render: (row) => row.client, hideOnStack: true },
    {
      key: "probability",
      header: "Score",
      align: "end",
      render: (row) => <span className="tabular">{formatProbability(row.probability)}</span>,
      sortValue: (row) => row.probability,
    },
    {
      key: "decision",
      header: "Decision",
      align: "end",
      render: (row) =>
        row.protected ? (
          <StatusPill label="Shielded" tone="warn" />
        ) : row.decision === "block" ? (
          <StatusPill label="Blocked" tone="bad" />
        ) : (
          <StatusPill label="Allowed" tone="good" />
        ),
    },
    {
      key: "observed",
      header: "Seen",
      align: "end",
      hideOnStack: true,
      render: (row) => (
        <span className="text-muted-foreground text-xs">{formatDateTime(row.observedAt)}</span>
      ),
      sortValue: (row) => row.observedAt,
    },
  ];

  return (
    <PageShell>
      <PageHeader
        description="A small on-device model that scores domains it has never seen before. It works alongside the blocklists, not instead of them."
        title="Classifier"
      />

      <PageSections>
        {/* The single most important thing an operator can misunderstand about
            this feature, said before any of the numbers. */}
        <div className="flex gap-3 rounded-xl border border-border bg-muted/50 p-4">
          <InfoIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
          <div className="space-y-1 text-sm">
            <p className="font-medium text-foreground">Scoring happens after the answer is sent.</p>
            <p className="text-muted-foreground">
              The first query for a domain the model has not seen resolves normally — the verdict does not
              exist yet. Scoring runs asynchronously and the result is cached, so enforcement begins on
              subsequent queries for that domain. This is not real-time blocking of first contact, and the
              screen will not pretend otherwise.
            </p>
          </div>
        </div>

        {phase === "loading" && !status ? <LoadingSkeleton rows={4} variant="cards" /> : null}

        {phase === "ready" && !status ? (
          <ErrorState
            detail={
              error ??
              "GET /api/v1/classifier did not return a model. The classifier may not be built into this appliance yet."
            }
            onRetry={() => void reload()}
            title="Classifier status unavailable"
          />
        ) : null}

        {status ? (
          <>
            <ModelCard status={status} />

            <SectionCard
              description="Mode decides whether the model can block. Sensitivity decides how eager it is when it can."
              title="Enforcement"
            >
              <div className="space-y-6">
                <fieldset>
                  <legend className="font-medium text-foreground text-sm">Mode</legend>
                  <div className="mt-2 grid gap-2 sm:grid-cols-3">
                    {MODES.map((mode) => (
                      <ChoiceCard
                        busy={busy === `classifier-mode-${mode}`}
                        description={CLASSIFIER_MODE_BLURB[mode]}
                        key={mode}
                        onSelect={() =>
                          void mutate({
                            key: `classifier-mode-${mode}`,
                            action: () => api.updateClassifier(mode, status.settings.sensitivity),
                            successTitle: "Classifier mode updated",
                            successDetail: `Mode is now ${mode}.`,
                            failureTitle: "Could not change classifier mode",
                            optimistic: {
                              classifier: { ...status, settings: { ...status.settings, mode } },
                            },
                            after: "light",
                          })
                        }
                        selected={status.settings.mode === mode}
                        title={mode === "off" ? "Off" : mode === "monitor" ? "Monitor" : "Protect"}
                      />
                    ))}
                  </div>
                </fieldset>

                <fieldset>
                  <legend className="font-medium text-foreground text-sm">Sensitivity</legend>
                  <p className="mt-1 text-muted-foreground text-sm">
                    These are the model's measured rates on its held-out evaluation split, not estimates.
                    Higher recall always costs false positives.
                  </p>
                  <div className="mt-3 grid gap-2 sm:grid-cols-3">
                    {SENSITIVITY_ORDER.map((sensitivity) => (
                      <SensitivityCard
                        busy={busy === `classifier-sensitivity-${sensitivity}`}
                        key={sensitivity}
                        onSelect={() =>
                          void mutate({
                            key: `classifier-sensitivity-${sensitivity}`,
                            action: () => api.updateClassifier(status.settings.mode, sensitivity),
                            successTitle: "Sensitivity updated",
                            successDetail: `Threshold is now ${formatProbability(
                              status.model.thresholds[sensitivity],
                            )}.`,
                            failureTitle: "Could not change sensitivity",
                            optimistic: {
                              classifier: { ...status, settings: { ...status.settings, sensitivity } },
                            },
                            after: "light",
                          })
                        }
                        selected={status.settings.sensitivity === sensitivity}
                        sensitivity={sensitivity}
                        status={status}
                      />
                    ))}
                  </div>
                </fieldset>

                {status.settings.mode === "monitor" ? (
                  <NoticeBanner
                    detail="Verdicts are recorded and shown below, but no query is blocked by the model in this mode."
                    title="Monitoring only"
                    tone="neutral"
                  />
                ) : null}
                {status.settings.mode === "off" ? (
                  <NoticeBanner
                    detail="Domains are not scored at all. Only blocklists and service rules apply."
                    title="The model is switched off"
                    tone="neutral"
                  />
                ) : null}
              </div>
            </SectionCard>

            <SectionCard
              description="Counters since the appliance started."
              title="Runtime behaviour"
            >
              <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
                <StatTile
                  hint="Domains the model has actually scored"
                  label="Scored"
                  value={formatCompact(status.stats.scored)}
                />
                <StatTile
                  delta={`${formatCount(status.stats.cacheMisses)} misses · ${formatCount(status.stats.cachedEntries)} cached`}
                  hint="Cache hits answer without re-running the model"
                  label="Cache hits"
                  value={formatCompact(status.stats.cacheHits)}
                />
                <StatTile
                  delta={`${formatCount(status.stats.protectedOverrides)} blocks the allowlist prevented`}
                  hint="Queries the model blocked in protect mode"
                  label="Blocked"
                  tone={status.stats.blocked > 0 ? "bad" : "neutral"}
                  toneLabel={status.stats.blocked > 0 ? "Enforcing" : undefined}
                  value={formatCompact(status.stats.blocked)}
                />
                <StatTile
                  hint="Scoring jobs shed because the queue was full. Those domains were never given a verdict."
                  label="Dropped under load"
                  tone={status.stats.dropped > 0 ? "warn" : "neutral"}
                  toneLabel={status.stats.dropped > 0 ? "Shedding" : undefined}
                  value={formatCompact(status.stats.dropped)}
                />
              </div>
            </SectionCard>
          </>
        ) : null}

        <SectionCard
          description="Paste a domain to see its score, the active threshold, whether a blocklist already covers it, and the exact signed contributions behind the verdict."
          title="Why was this blocked?"
        >
          <form
            className="flex flex-col gap-3 sm:flex-row sm:items-end"
            onSubmit={(event) => {
              event.preventDefault();
              const trimmed = domain.trim().toLowerCase();
              if (looksLikeDomain(trimmed)) inspect(trimmed);
            }}
          >
            <TextField
              className="flex-1"
              error={
                domain.trim() && !looksLikeDomain(domain.trim().toLowerCase())
                  ? "That does not look like a domain name."
                  : undefined
              }
              label="Domain"
              onChange={setDomain}
              placeholder="doubleclick.net"
              searchTarget
              value={domain}
            />
            <Button
              className="sm:mb-0.5"
              disabled={!looksLikeDomain(domain.trim().toLowerCase())}
              type="submit"
            >
              <SearchIcon aria-hidden />
              Inspect
            </Button>
          </form>
        </SectionCard>

        <SectionCard
          actions={
            <Button onClick={detections.reload} size="sm" variant="outline">
              Refresh
            </Button>
          }
          description="The most recent domains the model scored, newest first."
          title="Recent detections"
        >
          <DataTable
            columns={detectionColumns}
            empty={{
              icon: BrainCircuitIcon,
              title: "No detections recorded yet",
              description:
                "Detections appear once devices query domains that are not already covered by a blocklist.",
            }}
            error={detections.error}
            loading={detections.loading}
            onRetry={detections.reload}
            onRowClick={(row) => inspect(row.domain)}
            rowActionLabel={(row) => `Inspect ${row.domain}`}
            rowKey={(row) => `${row.domain}-${row.observedAt}`}
            rows={detections.data ?? []}
          />
        </SectionCard>
      </PageSections>
    </PageShell>
  );
}

function ModelCard({ status }: { status: ClassifierStatus }) {
  const { model, activeThreshold, settings } = status;

  return (
    <SectionCard
      description="Everything below is reported by the appliance, not claimed by this page."
      title="Model"
    >
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <StatTile
          hint="Area under the ROC curve on the held-out split"
          label="ROC-AUC"
          value={model.rocAuc.toFixed(3)}
        />
        <StatTile
          hint="Area under the precision–recall curve"
          label="PR-AUC"
          value={model.prAuc.toFixed(3)}
        />
        <StatTile
          hint={`Version ${model.version}, trained ${formatDateTime(model.trainedAt)}`}
          label="Resident size"
          value={formatBytes(model.residentBytes)}
        />
        <StatTile
          hint={`Active sensitivity: ${SENSITIVITY_LABEL[settings.sensitivity]}`}
          label="Active threshold"
          value={formatProbability(activeThreshold)}
        />
      </div>
    </SectionCard>
  );
}

function SensitivityCard({
  sensitivity,
  status,
  selected,
  busy,
  onSelect,
}: {
  sensitivity: ClassifierSensitivity;
  status: ClassifierStatus;
  selected: boolean;
  busy: boolean;
  onSelect: () => void;
}) {
  const fpr = status.model.falsePositiveRate[sensitivity];
  const recall = status.model.recall[sensitivity];
  const threshold = status.model.thresholds[sensitivity];

  return (
    <button
      aria-pressed={selected}
      className={cn(
        "flex flex-col gap-2 rounded-xl border p-3 text-left",
        "hover:bg-muted focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2",
        selected ? "border-primary bg-muted" : "border-border",
        busy && "pointer-events-none opacity-60",
      )}
      onClick={onSelect}
      type="button"
    >
      <span className="flex items-center justify-between gap-2">
        <span className="font-medium text-foreground text-sm">{SENSITIVITY_LABEL[sensitivity]}</span>
        {selected ? (
          <span className="inline-flex items-center gap-1 text-foreground text-xs">
            <ShieldCheckIcon aria-hidden className="size-3.5" />
            Active
          </span>
        ) : null}
      </span>

      <span className="text-muted-foreground text-xs">{SENSITIVITY_BLURB[sensitivity]}</span>

      <dl className="mt-1 grid grid-cols-2 gap-x-3 gap-y-1 text-xs">
        <dt className="text-muted-foreground">False positives</dt>
        <dd className="tabular text-right text-foreground">{formatPercent(fpr, 3)}</dd>
        <dt className="text-muted-foreground">Recall</dt>
        <dd className="tabular text-right text-foreground">{formatPercent(recall, 1)}</dd>
        <dt className="text-muted-foreground">Threshold</dt>
        <dd className="tabular text-right text-foreground">{formatProbability(threshold)}</dd>
      </dl>

      <span className="text-muted-foreground text-[11px]">
        At this setting the model catches {formatPercent(recall, 1)} of ad domains and wrongly flags{" "}
        {formatPercent(fpr, 3)} of legitimate ones.
      </span>
    </button>
  );
}

function ChoiceCard({
  title,
  description,
  selected,
  busy,
  onSelect,
}: {
  title: string;
  description: string;
  selected: boolean;
  busy: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      aria-pressed={selected}
      className={cn(
        "flex flex-col gap-1 rounded-xl border p-3 text-left",
        "hover:bg-muted focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2",
        selected ? "border-primary bg-muted" : "border-border",
        busy && "pointer-events-none opacity-60",
      )}
      onClick={onSelect}
      type="button"
    >
      <span className="flex items-center justify-between gap-2">
        <span className="font-medium text-foreground text-sm">{title}</span>
        {selected ? <span className="text-foreground text-xs">Active</span> : null}
      </span>
      <span className="text-muted-foreground text-xs">{description}</span>
    </button>
  );
}
