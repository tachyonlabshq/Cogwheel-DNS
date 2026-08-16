import type React from "react";
import { CircleCheckIcon, CircleIcon, CircleSlashIcon, TriangleAlertIcon } from "lucide-react";
import { cn } from "@/lib/utils";
import { Status } from "@/components/ui/status";

export type Tone = "good" | "warn" | "bad" | "idle";

/**
 * Colour never carries meaning on its own here: every tone pairs a 400-weight
 * dot with a word and a distinct glyph, and the accessible name states the
 * status in text.
 */
const TONE: Record<
  Tone,
  { variant: "success" | "warning" | "destructive" | "default"; icon: React.ElementType; word: string }
> = {
  good: { variant: "success", icon: CircleCheckIcon, word: "OK" },
  warn: { variant: "warning", icon: TriangleAlertIcon, word: "Warning" },
  bad: { variant: "destructive", icon: CircleSlashIcon, word: "Problem" },
  idle: { variant: "default", icon: CircleIcon, word: "Idle" },
};

export function StatusIndicator({
  tone,
  label,
  description,
  showIcon = false,
  className,
}: {
  tone: Tone;
  label: string;
  description?: string;
  showIcon?: boolean;
  className?: string;
}) {
  const { variant, icon: Icon, word } = TONE[tone];

  return (
    <div className={cn("flex items-start gap-2", className)}>
      <span
        aria-label={`${word}: ${label}`}
        className="flex h-5 shrink-0 items-center gap-1.5"
        role="img"
      >
        <Status size="sm" variant={variant} />
        {showIcon ? <Icon aria-hidden className="size-3.5 text-muted-foreground" /> : null}
      </span>
      <span className="min-w-0">
        <span className="block font-medium text-foreground text-sm leading-5">{label}</span>
        {description ? (
          <span className="block text-muted-foreground text-xs leading-4">{description}</span>
        ) : null}
      </span>
    </div>
  );
}

/**
 * The compact form used inside table rows: a bordered pill whose text sits in
 * `--foreground`, never in the 400 accent (which fails contrast on white).
 */
export function StatusPill({ tone, label, className }: { tone: Tone; label: string; className?: string }) {
  const { variant, word } = TONE[tone];

  return (
    <span
      className={cn(
        "inline-flex h-6 items-center gap-1.5 rounded-full border border-border px-2",
        "font-medium text-foreground text-xs",
        className,
      )}
    >
      <Status size="sm" variant={variant} />
      <span className="sr-only">{word}: </span>
      {label}
    </span>
  );
}
