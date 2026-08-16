import type React from "react";
import { cn } from "@/lib/utils";
import { Field, FieldError, FieldHelper, FieldLabel } from "@/components/ui/field";

/**
 * Every control in the app is wrapped in this so the label, hint and error are
 * wired to the input by Ark's Field context rather than by hand.
 */
export function FormField({
  label,
  hint,
  error,
  required,
  children,
  className,
  orientation = "vertical",
}: {
  label: string;
  hint?: string;
  error?: string;
  required?: boolean;
  children: React.ReactNode;
  className?: string;
  orientation?: "vertical" | "horizontal";
}) {
  return (
    <Field
      className={cn(className)}
      invalid={Boolean(error)}
      orientation={orientation}
      required={required}
    >
      <FieldLabel>{label}</FieldLabel>
      {children}
      {hint && !error ? <FieldHelper>{hint}</FieldHelper> : null}
      {error ? <FieldError>{error}</FieldError> : null}
    </Field>
  );
}

export function FieldRow({ children, className }: { children: React.ReactNode; className?: string }) {
  return <div className={cn("grid gap-4 sm:grid-cols-2", className)}>{children}</div>;
}
