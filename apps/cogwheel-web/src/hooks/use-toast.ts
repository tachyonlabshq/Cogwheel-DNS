import { toast } from "sonner";

export type ToastTone = "success" | "error" | "info";

/**
 * Thin wrapper around sonner's `toast()` that maps the app's three-tone model
 * (success / error / info) into the matching sonner methods.
 *
 * Usage:
 *   const t = useToast();
 *   t.success("Saved", "Your changes have been applied.");
 */
export function useToast() {
  return {
    success: (title: string, detail?: string) =>
      toast.success(title, { description: detail }),
    error: (title: string, detail?: string) =>
      toast.error(title, { description: detail }),
    info: (title: string, detail?: string) =>
      toast.info(title, { description: detail }),
  };
}

/**
 * Standalone push function for use outside of React components (e.g. inside
 * the context provider's async handlers where a hook instance is already
 * captured).
 */
export function pushToast(
  title: string,
  detail: string | undefined,
  tone: ToastTone,
) {
  switch (tone) {
    case "success":
      toast.success(title, { description: detail });
      break;
    case "error":
      toast.error(title, { description: detail });
      break;
    case "info":
      toast.info(title, { description: detail });
      break;
  }
}
