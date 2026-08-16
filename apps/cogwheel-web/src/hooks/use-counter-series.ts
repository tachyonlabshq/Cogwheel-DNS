import React from "react";
import { useCogwheel } from "@/data/context";

export type SeriesPoint = { label: string; value: number };

/**
 * The appliance exposes lifetime counters, not a time series, and nothing on it
 * records history. Rather than invent a trend, this samples the counter on each
 * successful poll and plots the *deltas* between samples — genuinely measured,
 * but only for as long as this page has been open, which is what the caller
 * must say on screen.
 */
export function useCounterSeries(counter: number, maxPoints = 40): SeriesPoint[] {
  const { lastUpdatedAt } = useCogwheel();
  const [series, setSeries] = React.useState<SeriesPoint[]>([]);
  const previous = React.useRef<number | null>(null);
  const lastSample = React.useRef<number | null>(null);

  React.useEffect(() => {
    if (lastUpdatedAt === null || lastUpdatedAt === lastSample.current) return;
    lastSample.current = lastUpdatedAt;

    const prior = previous.current;
    previous.current = counter;
    // The first sample only establishes a baseline; there is no delta yet.
    if (prior === null) return;

    setSeries((current) =>
      [...current, { label: new Date(lastUpdatedAt).toISOString(), value: Math.max(0, counter - prior) }].slice(
        -maxPoints,
      ),
    );
  }, [counter, lastUpdatedAt, maxPoints]);

  return series;
}
