import React from "react";
import {
  BrainCircuitIcon,
  CircleCheckIcon,
  HourglassIcon,
  InfoIcon,
  PlayIcon,
  RotateCcwIcon,
  SearchIcon,
  ShieldAlertIcon,
  ShieldCheckIcon,
} from "lucide-react";
import {
  api,
  errorMessage,
  type AdaptationOutcome,
  type ClassifierAdaptation,
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
import { notify } from "@/lib/toast";
import { cn } from "@/lib/utils";
import { useAsync } from "@/hooks/use-async";
import { useCogwheel } from "@/data/context";
import { Button } from "@/components/ui/button";
import { PageHeader, PageSections, PageShell } from "@/components/app/page";
import { SectionCard } from "@/components/app/section-card";
import { StatTile } from "@/components/app/stat-tile";
import { ConfirmDialog } from "@/components/app/confirm-dialog";
import { DataTable, type Column } from "@/components/app/data-table";
import { TextField } from "@/components/app/text-field";
import { StatusIndicator, StatusPill } from "@/components/app/status-indicator";
import {
  AsyncRegion,
  EmptyState,
  ErrorState,
  LoadingSkeleton,
  NoticeBanner,
} from "@/components/app/states";
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

            <AdaptationCard status={status} />

            <SectionCard
              description="Mode decides whether the model can block. Sensitivity decides how eager it is when it can."
              title="Enforcement"
            >
              <div className="space-y-6">
                <fieldset>
                  <legend className="font-medium text-foreground text-sm">Mode</legend>
                  <div className="mt-2 grid gap-6 sm:grid-cols-3">
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
                  <div className="mt-3 grid gap-6 sm:grid-cols-3">
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
              <div className="grid gap-6 sm:grid-cols-2 xl:grid-cols-4">
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
            stackBelow="xl"
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
      <div className="grid gap-6 sm:grid-cols-2 xl:grid-cols-4">
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

/**
 * On-device adaptation.
 *
 * The feature is only usable by a household if the household can see that it is
 * safe, so this section reports the gate's actual measurements rather than a
 * thumbs-up, and treats a refusal as the guard working rather than as an error.
 */
function AdaptationCard({ status }: { status: ClassifierStatus }) {
  const { phase, error, refresh } = useCogwheel();
  const [outcome, setOutcome] = React.useState<AdaptationOutcome | null>(null);
  const [failure, setFailure] = React.useState<string | null>(null);
  const [running, setRunning] = React.useState<"adapt" | "revert" | null>(null);
  const [confirmingRevert, setConfirmingRevert] = React.useState(false);

  // Widened deliberately: the live contract always sends this block, but the
  // provider rehydrates `classifier` from localStorage on first paint, and a
  // snapshot cached by an older build predates the field entirely.
  const adaptation: ClassifierAdaptation | undefined = status.adaptation;

  const run = async () => {
    setRunning("adapt");
    setFailure(null);
    try {
      const result = await api.adaptClassifier();
      setOutcome(result);
      if (result.status === "promoted") {
        notify.success(
          "Correction promoted",
          `Trained on ${formatCount(result.exampleCount)} reports and measured against the held-out set. The shipped model is unchanged.`,
        );
      } else if (result.status === "rejected") {
        // Not an error toast: the gate refusing a bad correction is the feature
        // behaving correctly, and colouring it red would teach the household to
        // dread the one thing protecting them.
        notify.warning(
          "Correction refused by the safety check",
          "It measured worse than the shipped model, so nothing was applied.",
        );
      } else {
        notify.info(
          "Not enough feedback yet",
          `${formatCount(result.have)} of ${formatCount(result.need)} reports stored.`,
        );
      }
      await refresh();
    } catch (cause) {
      setOutcome(null);
      setFailure(errorMessage(cause));
      notify.error("Could not run adaptation", errorMessage(cause));
    } finally {
      setRunning(null);
    }
  };

  const revert = async () => {
    setRunning("revert");
    setFailure(null);
    try {
      await api.rollbackClassifierAdaptation();
      setOutcome(null);
      notify.success(
        "Reverted to the base model",
        "The correction was discarded. Your stored reports were kept, so you can adapt again whenever you like.",
      );
      await refresh();
    } catch (cause) {
      setFailure(errorMessage(cause));
      notify.error("Could not revert to the base model", errorMessage(cause));
    } finally {
      setRunning(null);
    }
  };

  return (
    <>
      <SectionCard
        description="Corrections trained on this appliance from your own reports, measured before anything is applied."
        footer={
          <div className="flex flex-wrap items-center gap-2">
            <Button disabled={running !== null} isLoading={running === "adapt"} onClick={() => void run()}>
              <PlayIcon aria-hidden />
              Run adaptation
            </Button>
            {adaptation?.active ? (
              <Button
                disabled={running !== null}
                onClick={() => setConfirmingRevert(true)}
                variant="destructive"
              >
                <RotateCcwIcon aria-hidden />
                Revert to base model
              </Button>
            ) : null}
          </div>
        }
        id="adaptation"
        title="On-device adaptation"
      >
        <AsyncRegion
          empty={
            <EmptyState
              description="GET /api/v1/classifier answered without an adaptation block. This appliance is probably running a build from before the feature existed."
              icon={BrainCircuitIcon}
              title="No adaptation state reported"
            />
          }
          error={adaptation === undefined ? error : null}
          errorTitle="Could not read the adaptation state"
          isEmpty={adaptation === undefined}
          loading={phase === "loading"}
          onRetry={() => void refresh()}
          skeleton="cards"
          skeletonRows={4}
        >
          {adaptation ? (
            <AdaptationDetail adaptation={adaptation} failure={failure} outcome={outcome} />
          ) : null}
        </AsyncRegion>
      </SectionCard>

      {adaptation?.active ? (
        <ConfirmDialog
          confirmLabel="Revert to base model"
          consequence="Your stored reports are kept, so you can run adaptation again at any time. The shipped model itself was never modified and needs no restoring."
          description={`Discard the correction trained on ${formatCount(adaptation.exampleCount)} reports on ${formatDateTime(adaptation.trainedAt)}? Scoring returns to the model exactly as it shipped.`}
          destructive
          onConfirm={revert}
          onOpenChange={setConfirmingRevert}
          open={confirmingRevert}
          title="Revert to base model"
        />
      ) : null}
    </>
  );
}

function AdaptationDetail({
  adaptation,
  outcome,
  failure,
}: {
  adaptation: ClassifierAdaptation;
  outcome: AdaptationOutcome | null;
  failure: string | null;
}) {
  const ready = adaptation.pendingFeedback >= adaptation.minimumFeedback;

  return (
    <div className="space-y-5">
      <StatusIndicator
        description={
          adaptation.active
            ? `Trained ${formatDateTime(adaptation.trainedAt)} on ${formatCount(adaptation.exampleCount)} of your reports.`
            : "Every verdict comes from the model exactly as it shipped."
        }
        label={adaptation.active ? "A correction is active" : "Running the shipped model"}
        showIcon
        tone={adaptation.active ? "good" : "idle"}
      />

      {/* §3 of the brief: the reassurance that makes this usable by a household. */}
      <div className="flex gap-3 rounded-xl border border-border bg-muted/50 p-4">
        <InfoIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <p className="text-muted-foreground text-sm">
          The shipped model is never modified. Adaptation trains a separate correction from your
          reports and installs it only if it does no worse than the shipped model on a held-out set of
          25,000 domains — and because the original is still there untouched, reverting is one click.
        </p>
      </div>

      <div className="grid gap-6 sm:grid-cols-2 xl:grid-cols-4">
        <StatTile
          hint={
            ready
              ? `At or above the ${formatCount(adaptation.minimumFeedback)} needed to judge a correction.`
              : `${formatCount(adaptation.minimumFeedback)} needed before a correction can be judged at all.`
          }
          label="Pending reports"
          tone={ready ? "good" : "warn"}
          toneLabel={ready ? "Enough to run" : "Below minimum"}
          value={formatCompact(adaptation.pendingFeedback)}
        />
        <StatTile
          delta={
            adaptation.active
              ? `${formatCount(adaptation.ngramEntries)} character runs stored`
              : undefined
          }
          hint={
            adaptation.active
              ? `Trained ${formatDateTime(adaptation.trainedAt)}.`
              : "No correction is active, so nothing has been trained."
          }
          label="Trained on"
          value={adaptation.active ? formatCompact(adaptation.exampleCount) : "—"}
        />
        <StatTile
          hint="Base plus correction, measured on the committed holdout. Not comparable with the model figure above, which was measured on a different split."
          label="Corrected ROC-AUC"
          value={adaptation.rocAuc === null ? "—" : adaptation.rocAuc.toFixed(3)}
        />
        <StatTile
          delta={adaptation.active ? `of a ${adaptation.logitBudget.toFixed(1)} ceiling` : undefined}
          hint="The furthest this correction can move any score, in logits. It is computed from the correction itself, not estimated from data."
          label="Certified shift"
          value={adaptation.active ? adaptation.maxLogitShift.toFixed(2) : "—"}
        />
      </div>

      {adaptation.falsePositiveRate ? (
        <FalsePositiveTable
          caption="Measured with the correction applied, at each sensitivity's threshold. This is the share of legitimate domains that would be wrongly flagged."
          rates={adaptation.falsePositiveRate}
          title="False positives under this correction"
        />
      ) : null}

      {failure ? (
        <ErrorState
          detail={failure}
          title="The appliance could not complete that request"
        />
      ) : null}

      {outcome ? <OutcomePanel outcome={outcome} /> : null}
    </div>
  );
}

function FalsePositiveTable({
  title,
  caption,
  rates,
}: {
  title: string;
  caption: string;
  rates: Record<ClassifierSensitivity, number>;
}) {
  return (
    <div className="rounded-xl border border-border p-3">
      <p className="font-medium text-foreground text-sm">{title}</p>
      <dl className="mt-2 grid grid-cols-3 gap-x-3 gap-y-1 text-sm">
        {SENSITIVITY_ORDER.map((sensitivity) => (
          <React.Fragment key={sensitivity}>
            <dt className="col-span-2 text-muted-foreground">{SENSITIVITY_LABEL[sensitivity]}</dt>
            <dd className="tabular text-right text-foreground">
              {formatPercent(rates[sensitivity], 3)}
            </dd>
          </React.Fragment>
        ))}
      </dl>
      <p className="mt-2 text-muted-foreground text-xs">{caption}</p>
    </div>
  );
}

/**
 * The three outcomes, told honestly.
 *
 * A refusal is deliberately not styled as a failure. The gate turning down a
 * correction is the safety property working, so it gets `yellow-400` as a
 * warning surface and copy that says plainly that nothing changed — never the
 * destructive red that would read as "something broke".
 */
function OutcomePanel({ outcome }: { outcome: AdaptationOutcome }) {
  if (outcome.status === "promoted") {
    return (
      <OutcomeFrame icon={CircleCheckIcon} title="Correction promoted" tone="good">
        <p>
          It cleared every check on the committed holdout and is now scoring alongside the model.
        </p>
        <OutcomeFigures outcome={outcome} />
        <p>
          The shipped model was not modified — it is still on the appliance exactly as it arrived, and{" "}
          <span className="font-medium">Revert to base model</span> puts it back in sole charge in one
          click.
        </p>
      </OutcomeFrame>
    );
  }

  if (outcome.status === "rejected") {
    return (
      <OutcomeFrame
        icon={ShieldAlertIcon}
        title="Correction refused — the safety check did its job"
        tone="warn"
      >
        <p>
          The model was left untouched. Nothing was installed, whatever was already active stayed
          active, and your reports are still stored.
        </p>
        <div className="rounded-lg border border-border bg-background/60 p-3">
          <p className="text-muted-foreground text-xs">Reason reported by the appliance</p>
          <p className="mt-1 break-words text-foreground text-sm">{outcome.reason}</p>
        </div>
        <OutcomeFigures outcome={outcome} />
      </OutcomeFrame>
    );
  }

  return (
    <OutcomeFrame icon={HourglassIcon} title="Not enough feedback yet" tone="idle">
      <p>
        {formatCount(outcome.have)} of {formatCount(outcome.need)} reports stored. Below that, a
        correction cannot be told apart from the noise of its own training, so nothing was trained and
        nothing was measured.
      </p>
      <p>
        Report wrong verdicts from the domain inspector — open any domain, then use “This is not an
        ad” or “This should be blocked” — and run adaptation again.
      </p>
    </OutcomeFrame>
  );
}

/** The gate's own measurements, shown whenever it got far enough to take them. */
function OutcomeFigures({ outcome }: { outcome: AdaptationOutcome }) {
  // Hoisted so the narrowing survives into the map callback below.
  const rates = outcome.falsePositiveRate;
  if (outcome.rocAuc === null && rates === null) return null;

  return (
    <dl className="grid gap-x-4 gap-y-1 text-xs sm:grid-cols-2">
      <div className="flex justify-between gap-3">
        <dt className="text-muted-foreground">ROC-AUC on the holdout</dt>
        <dd className="tabular text-foreground">
          {outcome.rocAuc === null ? "—" : outcome.rocAuc.toFixed(5)}
        </dd>
      </div>
      {outcome.exampleCount === null ? null : (
        <div className="flex justify-between gap-3">
          <dt className="text-muted-foreground">Reports used</dt>
          <dd className="tabular text-foreground">{formatCount(outcome.exampleCount)}</dd>
        </div>
      )}
      {rates === null
        ? null
        : SENSITIVITY_ORDER.map((sensitivity) => (
            <div className="flex justify-between gap-3" key={sensitivity}>
              <dt className="text-muted-foreground">
                False positives, {SENSITIVITY_LABEL[sensitivity].toLowerCase()}
              </dt>
              <dd className="tabular text-foreground">{formatPercent(rates[sensitivity], 3)}</dd>
            </div>
          ))}
    </dl>
  );
}

function OutcomeFrame({
  tone,
  icon: Icon,
  title,
  children,
}: {
  tone: "good" | "warn" | "idle";
  icon: React.ElementType;
  title: string;
  children: React.ReactNode;
}) {
  // §3.3: the 400 hue is the surface, the border and the dot. Readable copy sits
  // in the matching `-foreground` (700 light / 300 dark) or in `--foreground`.
  const toneClass =
    tone === "good"
      ? "border-success/24 bg-success/8 text-success-foreground"
      : tone === "warn"
        ? "border-warning/32 bg-warning/10 text-warning-foreground"
        : "border-border bg-muted text-foreground";

  return (
    <div className={cn("space-y-2 rounded-xl border px-4 py-3", toneClass)} role="status">
      <p className="flex items-center gap-2 font-medium text-sm">
        <Icon aria-hidden className="size-4 shrink-0" />
        {title}
      </p>
      <div className="space-y-2 text-foreground/80 text-sm">{children}</div>
    </div>
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
