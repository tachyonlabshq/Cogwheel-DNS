import React from "react";

/**
 * Recharts is ~320 kB — larger than the rest of the app put together. The
 * Overview sparkline needs two polls of data before it can draw anything, so
 * the library is fetched only once there is something to plot rather than
 * sitting on the critical path of the landing screen.
 */
const MetricSparkline = React.lazy(() =>
  import("@/components/app/metric-sparkline").then((module) => ({ default: module.MetricSparkline })),
);

export function LazySparkline(props: {
  data: { label: string; value: number }[];
  ariaLabel: string;
  className?: string;
}) {
  return (
    <React.Suspense fallback={<div className="h-16" />}>
      <MetricSparkline {...props} />
    </React.Suspense>
  );
}
