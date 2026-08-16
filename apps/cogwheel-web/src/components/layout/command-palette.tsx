import React from "react";
import { useNavigate } from "react-router-dom";
import { createListCollection } from "@ark-ui/react/collection";
import {
  KeyboardIcon,
  LaptopIcon,
  MoonIcon,
  PauseIcon,
  PlayIcon,
  RotateCcwIcon,
  RotateCwIcon,
  SearchIcon,
  ShieldIcon,
  StethoscopeIcon,
  SunIcon,
} from "lucide-react";
import { api } from "@/lib/api";
import { ALL_NAV } from "@/lib/nav";
import { SNOOZE_OPTIONS } from "@/lib/constants";
import { looksLikeDomain } from "@/lib/derive";
import { useCogwheel } from "@/data/context";
import { useTheme } from "@/hooks/use-theme";
import { useDomainInspector } from "@/components/app/inspector-context";
import { useProtectionActions } from "@/hooks/use-protection";
import {
  Command,
  CommandContent,
  CommandDialog,
  CommandDialogContent,
  CommandEmpty,
  CommandFooter,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandShortcut,
} from "@/components/ui/command";
import { Kbd, KbdGroup } from "@/components/ui/kbd";

type PaletteItem = {
  value: string;
  label: string;
  group: string;
  hint?: string;
  icon: React.ElementType;
  perform: () => void;
};

const GROUP_ORDER = ["Go to", "Protection", "Data", "Devices", "Blocklists", "Appearance", "Help"];

