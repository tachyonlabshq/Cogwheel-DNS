import { Dialog, DialogBody, DialogContent, DialogHeader } from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import { ALL_NAV } from "@/lib/nav";

const GLOBAL_SHORTCUTS: { keys: string[]; description: string }[] = [
  { keys: ["⌘", "K"], description: "Open the command palette" },
  { keys: ["⌘", ","], description: "Open Settings" },
  { keys: ["⌘", "B"], description: "Collapse or expand the sidebar" },
  { keys: ["/"], description: "Focus the search field on the current screen" },
  { keys: ["?"], description: "Show this list" },
  { keys: ["Esc"], description: "Close a dialog, the palette or the inspector" },
];

export function ShortcutsDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Dialog onOpenChange={(details) => onOpenChange(details.open)} open={open}>
      <DialogContent size="md">
        <DialogHeader
          description="Modifier is ⌘ on macOS and Ctrl elsewhere."
          title="Keyboard shortcuts"
        />
        <DialogBody>
          <section>
            <h3 className="font-medium text-foreground text-sm">Navigation</h3>
            <ul className="mt-2 space-y-1.5">
              {ALL_NAV.filter((item) => item.digit).map((item) => (
                <li className="flex items-center justify-between gap-4" key={item.to}>
                  <span className="text-foreground text-sm">{item.label}</span>
                  <span className="flex items-center gap-1">
                    <Kbd>⌘</Kbd>
                    <Kbd>{item.digit}</Kbd>
                  </span>
                </li>
              ))}
            </ul>
          </section>

          <section className="mt-5">
            <h3 className="font-medium text-foreground text-sm">Global</h3>
            <ul className="mt-2 space-y-1.5">
              {GLOBAL_SHORTCUTS.map((shortcut) => (
                <li className="flex items-center justify-between gap-4" key={shortcut.description}>
                  <span className="text-foreground text-sm">{shortcut.description}</span>
                  <span className="flex items-center gap-1">
                    {shortcut.keys.map((key) => (
                      <Kbd key={key}>{key}</Kbd>
                    ))}
                  </span>
                </li>
              ))}
            </ul>
          </section>
        </DialogBody>
      </DialogContent>
    </Dialog>
  );
}
