import React from "react";
import { useSearchParams } from "react-router-dom";
import { BanIcon, CircleCheckIcon, SearchIcon, ShieldCheckIcon } from "lucide-react";
import { api, errorMessage, type ClassifierFeedbackResult, type Inspection } from "@/lib/api";
import { formatPercent, formatProbability } from "@/lib/format";
import { looksLikeDomain } from "@/lib/derive";
import { notify } from "@/lib/toast";
import { cn } from "@/lib/utils";
import { useCogwheel } from "@/data/context";
import { Button } from "@/components/ui/button";
import { Dialog, DialogBody, DialogContent, DialogHeader } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Field } from "@/components/ui/field";
import { ErrorState, LoadingSkeleton } from "@/components/app/states";
import { StatusPill } from "@/components/app/status-indicator";
import { InspectorContext } from "@/components/app/inspector-context";

/**
 * The inspector lives in the URL (`?inspect=example.com`) so a verdict can be
 * pasted into a chat and reopened exactly as the sender saw it.
 */
export function DomainInspectorProvider({ children }: { children: React.ReactNode }) {
  const [params, setParams] = useSearchParams();
  const target = params.get("inspect");

  const inspect = React.useCallback(
    (domain: string) => {
      setParams(
        (current) => {
          const next = new URLSearchParams(current);
          next.set("inspect", domain);
          return next;
        },
        { replace: false },
      );
    },
    [setParams],
  );

  const close = React.useCallback(() => {
    setParams(
      (current) => {
        const next = new URLSearchParams(current);
        next.delete("inspect");
        return next;
      },
      { replace: true },
    );
  }, [setParams]);

  const value = React.useMemo(() => ({ inspect }), [inspect]);

  return (
    <InspectorContext.Provider value={value}>
      {children}
      <InspectorDialog domain={target} onClose={close} onInspect={inspect} />
    </InspectorContext.Provider>
  );
}

function InspectorDialog({
  domain,
  onClose,
  onInspect,
}: {
  domain: string | null;
  onClose: () => void;
  onInspect: (domain: string) => void;
}) {
  return (
    <Dialog onOpenChange={(details) => (details.open ? undefined : onClose())} open={domain !== null}>
      <DialogContent size="lg">
        <DialogHeader
          description="Ask the classifier why a domain resolves the way it does. Contributions are the model's own signed evidence, not a summary."
          title="Domain inspector"
        />
        <DialogBody>
          {domain === null ? null : <InspectorBody domain={domain} onInspect={onInspect} />}
        </DialogBody>
      </DialogContent>
    </Dialog>
  );
}

function InspectorBody({ domain, onInspect }: { domain: string; onInspect: (domain: string) => void }) {
  const [draft, setDraft] = React.useState(domain);
  const [result, setResult] = React.useState<Inspection | null>(null);
  const [loading, setLoading] = React.useState(true);
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    setDraft(domain);
  }, [domain]);

  React.useEffect(() => {
    const controller = new AbortController();
    let active = true;

    setLoading(true);
    setError(null);
    api
      .inspectDomain(domain, { signal: controller.signal })
      .then((inspection) => {
        if (active) {
          setResult(inspection);
          setLoading(false);
        }
      })
      .catch((cause: unknown) => {
        if (!active || (cause instanceof DOMException && cause.name === "AbortError")) return;
        setResult(null);
        setError(errorMessage(cause));
        setLoading(false);
      });

    return () => {
      active = false;
      controller.abort();
    };
  }, [domain]);

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    const trimmed = draft.trim().toLowerCase();
    if (!looksLikeDomain(trimmed)) {
      setError(`"${draft.trim() || "(empty)"}" does not look like a domain name.`);
      return;
    }
    onInspect(trimmed);
  };

  return (
    <div className="space-y-5">
      <form className="flex items-end gap-2" onSubmit={submit}>
        <Field className="flex-1">
          <label className="sr-only" htmlFor="inspector-domain">
            Domain
          </label>
          <Input
            autoComplete="off"
            id="inspector-domain"
            onChange={(event) => setDraft(event.target.value)}
            placeholder="doubleclick.net"
            spellCheck={false}
            value={draft}
          />
        </Field>
        <Button type="submit">
          <SearchIcon aria-hidden />
          Inspect
        </Button>
      </form>

      {loading ? <LoadingSkeleton rows={5} variant="text" /> : null}

      {!loading && error ? (
        <ErrorState
          detail={error}
          title="Could not inspect this domain"
          onRetry={() => onInspect(domain)}
        />
      ) : null}

      {!loading && result ? <InspectionReport result={result} /> : null}
    </div>
  );
}