export function CommandPalette({
  open,
  onOpenChange,
  onShowShortcuts,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onShowShortcuts: () => void;
}) {
  const navigate = useNavigate();
  const { data, mutate, reload } = useCogwheel();
  const { pause, resume } = useProtectionActions();
  const { inspect } = useDomainInspector();
  const { preference, setPreference } = useTheme();
  const [query, setQuery] = React.useState("");

  const close = React.useCallback(() => {
    onOpenChange(false);
    setQuery("");
  }, [onOpenChange]);

  const items = React.useMemo<PaletteItem[]>(() => {
    const entries: PaletteItem[] = [];

    for (const item of ALL_NAV) {
      entries.push({
        value: `nav:${item.to}`,
        label: item.label,
        group: "Go to",
        hint: item.shortcut ?? item.description,
        icon: item.icon,
        perform: () => navigate(item.to),
      });
    }

    for (const minutes of SNOOZE_OPTIONS) {
      entries.push({
        value: `pause:${minutes}`,
        label: `Pause protection for ${minutes} minutes`,
        group: "Protection",
        hint: "Network-wide",
        icon: PauseIcon,
        perform: () => void pause(minutes),
      });
    }

    entries.push({
      value: "resume",
      label: "Resume protection",
      group: "Protection",
      icon: PlayIcon,
      perform: () => void resume(),
    });

    entries.push({
      value: "refresh-sources",
      label: "Refresh blocklist sources",
      group: "Data",
      hint: "Re-fetches every enabled source and rebuilds the ruleset",
      icon: RotateCwIcon,
      perform: () =>
        void mutate({
          key: "refresh-sources",
          action: () => api.refreshSources(),
          successTitle: "Sources refreshed",
          successDetail: (result) => result.notes[0],
          failureTitle: "Could not refresh sources",
        }),
    });

    entries.push({
      value: "reload",
      label: "Reload control-plane data",
      group: "Data",
      icon: RotateCcwIcon,
      perform: () => void reload(),
    });

    entries.push({
      value: "health-check",
      label: "Run runtime health check",
      group: "Data",
      hint: "Issues live probes against the configured probe domains",
      icon: StethoscopeIcon,
      perform: () =>
        void mutate({
          key: "runtime-health-check",
          action: () => api.runtimeHealthCheck(),
          successTitle: (report) => (report.degraded ? "Runtime degraded" : "Runtime healthy"),
          successDetail: (report) => report.notes[0] ?? "Guard probes completed without regressions.",
          failureTitle: "Health check failed",
        }),
    });

    for (const device of data.settings.devices) {
      entries.push({
        value: `device:${device.id}`,
        label: device.name,
        group: "Devices",
        hint: device.ip_address,
        icon: LaptopIcon,
        perform: () => navigate(`/devices?device=${encodeURIComponent(device.id)}`),
      });
    }

    for (const source of data.settings.blocklists) {
      entries.push({
        value: `source:${source.id}`,
        label: source.name,
        group: "Blocklists",
        hint: `${source.enabled ? "Enabled" : "Disabled"} · ${source.profile}`,
        icon: ShieldIcon,
        perform: () => navigate(`/protection?source=${encodeURIComponent(source.id)}`),
      });
    }

    entries.push({
      value: "theme",
      label: preference === "dark" ? "Switch to light theme" : "Switch to dark theme",
      group: "Appearance",
      icon: preference === "dark" ? SunIcon : MoonIcon,
      perform: () => setPreference(preference === "dark" ? "light" : "dark"),
    });

    entries.push({
      value: "shortcuts",
      label: "Show keyboard shortcuts",
      group: "Help",
      hint: "?",
      icon: KeyboardIcon,
      perform: onShowShortcuts,
    });

    return entries;
  }, [data.settings.blocklists, data.settings.devices, mutate, navigate, onShowShortcuts, pause, preference, reload, resume, setPreference]);

  const filtered = React.useMemo(() => {
    const needle = query.trim().toLowerCase();
    const matches = needle
      ? items.filter(
          (item) =>
            item.label.toLowerCase().includes(needle) ||
            item.group.toLowerCase().includes(needle) ||
            (item.hint ?? "").toLowerCase().includes(needle),
        )
      : items;

    // Anything shaped like a hostname gets a first-class "inspect it" action,
    // which is the fastest path from "why is this broken?" to an answer.
    if (looksLikeDomain(needle)) {
      return [
        {
          value: `inspect:${needle}`,
          label: `Inspect ${needle}`,
          group: "Go to",
          hint: "Show the classifier's verdict and evidence",
          icon: SearchIcon,
          perform: () => inspect(needle),
        } satisfies PaletteItem,
        ...matches,
      ];
    }

    return matches;
  }, [inspect, items, query]);

  const collection = React.useMemo(
    () =>
      createListCollection({
        items: filtered,
        itemToValue: (item) => item.value,
        itemToString: (item) => item.label,
      }),
    [filtered],
  );

  const grouped = React.useMemo(() => {
    const buckets = new Map<string, PaletteItem[]>();
    for (const item of filtered) {
      const bucket = buckets.get(item.group);
      if (bucket) bucket.push(item);
      else buckets.set(item.group, [item]);
    }
    return [...buckets.entries()].sort(
      ([left], [right]) => GROUP_ORDER.indexOf(left) - GROUP_ORDER.indexOf(right),
    );
  }, [filtered]);

  return (
    <CommandDialog onOpenChange={(details) => (details.open ? onOpenChange(true) : close())} open={open}>
      <CommandDialogContent
        description="Jump to a screen, run an action, or inspect a domain."
        title="Command palette"
      >
        <Command
          collection={collection}
          inputValue={query}
          onInputValueChange={(details) => setQuery(details.inputValue)}
          onValueChange={(details) => {
            const selected = filtered.find((item) => item.value === details.value[0]);
            if (!selected) return;
            close();
            selected.perform();
          }}
        >
          <CommandInput placeholder="Search screens, devices, blocklists or a domain…" />
          <CommandContent>
            <CommandList>
              <CommandEmpty>Nothing matches that.</CommandEmpty>
              {grouped.map(([group, entries]) => (
                <CommandGroup heading={group} key={group}>
                  {entries.map((entry) => (
                    <CommandItem item={entry} key={entry.value}>
                      <entry.icon aria-hidden />
                      <span className="flex-1 truncate">{entry.label}</span>
                      {entry.hint ? (
                        <CommandShortcut className="truncate">{entry.hint}</CommandShortcut>
                      ) : null}
                    </CommandItem>
                  ))}
                </CommandGroup>
              ))}
            </CommandList>
          </CommandContent>
          <CommandFooter>
            <KbdGroup>
              <Kbd>↑</Kbd>
              <Kbd>↓</Kbd>
              <span>navigate</span>
            </KbdGroup>
            <KbdGroup>
              <Kbd>↵</Kbd>
              <span>run</span>
              <Kbd>esc</Kbd>
              <span>close</span>
            </KbdGroup>
          </CommandFooter>
        </Command>
      </CommandDialogContent>
    </CommandDialog>
  );
}
