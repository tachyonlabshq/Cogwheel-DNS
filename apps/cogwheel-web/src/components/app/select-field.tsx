import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { FormField } from "@/components/app/form-field";

export type SelectOption = { value: string; label: string; disabled?: boolean };

/**
 * The app uses the native select everywhere rather than the listbox variant:
 * it is keyboard- and screen-reader-correct on every platform for free, and
 * every dropdown here is a short, flat list of policy values.
 */
export function SelectField({
  label,
  value,
  options,
  onChange,
  hint,
  error,
  disabled,
  placeholder,
  className,
}: {
  label: string;
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  hint?: string;
  error?: string;
  disabled?: boolean;
  placeholder?: string;
  className?: string;
}) {
  return (
    <FormField className={className} error={error} hint={hint} label={label}>
      <NativeSelect
        className="w-full"
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
        value={value}
      >
        {placeholder ? <NativeSelectOption value="">{placeholder}</NativeSelectOption> : null}
        {options.map((option) => (
          <NativeSelectOption disabled={option.disabled} key={option.value} value={option.value}>
            {option.label}
          </NativeSelectOption>
        ))}
      </NativeSelect>
    </FormField>
  );
}
