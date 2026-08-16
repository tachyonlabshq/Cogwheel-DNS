import type React from "react";
import { Input } from "@/components/ui/input";
import { FormField } from "@/components/app/form-field";

export function TextField({
  label,
  value,
  onChange,
  placeholder,
  hint,
  error,
  disabled,
  type = "text",
  inputMode,
  className,
  searchTarget = false,
  autoComplete = "off",
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  hint?: string;
  error?: string;
  disabled?: boolean;
  type?: string;
  inputMode?: React.HTMLAttributes<HTMLInputElement>["inputMode"];
  className?: string;
  /** Marks this input as the one `/` focuses on the current screen. */
  searchTarget?: boolean;
  autoComplete?: string;
}) {
  return (
    <FormField className={className} error={error} hint={hint} label={label}>
      <Input
        autoComplete={autoComplete}
        data-screen-search={searchTarget ? "true" : undefined}
        disabled={disabled}
        inputMode={inputMode}
        onChange={(event) => onChange(event.target.value)}
        placeholder={placeholder}
        spellCheck={false}
        type={type}
        value={value}
      />
    </FormField>
  );
}
