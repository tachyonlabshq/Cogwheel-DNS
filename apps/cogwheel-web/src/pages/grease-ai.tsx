import { useMemo } from "react";
import { useCogwheel } from "@/contexts/cogwheel-context";
import { Badge } from "@/components/ui/badge";
import { Card, CardDescription, CardTitle } from "@/components/ui/card";
import { ListRow, CompactStat } from "@/components/shared";

export default function GreaseAiPage() {
  const { dashboard, settings, latencyBudget } = useCogwheel();

  const greaseAiSignals = useMemo(() => {
    const totalQueries = Math.max(
      dashboard.runtime_health.snapshot.queries_total,
      1,
    );
    const blockedRatio =
      dashboard.runtime_health.snapshot.blocked_total / totalQueries;
    const riskyEventRatio = Math.min(
      dashboard.recent_security_events.length / 6,
      1,
    );
    const latencyHeadroom = latencyBudget.within_budget ? 0.78 : 0.46;
    return [
      {
        label: "Classifier confidence",
        value: Math.min(0.35 + blockedRatio * 1.8, 0.96),
        tint: "from-sky-400/80 to-cyan-300/80",
      },
      {
        label: "Risk memory",
        value: Math.min(0.22 + riskyEventRatio * 0.7, 0.92),
        tint: "from-amber-400/85 to-orange-300/80",
      },
      {
        label: "Latency headroom",
        value: latencyHeadroom,
        tint: "from-emerald-400/85 to-lime-300/80",
      },
    ];
  }, [
    dashboard.recent_security_events.length,
    dashboard.runtime_health.snapshot.blocked_total,
    dashboard.runtime_health.snapshot.queries_total,
    latencyBudget.within_budget,
  ]);

  return (
    <section className="grid gap-6 xl:grid-cols-[1.05fr_0.95fr]">
      <Card className="overflow-hidden p-0">
        <div className="border-b border-border bg-muted/30 px-6 py-5">
          <CardTitle>Grease-AI</CardTitle>
          <CardDescription className="mt-1">
            A calm classifier workspace that shows live learning signals without
            overwhelming the rest of the control plane.
          </CardDescription>
        </div>
        <div className="grid gap-4 px-6 py-6 lg:grid-cols-[0.92fr_1.08fr]">
          <div className="space-y-3">
            <div className="text-sm font-medium text-foreground">
              Learning pulse
            </div>
            <div className="space-y-3">
              {greaseAiSignals.map((signal) => (
                <ListRow
                  key={signal.label}
                  tone="muted"
                  title={signal.label}
                  right={
                    <span className="text-muted-foreground">
                      {Math.round(signal.value * 100)}%
                    </span>
                  }
                  footer={
                    <div className="mt-3 h-3 overflow-hidden rounded-full bg-muted/60">
                      <div
                        className={`h-full rounded-full bg-gradient-to-r ${signal.tint}`}
                        style={{
                          width: `${Math.max(signal.value * 100, 6)}%`,
                        }}
                      />
                    </div>
                  }
                />
              ))}
            </div>
          </div>
          <div className="rounded-2xl border border-border bg-background p-5">
            <div className="text-xs uppercase tracking-[0.24em] text-muted-foreground">
              Classifier animation
            </div>
            <div className="mt-4 grid gap-3">
              {[0, 1, 2, 3, 4].map((row) => (
                <div key={row} className="grid grid-cols-8 gap-2">
                  {greaseAiSignals.map((signal, index) => (
                    <div
                      key={`${row}-${signal.label}-${index}`}
                      className="h-5 rounded-full bg-gradient-to-r from-primary/10 via-primary/40 to-secondary/30"
                      style={{
                        opacity: Math.max(
                          0.2,
                          signal.value - row * 0.12 + index * 0.04,
                        ),
                        animation: `pulse-bar ${2 + row * 0.4}s ease-in-out ${row * 0.3 + index * 0.1}s infinite alternate`,
                        "--bar-opacity": Math.max(
                          0.2,
                          signal.value - row * 0.12 + index * 0.04,
                        ),
                      } as React.CSSProperties}
                    />
                  ))}
                  <div className="h-5 rounded-full bg-background/80" />
                  <div className="h-5 rounded-full bg-background/60" />
                  <div className="h-5 rounded-full bg-background/80" />
                  <div className="h-5 rounded-full bg-background/60" />
                  <div className="h-5 rounded-full bg-background/80" />
                </div>
              ))}
            </div>
            <div className="mt-4 text-sm text-muted-foreground">
              The bars brighten as more DNS activity arrives, blocked decisions
              climb, and the runtime stays inside latency budget.
            </div>
          </div>
        </div>
      </Card>

      <div className="grid gap-6">
        <Card className="overflow-hidden p-0">
          <div className="border-b border-border bg-muted/30 px-6 py-5">
            <CardTitle>Classifier stats</CardTitle>
            <CardDescription className="mt-1">
              Operational numbers behind the current learning pulse.
            </CardDescription>
          </div>
          <div className="grid gap-3 px-6 py-6 sm:grid-cols-2">
            <CompactStat label="Mode" value={settings.classifier.mode} />
            <CompactStat
              label="Threshold"
              value={settings.classifier.threshold.toFixed(2)}
            />
            <CompactStat
              label="Queries observed"
              value={dashboard.runtime_health.snapshot.queries_total.toLocaleString()}
            />
            <CompactStat
              label="Blocked queries"
              value={dashboard.runtime_health.snapshot.blocked_total.toLocaleString()}
            />
          </div>
        </Card>

        <Card className="overflow-hidden p-0">
          <div className="border-b border-border bg-muted/30 px-6 py-5">
            <CardTitle>Latency budgets</CardTitle>
            <CardDescription className="mt-1">
              Live hot-path budget checks after the latest traffic observed by
              this resolver.
            </CardDescription>
          </div>
          <div className="grid gap-3 px-6 py-6 lg:grid-cols-3">
            {latencyBudget.checks.map((check) => (
              <ListRow
                key={check.label}
                tone="muted"
                title={check.label}
                detail={`Target ${check.target_p50_ms.toFixed(1)} ms \u2022 ${check.sample_count} samples`}
                right={
                  <>
                    <Badge>{check.status}</Badge>
                    <div className="mt-2 text-xl font-semibold text-foreground">
                      {check.observed_ms.toFixed(3)} ms
                    </div>
                  </>
                }
                rightClassName="text-right"
              />
            ))}
          </div>
        </Card>
      </div>
    </section>
  );
}
