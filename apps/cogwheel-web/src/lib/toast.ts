import { toast } from "@/components/ui/toast";

/**
 * Thin wrapper over Shark's toaster so every call site uses the same tone
 * vocabulary and every mutation reports both outcomes.
 */
export const notify = {
  success(title: string, description?: string) {
    toast.create({ title, description, type: "success" });
  },
  error(title: string, description?: string) {
    toast.create({ title, description, type: "error", duration: 8_000 });
  },
  warning(title: string, description?: string) {
    toast.create({ title, description, type: "warning" });
  },
  info(title: string, description?: string) {
    toast.create({ title, description, type: "info" });
  },
};
