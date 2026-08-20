import type React from "react";
import { cn } from "@/lib/utils";

/** Max-width column with the brief's gutters: 24px mobile, 32–40px desktop. */
export function PageShell({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <div className={cn("mx-auto w-full max-w-[1200px] px-6 py-6 sm:px-8 lg:px-10 lg:py-8", className)}>
      {children}
    </div>
  );
}

/** 32px between major blocks, per the section rhythm in the brief. */
export function PageSections({ children, className }: { children: React.ReactNode; className?: string }) {
  return <div className={cn("flex flex-col gap-6", className)}>{children}</div>;
}

export function PageHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description?: string;
  actions?: React.ReactNode;
}) {
  return (
    <header className="mb-6 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between sm:gap-6">
      <div className="min-w-0">
        <h1 className="display-tight font-semibold text-2xl text-foreground">{title}</h1>
        {description ? (
          <p className="mt-1 max-w-2xl text-muted-foreground text-sm">{description}</p>
        ) : null}
      </div>
      {actions ? <div className="flex shrink-0 flex-wrap items-center gap-2">{actions}</div> : null}
    </header>
  );
}
