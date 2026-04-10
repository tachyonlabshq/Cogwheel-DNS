import { useMemo } from "react";
import { useCogwheel } from "@/contexts/cogwheel-context";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardDescription, CardTitle } from "@/components/ui/card";
import { ListRow, EmptyState, Row } from "@/components/shared";

export default function OverviewPage() {
  const {
    dashboard,
    settings,
    resolverAccess,
    state,
    error,
    busyAction,
    handlePauseRuntime,
    handleResumeRuntime,
  } = useCogwheel();

  const enabledBlocklists = useMemo(
    () => settings.blocklists.filter((source) => source.enabled),
    [settings.blocklists],
  );

  const overviewStats = useMemo(() => {
    const allowlistCount = settings.block_profiles.reduce(
      (total, profile) => total + profile.allowlists.length,
      0,
    );
    return [
      {
        label: "Sources",
        value: dashboard.enabled_source_count.toLocaleString(),
        accent: "border-sky-200 bg-sky-50/70 dark:border-sky-800 dark:bg-sky-950/50",
        detail: `${settings.blocklists.length} blocklist source${settings.blocklists.length === 1 ? "" : "s"} and ${allowlistCount} saved allowlist entr${allowlistCount === 1 ? "y" : "ies"}`,
      },
      {
        label: "Blocked DNS Queries",
        value: dashboard.runtime_health.snapshot.blocked_total.toLocaleString(),
        accent: "border-rose-200 bg-rose-50/70 dark:border-rose-800 dark:bg-rose-950/50",
        detail: `${dashboard.runtime_health.snapshot.queries_total.toLocaleString()} total queries observed by this node`,
      },
      {
        label: "Devices",
        value: dashboard.device_count.toLocaleString(),
        accent: "border-emerald-200 bg-emerald-50/70 dark:border-emerald-800 dark:bg-emerald-950/50",
        detail: "Recognized unique devices currently visible to the control plane",
      },
    ];
  }, [
    dashboard.device_count,
    dashboard.enabled_source_count,
    dashboard.runtime_health.snapshot.blocked_total,
    dashboard.runtime_health.snapshot.queries_total,
    settings.block_profiles,
    settings.blocklists.length,
  ]);

  const primaryDnsTarget = resolverAccess.dns_targets[0] ?? "fractal.local";
  const androidDnsTarget =
    resolverAccess.dns_targets.find((target) =>
      /^\d{1,3}(\.\d{1,3}){3}$/.test(target),
    ) ?? primaryDnsTarget;
  const ipv6DnsTarget = resolverAccess.dns_targets.find(
    (target) => target.includes(":") && !target.includes("."),
  );

  return (
    <>
      <section className="space-y-6">
        <Card className="overflow-hidden p-0">
          <div className="flex flex-col gap-4 border-b border-border px-6 py-5 bg-muted/30 md:flex-row md:items-center md:justify-between">
            <div>
              <h1 className="font-display text-3xl font-semibold tracking-tight">
                Dashboard
              </h1>
              <div className="mt-1 text-sm text-muted-foreground">
                A clean snapshot of household filtering, blocked traffic, and
                active devices.
              </div>
            </div>
            <div className="flex flex-wrap gap-2">
              {dashboard.protection_status === "Paused" ? (
                <Button
                  variant="secondary"
                  onClick={() => void handleResumeRuntime()}
                  disabled={busyAction === "resume-runtime"}
                >
                  Resume protection
                </Button>
              ) : (
                <Button
                  variant="outline"
                  onClick={() => void handlePauseRuntime(10)}
                  disabled={busyAction === "pause-runtime"}
                >
                  Pause 10m
                </Button>
              )}
            </div>
          </div>

          <div className="px-6 py-6">
            <section
              id="quick-health"
              className="grid gap-4 md:grid-cols-2 xl:grid-cols-3"
            >
              {overviewStats.map((item) => (
                <Card key={item.label} className={`p-5 ${item.accent} shadow-md hover:shadow-lg hover:-translate-y-0.5 transition-all duration-200 cursor-default`}>
                  <div className="text-sm text-muted-foreground">
                    {item.label}
                  </div>
                  <div className="mt-3 font-display text-4xl font-semibold tracking-tight sm:text-5xl">
                    {item.value}
                  </div>
                  <div className="mt-2 text-sm text-muted-foreground">
                    {item.detail}
                  </div>
                </Card>
              ))}
            </section>
          </div>
        </Card>
      </section>

      <section className="grid gap-6 xl:grid-cols-2">
        <Card className="overflow-hidden p-0">
          <div className="border-b border-border px-6 py-5 bg-muted/30">
            <CardTitle>Top queried domains</CardTitle>
            <CardDescription className="mt-1">
              Recent destinations seen by the resolver over the last day.
            </CardDescription>
          </div>
          <div className="grid gap-3 px-6 py-6">
            {dashboard.domain_insights.top_queried_domains.length === 0 ? (
              <EmptyState>
                Query activity will appear here once devices begin sending
                traffic through Cogwheel.
              </EmptyState>
            ) : (
              dashboard.domain_insights.top_queried_domains.map(
                (entry, index) => (
                  <ListRow
                    key={entry.domain}
                    tone="muted"
                    title={entry.domain}
                    detail={`#${String(index + 1).padStart(2, "0")} most active domain in the recent resolver window.`}
                    right={
                      <div className="text-right">
                        <div className="font-display text-2xl font-semibold">
                          {entry.count}
                        </div>
                        <div className="text-xs text-muted-foreground">
                          queries
                        </div>
                      </div>
                    }
                  />
                ),
              )
            )}
          </div>
        </Card>

        <Card className="overflow-hidden p-0">
          <div className="border-b border-border px-6 py-5 bg-muted/30">
            <CardTitle>Top blocked domains</CardTitle>
            <CardDescription className="mt-1">
              Where protection is actively stepping in right now.
            </CardDescription>
          </div>
          <div className="grid gap-3 px-6 py-6">
            {dashboard.domain_insights.top_blocked_domains.length === 0 ? (
              <EmptyState>
                No blocked domains yet. When filtering engages, the busiest
                blocked destinations will appear here.
              </EmptyState>
            ) : (
              dashboard.domain_insights.top_blocked_domains.map((entry) => (
                <ListRow
                  key={entry.domain}
                  tone="muted"
                  title={entry.domain}
                  detail="Blocked before the query could complete."
                  right={
                    <Badge className="bg-foreground text-background">
                      {entry.count} blocked
                    </Badge>
                  }
                />
              ))
            )}
          </div>
        </Card>
      </section>

      {error ? (
        <Card className="border-accent/30 bg-accent/10 text-accent-foreground">
          {error}
        </Card>
      ) : null}

      <section className="grid gap-6 lg:grid-cols-[1fr_1fr]">
        <Card id="resolver-access" className="overflow-hidden p-0">
          <div className="border-b border-border px-6 py-5 bg-muted/30">
            <CardTitle>How to connect devices</CardTitle>
            <CardDescription className="mt-1">
              Use one of these DNS targets on phones, laptops, TVs, or routers
              that should use this Cogwheel instance.
            </CardDescription>
          </div>
          <div className="grid gap-3 px-6 py-6">
            {resolverAccess.dns_targets.length === 0 ? (
              <EmptyState>
                Resolver targets will appear here once the control plane reports
                reachable DNS addresses.
              </EmptyState>
            ) : (
              resolverAccess.dns_targets.map((target) => (
                <ListRow
                  key={target}
                  title="DNS server"
                  detail={target}
                  detailClassName="mt-1 font-mono text-base font-semibold text-foreground"
                />
              ))
            )}
            <ListRow
              title="Tailscale"
              detail={
                resolverAccess.tailscale_ip ?? "Not available on this node"
              }
              tone="muted"
            />
            {resolverAccess.notes.length > 0 ? (
              <EmptyState>{resolverAccess.notes.join(" ")}</EmptyState>
            ) : null}
            <div className="grid gap-3 md:grid-cols-2">
              {[
                {
                  title: "Android",
                  detail: ipv6DnsTarget
                    ? "Use the Wi-Fi network DNS server setting with this LAN IPv4 and also add the IPv6 resolver shown below on dual-stack networks. Do not use Android Private DNS unless Cogwheel is serving DNS-over-TLS."
                    : "Use the Wi-Fi network DNS server setting with this LAN IP. Do not use Android Private DNS unless Cogwheel is serving DNS-over-TLS.",
                  target: androidDnsTarget,
                },
                {
                  title: "iPhone / iPad",
                  detail:
                    "Wi-Fi -> tap the info icon -> Configure DNS -> Manual.",
                  target: primaryDnsTarget,
                },
                {
                  title: "Mac",
                  detail:
                    "System Settings -> Wi-Fi -> Details -> DNS, then add this resolver.",
                  target: primaryDnsTarget,
                },
                {
                  title: "Windows",
                  detail:
                    "Network & Internet -> Hardware properties -> DNS server assignment -> Edit.",
                  target: primaryDnsTarget,
                },
              ].map((platform) => (
                <ListRow
                  key={platform.title}
                  title={platform.title}
                  detail={platform.detail}
                  right={
                    <div className="font-mono text-sm font-semibold text-foreground">
                      {platform.target}
                    </div>
                  }
                />
              ))}
            </div>
            {ipv6DnsTarget ? (
              <ListRow
                title="IPv6 DNS target"
                detail="If your router or clients use IPv6 DNS, point them here too so traffic does not bypass the IPv4 filter path."
                right={
                  <div className="break-all font-mono text-sm font-semibold text-foreground">
                    {ipv6DnsTarget}
                  </div>
                }
              />
            ) : null}
          </div>
        </Card>

        <Card className="overflow-hidden p-0">
          <div className="border-b border-border px-6 py-5 bg-muted/30">
            <CardTitle>Resolver summary</CardTitle>
            <CardDescription className="mt-1">
              Small operational details that are still useful on the main
              dashboard.
            </CardDescription>
          </div>
          <div className="grid gap-3 px-6 py-6 text-sm">
            <Row label="Protection" value={dashboard.protection_status} />
            <Row
              label="Active ruleset"
              value={
                dashboard.active_ruleset?.hash.slice(0, 12) ?? "None"
              }
            />
            <Row
              label="Cache hits"
              value={String(
                dashboard.runtime_health.snapshot.cache_hits_total,
              )}
            />
            <Row
              label="Fallback served"
              value={String(
                dashboard.runtime_health.snapshot.fallback_served_total,
              )}
            />
            <Row
              label="Runtime notes"
              value={String(dashboard.runtime_health.notes.length)}
            />
          </div>
        </Card>
      </section>

      <section className="grid gap-6 lg:grid-cols-[1fr_1fr]">
        <Card className="overflow-hidden p-0">
          <div className="border-b border-border px-6 py-5 bg-muted/30">
            <CardTitle>Recent risky events</CardTitle>
            <CardDescription className="mt-1">
              Newest high-signal security events without pulling in device
              management controls.
            </CardDescription>
          </div>
          <div className="grid gap-3 px-6 py-6">
            {dashboard.recent_security_events.length === 0 ? (
              <EmptyState>No risky DNS events recorded yet.</EmptyState>
            ) : (
              dashboard.recent_security_events.slice(0, 4).map((event) => (
                <ListRow
                  key={event.id}
                  tone="muted"
                  title={event.domain}
                  detail={`${event.device_name ?? "Unassigned device"} on ${event.client_ip}`}
                  right={<Badge>{event.severity}</Badge>}
                />
              ))
            )}
          </div>
        </Card>
      </section>

      {state === "loading" ? (
        <div className="text-sm text-muted-foreground">
          Loading control plane data...
        </div>
      ) : null}
      {state === "ready" ? (
        <div className="text-sm text-muted-foreground">
          {enabledBlocklists.length} enabled blocklists and{" "}
          {settings.devices.length} named devices.
        </div>
      ) : null}
    </>
  );
}
