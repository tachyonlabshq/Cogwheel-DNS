# Cogwheel Web — Current App Inventory (pre-rewrite baseline)

This document is a complete inventory of `apps/cogwheel-web` as it exists today, produced so the app can be deleted and rebuilt (Shark UI design system rewrite) **without losing any functionality**. Treat section 1 as the regression checklist for the new UI. Section 2 is the literal API contract the new UI must keep speaking (backend is not changing as part of this rewrite). Sections 3–5 describe behavior/mechanics that must be reimplemented, not just restyled. Section 6 tells you exactly what to delete and what (if anything) to carry forward.

Read date: 2026-08-16. Source: `/home/user/Cogwheel-DNS/apps/cogwheel-web/**` (excluding `node_modules`).

---

## 0. App shape at a glance

- Single-page app, **no client-side router** (`react-router-dom` is a listed dependency in `package.json` but is never imported anywhere in `src/**` — dead dependency).
- `recharts` is also a listed dependency but never imported anywhere — dead dependency. There are **no actual charts** in the current UI; "Learning Pulse" bars in Grease-AI tab are hand-rolled `<div>` bars, not a charting library.
- Tab navigation is done via component state (`Dashboard`'s `activeTab`) synchronized with the sidebar through two `window` `CustomEvent`s: `cogwheel:tab-change` (Dashboard → Sidebar, tells sidebar which tab is now active) and `cogwheel:sidebar-nav` (Sidebar → Dashboard, tells Dashboard to switch tabs). No URL state — refreshing the page always returns to the "overview" tab.
- Global state/data comes from a single React Context (`CogwheelProvider`, see §3) that polls the backend every 5 seconds.
- Root render tree (`src/main.tsx`):
  ```
  <React.StrictMode>
    <ErrorBoundary>
      <CogwheelProvider>
        <SidebarProvider>
          <AppSidebar />
          <SidebarInset className="flex h-screen flex-col">
            <div className="flex-1 min-h-0"><Dashboard /></div>
            <StatusBar />
          </SidebarInset>
        </SidebarProvider>
        <Toaster />           <!-- sonner toast portal -->
      </CogwheelProvider>
    </ErrorBoundary>
  </React.StrictMode>
  ```
- Five tabs, each its own file under `src/components/dashboard/`: `overview-tab.tsx`, `profiles-tab.tsx`, `devices-tab.tsx`, `grease-ai-tab.tsx`, `settings-tab.tsx`, composed by `dashboard.tsx`.

---

## 1. Feature inventory — the "do not regress" checklist

### 1.1 Global chrome (always visible)

**Sidebar** (`src/components/app-sidebar.tsx`)
- Brand header: cogwheel icon (lucide `Cog`) in a rounded tile + "Cogwheel" wordmark in the display/serif font.
- "Navigation" section label (uppercase, tiny, muted).
- 5 nav buttons, each: icon + label, active state shows a 3px left accent bar (primary color) and highlighted background (`bg-secondary/70`). Icons: `LayoutDashboard` (Overview), `Shield` (Block Profiles), `Laptop` (Devices), `BrainCircuit` (Grease-AI), `Settings` (Settings). Nav labels differ slightly from tab labels ("Block Profiles" in sidebar vs "Profiles" in the top tab bar — same tab).
- Clicking a nav item dispatches `cogwheel:sidebar-nav`, and on mobile also closes the mobile sidebar sheet.
- Footer (3 stacked rows, each icon + text, 11px muted):
  1. Shield icon + protection status dot + label: "Protected" (green dot) / "Paused" (destructive dot) / "Degraded" (destructive dot, computed from `dashboard.runtime_health.degraded`) / "Loading" (muted dot) / "Offline" (destructive dot, when `state === "error"`).
  2. Activity icon + `{queries_total.toLocaleString()} queries`.
  3. HardDrive icon + `{enabled_source_count.toLocaleString()} blocklists`.
- Collapsible/responsive sidebar behavior comes from the shadcn `Sidebar` primitive (`src/components/ui/sidebar.tsx`) — supports icon-collapse, mobile sheet overlay, keyboard shortcut (`Cmd/Ctrl+B` toggles), and persists collapsed state in a `sidebar_state` cookie (7-day max-age). Sidebar width vars: `--sidebar-width: 16rem`, `--sidebar-width-icon: 3rem`.

**Header / tab bar** (`src/components/dashboard/dashboard.tsx`)
- Centered pill-style tab switcher with 5 tabs: Overview, Profiles, Devices, Grease-AI, Settings (note: label is "Profiles" here, not "Block Profiles"). Active tab has white/background pill with subtle ring.
- Right-aligned action cluster:
  - **Theme toggle** button (sun/moon icon toggle, see §5 theme mechanism).
  - **Refresh** button (RotateCw icon + "Refresh" text, hidden text on small screens) — calls `refreshLiveData()` (the lighter polling refresh, not full `load()`).
  - **Pause/Resume** button — toggles based on `dashboard.protection_status === "Paused"`. Shows "Pause" (Pause icon, outline variant) when active, or "Resume" (Play icon, default/filled variant) when paused. Pause always requests **10 minutes** (`handlePauseRuntime(10)`, hardcoded — there is no UI to choose a different duration on this button, though the Overview tab's own pause button also hardcodes 10 min). Disabled while `busyAction` is `"pause-runtime"` or `"resume-runtime"`.
- Below the header: scrollable content area. While `state === "loading"`, shows a skeleton layout (4 stat-card skeletons + 2 content-card skeletons) instead of the real tab content.

**Status bar** (`src/components/status-bar.tsx`, footer, 24px tall, monospace 10px text)
- Connection dot + label: green "Connected" (`state === "ready"`), destructive "Offline" (`state === "error"`), muted "Loading" (otherwise).
- Vertical divider.
- `{queries_total} queries · {blocked_total} blocked` (truncates on overflow).
- Flexible spacer.
- Right-aligned: raw `dashboard.protection_status` string (e.g. "Active", "Paused").

**Toasts** (`sonner`, mounted once in `main.tsx` as `<Toaster />`) — every mutation handler in the context pushes a success or error toast with title + optional detail. Tones: success, error, info (info used only for the "Working offline" offline-fallback notice).

**Error boundary** (`src/components/error-boundary.tsx`) — class component wrapping the entire app. On an uncaught render error: full-screen centered card with AlertTriangle icon, "Something went wrong" heading, generic explanation text, the raw `error.message` in a monospace chip, and a "Try again" button (RotateCw icon) that resets the boundary's local state (does **not** reload the page or reset app state beyond the boundary).

### 1.2 Overview tab (`overview-tab.tsx`)

- Error banner (destructive-tinted) shown at top when `error` is set, regardless of tab.
- 4 stat cards (responsive grid: 1 col mobile → 2 col sm → 4 col xl), each with fade-up stagger animation:
  1. **Protection Status** — big value = `dashboard.protection_status`; badge mirrors it (variant: `default` if "Active", `secondary` if "Paused", `outline` otherwise); card gets a colored ring (primary ring if Active, destructive ring otherwise, no ring if Paused). Footer: a **Pause 10 min** button (outline) when active, or **Resume protection** button (secondary) when paused, disabled while the matching busy action is in flight; below that, `"Ruleset {hash.slice(0,12)}"` or `"No active ruleset"`.
  2. **Sources** — big value = `enabled_source_count`; badge "enabled" (secondary). Footer: `"{blocklists.length} blocklist source(s)"` and `"{allowlistCount} saved allowlist entr(y|ies)"` where `allowlistCount` is the sum of `allowlists.length` across all block profiles.
  3. **Blocked Queries** — big value = `runtime_health.snapshot.blocked_total`; badge "blocked" (destructive). Footer: `"{queries_total} total queries"` and "Observed by this node".
  4. **Devices** — big value = `device_count`; badge "visible" (outline). Footer: "Unique devices" and "Currently visible to the control plane".
- Two side-by-side tables (2-col grid on xl):
  - **Top Queried Domains** — columns: Domain, Queries (right-aligned). Source: `dashboard.domain_insights.top_queried_domains`. Empty state: Activity icon + "Query activity will appear here once devices begin sending traffic through Cogwheel."
  - **Top Blocked Domains** — columns: Domain, Blocked (right-aligned, value shown as a destructive Badge). Source: `dashboard.domain_insights.top_blocked_domains`. Empty state: ShieldOff icon + "No blocked domains yet. When filtering engages, the busiest blocked destinations will appear here."
- **How to Connect Devices** card — table with columns Target/Address:
  - One row per `resolverAccess.dns_targets` entry, label "DNS server", value monospace.
  - One row "Tailscale" → `resolverAccess.tailscale_ip` or "Not available on this node".
  - One optional row "IPv6 DNS" if an IPv6-looking target is found in `dns_targets` (heuristic: contains `:` and no `.`).
  - Empty state (no dns_targets): "Resolver targets will appear here once the control plane reports reachable DNS addresses."
  - Below the table: `resolverAccess.notes.join(" ")` shown as a paragraph if non-empty.
  - **Platform guide sub-table** (Platform / Instructions / DNS columns), 4 static rows with per-platform copy and a computed DNS target:
    - Android: target = first IPv4-looking `dns_targets` entry (regex `^\d{1,3}(\.\d{1,3}){3}$`) or fallback to primary target; instructions vary depending on whether an IPv6 target exists (mentions "also add the IPv6 resolver shown below on dual-stack networks" when one exists); always warns "Do not use Android Private DNS unless Cogwheel is serving DNS-over-TLS."
    - iPhone/iPad: target = primary (first) dns target; instructions "Wi-Fi -> tap the info icon -> Configure DNS -> Manual."
    - Mac: target = primary; "System Settings -> Wi-Fi -> Details -> DNS, then add this resolver."
    - Windows: target = primary; "Network & Internet -> Hardware properties -> DNS server assignment -> Edit."
    - `primaryDnsTarget` fallback when there are no dns_targets at all: literal string `"fractal.local"`.
- **Resolver Summary** card — metric/value table: Protection (badge), Active ruleset (monospace hash prefix or "None"), Cache hits, Fallback served, Runtime notes (count of `runtime_health.notes`).
- **Recent Risky Events** card — table columns Domain / Device / Client IP / Severity (right-aligned badge). Source: `dashboard.recent_security_events`, **sliced to first 4**. Device column shows `event.device_name ?? "Unassigned device"`. Severity badge variant: destructive if `severity === "high"`, secondary otherwise (note: "critical" also renders as secondary badge — only exact match "high" gets destructive styling; this is a latent styling quirk in the current app worth deciding on purpose during rewrite). Empty state: "No risky DNS events recorded yet."
- Footer line: while loading, "Loading control plane data..."; otherwise `"{enabledBlocklists.length} enabled blocklists and {settings.devices.length} named devices."` with `" (offline)"` appended when `error` is set.

### 1.3 Profiles tab — "Block Profiles" (`profiles-tab.tsx`)

Two-column layout (`360px` fixed left rail + flexible right editor on xl, stacked below xl).

**Left: Profiles library card**
- Title "Profiles", description "Manage block profiles for different devices and routines", a "New" button (Plus icon, outline) in the card action slot that starts a fresh blank draft (clears selection, resets emoji/name/description/blocklists/allowlist string, resets the custom-list inputs).
- List of saved profiles as clickable rows (each row: emoji or `◌` placeholder + name on the left, a secondary badge with `{blocklists.length} sources` on the right, optional description text below). Selecting a row loads it into the editor. Active row gets a primary border + tinted background.
- Empty state: "No saved profiles yet. Create one to get started."
- On mount / whenever the profile list changes: if nothing is selected and not currently creating a new one, auto-selects the first profile in the list (if any exist); if the list is empty, resets to a blank draft.

**Right: Profile editor card**
- Title switches between "Edit Profile" / "New Profile" depending on whether a profile is selected.
- **Profile Identity** section: Emoji input (100px wide) + Profile name input (side by side), then a full-width Description input.
- **OISD Blocklist Presets** section — helper text: "Core and NSFW families are kept mutually exclusive automatically." Renders the 4 hardcoded `oisdProfileOptions` (see §4) as toggle tiles in a 2-col grid, each showing name, a badge ("small"/"full" derived from the id containing "small"), and description text ("Adult-content focused OISD feed." for nsfw ids, else "General-purpose OISD protection feed."). Clicking toggles inclusion in `draft.blocklists`, and **enforces mutual exclusivity**: selecting `oisd-big` deselects `oisd-small` and vice versa; selecting `oisd-nsfw` deselects `oisd-nsfw-small` and vice versa. List is kept alphabetically sorted by name after every toggle.
- **Custom GitHub Lists** section — Name input + URL input + "Add list" button (secondary). Validation on add: both fields required (else toast "List details required" / "Enter both a list name and a GitHub URL before adding it."); URL must contain `github.com` or `raw.githubusercontent.com` (else toast "GitHub URL required" / "Manual lists should point at a GitHub or raw GitHub blocklist URL."). Generated id = slugified name (`lowercase, non-alnum → "-", trim leading/trailing "-"`), falling back to `custom-{Date.now()}` if the slug is empty. Adding replaces any existing entry with the same URL, then re-sorts alphabetically.
- **Allowlist Exceptions** section — single comma-separated text input, helper text "Comma-separated domains that should stay reachable even when blocked by a selected list."
- **Active Sources** table — Name / URL (truncated) / Kind (badge) / remove button (X icon) per row, sourced from `draft.blocklists`. Empty state: "Choose at least one OISD preset or add a custom GitHub list."
- Footer buttons: **Delete** (outline, only shown when editing an existing profile, label becomes "Deleting..." while busy) and **Save Profile** (primary, label becomes "Saving..." while busy). Save validates a non-empty trimmed name client-side (toast "Name required" otherwise) before calling the context handler, which itself re-validates. Allowlist string is split on commas, trimmed, and empty entries filtered before being sent.

### 1.4 Devices tab (`devices-tab.tsx`)

Two-column layout (`1.05fr` / `0.95fr` on xl).

**Left: Add/Edit Device card**
- Title "Add Device" / "Edit Device" depending on whether a device is being edited.
- Row 1: Device name input (placeholder "Kitchen iPad"), IP Address input (placeholder "192.168.1.42").
- Row 2: **Policy mode** select — "Household default" (`global`) / "Custom assignment" (`custom`); **Profile override** select — lists all saved block profiles by name (value = profile **name**, not id — note this quirk), disabled unless policy mode is `custom`.
- Row 3: **Protection** select — "Keep blocking on" (`inherit`) / "Bypass blocking" (`bypass`), disabled unless custom; **Allowed domains** comma-separated text input (placeholder "school.site, printer.local"), disabled unless custom.
- **Service override** sub-panel (bordered box): helper text "Add a focused allow or block rule for a known service when this device needs a small exception." Three controls: service select (lists `settings.services` by manifest display name, disabled unless custom), allow/block mode select ("Allow service" / "Block service", disabled unless custom), "Add service rule" button (outline, disabled unless custom mode + a service is chosen + the pending rule isn't already a no-op vs. queued state).
  - Adding validates: must be in custom mode (toast "Custom mode required"); a service must be selected (toast "Service required"); the service manifest must resolve (toast "Unknown service"); the computed preview domain list must be non-empty (toast "Service rule unavailable" — "This service does not currently expand into any device-specific domains for the selected mode."); must not be a no-op vs. the currently-queued override for that service (toast "Service rule already queued"). On success, upserts (replacing any existing queued override for that service) into `deviceServiceOverrides`, keeps the list sorted by service id, and shows a success toast describing how many domains it expands into.
  - **Preview panel** (shown when a service is selected): display name, risk notes, badges for mode/category/domain-count, and up to 4 sample domain badges. Domain set for "allow" mode = union of `allow_domains ∪ block_domains ∪ exceptions` (dedup'd); for "block" mode = `block_domains` only.
  - **Queued overrides** shown as removable pill buttons (`"{label} - {mode} x"`), tooltip title = `"{category} - {risk_notes}"` or "Custom device service rule" if manifest missing; clicking removes that override from the queue.
- When policy mode is not custom: dashed-border note "This device will follow the household default until you switch it to a custom assignment."
- Footer: **Cancel** button (ghost, only visible while editing an existing device — resets the whole form) and **Add device** / **Save device** button (disabled unless name + IP are both non-empty and not currently busy; label becomes "Saving...").
- **Important implementation detail**: this tab calls `api.upsertDevice(...)` and `load()` **directly** (via its own local `handleDeviceSubmit`), not through the context's `handleDeviceSubmit`. Behaviorally equivalent to the context version but is a separate code path — worth unifying in the rewrite rather than literally reproducing the duplication.

**Right: Devices table card**
- Title "Devices", description "Named devices tracked by the control plane".
- Columns: Name, IP Address (monospace), Policy (badge: "Custom" default-variant / "Default" secondary-variant), Profile (`blocklist_profile_override ?? "Default"`), Protection (badge outline: "Bypass" or "Active"), Edit button (ghost, populates the left form with that device's full state including its service overrides).
- Empty state: "No devices have been named yet. Start with the devices the household will recognize fastest."

### 1.5 Grease-AI tab (`grease-ai-tab.tsx`)

**Important**: this tab is largely a **client-side simulated visualization**, not a dedicated backend ML endpoint. It derives everything from data already present in `dashboard`, `settings`, and `latencyBudget` (all fetched for other tabs) — there is no `api.grease*()` call. Treat the "Learning Pulse" numbers as illustrative/decorative in the current app, not real classifier telemetry, when deciding what the rewrite's real ML engine should expose instead.

Two-column layout (`1.05fr` / `0.95fr` on xl).

**Left: Classifier workspace card**
- "Learning Pulse" — 3 labeled progress bars with percentage labels, computed client-side:
  - "Classifier confidence" = `min(0.35 + blockedRatio * 1.8, 0.96)` where `blockedRatio = blocked_total / max(queries_total, 1)`.
  - "Risk memory" = `min(0.22 + riskyEventRatio * 0.7, 0.92)` where `riskyEventRatio = min(recent_security_events.length / 6, 1)`.
  - "Latency headroom" = `0.78` if `latencyBudget.within_budget` else `0.46` (binary, not a real continuous metric).
  - Progress bars use a `.gold-shimmer` CSS class (animated gradient sweep, see §5).
- "Classifier animation" decorative panel — a 5-row × ~13-column grid of small pill divs with per-cell opacity/animation-delay derived from the 3 signal values above (purely visual flourish, no real data encoded beyond the 3 signals). Caption: "The bars brighten as more DNS activity arrives, blocked decisions climb, and the runtime stays inside latency budget."

**Right: Stats + latency table**
- 4 small stat cards (2×2 grid): Mode (`settings.classifier.mode`), Threshold (`settings.classifier.threshold.toFixed(2)`), Queries observed (`queries_total`), Blocked queries (`blocked_total`).
- **Latency Budgets** table — columns Path / Target p50 / Observed / Samples / Status (badge: secondary if `status === "ok"`, default otherwise). Source: `latencyBudget.checks`. Empty state: "No latency budget checks available yet."

There are no interactive controls on this tab — it is read-only (mode/threshold editing lives on the Settings tab's Advanced pane).

### 1.6 Settings tab (`settings-tab.tsx`)

Nested `Tabs` component with two sub-tabs: **Everyday** and **Advanced** (defaults to Everyday).

**Everyday sub-tab:**

1. **Alert delivery card** — badge shows `"Webhook {min_severity}+"` or "Disabled". Enable switch ("Enable outbound alert notifications"). Webhook URL text input. Min severity select (Medium+ / High+ / Critical only). Footer: "Send test" button (ghost, disabled unless a webhook URL is present or not busy) which always sends with `domain: "notification-test.cogwheel.local"`, `device_name: "Control Plane Test"`, `dry_run: false` (dry-run toggle and custom test domain/device exist in local state but are **not exposed as editable UI** — they're fixed constants read from `useState` initializers that are never given a setter that's wired to any control), and "Save alerts" button (primary).
2. **Sources card** — "Add blocklist" sub-form: Name input, Source URL input (placeholder "Source URL or data: URL" — implies `data:` URLs are supported by the backend), Profile select (Custom/Essential/Balanced/Aggressive), Strictness select (Strict/Balanced/Relaxed), Refresh interval (minutes) numeric-text input (default "60"), "Add blocklist" button (disabled unless name+url present). Always sends `kind: "domains"`, `enabled: true`. Below: a table of all `settings.blocklists` — Name / Profile / Refresh ("{n}m") / Status (badge Enabled/Disabled) / a per-row Enable/Disable toggle button. Empty state: "No blocklists configured yet."
3. **Services card** — table of `settings.services` (Service / Risk (risk_notes) / Mode (secondary badge) / a row of 3 mode buttons: Inherit/Allow/Block, active one shown filled). Only first 5 shown by default with a "Show all services" ghost button to reveal the rest (`showServicesView` toggle, one-way — no "show less"). There is search-filter plumbing (`filteredServices`, `serviceSearch`) but **no search input is rendered** — `serviceSearch` is a `useState("")` with no setter wired to a control, so it's permanently empty (dead filter). Empty state: "No services configured."

**Advanced sub-tab:**

4. **Sync and replication card** — read-only summary row (Profile / Revision / Peers count). Sync profile select (Full replication / Settings only / Read-only follower) + "Save profile" button (secondary). Transport mode select (Opportunistic / HTTPS required) + Bearer token text input (placeholder differs based on whether a token is already configured: "Set new token or leave blank to clear" vs "Optional bearer token") + "Save transport" button (secondary). Note: this tab has its own **local** `handleSyncProfileSave`/`handleSyncTransportSave` that call `api.*` directly and `load()`, duplicating (not reusing) the context's equivalent handlers — same duplication pattern as the Devices tab.
5. **Tailscale card** — badge: "Exit node advertised" / "Installed" / "Not installed". Read-only summary row (Host / Tailnet / Peers). Conditional info banner (primary-tinted) showing `tailscaleDnsCheck.message` when `tailscaleDnsCheck.suggestions.length > 0`. Footer buttons: toggle exit-node filtering (label + variant flip based on current state; text while busy: "Updating...") and "Roll back" (ghost; text while busy: "Rolling back...").
6. **Classifier card** — badge = current mode. 3 mode buttons (Off/Monitor/Protect, default variant when active else secondary). Threshold text input + "Save threshold" button (secondary). (Same underlying API as Grease-AI tab's read-only display, but this is the only place mode/threshold can be **edited**.)
7. **Threat intelligence feeds card** — badge = count of enabled providers. Table: Provider / Capabilities (joined with " • ") / Feed URL (inline-editable text input, updates local `threatIntelSettings` state optimistically as you type, not yet saved) / Interval minutes (inline-editable numeric text input, same optimistic-local-only pattern) / Status (badge) / per-row "Save" button (secondary, calls `api.updateThreatIntelProvider` with that provider's *current local* enabled/feed_url/interval — note there is **no UI control to toggle `enabled` itself** in this table, only feed URL and interval are editable; enabled is only ever set to whatever it already was). Empty state: "No threat intelligence providers configured."
8. **Federated learning card** — badge = `privacy_mode` when enabled else "Disabled". Enable switch. Coordinator URL text input. Round interval (hours) numeric-text input. Footer: single "Save" button (updates enabled/coordinator_url/round_interval_hours together).
9. **Latency budgets card** — badge "Within budget" / "Needs attention" based on `latencyBudget.within_budget`. Large cache-hit-rate percentage display. Table: Path / Target p50 / Observed / Samples / Status (same shape as Grease-AI's table but this one has **no empty-state row guard** — if `checks` is empty it just renders zero rows with headers only, unlike every other table in the app). Conditional recommendations paragraph (dashed border) when `recommendations.length > 0`.
10. **Audit trail card** — this card bundles three distinct sub-features:
    - **Guided recovery** — up to 3 dynamically-generated action cards, computed by a rules engine (`recoveryActions` memo) in this priority order: (a) if `runtime_health.degraded` → "Check runtime health again" action wired to `handleRuntimeHealthCheck`; (b) if `notification_health.failed_count > 0` → "Review notification delivery" action that just switches the audit filter to "notifications" (no API call); (c) if `!active_ruleset` → "Refresh sources now" wired to `handleRefreshSources`; (d) if `active_ruleset && runtime_health.degraded` → "Roll back to the previous ruleset" wired to `handleRollbackRuleset`; if none of the above triggered, a single fallback "System looks steady" card wired to `handleRefreshSources`. Each action shows title, one-line detail, and an action button whose label/disabled state reflects the matching `busyAction`. (Each action also carries a `steps: string[]` array of 3 more-detailed steps that is **computed but never rendered** in the current UI — dead data worth surfacing as an expandable detail in the rewrite, or dropping.)
    - **Audit event filter** — 5 pill buttons: All events / Runtime / Notifications / Devices / Rulesets. Filtering logic: `runtime` → `event_type` startsWith `"runtime."`; `notifications` → startsWith `"notification."` **or** `"security.alert"`; `devices` → startsWith `"device."`; `rulesets` → startsWith `"ruleset."`.
    - **Audit events table** — columns Event / Detail / Type (raw `event_type`, small muted text) / Category (outline badge, `event_type.split(".")[0]`). Shows first 8 of the filtered list. Each row is produced by `summarizeAuditEvent(event)` which special-cases these `event_type` values (falling back to a generic `"{firstPayloadKey}: {firstPayloadValue}"` summary otherwise, or "No structured payload details recorded." if the payload is empty/unparseable):
      - `ruleset.rollback` → "Ruleset rollback completed" / `"Recovered ruleset {hash.slice(0,12)} after an operator-triggered rollback."`
      - `ruleset.auto_rollback` → "Automatic rollback triggered" / first item of `payload.notes` or fallback text.
      - `ruleset.refresh_rejected` → "Ruleset refresh rejected" / first item of `payload.notes` or fallback text.
      - any `notification.delivery_*` or `security.alert_delivery_*` → title = `payload.title ?? payload.domain ?? "Notification delivery"`; detail = `payload.summary ?? "{severity} delivery to {client_ip or device_name or "control-plane"}."`.
      - any `runtime.health_check_*` → title depends on whether the event type ends with "degraded"; detail = first `payload.notes` item or fallback.
      - `device.upserted` → `"Updated device {name}"` / `"Policy mode {policy_mode} for {ip_address}."`
      - `parseAuditPayload` safely JSON-parses `event.payload` (a string), returning `{}` on failure or non-object JSON. `stringifyAuditValue` recursively unwraps arrays/objects to produce a short human string for the generic fallback case.
      - Empty state: "No audit events match the current filter."

---

## 2. API client surface — `src/lib/api.ts` (verbatim contract)

Base URL resolution: `const API_BASE = import.meta.env.VITE_COGWHEEL_API_BASE ?? (typeof window !== "undefined" ? window.location.origin : "http://127.0.0.1:8080")`.

`fetchJson<T>(path, init?)`: does a `fetch(`${API_BASE}${path}`, {...init, headers: {"Content-Type": "application/json", "X-Requested-With": "XMLHttpRequest", ...extraHeaders}})`. On non-`ok` response, reads response body as text, trims it, and throws `new Error(detail || `${status} ${statusText}`)`. On success, expects the JSON body to be `{ data: T }` and returns `payload.data` (i.e. **every** endpoint is expected to be wrapped in a top-level `data` envelope — the new UI must preserve or explicitly renegotiate this envelope convention with the backend).

### 2.1 All TypeScript types (verbatim)

```ts
export type RuntimeHealth = {
  snapshot: {
    upstream_failures_total: number;
    fallback_served_total: number;
    cache_hits_total: number;
    cname_uncloaks_total: number;
    cname_blocks_total: number;
    queries_total: number;
    blocked_total: number;
  };
  degraded: boolean;
  notes: string[];
};

export type RulesetSummary = {
  id: string;
  hash: string;
  status: string;
  created_at: string;
};

export type AuditEvent = {
  id: string;
  event_type: string;
  payload: string;
  created_at: string;
};

export type SourceRecord = {
  id: string;
  name: string;
  url: string;
  kind: string;
  enabled: boolean;
  refresh_interval_minutes: number;
  profile: string;
  verification_strictness: string;
};

export type BlocklistStatus = {
  id: string;
  name: string;
  last_refresh_attempt_at: string | null;
  due_for_refresh: boolean;
};

export type ServiceToggle = {
  manifest: {
    service_id: string;
    display_name: string;
    category: string;
    risk_notes: string;
    allow_domains: string[];
    block_domains: string[];
    exceptions: string[];
  };
  mode: "Inherit" | "Allow" | "Block";
};

export type DeviceRecord = {
  id: string;
  name: string;
  ip_address: string;
  policy_mode: "global" | "custom";
  blocklist_profile_override: string | null;
  protection_override: "inherit" | "bypass";
  allowed_domains: string[];
  service_overrides: DeviceServiceOverride[];
};

export type BlockProfileRecord = {
  id: string;
  emoji: string;
  name: string;
  description: string;
  blocklists: BlockProfileListRecord[];
  allowlists: string[];
  updated_at: string;
};

export type BlockProfileListRecord = {
  id: string;
  name: string;
  url: string;
  kind: string;
  family: string;
};

export type DeviceServiceOverride = {
  service_id: string;
  mode: "allow" | "block";
};

export type SecurityEventRecord = {
  id: string;
  device_id: string | null;
  device_name: string | null;
  client_ip: string;
  domain: string;
  classifier_score: number;
  severity: string;
  created_at: string;
};

export type DeviceSecuritySummary = {
  label: string;
  event_count: number;
  highest_severity: string;
};

export type SecuritySummary = {
  medium_count: number;
  high_count: number;
  critical_count: number;
  top_devices: DeviceSecuritySummary[];
};

export type DomainInsightEntry = {
  domain: string;
  count: number;
};

export type DomainInsights = {
  top_queried_domains: DomainInsightEntry[];
  top_blocked_domains: DomainInsightEntry[];
  observed_queries: number;
};

export type NotificationSettings = {
  enabled: boolean;
  webhook_url: string | null;
  min_severity: "medium" | "high" | "critical";
};

export type NotificationDeliveryEvent = {
  status: string;
  event_type: string;
  severity: string;
  title: string;
  summary: string;
  target: string;
  domain: string;
  device_name: string | null;
  client_ip: string;
  attempts: number;
  created_at: string;
};

export type NotificationHealthSummary = {
  delivered_count: number;
  failed_count: number;
  last_delivery_at: string | null;
  last_failure_at: string | null;
};

export type NotificationFailureDomain = {
  domain: string;
  failure_count: number;
};

export type NotificationFailureAnalytics = {
  success_rate_percent: number;
  top_failed_domains: NotificationFailureDomain[];
};

export type NotificationTestResult = {
  outcome: string;
  target: string;
};

export type NotificationTestRequest = {
  domain?: string;
  severity?: NotificationSettings["min_severity"];
  device_name?: string;
  dry_run?: boolean;
};

export type NotificationTestPreset = {
  name: string;
  domain: string;
  severity: NotificationSettings["min_severity"];
  device_name: string;
  dry_run: boolean;
};

export type DashboardSummary = {
  protection_status: string;
  protection_paused_until: string | null;
  active_ruleset: RulesetSummary | null;
  source_count: number;
  enabled_source_count: number;
  service_toggle_count: number;
  device_count: number;
  runtime_health: RuntimeHealth;
  latest_audit_events: AuditEvent[];
  recent_security_events: SecurityEventRecord[];
  recent_notification_deliveries: NotificationDeliveryEvent[];
  notification_health: NotificationHealthSummary;
  notification_failure_analytics: NotificationFailureAnalytics;
  security_summary: SecuritySummary;
  domain_insights: DomainInsights;
};

export type SyncPeerStatus = {
  node_public_key: string;
  imports: number;
  last_import_at: string;
  last_revision: number;
  profile: string;
};

export type SyncNodeStatus = {
  local_node_public_key: string;
  profile: string;
  revision: number;
  transport_mode: string;
  transport_token_configured: boolean;
  replay_cache_entries: number;
  peers: SyncPeerStatus[];
};

export type TailscaleStatus = {
  installed: boolean;
  daemon_running: boolean;
  backend_state: string | null;
  hostname: string | null;
  tailnet_name: string | null;
  peer_count: number;
  exit_node_active: boolean;
  version: string | null;
  health_warnings: string[];
  last_error: string | null;
};

export type TailscaleExitNodeResult = {
  success: boolean;
  message: string;
};

export type TailscaleRollbackResult = {
  success: boolean;
  message: string;
  previous_state: boolean | null;
};

export type TailscaleDnsCheckResult = {
  configured: boolean;
  message: string;
  local_dns_server: string | null;
  suggestions: string[];
};

export type LoadTestResult = {
  success: boolean;
  queries_sent: number;
  queries_succeeded: number;
  queries_failed: number;
  avg_latency_ms: number;
  p95_latency_ms: number;
  p99_latency_ms: number;
  throughput_qps: number;
  errors: string[];
};

export type FalsePositiveBudgetStatus = {
  release_ready: boolean;
  blocking_rate: number;
  blocked_total: number;
  queries_total: number;
  false_positive_estimate: number;
  budget_remaining: number;
  budget_limit: number;
  recommendations: string[];
};

export type LatencyBudgetCheck = {
  label: string;
  observed_ms: number;
  target_p50_ms: number;
  sample_count: number;
  status: string;
};

export type LatencyBudgetStatus = {
  within_budget: boolean;
  cache_hit_rate: number;
  checks: LatencyBudgetCheck[];
  recommendations: string[];
};

export type ConfigVersionStatus = {
  schema_version: number;
  config_version: number;
  cogwheel_version: string;
  migration_count: number;
  upgrade_available: boolean;
  recommendations: string[];
};

export type ThreatIntelProviderConfig = {
  id: string;
  display_name: string;
  enabled: boolean;
  feed_url: string | null;
  api_key_configured: boolean;
  update_interval_minutes: number;
  last_sync_at: string | null;
  last_error: string | null;
  capabilities: string[];
};

export type ThreatIntelSettings = {
  providers: ThreatIntelProviderConfig[];
  recommendations: string[];
};

export type FederatedLearningSettings = {
  enabled: boolean;
  coordinator_url: string | null;
  node_id: string;
  round_interval_hours: number;
  last_round_at: string | null;
  last_model_version: string | null;
  privacy_mode: string;
  raw_log_export_enabled: boolean;
  recommendations: string[];
};

export type SyncProfileView = {
  profile: string;
};

export type SyncTransportView = {
  mode: string;
  token_configured: boolean;
};

export type SettingsSummary = {
  blocklists: SourceRecord[];
  blocklist_statuses: BlocklistStatus[];
  block_profiles: BlockProfileRecord[];
  devices: DeviceRecord[];
  services: ServiceToggle[];
  classifier: {
    mode: "Off" | "Monitor" | "Protect";
    threshold: number;
  };
  notifications: NotificationSettings;
  notification_test_presets: NotificationTestPreset[];
  runtime_guard: {
    probe_domains: string[];
    max_upstream_failures_delta: number;
    max_fallback_served_delta: number;
  };
};

export type ResolverAccessStatus = {
  hostname: string | null;
  dns_targets: string[];
  tailscale_ip: string | null;
  notes: string[];
};
```

### 2.2 `api` object — every function, method, endpoint, request/response

| Function | HTTP | Path | Request body | Response type |
|---|---|---|---|---|
| `dashboard(notificationWindow?, notificationHistoryWindow?)` | GET | `/api/v1/dashboard` (+ optional `?notification_window=N&notification_history_window=N`) | — | `DashboardSummary` |
| `settings()` | GET | `/api/v1/settings` | — | `SettingsSummary` |
| `syncStatus()` | GET | `/api/v1/sync/status` | — | `SyncNodeStatus` |
| `syncProfile()` | GET | `/api/v1/sync/profile` | — | `SyncProfileView` |
| `updateSyncProfile(profile)` | POST | `/api/v1/sync/profile` | `{ profile }` | `SyncProfileView` |
| `syncTransport()` | GET | `/api/v1/sync/transport` | — | `SyncTransportView` |
| `updateSyncTransport(mode, token?)` | POST | `/api/v1/sync/transport` | `{ mode, token }` | `SyncTransportView` |
| `refreshSources()` | POST | `/api/v1/sources/refresh` | — | `{ outcome: string; notes: string[] }` |
| `rollbackRuleset()` | POST | `/api/v1/rulesets/rollback` | — | `{ id: string; hash: string; status: string; created_at: string }` |
| `runtimeHealthCheck()` | POST | `/api/v1/runtime/health/check` | — | `RuntimeHealth` |
| `pauseRuntime(minutes)` | POST | `/api/v1/runtime/pause` | `{ minutes }` | `void` |
| `resumeRuntime()` | POST | `/api/v1/runtime/resume` | — | `void` |
| `updateClassifier(mode, threshold)` | POST | `/api/v1/settings/classifier` | `{ mode, threshold }` | `SettingsSummary["classifier"]` |
| `updateNotifications(input)` | POST | `/api/v1/settings/notifications` | `NotificationSettings` | `NotificationSettings` |
| `testNotifications(input?)` | POST | `/api/v1/settings/notifications/test` | `NotificationTestRequest` (or `{}`) | `NotificationTestResult` |
| `updateNotificationTestPresets(presets)` | POST | `/api/v1/settings/notifications/presets` | `{ presets: NotificationTestPreset[] }` | `NotificationTestPreset[]` |
| `upsertBlocklist(input)` | POST | `/api/v1/settings/blocklists` | `Partial<SourceRecord> & { name; url; kind }` merged with `{ refresh_now: true }` | `{ outcome: string; notes: string[] }` |
| `upsertBlockProfile(input)` | POST | `/api/v1/settings/block-profiles` | `{ id?; emoji; name; description?; blocklists: BlockProfileListRecord[]; allowlists: string[] }` | `BlockProfileRecord[]` (full updated list) |
| `deleteBlockProfile(id)` | POST | `/api/v1/settings/block-profiles/delete` | `{ id }` | `BlockProfileRecord[]` (full updated list) |
| `setBlocklistEnabled(id, enabled)` | POST | `/api/v1/settings/blocklists/state` | `{ id, enabled, refresh_now: true }` | `{ outcome: string; notes: string[] }` |
| `deleteBlocklist(id)` | POST | `/api/v1/settings/blocklists/delete` | `{ id, refresh_now: true }` | `{ outcome: string; notes: string[] }` |
| `updateService(service_id, mode)` | POST | `/api/v1/services/toggles` | `{ service_id, mode }` | `{ outcome: string; notes: string[] }` |
| `upsertDevice(input)` | POST | `/api/v1/devices` | `{ id?; name; ip_address; policy_mode?; blocklist_profile_override?; protection_override?; allowed_domains?; service_overrides? }` | `DeviceRecord` |
| `securityEvents()` | GET | `/api/v1/security-events` | — | `SecurityEventRecord[]` |
| `tailscaleStatus()` | GET | `/api/v1/tailscale/status` | — | `TailscaleStatus` |
| `tailscaleExitNode(enabled)` | POST | `/api/v1/tailscale/exit-node` | `{ enabled }` | `TailscaleExitNodeResult` |
| `tailscaleRollback()` | POST | `/api/v1/tailscale/rollback` | — | `TailscaleRollbackResult` |
| `tailscaleDnsCheck()` | GET | `/api/v1/tailscale/dns-check` | — | `TailscaleDnsCheckResult` |
| `resolverAccess()` | GET | `/api/v1/resolver-access` | — | `ResolverAccessStatus` |
| `falsePositiveBudget()` | GET | `/api/v1/false-positive-budget` | — | `FalsePositiveBudgetStatus` |
| `latencyBudget()` | GET | `/api/v1/latency-budget` | — | `LatencyBudgetStatus` |
| `runLoadTest(duration_secs, qps, cache_hit_ratio)` | POST | `/api/v1/load-test` | `{ duration_secs, qps, cache_hit_ratio }` | `LoadTestResult` |
| `configVersion()` | GET | `/api/v1/config/version` | — | `ConfigVersionStatus` |
| `threatIntelProviders()` | GET | `/api/v1/threat-intel/providers` | — | `ThreatIntelSettings` |
| `updateThreatIntelProvider(id, enabled, feed_url, update_interval_minutes)` | POST | `/api/v1/threat-intel/providers` | `{ id, enabled, feed_url, update_interval_minutes }` | `ThreatIntelSettings` |
| `federatedLearningStatus()` | GET | `/api/v1/federated-learning/status` | — | `FederatedLearningSettings` |
| `updateFederatedLearningStatus(enabled, coordinator_url, round_interval_hours)` | POST | `/api/v1/federated-learning/status` | `{ enabled, coordinator_url, round_interval_hours }` | `FederatedLearningSettings` |

**Endpoints defined in `api.ts` but never called from any UI component** (dead client bindings — confirm with the backend spec (`docs/architecture/01-backend-api.md`) whether these still need surfacing in the new UI, since they represent real backend capability the old UI simply never exposed):
- `syncProfile()` (GET `/api/v1/sync/profile`) — settings tab reads sync state from `syncStatus()` / the `SyncNodeStatus` object instead.
- `updateNotificationTestPresets(presets)` — `SettingsSummary.notification_test_presets` is fetched and held in state but never rendered or edited anywhere.
- `deleteBlocklist(id)` — there is no "delete blocklist" button anywhere; only enable/disable exists (Sources card only ever calls `setBlocklistEnabled`).
- `securityEvents()` — the dedicated `/api/v1/security-events` list endpoint is never called; the Overview tab instead uses `dashboard.recent_security_events` (a bounded subset embedded in the dashboard payload).
- `syncTransport()` (GET) — read via `syncStatus()` fields instead (`transport_mode`, `transport_token_configured`); the standalone GET is unused, only the POST `updateSyncTransport` is used.
- `falsePositiveBudget()` — fetched nowhere, no card/section exists for it.
- `runLoadTest()` — fetched nowhere, no load-test UI exists.
- `configVersion()` — fetched nowhere, no version/upgrade UI exists.

---

## 3. `src/contexts/cogwheel-context.tsx` — state, polling, refresh semantics

### 3.1 What it holds (`CogwheelContextValue`)

Data state: `dashboard: DashboardSummary`, `settings: SettingsSummary`, `syncStatus: SyncNodeStatus`, `tailscaleStatus: TailscaleStatus`, `tailscaleDnsCheck: TailscaleDnsCheckResult`, `threatIntelSettings: ThreatIntelSettings`, `federatedLearningSettings: FederatedLearningSettings`, `latencyBudget: LatencyBudgetStatus`, `resolverAccess: ResolverAccessStatus`. All initialized from the "empty" defaults in `src/lib/constants.ts` (see §4) so the UI never has to null-check.

UI state: `state: "idle" | "loading" | "ready" | "error"`, `error: string | null`, `busyAction: string | null` (+ its setter `setBusyAction`, exposed directly so individual tabs can drive their own busy indicators for actions the context doesn't wrap).

Setters exposed for local/optimistic mutation from child components: `setSettings`, `setThreatIntelSettings`, `setFederatedLearningSettings` (used by the Settings tab's inline-editable threat-intel table and federated-learning form, and by Profiles tab's optimistic `block_profiles` update after save/delete).

`pushToast(title, detail, tone)` — re-exported from `src/hooks/use-toast.ts`.

### 3.2 Data loading: `load()` vs `refreshLiveData()`

**`load()`** — the "full" loader. Sets `state = "loading"`, clears `error`, then `Promise.all`s **9** endpoints: `dashboard(30, 10)`, `settings()`, `syncStatus()`, `tailscaleStatus()`, `tailscaleDnsCheck()`, `threatIntelProviders()`, `federatedLearningStatus()`, `latencyBudget()`, `resolverAccess()`. On success: writes every result to both React state and `localStorage` (see §3.4), sets `state = "ready"`. On failure: attempts an **offline fallback** from `localStorage` (see §3.4); if that fully succeeds, sets `state = "ready"` and pushes an info toast "Working offline" / "Showing cached data while the server is unreachable."; if cache is missing/partial or fails to parse, sets `error` to the caught error's message (or "Unknown error") and still sets `state = "ready"` (not `"error"` — note: `state` only ever becomes `"error"` conceptually via `refreshLiveData`'s check, never directly inside `load()`; `state === "error"` is actually never set anywhere in this file as written — it's declared in the `LoadState` union and read by the sidebar/status-bar UI, but nothing assigns it. This is a latent gap: the app currently has no code path that produces `state === "error"`, so the "Offline" sidebar/status-bar label is effectively dead code today. The rewrite should decide deliberately whether "error" is a real terminal UI state or should be removed.). Called once on mount via a `useEffect`.

**`refreshLiveData()`** — the lightweight poller, called every 5 seconds (see §3.3) and by the header's manual "Refresh" button. `Promise.all`s a **smaller** set of **6** endpoints (intentionally excludes `settings()`, `threatIntelProviders()`, `federatedLearningStatus()` — those are assumed to change rarely and are only refreshed by a full `load()`): `dashboard(30, 10)`, `syncStatus()`, `tailscaleStatus()`, `tailscaleDnsCheck()`, `latencyBudget()`, `resolverAccess()`. On success: writes to state + localStorage for just those 6, clears `error`, sets `state = "ready"`. On failure: only sets `error` (message or "Unknown error") **if `state === "ready"`** at the time of failure (i.e. transient poll failures while still in the initial `"loading"` state are silently swallowed) — again never sets `state = "error"`.

`notificationAnalyticsWindow = 30` and `notificationHistoryWindow = 10` are hardcoded constants (comment notes they could become user-editable state later, but currently aren't).

### 3.3 Polling mechanics

`REFRESH_INTERVAL_MS = 5_000` (5 seconds). A `useEffect` gated on `state !== "ready"` (i.e. only starts once the first `load()` has completed) sets up:
- `window.setInterval(refreshIfVisible, 5000)`
- a `window` `"focus"` listener calling `refreshIfVisible`
- a `document` `"visibilitychange"` listener calling `refreshIfVisible`

`refreshIfVisible()` only actually calls `refreshLiveData()` when `document.visibilityState === "visible"` — so backgrounded/hidden tabs stop polling (interval keeps firing every 5s but each firing is a no-op check) and immediately catch up on refocus. All three listeners are cleaned up on unmount / when `state` changes away from ready.

### 3.4 localStorage cache / offline fallback

Cache keys (from `src/lib/constants.ts` `CACHE_KEYS`, all string literals): `cogwheel_dashboard_cache`, `cogwheel_settings_cache`, `cogwheel_sync_status_cache`, `cogwheel_tailscale_cache`, `cogwheel_tailscale_dns_cache`, `cogwheel_threat_intel_cache`, `cogwheel_federated_learning_cache`, `cogwheel_latency_budget_cache`, `cogwheel_resolver_access_cache`. Every successful `load()` or `refreshLiveData()` writes `JSON.stringify(result)` to the relevant key(s) (refreshLiveData only touches the 6 keys it fetches). Only `load()`'s catch block reads them back, and only as an **all-or-nothing** fallback — all 9 keys must be present or the fallback path is skipped entirely and the real error is surfaced instead.

### 3.5 Mutation handlers (all follow the same shape)

Every handler: `setBusyAction("<key>")` → `try { await api.X(...); pushToast(<success title>, <success detail>, "success"); await load(); } catch (err) { pushToast(<failure title>, err.message ?? "Unknown error", "error"); } finally { setBusyAction(null); }`. Every successful mutation triggers a full `load()` (not a targeted patch), so **every mutation implicitly refetches all 9 endpoints**. Busy-action keys used (string literals, must be reproduced exactly if any component logic keys off them): `pause-runtime`, `resume-runtime`, `refresh-sources`, `rollback-ruleset`, `runtime-health-check`, `classifier-mode-{mode}` (templated per mode button), `classifier-threshold`, `notifications-save`, `notifications-test`, `sync-profile-save`, `sync-transport-save`, `tailscale-exit-node`, `tailscale-rollback`, `threat-intel-{providerId}` (templated), `federated-learning-save`, `create-blocklist`, `blocklist-toggle-{id}` (templated), `service-{serviceId}` (templated), `block-profile-save`, `block-profile-delete`, `device-submit`.

Full list of context-exposed mutation handlers and their one-line business rules:
- `handlePauseRuntime(minutes)` — success toast: `"Protection paused"` / `"Adblocking and classification paused for {minutes} minutes."`
- `handleResumeRuntime()` — `"Protection resumed"` / `"Adblocking and classification are active again."`
- `handleRefreshSources()` — `"Sources refreshed"` / `result.notes[0]` (first note from the API response, may be `undefined`)
- `handleRollbackRuleset()` — `"Rollback completed"` / `"Restored ruleset {hash.slice(0,12)}."`
- `handleRuntimeHealthCheck()` — tone/title flip on `report.degraded`: `"Runtime degraded"` (error tone) vs `"Runtime healthy"` (success tone); detail = `report.notes[0] ?? "Runtime guard probes completed without regressions."`
- `handleClassifierUpdate(mode, thresholdStr)` — parses `thresholdStr` with `Number.parseFloat(...) || settings.classifier.threshold` (falls back to current threshold if parse fails/NaN/0); `"Classifier updated"` / `"Mode switched to {mode}."`
- `handleClassifierThresholdSave(thresholdStr)` — same parse fallback; `"Threshold saved"` / `"Classifier threshold is now {threshold.toFixed(2)}."`
- `handleNotificationSave(input)` — `"Notifications updated"` / `"Webhook delivery is configured."` or `"...disabled."` depending on `input.enabled`
- `handleNotificationTest(request?)` — title/detail flip on `request?.dry_run ?? false`: dry run → `"Webhook validated"` / `"Validated {target} without sending a live request."`; live → `"Test notification sent"` / `"Delivered to {target} and added to recent history."`
- `handleSyncProfileSave(profile)` — `"Sync profile updated"` / `"Node sync profile is now {profile}."`
- `handleSyncTransportSave(mode, token)` — `"Sync transport updated"` / `"Transport mode is now {mode}."`
- `handleTailscaleExitNodeToggle()` — computes `newState = !tailscaleStatus.exit_node_active`, calls `api.tailscaleExitNode(newState)`; title flips on `newState`: `"Exit node enabled"` / `"Exit node disabled"`; detail = `result.message`.
- `handleTailscaleRollback()` — `"Exit node rolled back"` / `result.message`
- `handleThreatIntelProviderSave(providerId)` — looks up the provider from **current context `threatIntelSettings`** state (not a param), toasts `"Provider missing"` (error) if not found and returns early without an API call; otherwise saves `provider.enabled, provider.feed_url, provider.update_interval_minutes` as currently held in state, then `setThreatIntelSettings(next)` (optimistic local overwrite) in addition to the later `load()`.
- `handleFederatedLearningSave()` — reads current `federatedLearningSettings` state (enabled/coordinator_url/round_interval_hours) and saves it; `setFederatedLearningSettings(next)` optimistically; success detail depends on `next.enabled`.
- `handleBlocklistCreate(input: {name,url,profile,strictness,interval})` — always sends `kind: "domains", enabled: true`; `refresh_interval_minutes = parseInt(interval,10) || 60`; success: `"Blocklist added"` / `"The source was saved and refreshed."`
- `handleBlocklistToggle(id, enabled)` — success title flips on `enabled`: `"Blocklist enabled"`/`"Blocklist disabled"`; detail always `"Ruleset refresh requested."`
- `handleServiceUpdate(serviceId, mode)` — `"Service updated"` / `"Service mode set to {mode}."`
- `handleBlockProfileSave(draft, allowlistStr)` — client-side guard: if `!draft.name.trim()`, toasts `"Name required"` (error) and returns before setting `busyAction` at all (so this path never shows a loading state). Splits `allowlistStr` on commas, trims, filters empties. On success, `setSettings(current => ({...current, block_profiles: updatedProfiles}))` optimistically **before** the trailing `load()`. Success: `"Block profile saved"` / `"{name} is ready for device assignment."`
- `handleBlockProfileDelete(profileId, profileName)` — guard: if `!profileId`, toasts `"Profile required"` and returns (no busyAction set). Same optimistic `setSettings` pattern on success. `"Block profile deleted"` / `"{profileName} was removed."`
- `handleDeviceSubmit(input)` — when `input.policy_mode !== "custom"`, forces `blocklist_profile_override: null, protection_override: "inherit", allowed_domains: [], service_overrides: []` regardless of what was passed in (server-side-equivalent normalization done client-side). Success title flips on whether `input.id` was set: `"Device updated"` vs `"Device added"`; detail `"{name} is now tracked in the control plane."`

**Note**: the Devices tab and the Settings tab's Sync cards each maintain their **own local copies** of some of these handlers (`handleDeviceSubmit`, `handleSyncProfileSave`, `handleSyncTransportSave`) that call `api.*` and `load()` directly rather than using the context versions — functionally equivalent but duplicated logic. The rewrite should pick one pattern (context-owned mutations) and not reproduce the duplication.

---

## 4. `src/lib/constants.ts` contents (verbatim summary)

- `emptyDashboard: DashboardSummary` — all-zero/empty default, `protection_status: "Loading"`.
- `emptySettings: SettingsSummary` — empty arrays everywhere; `classifier: { mode: "Monitor", threshold: 0.92 }`; `notifications: { enabled: false, webhook_url: null, min_severity: "high" }`; `runtime_guard: { probe_domains: [], max_upstream_failures_delta: 0, max_fallback_served_delta: 0 }`.
- `emptySyncStatus: SyncNodeStatus` — `profile: "full"`, `transport_mode: "opportunistic"`, empty peers.
- `emptyTailscaleStatus`, `emptyTailscaleDnsCheck`, `emptyThreatIntelSettings`, `emptyLatencyBudget` (`within_budget: true`), `emptyResolverAccess` — all-empty/neutral defaults.
- `emptyFederatedLearningSettings` — `round_interval_hours: 24`, `privacy_mode: "model-updates-only"`, `raw_log_export_enabled: false`.
- `emptyBlockProfileDraft: BlockProfileRecord` — blank profile with `updated_at: new Date(0).toISOString()` (epoch).
- `oisdProfileOptions: BlockProfileListRecord[]` — exactly 4 hardcoded preset entries:
  1. `{ id: "oisd-small", name: "OISD Small", url: "https://small.oisd.nl", kind: "preset", family: "core-small" }`
  2. `{ id: "oisd-big", name: "OISD Big", url: "https://big.oisd.nl", kind: "preset", family: "core-full" }`
  3. `{ id: "oisd-nsfw-small", name: "OISD NSFW Small", url: "https://nsfw-small.oisd.nl", kind: "preset", family: "nsfw-small" }`
  4. `{ id: "oisd-nsfw", name: "OISD NSFW", url: "https://nsfw.oisd.nl", kind: "preset", family: "nsfw-full" }`
- `CACHE_KEYS` — the 9 localStorage key literals documented in §3.4.

---

## 5. Build config facts

- **Vite** (`vite.config.ts`): plugin = `@vitejs/plugin-react`. Alias `"@"` → `./src`. Dev server `port: 5174`. **No `server.proxy` configured** — the dev server does not proxy `/api/*` to the Rust backend; instead the API base resolves via `VITE_COGWHEEL_API_BASE` env var or falls back to `window.location.origin` (so in dev, without setting that env var, API calls go to `http://localhost:5174/api/...` and will 404 unless the backend is actually reachable at that origin or the env var is set — worth fixing/documenting explicitly in the rewrite, e.g. either add a dev proxy or require `VITE_COGWHEEL_API_BASE`). **No `base` path override** (defaults to `/`). **No custom `build.outDir`** — defaults to Vite's standard `dist/`, which is exactly where the Rust server (`apps/cogwheel-server/src/main.rs`, `resolve_web_dist_dir()`) looks for bundled assets: it tries `<cwd>/apps/cogwheel-web/dist` then `<cwd>/dist`, serving `index.html` as the SPA fallback via `ServeDir`/`ServeFile` (tower-http). The repo's `Dockerfile` builds the web app in a separate stage (`npm ci && npm run build` in `apps/cogwheel-web`) and copies `dist/` to `/app/web` in the final image — **any rewrite must keep producing a `dist/` folder with a self-contained `index.html` + assets, servable as static files with SPA fallback to `index.html` for unmatched paths** (though today there are no client routes, so SPA fallback is only a forward-looking concern if the rewrite adds router-based URLs).
- **package.json scripts**: `dev` = `vite`; `build` = `tsc --noEmit -p tsconfig.app.json && vite build` (type-checks before bundling, does not emit `.js` from `tsc` — Vite/esbuild does the actual transpile); `preview` = `vite preview`; `lint` = `eslint .`; `check` = `npm run build` (alias).
- **Tailwind**: v3 (`"tailwindcss": "^3.4.17"`, config file format `tailwind.config.ts` — this is the **old** Tailwind v3 JS-config style, not v4's CSS-first `@theme` approach). `darkMode: ["class"]` (class-strategy, not media-query). Content globs: `./index.html`, `./src/**/*.{ts,tsx}`. PostCSS plugins: `tailwindcss` + `autoprefixer` (`postcss.config.js`).
- **shadcn/ui config** (`components.json`): style `"default"`, not RSC, TSX, base color `"slate"`, CSS variables enabled, no class prefix, aliases `components → @/components`, `utils → @/lib/utils`.
- **Fonts** (loaded via Google Fonts `<link>` tags in `index.html`, not self-hosted/bundled): `DM Sans` (weights 400/500/600) as `font-sans` (body default), `Instrument Serif` as `font-display` (used for the "Cogwheel" wordmark in the sidebar header), `JetBrains Mono` (weights 400/500) as `font-mono` (status bar, hashes, numeric/code-like values). Tailwind `fontFamily` extension: `sans: ['DM Sans','system-ui','sans-serif']`, `display: ['Instrument Serif','Georgia','serif']`, `mono: ['JetBrains Mono','monospace']`.
- **Theme mechanism**: **not** `next-themes` despite it being listed in `package.json` dependencies (`^0.4.6`) — grep confirms it is never imported anywhere in `src/**`, so it's a dead dependency. The actual mechanism is hand-rolled in `src/components/theme-toggle.tsx`: reads `document.documentElement.classList.contains("dark")` for initial state, then on mount checks `localStorage.getItem("cogwheel-theme")` and applies `"dark"` class + state if it equals `"dark"` (note: if the saved value is `"light"` or absent, no `useEffect` action is taken — light is simply the default no-class state). Toggling calls `document.documentElement.classList.toggle("dark", next)` directly and persists to the **same** `"cogwheel-theme"` localStorage key (distinct from the 9 `CACHE_KEYS` cache keys). There is **no system-preference (`prefers-color-scheme`) detection at all** in the current app — first paint is always light unless a previous explicit toggle was persisted, and there is a flash-of-unstyled-theme risk since the theme class is applied in a `useEffect` (post-mount) rather than a blocking inline script in `index.html`.
- **CSS variables** (`src/index.css`, HSL triplets consumed as `hsl(var(--x))` by Tailwind, described as "Chrysopeia Design System - adapted from OKLch to HSL for Tailwind v3"):
  - Light (`:root`): `--background: 40 5% 96%`, `--foreground: 40 10% 10%`, `--card: 40 5% 99%` / `--card-foreground: 40 10% 10%`, `--popover` same as card, `--primary: 42 80% 48%` (warm gold) / `--primary-foreground: 40 5% 99%`, `--secondary: 40 3% 91%` / `--secondary-foreground: 40 5% 25%`, `--muted: 40 3% 93%` / `--muted-foreground: 40 3% 45%`, `--accent: 40 4% 94%` / `--accent-foreground: 40 10% 10%`, `--destructive: 0 70% 45%` / `--destructive-foreground: 0 0% 100%`, `--border: 40 4% 89%`, `--input: 40 3% 90%`, `--ring: 42 80% 48%`, chart colors `--chart-1..5` = `42 80% 48% / 160 50% 45% / 210 60% 55% / 280 45% 55% / 340 55% 50%`, sidebar tokens mirror background/foreground/primary/accent/border/ring, `--sidebar-width: 16rem`, `--sidebar-width-icon: 3rem`, `--radius: 0.5rem`.
  - Dark (`.dark`): `--background: 40 5% 7%`, `--foreground: 40 3% 91%`, `--card: 40 6% 9%`, `--primary: 42 65% 55%`, `--secondary: 40 3% 12%`, `--muted: 40 3% 12%`, `--accent: 40 5% 14%`, `--destructive: 0 65% 50%`, `--border: 40 5% 14%`, `--input: 40 3% 14%`, `--ring: 42 65% 55%`, analogous chart/sidebar token overrides (slightly desaturated versions of the light values).
  - Border radius scale derived from `--radius` via `calc()`: `sm = 0.6x`, `md = 0.8x`, `lg = 1x`, `xl = 1.4x`, `2xl = 1.8x`, `3xl = 2.2x`, `4xl = 2.6x`, `full = 9999px`.
  - Base layer: all elements get `border-border`; `body` gets `bg-background text-foreground font-sans min-h-screen antialiased` plus two radial-gradient "atmospheric" background washes tinted with `--primary` (subtler in light, stronger in dark via a `.dark body` override); all interactive elements get a `0.15s ease` transition on color/background/border/box-shadow/opacity; `:focus-visible` gets a 2px solid `--ring` outline with 2px offset.
  - Custom scrollbar styling (6px, transparent track, `--border` thumb, `--muted-foreground` on hover) — webkit only.
  - Custom keyframes/utility classes used throughout the tab components: `.animate-fade-up` (translateY(12px)→0 + fade, 0.4s), `.animate-fade-in` (0.3s fade, applied to the tab-content wrapper on every tab switch in `dashboard.tsx`), `.stagger-1` through `.stagger-6` (50ms increments of `animation-delay`, used to stagger card entrances on Overview), `@keyframes status-pulse` (defined but appears unused by any class in this file — dead keyframe), `.gold-shimmer` (animated gradient sweep using `--primary` at varying opacity, applied to Grease-AI's progress bars), `[data-slot="card"]:hover` lift effect (translateY(-1px) + shadow), `.nav-active::before` (3px left accent bar — actually superseded by inline styling in `app-sidebar.tsx` which draws its own absolute-positioned span rather than using this class; also effectively dead CSS), `@keyframes scan-sweep` (defined, appears unused — dead keyframe), `[data-slot="table-row"]:hover` background tint, `[data-slot="badge"]` letter-spacing polish, and a `prefers-reduced-motion: reduce` media query that collapses all animation/transition durations to near-zero.
  - Tailwind `tailwind.config.ts` also defines its own `keyframes`/`animation` entries for `accordion-down/up`, `dialog-in/out`, `fade-in/out`, and `sheet-in/out-{right,left,top,bottom}` — these back the shadcn `Dialog` and `Sheet` primitives' open/close transitions specifically (the `Sheet` variant is actively used by the mobile sidebar; the `Dialog` variant's keyframes exist but `ui/dialog.tsx` itself is never imported anywhere in `src/**`, so those are provisioned-but-unused today).

---

## 6. Files to DELETE vs. KEEP for the clean-slate rewrite

This app is being fully replaced with a new design system ("Shark UI" per the roadmap). Recommendation: **treat effectively everything under `apps/cogwheel-web` as delete-and-rebuild**, since even the build tooling files are small enough to regenerate cleanly and will need edits anyway (new fonts, new Tailwind version/config decision, new theme mechanism, etc.). Below is the precise breakdown.

### 6.1 DELETE (all of `src/**` — every UI/behavior file listed in this document)

```
apps/cogwheel-web/src/components/app-sidebar.tsx
apps/cogwheel-web/src/components/dashboard/dashboard.tsx
apps/cogwheel-web/src/components/dashboard/devices-tab.tsx
apps/cogwheel-web/src/components/dashboard/grease-ai-tab.tsx
apps/cogwheel-web/src/components/dashboard/overview-tab.tsx
apps/cogwheel-web/src/components/dashboard/profiles-tab.tsx
apps/cogwheel-web/src/components/dashboard/settings-tab.tsx
apps/cogwheel-web/src/components/error-boundary.tsx
apps/cogwheel-web/src/components/status-bar.tsx
apps/cogwheel-web/src/components/theme-toggle.tsx
apps/cogwheel-web/src/components/ui/badge.tsx
apps/cogwheel-web/src/components/ui/button.tsx
apps/cogwheel-web/src/components/ui/card.tsx
apps/cogwheel-web/src/components/ui/dialog.tsx          (unused today — confirm not needed before dropping)
apps/cogwheel-web/src/components/ui/dropdown-menu.tsx   (unused today — confirm not needed before dropping)
apps/cogwheel-web/src/components/ui/input.tsx
apps/cogwheel-web/src/components/ui/label.tsx
apps/cogwheel-web/src/components/ui/progress.tsx
apps/cogwheel-web/src/components/ui/scroll-area.tsx
apps/cogwheel-web/src/components/ui/select.tsx
apps/cogwheel-web/src/components/ui/separator.tsx
apps/cogwheel-web/src/components/ui/sheet.tsx
apps/cogwheel-web/src/components/ui/sidebar.tsx
apps/cogwheel-web/src/components/ui/skeleton.tsx
apps/cogwheel-web/src/components/ui/sonner.tsx
apps/cogwheel-web/src/components/ui/switch.tsx
apps/cogwheel-web/src/components/ui/table.tsx
apps/cogwheel-web/src/components/ui/tabs.tsx
apps/cogwheel-web/src/components/ui/tooltip.tsx
apps/cogwheel-web/src/contexts/cogwheel-context.tsx
apps/cogwheel-web/src/hooks/use-mobile.ts
apps/cogwheel-web/src/hooks/use-toast.ts
apps/cogwheel-web/src/index.css
apps/cogwheel-web/src/lib/constants.ts
apps/cogwheel-web/src/lib/utils.ts
apps/cogwheel-web/src/main.tsx
```
Also delete/regenerate: `index.html` (Google Fonts links + title are design-specific), `tailwind.config.ts` (design tokens are being replaced wholesale), `components.json` (only meaningful if the rewrite keeps using shadcn's CLI conventions — keep the file only if the new design system is still shadcn-based; otherwise delete).

### 6.2 REWRITE-BUT-DO-NOT-BLINDLY-DELETE (contains the contract the new UI must still speak)

```
apps/cogwheel-web/src/lib/api.ts
```
This file is not a UI file — it is the **complete backend contract** documented verbatim in §2 above. The backend is not being rewritten as part of this UI swap (aside from whatever new endpoints land from the "wire classifier + QOL endpoints into server" workstream), so the new UI's data layer needs to reproduce or explicitly supersede every type and function here. Do not delete it before its contents are fully captured elsewhere (this document, §2, already does that) — once §2 is confirmed sufficient as the spec for the new data layer, this file can be deleted and rewritten too, but a downstream agent building the new API client should treat §2 as ground truth and cross-check against the live backend spec at `docs/architecture/01-backend-api.md` for any drift, since the backend workstream may add endpoints (classifier ML, QOL) that don't exist in this snapshot.

### 6.3 KEEP AS-IS (generic tooling/config, no design-system coupling)

```
apps/cogwheel-web/.gitignore
apps/cogwheel-web/vite.config.ts        -- keep, but see §5 note about missing dev proxy; consider adding one
apps/cogwheel-web/tsconfig.json          -- project-references shell, no design coupling
apps/cogwheel-web/tsconfig.app.json      -- compiler options, no design coupling
apps/cogwheel-web/tsconfig.node.json     -- compiler options, no design coupling
apps/cogwheel-web/eslint.config.js       -- generic lint config, no design coupling
apps/cogwheel-web/postcss.config.js      -- only meaningful if staying on Tailwind; keep if so
```

### 6.4 REWRITE (package manifest — dependency list needs pruning + new design-system deps)

```
apps/cogwheel-web/package.json
```
Keep the framework baseline (`react`, `react-dom`, `typescript`, `vite`, `@vitejs/plugin-react`, `@types/*`, `eslint*`) but **drop the confirmed-dead dependencies** found during this inventory unless the rewrite has an actual planned use for them:
- `react-router-dom` — never imported; only add back if the rewrite introduces real client-side routes (recommended, since today refreshing mid-tab loses your place — see §0).
- `recharts` — never imported; only add back if the rewrite adds real charts (there's a strong case for this given Overview/Grease-AI show numeric trends that would benefit from an actual chart).
- `next-themes` — never imported; either actually adopt it for theme handling (recommended, fixes the FOUC/system-preference gaps noted in §5) or drop it.
- All `@radix-ui/*`, `class-variance-authority`, `clsx`, `tailwind-merge`, `lucide-react`, `sonner`, `tailwindcss`, `autoprefixer` — these are shadcn/ui's usual stack; keep only if the new "Shark UI" design system continues to build on shadcn primitives, otherwise drop wholesale in favor of whatever the new design system specifies.

### 6.5 Deployment-facing contract that MUST be preserved regardless of rewrite internals

- `npm run build` must still produce a `dist/` directory (Vite default output) containing a self-contained, statically-servable SPA (`index.html` + hashed assets) — the Rust server at `apps/cogwheel-server/src/main.rs::resolve_web_dist_dir()` hardcodes the search paths `<cwd>/apps/cogwheel-web/dist` and `<cwd>/dist`, and the `Dockerfile` hardcodes copying `apps/cogwheel-web/dist` → `/app/web`. Changing the build output directory name/location requires updating both of those.
- `VITE_COGWHEEL_API_BASE` env var convention (or an equivalent) should be preserved or deliberately replaced — it's the only override point between "talk to same-origin backend" (production, when server serves the built SPA) and "talk to a different backend host" (local dev against a remote/different Cogwheel instance).
