import {
  ActivityIcon,
  BrainCircuitIcon,
  ChartNoAxesColumnIcon,
  LaptopIcon,
  LayoutDashboardIcon,
  ServerCogIcon,
  SettingsIcon,
  ShieldIcon,
} from "lucide-react";
import type React from "react";

export type NavItem = {
  to: string;
  label: string;
  /** Shown in the sidebar and the command palette. */
  shortcut?: string;
  /** The digit ⌘/Ctrl combines with; `undefined` means no numeric shortcut. */
  digit?: string;
  icon: React.ElementType;
  description: string;
};

export const PRIMARY_NAV: NavItem[] = [
  {
    to: "/",
    label: "Overview",
    shortcut: "⌘1",
    digit: "1",
    icon: LayoutDashboardIcon,
    description: "Protection state, traffic and connection instructions",
  },
  {
    to: "/activity",
    label: "Activity",
    shortcut: "⌘2",
    digit: "2",
    icon: ActivityIcon,
    description: "Live query stream and recent risky events",
  },
  {
    to: "/devices",
    label: "Devices",
    shortcut: "⌘3",
    digit: "3",
    icon: LaptopIcon,
    description: "Named devices and per-device policy",
  },
  {
    to: "/protection",
    label: "Protection",
    shortcut: "⌘4",
    digit: "4",
    icon: ShieldIcon,
    description: "Blocklists, services and block profiles",
  },
  {
    to: "/classifier",
    label: "Classifier",
    shortcut: "⌘5",
    digit: "5",
    icon: BrainCircuitIcon,
    description: "Model status, sensitivity and the domain inspector",
  },
  {
    to: "/insights",
    label: "Insights",
    shortcut: "⌘6",
    digit: "6",
    icon: ChartNoAxesColumnIcon,
    description: "Top domains, severity mix and ruleset history",
  },
];

export const SECONDARY_NAV: NavItem[] = [
  {
    to: "/settings",
    label: "Settings",
    shortcut: "⌘,",
    icon: SettingsIcon,
    description: "Alerts, sync, upstream and threat intelligence",
  },
  {
    to: "/system",
    label: "System",
    icon: ServerCogIcon,
    description: "Diagnostics, backup, audit trail and drills",
  },
];

export const ALL_NAV = [...PRIMARY_NAV, ...SECONDARY_NAV];
