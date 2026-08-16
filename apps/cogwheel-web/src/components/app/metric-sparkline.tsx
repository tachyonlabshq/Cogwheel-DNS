import { Area, AreaChart, Bar, BarChart, Cell, ResponsiveContainer, XAxis, YAxis } from "recharts";
import { cn } from "@/lib/utils";

/**
 * Monochrome by design: charts use the neutral `--chart-*` ramp so the three
 * accents stay reserved for status. The one exception is `BlockRatioBars`,
 * where blocked-vs-allowed *is* the status.
 */
export function MetricSparkline({
  data,
  className,
  ariaLabel,
}: {
  data: { label: string; value: number }[];
  className?: string;
  ariaLabel: string;
}) {
  if (data.length === 0) {
    return (
      <div
        className={cn("flex h-16 items-center justify-center text-muted-foreground text-xs", className)}
      >
        No samples yet
      </div>
    );
  }

  return (
    <div aria-label={ariaLabel} className={cn("h-16 w-full", className)} role="img">
      <ResponsiveContainer height="100%" width="100%">
        <AreaChart data={data} margin={{ top: 4, right: 0, bottom: 0, left: 0 }}>
          <defs>
            <linearGradient id="sparkline-fill" x1="0" x2="0" y1="0" y2="1">
              <stop offset="0%" stopColor="var(--chart-1)" stopOpacity={0.18} />
              <stop offset="100%" stopColor="var(--chart-1)" stopOpacity={0} />
            </linearGradient>
          </defs>
          <Area
            dataKey="value"
            fill="url(#sparkline-fill)"
            isAnimationActive={false}
            stroke="var(--chart-1)"
            strokeWidth={1.5}
            type="monotone"
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
}

/** Horizontal ranking bars, e.g. top queried domains. */
export function RankBars({
  data,
  className,
  ariaLabel,
  tone = "neutral",
}: {
  data: { label: string; value: number }[];
  className?: string;
  ariaLabel: string;
  /** "blocked" is a genuinely status-valued series, so it may use the accent. */
  tone?: "neutral" | "blocked";
}) {
  if (data.length === 0) return null;

  const fill = tone === "blocked" ? "var(--destructive)" : "var(--chart-2)";

  return (
    <div
      aria-label={ariaLabel}
      className={cn("w-full", className)}
      role="img"
      style={{ height: `${Math.max(data.length * 28, 56)}px` }}
    >
      <ResponsiveContainer height="100%" width="100%">
        <BarChart data={data} layout="vertical" margin={{ top: 0, right: 8, bottom: 0, left: 0 }}>
          <XAxis hide type="number" />
          <YAxis
            axisLine={false}
            dataKey="label"
            tick={{ fill: "var(--muted-foreground)", fontSize: 11 }}
            tickLine={false}
            type="category"
            width={140}
          />
          <Bar dataKey="value" isAnimationActive={false} radius={[0, 3, 3, 0]}>
            {data.map((entry) => (
              <Cell fill={fill} key={entry.label} />
            ))}
          </Bar>
        </BarChart>
      </ResponsiveContainer>
    </div>
  );
}
