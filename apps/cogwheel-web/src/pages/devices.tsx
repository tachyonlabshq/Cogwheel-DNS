import { useMemo, useState } from "react";
import { useCogwheel } from "@/contexts/cogwheel-context";
import { api, type DeviceServiceOverride, type SettingsSummary } from "@/lib/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardDescription, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { ListRow, EmptyState } from "@/components/shared";

export default function DevicesPage() {
  const { settings, busyAction, setBusyAction, pushToast, load } =
    useCogwheel();

  const [deviceId, setDeviceId] = useState<string | null>(null);
  const [deviceName, setDeviceName] = useState("");
  const [deviceIpAddress, setDeviceIpAddress] = useState("");
  const [devicePolicyMode, setDevicePolicyMode] = useState<
    "global" | "custom"
  >("global");
  const [deviceProfileOverride, setDeviceProfileOverride] = useState("");
  const [deviceProtectionOverride, setDeviceProtectionOverride] = useState<
    "inherit" | "bypass"
  >("inherit");
  const [deviceAllowedDomains, setDeviceAllowedDomains] = useState("");
  const [deviceServiceOverrides, setDeviceServiceOverrides] = useState<
    DeviceServiceOverride[]
  >([]);
  const [deviceServiceOverrideId, setDeviceServiceOverrideId] = useState("");
  const [deviceServiceOverrideMode, setDeviceServiceOverrideMode] = useState<
    "allow" | "block"
  >("allow");

  const serviceLabelMap = useMemo(
    () =>
      new Map(
        settings.services.map((service) => [
          service.manifest.service_id,
          service.manifest.display_name,
        ]),
      ),
    [settings.services],
  );

  const serviceInfoMap = useMemo(
    () =>
      new Map(
        settings.services.map((service) => [
          service.manifest.service_id,
          service.manifest,
        ]),
      ),
    [settings.services],
  );

  const selectedDeviceServiceManifest = useMemo(
    () =>
      deviceServiceOverrideId
        ? serviceInfoMap.get(deviceServiceOverrideId) ?? null
        : null,
    [deviceServiceOverrideId, serviceInfoMap],
  );

  const pendingDeviceServiceOverride = useMemo(
    () =>
      deviceServiceOverrides.find(
        (item) => item.service_id === deviceServiceOverrideId,
      ) ?? null,
    [deviceServiceOverrideId, deviceServiceOverrides],
  );

  const deviceServiceOverrideIsNoop =
    pendingDeviceServiceOverride?.mode === deviceServiceOverrideMode;

  const deviceServiceOverridePreview = useMemo(() => {
    if (!selectedDeviceServiceManifest) return null;

    const domains =
      deviceServiceOverrideMode === "allow"
        ? Array.from(
            new Set([
              ...selectedDeviceServiceManifest.allow_domains,
              ...selectedDeviceServiceManifest.block_domains,
              ...selectedDeviceServiceManifest.exceptions,
            ]),
          )
        : selectedDeviceServiceManifest.block_domains;

    return {
      serviceId: selectedDeviceServiceManifest.service_id,
      displayName: selectedDeviceServiceManifest.display_name,
      category: selectedDeviceServiceManifest.category,
      riskNotes: selectedDeviceServiceManifest.risk_notes,
      domains,
      exceptions: selectedDeviceServiceManifest.exceptions,
      sampleDomains: domains.slice(0, 4),
    };
  }, [deviceServiceOverrideMode, selectedDeviceServiceManifest]);

  function resetDeviceForm() {
    setDeviceId(null);
    setDeviceName("");
    setDeviceIpAddress("");
    setDevicePolicyMode("global");
    setDeviceProfileOverride("");
    setDeviceProtectionOverride("inherit");
    setDeviceAllowedDomains("");
    setDeviceServiceOverrides([]);
    setDeviceServiceOverrideId("");
    setDeviceServiceOverrideMode("allow");
  }

  async function handleDeviceSubmit() {
    setBusyAction("device-submit");
    try {
      await api.upsertDevice({
        id: deviceId ?? undefined,
        name: deviceName,
        ip_address: deviceIpAddress,
        policy_mode: devicePolicyMode,
        blocklist_profile_override:
          devicePolicyMode === "custom"
            ? deviceProfileOverride || null
            : null,
        protection_override:
          devicePolicyMode === "custom" ? deviceProtectionOverride : "inherit",
        allowed_domains:
          devicePolicyMode === "custom"
            ? deviceAllowedDomains
                .split(",")
                .map((domain) => domain.trim())
                .filter(Boolean)
            : [],
        service_overrides:
          devicePolicyMode === "custom" ? deviceServiceOverrides : [],
      });
      pushToast(
        deviceId ? "Device updated" : "Device added",
        `${deviceName} is now tracked in the control plane.`,
        "success",
      );
      resetDeviceForm();
      await load();
    } catch (mutationError) {
      pushToast(
        "Device save failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  function startDeviceEdit(device: SettingsSummary["devices"][number]) {
    setDeviceId(device.id);
    setDeviceName(device.name);
    setDeviceIpAddress(device.ip_address);
    setDevicePolicyMode(device.policy_mode);
    setDeviceProfileOverride(device.blocklist_profile_override ?? "");
    setDeviceProtectionOverride(device.protection_override);
    setDeviceAllowedDomains(device.allowed_domains.join(", "));
    setDeviceServiceOverrides(device.service_overrides);
    setDeviceServiceOverrideId("");
    setDeviceServiceOverrideMode("allow");
  }

  function addDeviceServiceOverride() {
    if (devicePolicyMode !== "custom") {
      pushToast(
        "Custom mode required",
        "Switch the device to custom policy mode before adding service rules.",
        "error",
      );
      return;
    }
    if (!deviceServiceOverrideId) {
      pushToast(
        "Service required",
        "Choose a built-in service before adding a device rule.",
        "error",
      );
      return;
    }
    if (!selectedDeviceServiceManifest) {
      pushToast(
        "Unknown service",
        "Reload settings and pick the service again before saving the device rule.",
        "error",
      );
      return;
    }
    if (
      !deviceServiceOverridePreview ||
      deviceServiceOverridePreview.domains.length === 0
    ) {
      pushToast(
        "Service rule unavailable",
        "This service does not currently expand into any device-specific domains for the selected mode.",
        "error",
      );
      return;
    }
    if (deviceServiceOverrideIsNoop) {
      pushToast(
        "Service rule already queued",
        `${selectedDeviceServiceManifest.display_name} is already using ${deviceServiceOverrideMode} mode for this device.`,
        "error",
      );
      return;
    }

    setDeviceServiceOverrides((current) => {
      const next = current.filter(
        (item) => item.service_id !== deviceServiceOverrideId,
      );
      next.push({
        service_id: deviceServiceOverrideId,
        mode: deviceServiceOverrideMode,
      });
      next.sort((left, right) =>
        left.service_id.localeCompare(right.service_id),
      );
      return next;
    });
    pushToast(
      "Service rule added",
      pendingDeviceServiceOverride
        ? `${selectedDeviceServiceManifest.display_name} now uses ${deviceServiceOverrideMode} mode for this device.`
        : `${selectedDeviceServiceManifest.display_name} expands into ${deviceServiceOverridePreview.domains.length} device-specific domain rule${deviceServiceOverridePreview.domains.length === 1 ? "" : "s"}.`,
      "success",
    );
  }

  function removeDeviceServiceOverride(serviceId: string) {
    setDeviceServiceOverrides((current) =>
      current.filter((item) => item.service_id !== serviceId),
    );
  }

  function formatDeviceServiceOverride(
    serviceId: string,
    mode: "allow" | "block",
  ) {
    const label = serviceLabelMap.get(serviceId) ?? serviceId;
    return `${label} - ${mode}`;
  }

  function describeDeviceServiceOverride(serviceId: string) {
    const info = serviceInfoMap.get(serviceId);
    if (!info) return "Custom device service rule";
    return `${info.category} - ${info.risk_notes}`;
  }

  return (
    <section className="grid gap-6 xl:grid-cols-[1.05fr_0.95fr]">
      <Card id="devices-page" className="overflow-hidden p-0">
        <div className="border-b border-border bg-muted/30 px-6 py-5">
          <CardTitle>Devices</CardTitle>
          <CardDescription className="mt-1">
            Give each device a clear name, then decide whether it keeps the
            household default or receives a saved profile.
          </CardDescription>
        </div>
        <div className="grid gap-5 px-6 py-6">
          <div className="grid gap-3 lg:grid-cols-2">
            <Input
              value={deviceName}
              onChange={(event) => setDeviceName(event.target.value)}
              placeholder="Kitchen iPad"
            />
            <Input
              value={deviceIpAddress}
              onChange={(event) => setDeviceIpAddress(event.target.value)}
              placeholder="192.168.1.42"
            />
          </div>
          <div className="grid gap-3 lg:grid-cols-2">
            <select
              className="h-11 rounded-xl border border-input bg-background px-4 text-sm"
              value={devicePolicyMode}
              onChange={(event) =>
                setDevicePolicyMode(
                  event.target.value as "global" | "custom",
                )
              }
            >
              <option value="global">Household default</option>
              <option value="custom">Custom assignment</option>
            </select>
            <select
              className="h-11 rounded-xl border border-input bg-background px-4 text-sm"
              value={deviceProfileOverride}
              onChange={(event) =>
                setDeviceProfileOverride(event.target.value)
              }
              disabled={devicePolicyMode !== "custom"}
            >
              <option value="">Choose a saved profile</option>
              {settings.block_profiles.map((profile) => (
                <option key={profile.id} value={profile.name}>
                  {profile.emoji} {profile.name}
                </option>
              ))}
            </select>
            <select
              className="h-11 rounded-xl border border-input bg-background px-4 text-sm"
              value={deviceProtectionOverride}
              onChange={(event) =>
                setDeviceProtectionOverride(
                  event.target.value as "inherit" | "bypass",
                )
              }
              disabled={devicePolicyMode !== "custom"}
            >
              <option value="inherit">Keep blocking on</option>
              <option value="bypass">Bypass blocking</option>
            </select>
            <Input
              value={deviceAllowedDomains}
              onChange={(event) =>
                setDeviceAllowedDomains(event.target.value)
              }
              placeholder="school.site, printer.local"
              disabled={devicePolicyMode !== "custom"}
            />
          </div>
          <div className="rounded-2xl border border-border bg-muted/20 p-4">
            <div className="flex flex-col gap-1">
              <div className="text-sm font-medium text-foreground">
                Service override
              </div>
              <div className="text-sm text-muted-foreground">
                Add a focused allow or block rule for a known service when this
                device needs a small exception.
              </div>
            </div>
            <div className="mt-4 grid gap-3 xl:grid-cols-[minmax(0,1fr)_180px_auto]">
              <select
                className="h-11 rounded-xl border border-input bg-background px-4 text-sm"
                value={deviceServiceOverrideId}
                onChange={(event) =>
                  setDeviceServiceOverrideId(event.target.value)
                }
                disabled={devicePolicyMode !== "custom"}
              >
                <option value="">Select service override</option>
                {settings.services.map((service) => (
                  <option
                    key={service.manifest.service_id}
                    value={service.manifest.service_id}
                  >
                    {service.manifest.display_name}
                  </option>
                ))}
              </select>
              <select
                className="h-11 rounded-xl border border-input bg-background px-4 text-sm"
                value={deviceServiceOverrideMode}
                onChange={(event) =>
                  setDeviceServiceOverrideMode(
                    event.target.value as "allow" | "block",
                  )
                }
                disabled={devicePolicyMode !== "custom"}
              >
                <option value="allow">Allow service</option>
                <option value="block">Block service</option>
              </select>
              <Button
                variant="outline"
                onClick={addDeviceServiceOverride}
                disabled={
                  devicePolicyMode !== "custom" ||
                  !deviceServiceOverrideId ||
                  deviceServiceOverrideIsNoop
                }
              >
                Add service rule
              </Button>
            </div>
          </div>
          <div className="flex flex-wrap justify-end gap-3">
            {deviceId ? (
              <Button variant="ghost" onClick={resetDeviceForm}>
                Cancel
              </Button>
            ) : null}
            <Button
              onClick={() => void handleDeviceSubmit()}
              disabled={
                !deviceName ||
                !deviceIpAddress ||
                busyAction === "device-submit"
              }
            >
              {busyAction === "device-submit"
                ? "Saving..."
                : deviceId
                  ? "Save device"
                  : "Add device"}
            </Button>
          </div>
          {devicePolicyMode !== "custom" ? (
            <div className="rounded-2xl border border-dashed border-border bg-muted/20 p-4 text-sm text-muted-foreground">
              This device will follow the household default until you switch it
              to a custom assignment.
            </div>
          ) : null}
          {deviceServiceOverrideId && deviceServiceOverridePreview ? (
            <div className="rounded-2xl border border-border bg-background p-4 text-sm">
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <div className="font-medium">
                    {deviceServiceOverridePreview.displayName}
                  </div>
                  <div className="mt-1 text-muted-foreground">
                    {deviceServiceOverridePreview.riskNotes}
                  </div>
                </div>
                <div className="flex flex-wrap gap-2 text-xs text-muted-foreground">
                  <Badge>{deviceServiceOverrideMode}</Badge>
                  <Badge>{deviceServiceOverridePreview.category}</Badge>
                  <Badge>
                    {deviceServiceOverridePreview.domains.length} domains
                  </Badge>
                </div>
              </div>
              <div className="mt-3 flex flex-wrap gap-2">
                {deviceServiceOverridePreview.sampleDomains.map((domain) => (
                  <Badge key={domain}>{domain}</Badge>
                ))}
              </div>
            </div>
          ) : null}
          {deviceServiceOverrides.length > 0 ? (
            <div className="flex flex-wrap gap-2">
              {deviceServiceOverrides.map((override) => (
                <button
                  key={`${override.service_id}-${override.mode}`}
                  type="button"
                  title={describeDeviceServiceOverride(override.service_id)}
                  className="rounded-full border border-border bg-background px-3 py-1 text-xs text-muted-foreground transition hover:bg-muted/30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  onClick={() =>
                    removeDeviceServiceOverride(override.service_id)
                  }
                >
                  {formatDeviceServiceOverride(
                    override.service_id,
                    override.mode,
                  )}{" "}
                  x
                </button>
              ))}
            </div>
          ) : null}
        </div>
      </Card>

      <div className="grid gap-6">
        <Card className="overflow-hidden p-0">
          <div className="border-b border-border bg-muted/30 px-6 py-5">
            <CardTitle>Saved devices</CardTitle>
            <CardDescription className="mt-1">
              Detected and named devices stay easy to scan, edit, and reassign.
            </CardDescription>
          </div>
          <div className="grid gap-3 px-6 py-6">
            {settings.devices.length === 0 ? (
              <EmptyState>
                No devices have been named yet. Start with the devices the
                household will recognize fastest.
              </EmptyState>
            ) : (
              settings.devices.map((device) => (
                <ListRow
                  key={device.id}
                  title={device.name}
                  detail={device.ip_address}
                  right={
                    <Badge>
                      {device.policy_mode === "custom" ? "Custom" : "Default"}
                    </Badge>
                  }
                  footer={
                    <>
                      <div className="flex flex-wrap gap-2 text-xs text-muted-foreground">
                        <Badge>
                          {device.blocklist_profile_override ??
                            "Household default"}
                        </Badge>
                        <Badge>
                          {device.protection_override === "bypass"
                            ? "Bypass enabled"
                            : "Blocking on"}
                        </Badge>
                        <Badge>
                          {device.allowed_domains.length} allowlisted
                        </Badge>
                        <Badge>
                          {device.service_overrides.length} service rules
                        </Badge>
                      </div>
                      <div className="mt-4 flex justify-end">
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => startDeviceEdit(device)}
                        >
                          Edit device
                        </Button>
                      </div>
                    </>
                  }
                />
              ))
            )}
          </div>
        </Card>

        <Card className="overflow-hidden p-0">
          <div className="border-b border-border bg-muted/30 px-6 py-5">
            <CardTitle>Assignment help</CardTitle>
            <CardDescription className="mt-1">
              Use friendly names from saved block profiles so the household can
              tell what each device is using at a glance.
            </CardDescription>
          </div>
          <div className="grid gap-3 px-6 py-6 text-sm text-muted-foreground">
            {settings.block_profiles.length === 0 ? (
              <EmptyState>
                Create a block profile first, then come back here to assign it
                to a device.
              </EmptyState>
            ) : (
              settings.block_profiles.map((profile) => (
                <ListRow
                  key={profile.id}
                  tone="muted"
                  title={`${profile.emoji || "\u25CC"} ${profile.name}`}
                  detail={profile.description}
                />
              ))
            )}
          </div>
        </Card>
      </div>
    </section>
  );
}
