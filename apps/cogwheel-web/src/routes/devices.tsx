import React from "react";
import { useSearchParams } from "react-router-dom";
import { LaptopIcon, PlusIcon, SearchIcon, XIcon } from "lucide-react";
import { api, type DeviceRecord, type DeviceServiceOverride } from "@/lib/api";
import { normaliseDeviceInput, serviceOverrideDomains, splitDomainList } from "@/lib/derive";
import { formatCount } from "@/lib/format";
import { notify } from "@/lib/toast";
import { useCogwheel } from "@/data/context";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { PageHeader, PageSections, PageShell } from "@/components/app/page";
import { SectionCard } from "@/components/app/section-card";
import { DataTable, type Column } from "@/components/app/data-table";
import { SelectField } from "@/components/app/select-field";
import { TextField } from "@/components/app/text-field";
import { FieldRow } from "@/components/app/form-field";
import { StatusPill } from "@/components/app/status-indicator";
import { NoticeBanner } from "@/components/app/states";
import { useDomainInspector } from "@/components/app/inspector-context";

type Draft = {
  id?: string;
  name: string;
  ip_address: string;
  policy_mode: DeviceRecord["policy_mode"];
  blocklist_profile_override: string;
  protection_override: DeviceRecord["protection_override"];
  allowed_domains: string;
  service_overrides: DeviceServiceOverride[];
};

const BLANK: Draft = {
  name: "",
  ip_address: "",
  policy_mode: "global",
  blocklist_profile_override: "",
  protection_override: "inherit",
  allowed_domains: "",
  service_overrides: [],
};

function toDraft(device: DeviceRecord): Draft {
  return {
    id: device.id,
    name: device.name,
    ip_address: device.ip_address,
    policy_mode: device.policy_mode,
    blocklist_profile_override: device.blocklist_profile_override ?? "",
    protection_override: device.protection_override,
    allowed_domains: device.allowed_domains.join(", "),
    service_overrides: device.service_overrides,
  };
}

