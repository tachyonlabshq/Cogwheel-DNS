import { useEffect, useState } from "react";
import {
  Shield,
  Activity,
  HardDrive,
  Moon,
  Sun,
  ChevronLeft,
  LayoutDashboard,
  Laptop,
  BrainCircuit,
  Settings,
} from "lucide-react";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarHeader,
  SidebarTrigger,
  useSidebar,
} from "@/components/ui/sidebar";
import { useCogwheel } from "@/contexts/cogwheel-context";

const navItems = [
  { key: "overview", label: "Overview", icon: LayoutDashboard },
  { key: "profiles", label: "Block Profiles", icon: Shield },
  { key: "devices", label: "Devices", icon: Laptop },
  { key: "grease-ai", label: "Grease-AI", icon: BrainCircuit },
  { key: "settings", label: "Settings", icon: Settings },
] as const;

export function AppSidebar() {
  const { state: sidebarState } = useSidebar();
  const { dashboard, state } = useCogwheel();
  const collapsed = sidebarState === "collapsed";

  const [isDark, setIsDark] = useState(() =>
    document.documentElement.classList.contains("dark"),
  );

  const [activeTab, setActiveTab] = useState("overview");

  // Listen for tab changes from the dashboard via a custom event
  useEffect(() => {
    function handleTabChange(e: Event) {
      const detail = (e as CustomEvent<string>).detail;
      if (detail) setActiveTab(detail);
    }
    window.addEventListener("cogwheel:tab-change", handleTabChange);
    return () =>
      window.removeEventListener("cogwheel:tab-change", handleTabChange);
  }, []);

  useEffect(() => {
    const saved = localStorage.getItem("cogwheel-theme");
    if (saved === "dark") {
      document.documentElement.classList.add("dark");
      setIsDark(true);
    }
  }, []);

  function toggleDarkMode() {
    const next = !isDark;
    setIsDark(next);
    document.documentElement.classList.toggle("dark", next);
    localStorage.setItem("cogwheel-theme", next ? "dark" : "light");
  }

  const protectionLabel =
    state === "loading"
      ? "Loading"
      : state === "error"
        ? "Offline"
        : dashboard.protection_status === "Paused"
          ? "Paused"
          : dashboard.runtime_health.degraded
            ? "Degraded"
            : "Protected";

  const protectionDot =
    protectionLabel === "Protected"
      ? "bg-emerald-400"
      : protectionLabel === "Loading"
        ? "bg-muted-foreground"
        : "bg-destructive";

  return (
    <Sidebar className="border-r border-sidebar-border">
      <SidebarHeader className="p-4">
        <div className="flex items-center gap-2.5">
          <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-gold/10">
            <img
              src="/cogwheel.png"
              alt=""
              className="h-4.5 w-4.5 rounded"
              onError={(e) => {
                const img = e.target as HTMLImageElement;
                img.style.display = "none";
                img.parentElement!.innerHTML =
                  '<svg viewBox="0 0 24 24" class="h-4.5 w-4.5 text-gold" fill="none" stroke="currentColor" stroke-width="1.5"><circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="4"/><line x1="12" y1="3" x2="12" y2="8"/><line x1="12" y1="16" x2="12" y2="21"/><line x1="3" y1="12" x2="8" y2="12"/><line x1="16" y1="12" x2="21" y2="12"/></svg>';
              }}
            />
          </div>
          <h1 className="font-heading text-xl font-normal tracking-tight">
            Cogwheel
          </h1>
        </div>
      </SidebarHeader>

      <SidebarContent>
        <SidebarGroup>
          {/* Navigation header */}
          <div className="flex items-center justify-between px-4 py-1">
            <span className="text-[10px] uppercase tracking-widest text-muted-foreground/60 font-medium">
              Navigation
            </span>
          </div>

          <SidebarGroupContent>
            <div className="px-2">
              {navItems.map((item) => {
                const isActive = activeTab === item.key;
                return (
                  <button
                    key={item.key}
                    type="button"
                    aria-label={item.label}
                    aria-pressed={isActive}
                    onClick={() => {
                      setActiveTab(item.key);
                      window.dispatchEvent(
                        new CustomEvent("cogwheel:sidebar-nav", {
                          detail: item.key,
                        }),
                      );
                    }}
                    className={`group flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-left transition-colors mb-0.5 ${
                      isActive
                        ? "bg-secondary/70"
                        : "hover:bg-secondary/30"
                    }`}
                  >
                    <item.icon
                      className={`h-4 w-4 shrink-0 ${
                        isActive
                          ? "text-foreground"
                          : "text-muted-foreground/60"
                      }`}
                    />
                    <span
                      className={`text-sm ${
                        isActive
                          ? "text-foreground font-medium"
                          : "text-foreground/80"
                      }`}
                    >
                      {item.label}
                    </span>
                  </button>
                );
              })}
            </div>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>

      <SidebarFooter className="px-4 py-3 space-y-1.5">
        {/* Protection status */}
        <div className="flex items-center gap-2 text-[11px] text-muted-foreground/70">
          <Shield className="h-3 w-3 shrink-0 text-muted-foreground/40" />
          <span className="flex items-center gap-1.5">
            <span
              className={`inline-block h-1.5 w-1.5 rounded-full ${protectionDot}`}
            />
            {protectionLabel}
          </span>
        </div>

        {/* Query count */}
        <div className="flex items-center gap-2 text-[11px] text-muted-foreground/70">
          <Activity className="h-3 w-3 shrink-0 text-muted-foreground/40" />
          <span className="tabular-nums">
            {dashboard.runtime_health.snapshot.queries_total.toLocaleString()}{" "}
            queries
          </span>
        </div>

        {/* Blocklist count */}
        <div className="flex items-center gap-2 text-[11px] text-muted-foreground/70">
          <HardDrive className="h-3 w-3 shrink-0 text-muted-foreground/40" />
          <span className="tabular-nums">
            {dashboard.enabled_source_count.toLocaleString()} blocklists
          </span>
        </div>

        {/* Dark mode toggle + collapse trigger */}
        <div className="flex items-center justify-between pt-1">
          <button
            onClick={toggleDarkMode}
            className="inline-flex size-7 items-center justify-center rounded-lg text-muted-foreground/70 transition-colors hover:bg-muted hover:text-foreground"
            aria-label={isDark ? "Switch to light mode" : "Switch to dark mode"}
          >
            {isDark ? (
              <Sun className="size-3.5" />
            ) : (
              <Moon className="size-3.5" />
            )}
          </button>
          <SidebarTrigger>
            <ChevronLeft
              className={`size-4 transition-transform ${collapsed ? "rotate-180" : ""}`}
            />
          </SidebarTrigger>
        </div>
      </SidebarFooter>
    </Sidebar>
  );
}