function InspectionReport({ result }: { result: Inspection }) {
  const positives = result.contributions.filter((item) => item.value > 0);
  const negatives = result.contributions.filter((item) => item.value < 0);
  const magnitude = Math.max(...result.contributions.map((item) => Math.abs(item.value)), 0.0001);

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center gap-2">
        <code className="rounded-md bg-muted px-2 py-1 font-mono text-sm">{result.domain}</code>
        <StatusPill
          label={result.decision === "block" ? "Blocked" : "Allowed"}
          tone={result.decision === "block" ? "bad" : "good"}
        />
        {result.protected ? (
          <span className="inline-flex items-center gap-1.5 rounded-full border border-border px-2 py-0.5 text-xs">
            <ShieldCheckIcon aria-hidden className="size-3.5" />
            Protected by allowlist
          </span>
        ) : null}
      </div>

      <dl className="grid gap-3 sm:grid-cols-3">
        <div className="rounded-xl border border-border p-3">
          <dt className="text-muted-foreground text-xs">Ad probability</dt>
          <dd className="tabular mt-1 font-semibold text-foreground text-lg">
            {formatProbability(result.probability)}
          </dd>
        </div>
        <div className="rounded-xl border border-border p-3">
          <dt className="text-muted-foreground text-xs">Active threshold</dt>
          <dd className="tabular mt-1 font-semibold text-foreground text-lg">
            {formatProbability(result.activeThreshold)}
          </dd>
        </div>
        <div className="rounded-xl border border-border p-3">
          <dt className="text-muted-foreground text-xs">Margin</dt>
          <dd className="tabular mt-1 font-semibold text-foreground text-lg">
            {formatPercent(result.probability - result.activeThreshold, 1)}
          </dd>
        </div>
      </dl>

      <div className="rounded-xl border border-border p-3">
        <p className="text-muted-foreground text-xs">Blocklist coverage</p>
        <p className="mt-1 text-foreground text-sm">
          {result.blocklistMatch
            ? `Already covered by ${result.blocklistMatch}. The model's verdict is not what decides this query.`
            : "No blocklist rule covers this domain, so the model's verdict decides it."}
        </p>
      </div>

      <FeedbackActions domain={result.domain} />

      {result.contributions.length === 0 ? (
        <p className="text-muted-foreground text-sm">
          The model returned no per-feature contributions for this domain.
        </p>
      ) : (
        <div className="space-y-4">
          <ContributionGroup
            direction="up"
            items={positives}
            magnitude={magnitude}
            title="Pushes toward “ad domain”"
          />
          <ContributionGroup
            direction="down"
            items={negatives}
            magnitude={magnitude}
            title="Pushes away from “ad domain”"
          />

          <dl className="space-y-1 rounded-xl border border-border p-3 text-xs">
            <div className="flex gap-2">
              <dt className="w-28 shrink-0 font-medium text-foreground">
                {KIND_COPY.dense.label}
              </dt>
              <dd className="min-w-0 text-muted-foreground">{KIND_COPY.dense.hint}</dd>
            </div>
            <div className="flex gap-2">
              <dt className="w-28 shrink-0 font-medium text-foreground">
                {KIND_COPY.ngram.label}
              </dt>
              <dd className="min-w-0 text-muted-foreground">{KIND_COPY.ngram.hint}</dd>
            </div>
          </dl>
        </div>
      )}
    </div>
  );
}

/**
 * Turns "that verdict is wrong" into a stored report.
 *
 * The copy here carries the whole safety model in miniature, because this is the
 * one place a household interacts with adaptation without reading the Classifier
 * screen. Pressing a button stores a claim and changes nothing else: no score
 * moves until adaptation is run deliberately and clears its gate. Saying anything
 * warmer than that — "thanks, we've learned!" — would be a lie the appliance
 * would then have to live down the next time the same domain is blocked.
 */