export function DevicesScreen() {
  const { data, phase, error, busy, mutate, reload } = useCogwheel();
  const { inspect } = useDomainInspector();
  const [params, setParams] = useSearchParams();
  const [draft, setDraft] = React.useState<Draft>(BLANK);
  const [search, setSearch] = React.useState("");
  const [pendingService, setPendingService] = React.useState("");
  const [pendingMode, setPendingMode] = React.useState<"allow" | "block">("allow");

  const devices = data.settings.devices;
  const selectedId = params.get("device");

  // The URL owns which device is being edited, so a link can drop someone
  // straight into the right form.
  React.useEffect(() => {
    if (!selectedId) return;
    const match = devices.find((device) => device.id === selectedId);
    if (match) setDraft(toDraft(match));
  }, [devices, selectedId]);

  const select = (device: DeviceRecord | null) => {
    setParams(
      (current) => {
        const next = new URLSearchParams(current);
        if (device) next.set("device", device.id);
        else next.delete("device");
        return next;
      },
      { replace: true },
    );
    setDraft(device ? toDraft(device) : BLANK);
  };

  const custom = draft.policy_mode === "custom";
  const manifest = data.settings.services.find(
    (service) => service.manifest.service_id === pendingService,
  )?.manifest;
  const previewDomains = manifest ? serviceOverrideDomains(manifest, pendingMode) : [];

  const filtered = React.useMemo(() => {
    const needle = search.trim().toLowerCase();
    if (!needle) return devices;
    return devices.filter(
      (device) =>
        device.name.toLowerCase().includes(needle) || device.ip_address.toLowerCase().includes(needle),
    );
  }, [devices, search]);

  const addServiceOverride = () => {
    if (!custom) {
      notify.error("Custom mode required", "Switch the device to a custom assignment first.");
      return;
    }
    if (!pendingService) {
      notify.error("Service required", "Choose a service before adding a rule.");
      return;
    }
    if (!manifest) {
      notify.error("Unknown service", "That service is no longer offered by the control plane.");
      return;
    }
    if (previewDomains.length === 0) {
      notify.error(
        "Service rule unavailable",
        `${manifest.display_name} does not expand into any device-specific domains in ${pendingMode} mode.`,
      );
      return;
    }
    const existing = draft.service_overrides.find((rule) => rule.service_id === pendingService);
    if (existing?.mode === pendingMode) {
      notify.warning("Service rule already queued", `${manifest.display_name} is already set to ${pendingMode}.`);
      return;
    }

    setDraft((current) => ({
      ...current,
      service_overrides: [
        ...current.service_overrides.filter((rule) => rule.service_id !== pendingService),
        { service_id: pendingService, mode: pendingMode },
      ].sort((left, right) => left.service_id.localeCompare(right.service_id)),
    }));
    notify.success(
      "Service rule queued",
      `${manifest.display_name} expands into ${formatCount(previewDomains.length)} domain(s). Save the device to apply it.`,
    );
  };

  const save = async () => {
    if (!draft.name.trim() || !draft.ip_address.trim()) {
      notify.error("Name and address required", "A device needs both a friendly name and an IP address.");
      return;
    }

    const payload = normaliseDeviceInput({
      ...(draft.id ? { id: draft.id } : {}),
      name: draft.name.trim(),
      ip_address: draft.ip_address.trim(),
      policy_mode: draft.policy_mode,
      blocklist_profile_override: draft.blocklist_profile_override || null,
      protection_override: draft.protection_override,
      allowed_domains: splitDomainList(draft.allowed_domains),
      service_overrides: draft.service_overrides,
    });

    const result = await mutate({
      key: "device-submit",
      action: () => api.upsertDevice(payload),
      successTitle: draft.id ? "Device updated" : "Device added",
      successDetail: (device) => `${device.name} is now tracked in the control plane.`,
      failureTitle: draft.id ? "Could not update device" : "Could not add device",
    });

    if (result) select(null);
  };

  const columns: Column<DeviceRecord>[] = [
    { key: "name", header: "Name", render: (row) => row.name, sortValue: (row) => row.name },
    {
      key: "ip",
      header: "IP address",
      render: (row) => <span className="font-mono text-xs">{row.ip_address}</span>,
      sortValue: (row) => row.ip_address,
    },
    {
      key: "policy",
      header: "Policy",
      render: (row) => (
        <Badge variant={row.policy_mode === "custom" ? "default" : "secondary"}>
          {row.policy_mode === "custom" ? "Custom" : "Household default"}
        </Badge>
      ),
      sortValue: (row) => row.policy_mode,
    },
    {
      key: "profile",
      header: "Profile",
      hideOnStack: true,
      render: (row) => row.blocklist_profile_override ?? "Default",
    },
    {
      key: "protection",
      header: "Protection",
      render: (row) =>
        row.protection_override === "bypass" ? (
          <StatusPill label="Bypassing filters" tone="warn" />
        ) : (
          <StatusPill label="Filtered" tone="good" />
        ),
    },
    {
      key: "overrides",
      header: "Service rules",
      align: "end",
      hideOnStack: true,
      render: (row) => <span className="tabular">{formatCount(row.service_overrides.length)}</span>,
      sortValue: (row) => row.service_overrides.length,
    },
  ];

  return (
    <PageShell>
      <PageHeader
        actions={
          <Button onClick={() => select(null)} variant="outline">
            <PlusIcon aria-hidden />
            New device
          </Button>
        }
        description="Name the devices on the network so events, exceptions and per-device policy read in plain language."
        title="Devices"
      />

      <PageSections>
        {devices.some((device) => device.protection_override === "bypass") ? (
          <NoticeBanner
            detail="Those devices resolve unfiltered. Their traffic is still counted, but nothing is blocked for them."
            title="Some devices bypass filtering"
            tone="warn"
          />
        ) : null}

        <div className="grid gap-4 xl:grid-cols-[minmax(0,1.05fr)_minmax(0,0.95fr)]">
          <SectionCard
            description={
              custom
                ? "Custom devices ignore the household default and use exactly what you set here."
                : "This device follows the household default until you switch it to a custom assignment."
            }
            footer={
              <>
                {draft.id ? (
                  <Button onClick={() => select(null)} variant="ghost">
                    Cancel
                  </Button>
                ) : null}
                <Button
                  disabled={!draft.name.trim() || !draft.ip_address.trim()}
                  isLoading={busy === "device-submit"}
                  onClick={() => void save()}
                >
                  {draft.id ? "Save device" : "Add device"}
                </Button>
              </>
            }
            title={draft.id ? `Edit ${draft.name || "device"}` : "Add device"}
          >
            <div className="space-y-4">
              <FieldRow>
                <TextField
                  label="Device name"
                  onChange={(value) => setDraft((current) => ({ ...current, name: value }))}
                  placeholder="Kitchen iPad"
                  value={draft.name}
                />
                <TextField
                  hint="Saved even if unparseable, but only valid addresses affect DNS."
                  label="IP address"
                  onChange={(value) => setDraft((current) => ({ ...current, ip_address: value }))}
                  placeholder="192.168.1.42"
                  value={draft.ip_address}
                />
              </FieldRow>

              <FieldRow>
                <SelectField
                  label="Policy mode"
                  onChange={(value) =>
                    setDraft((current) => ({
                      ...current,
                      policy_mode: value as DeviceRecord["policy_mode"],
                    }))
                  }
                  options={[
                    { value: "global", label: "Household default" },
                    { value: "custom", label: "Custom assignment" },
                  ]}
                  value={draft.policy_mode}
                />
                <SelectField
                  disabled={!custom}
                  hint="Block profiles are stored but not yet consulted by the DNS pipeline."
                  label="Profile override"
                  onChange={(value) =>
                    setDraft((current) => ({ ...current, blocklist_profile_override: value }))
                  }
                  options={data.settings.block_profiles.map((profile) => ({
                    value: profile.name,
                    label: `${profile.emoji || "◌"} ${profile.name}`,
                  }))}
                  placeholder="No override"
                  value={draft.blocklist_profile_override}
                />
              </FieldRow>

              <FieldRow>
                <SelectField
                  disabled={!custom}
                  label="Protection"
                  onChange={(value) =>
                    setDraft((current) => ({
                      ...current,
                      protection_override: value as DeviceRecord["protection_override"],
                    }))
                  }
                  options={[
                    { value: "inherit", label: "Keep blocking on" },
                    { value: "bypass", label: "Bypass blocking" },
                  ]}
                  value={draft.protection_override}
                />
                <TextField
                  disabled={!custom}
                  hint="Comma-separated. Always reachable from this device."
                  label="Allowed domains"
                  onChange={(value) => setDraft((current) => ({ ...current, allowed_domains: value }))}
                  placeholder="school.site, printer.local"
                  value={draft.allowed_domains}
                />
              </FieldRow>

              <div className="rounded-xl border border-border p-4">
                <p className="font-medium text-foreground text-sm">Service override</p>
                <p className="mt-1 text-muted-foreground text-sm">
                  Add a focused allow or block rule for a known service when this device needs a small
                  exception.
                </p>

                <div className="mt-3 grid gap-3 sm:grid-cols-2">
                  <SelectField
                    disabled={!custom}
                    label="Service"
                    onChange={setPendingService}
                    options={data.settings.services.map((service) => ({
                      value: service.manifest.service_id,
                      label: service.manifest.display_name,
                    }))}
                    placeholder="Choose a service"
                    value={pendingService}
                  />
                  <SelectField
                    disabled={!custom}
                    label="Rule"
                    onChange={(value) => setPendingMode(value as "allow" | "block")}
                    options={[
                      { value: "allow", label: "Allow service" },
                      { value: "block", label: "Block service" },
                    ]}
                    value={pendingMode}
                  />
                </div>

                {manifest ? (
                  <div className="mt-3 rounded-lg border border-border bg-muted/40 p-3">
                    <p className="font-medium text-foreground text-sm">{manifest.display_name}</p>
                    <p className="mt-1 text-muted-foreground text-xs">{manifest.risk_notes}</p>
                    <div className="mt-2 flex flex-wrap gap-1.5">
                      <Badge variant="secondary">{pendingMode}</Badge>
                      <Badge variant="outline">{manifest.category}</Badge>
                      <Badge variant="outline">{formatCount(previewDomains.length)} domains</Badge>
                    </div>
                    {previewDomains.length > 0 ? (
                      <div className="mt-2 flex flex-wrap gap-1.5">
                        {previewDomains.slice(0, 4).map((domain) => (
                          <Badge key={domain} variant="outline">
                            {domain}
                          </Badge>
                        ))}
                      </div>
                    ) : null}
                  </div>
                ) : null}

                <Button
                  className="mt-3"
                  disabled={!custom || !pendingService}
                  onClick={addServiceOverride}
                  size="sm"
                  variant="outline"
                >
                  <PlusIcon aria-hidden />
                  Add service rule
                </Button>

                {draft.service_overrides.length > 0 ? (
                  <ul className="mt-3 flex flex-wrap gap-1.5">
                    {draft.service_overrides.map((rule) => {
                      const info = data.settings.services.find(
                        (service) => service.manifest.service_id === rule.service_id,
                      )?.manifest;
                      return (
                        <li key={rule.service_id}>
                          <button
                            className="inline-flex items-center gap-1.5 rounded-full border border-border px-2 py-1 text-xs hover:bg-muted"
                            onClick={() =>
                              setDraft((current) => ({
                                ...current,
                                service_overrides: current.service_overrides.filter(
                                  (entry) => entry.service_id !== rule.service_id,
                                ),
                              }))
                            }
                            title={info ? `${info.category} — ${info.risk_notes}` : "Custom device service rule"}
                            type="button"
                          >
                            {info?.display_name ?? rule.service_id} — {rule.mode}
                            <XIcon aria-hidden className="size-3" />
                            <span className="sr-only">Remove this rule</span>
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                ) : null}
              </div>
            </div>
          </SectionCard>

          <SectionCard
            description="Named devices tracked by the control plane."
            title={`Devices (${formatCount(devices.length)})`}
          >
            <TextField
              className="mb-4"
              label="Search"
              onChange={setSearch}
              placeholder="Name or address"
              searchTarget
              value={search}
            />
            <DataTable
              columns={columns}
              empty={{
                icon: LaptopIcon,
                title: search ? "No devices match that search" : "No devices named yet",
                description: search
                  ? "Clear the search to see every tracked device."
                  : "Start with the devices the household will recognise fastest — the TV, the kids' tablets, the work laptop.",
              }}
              error={error}
              loading={phase === "loading"}
              onRetry={() => void reload()}
              onRowClick={(row) => select(row)}
              rowActionLabel={(row) => `Edit ${row.name}`}
              rowKey={(row) => row.id}
              rows={filtered}
            />
          </SectionCard>
        </div>

        <SectionCard
          description="Look up any domain to see the classifier's verdict and the exact features behind it."
          title="Troubleshoot a device"
        >
          <p className="text-muted-foreground text-sm">
            When someone reports a broken site, inspect the domain first: the inspector shows whether a
            blocklist rule or the model made the call.
          </p>
          <Button className="mt-3" onClick={() => inspect("doubleclick.net")} variant="outline">
            <SearchIcon aria-hidden />
            Open domain inspector
          </Button>
        </SectionCard>
      </PageSections>
    </PageShell>
  );
}
