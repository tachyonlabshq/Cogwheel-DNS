import { type ReactNode } from "react";

export function ListRow({
  title,
  detail,
  meta,
  right,
  footer,
  tone = "default",
  compact = false,
  detailClassName,
  rightClassName,
  children,
}: {
  title: ReactNode;
  detail?: ReactNode;
  meta?: ReactNode;
  right?: ReactNode;
  footer?: ReactNode;
  tone?: "default" | "muted";
  compact?: boolean;
  detailClassName?: string;
  rightClassName?: string;
  children?: ReactNode;
}) {
  return (
    <div
      className={`rounded-xl border border-border ${tone === "muted" ? "bg-muted/20" : "bg-background"} ${compact ? "p-3" : "p-4"} transition-colors duration-150 hover:border-primary/20 hover:bg-muted/30`}
    >
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <div className="font-medium text-foreground">{title}</div>
          {meta ? meta : null}
          {detail ? (
            <div className={detailClassName ?? "mt-1 text-sm text-muted-foreground"}>
              {detail}
            </div>
          ) : null}
        </div>
        {right ? <div className={`shrink-0 ${rightClassName ?? ""}`}>{right}</div> : null}
      </div>
      {footer ? footer : null}
      {children}
    </div>
  );
}

export function EmptyState({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-2xl border border-dashed border-border bg-muted/20 py-6 px-5 text-sm text-muted-foreground text-center">
      {children}
    </div>
  );
}

export function Row({ label, value }: { label: string; value: string }) {
  return (
    <ListRow
      title={label}
      right={<span className="font-medium">{value}</span>}
      compact
    />
  );
}

export function CompactStat({ label, value }: { label: string; value: string }) {
  return (
    <ListRow
      title={label}
      tone="muted"
      compact
      right={<div className="text-xl font-semibold text-foreground">{value}</div>}
      rightClassName="text-right"
    />
  );
}