function FeedbackActions({ domain }: { domain: string }) {
  const { refresh } = useCogwheel();
  const [pending, setPending] = React.useState<"ad" | "not-ad" | null>(null);

  const report = async (isAd: boolean) => {
    setPending(isAd ? "ad" : "not-ad");
    try {
      const result = await api.classifierFeedback(domain, isAd);
      notify.success(
        isAd ? `Reported ${result.domain} as an ad domain` : `Reported ${result.domain} as not an ad`,
        `Stored, not applied — scoring for this domain is unchanged. ${nextStep(result)}`,
      );
      // Keeps the pending-report count on the Classifier screen honest without
      // waiting for the next poll.
      await refresh();
    } catch (cause) {
      notify.error("Could not record that report", errorMessage(cause));
    } finally {
      setPending(null);
    }
  };

  return (
    <div className="rounded-xl border border-border p-3">
      <p className="font-medium text-foreground text-sm">Is this verdict wrong?</p>
      <p className="mt-1 text-muted-foreground text-sm">
        Your report is stored on this appliance. It affects scoring only after you run adaptation on
        the Classifier screen and the correction passes its safety check.
      </p>
      <div className="mt-3 flex flex-wrap gap-2">
        <Button
          disabled={pending !== null}
          isLoading={pending === "not-ad"}
          onClick={() => void report(false)}
          size="sm"
          variant="outline"
        >
          <CircleCheckIcon aria-hidden />
          This is not an ad
        </Button>
        <Button
          disabled={pending !== null}
          isLoading={pending === "ad"}
          onClick={() => void report(true)}
          size="sm"
          variant="outline"
        >
          <BanIcon aria-hidden />
          This should be blocked
        </Button>
      </div>
    </div>
  );
}

/** What happens next, stated in counts rather than in reassurance. */
function nextStep(result: ClassifierFeedbackResult): string {
  return result.pendingFeedback >= result.minimumFeedback
    ? `${result.pendingFeedback} reports are now stored, enough to run adaptation on the Classifier screen.`
    : `${result.pendingFeedback} of ${result.minimumFeedback} reports stored; adaptation needs ${result.minimumFeedback} before it can judge a correction.`;
}

/**
 * `kind` arrives from the API as the model's own vocabulary. Neither word means
 * anything to the person asking why their printer stopped resolving, so the
 * chip carries the plain-language name and the report carries a legend.
 */
const KIND_COPY: Record<Inspection["contributions"][number]["kind"], { label: string; hint: string }> = {
  dense: {
    label: "Measured trait",
    hint: "A measured property of the hostname itself, such as how long it is, how many digits or hyphens it has, or how random the letters look.",
  },
  ngram: {
    label: "Character run",
    hint: "A specific short sequence of characters the model learned to associate with ad and tracking hostnames.",
  },
};

function ContributionGroup({
  title,
  items,
  magnitude,
  direction,
}: {
  title: string;
  items: Inspection["contributions"];
  magnitude: number;
  direction: "up" | "down";
}) {
  if (items.length === 0) {
    return (
      <div>
        <h3 className="font-medium text-foreground text-sm">{title}</h3>
        <p className="mt-1 text-muted-foreground text-sm">No features in this direction.</p>
      </div>
    );
  }

  const sorted = [...items].sort((left, right) => Math.abs(right.value) - Math.abs(left.value));

  return (
    <div>
      <h3 className="font-medium text-foreground text-sm">{title}</h3>
      <ul className="mt-2 space-y-1.5">
        {sorted.map((item) => (
          <li className="flex items-center gap-3" key={`${item.kind}-${item.label}`}>
            <span className="w-40 shrink-0 truncate font-mono text-foreground text-xs" title={item.label}>
              {item.label}
            </span>
            <span
              className="w-28 shrink-0 text-muted-foreground text-[11px]"
              title={KIND_COPY[item.kind].hint}
            >
              {KIND_COPY[item.kind].label}
            </span>
            <span className="h-2 flex-1 overflow-hidden rounded-full bg-muted">
              <span
                className={cn("block h-full rounded-full", direction === "up" ? "bg-destructive" : "bg-success")}
                style={{ width: `${Math.min(100, (Math.abs(item.value) / magnitude) * 100)}%` }}
              />
            </span>
            <span className="tabular w-16 shrink-0 text-right text-foreground text-xs">
              {item.value > 0 ? "+" : ""}
              {item.value.toFixed(4)}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
