import { useEffect, useState } from "react";
import { useCogwheel } from "@/contexts/cogwheel-context";
import { api, type BlockProfileListRecord, type BlockProfileRecord } from "@/lib/api";
import { oisdProfileOptions } from "@/lib/constants";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardDescription, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { EmptyState } from "@/components/shared";

const emptyBlockProfileDraft: BlockProfileRecord = {
  id: "",
  emoji: "",
  name: "",
  description: "",
  blocklists: [],
  allowlists: [],
  updated_at: new Date(0).toISOString(),
};

export default function ProfilesPage() {
  const { settings, setSettings, busyAction, setBusyAction, pushToast, load } =
    useCogwheel();

  const [selectedBlockProfileId, setSelectedBlockProfileId] = useState<
    string | null
  >(null);
  const [creatingNewBlockProfile, setCreatingNewBlockProfile] = useState(false);
  const [blockProfileDraft, setBlockProfileDraft] =
    useState<BlockProfileRecord>(emptyBlockProfileDraft);
  const [blockProfileAllowlistDraft, setBlockProfileAllowlistDraft] =
    useState("");
  const [customProfileListName, setCustomProfileListName] = useState("");
  const [customProfileListUrl, setCustomProfileListUrl] = useState("");

  // Sync selected profile into draft when profiles change
  useEffect(() => {
    const selectedProfile = settings.block_profiles.find(
      (profile) => profile.id === selectedBlockProfileId,
    );
    if (selectedProfile) {
      setCreatingNewBlockProfile(false);
      setBlockProfileDraft(selectedProfile);
      setBlockProfileAllowlistDraft(selectedProfile.allowlists.join(", "));
      return;
    }

    if (creatingNewBlockProfile) {
      return;
    }

    if (settings.block_profiles.length > 0 && selectedBlockProfileId === null) {
      const firstProfile = settings.block_profiles[0];
      setSelectedBlockProfileId(firstProfile.id);
      setBlockProfileDraft(firstProfile);
      setBlockProfileAllowlistDraft(firstProfile.allowlists.join(", "));
      return;
    }

    if (settings.block_profiles.length === 0) {
      setBlockProfileDraft(emptyBlockProfileDraft);
      setBlockProfileAllowlistDraft("");
    }
  }, [creatingNewBlockProfile, selectedBlockProfileId, settings.block_profiles]);

  function startNewBlockProfile() {
    setCreatingNewBlockProfile(true);
    setSelectedBlockProfileId(null);
    setBlockProfileDraft({
      ...emptyBlockProfileDraft,
      updated_at: new Date().toISOString(),
    });
    setBlockProfileAllowlistDraft("");
    setCustomProfileListName("");
    setCustomProfileListUrl("");
  }

  function selectBlockProfile(profile: BlockProfileRecord) {
    setCreatingNewBlockProfile(false);
    setSelectedBlockProfileId(profile.id);
    setBlockProfileDraft(profile);
    setBlockProfileAllowlistDraft(profile.allowlists.join(", "));
    setCustomProfileListName("");
    setCustomProfileListUrl("");
  }

  function togglePresetBlocklist(option: BlockProfileListRecord) {
    setBlockProfileDraft((current) => {
      const exists = current.blocklists.some((entry) => entry.id === option.id);
      if (exists) {
        return {
          ...current,
          blocklists: current.blocklists.filter(
            (entry) => entry.id !== option.id,
          ),
        };
      }

      let nextLists = current.blocklists.filter((entry) => {
        if (option.id === "oisd-big") return entry.id !== "oisd-small";
        if (option.id === "oisd-small") return entry.id !== "oisd-big";
        if (option.id === "oisd-nsfw") return entry.id !== "oisd-nsfw-small";
        if (option.id === "oisd-nsfw-small") return entry.id !== "oisd-nsfw";
        return true;
      });

      nextLists = [...nextLists, option].sort((left, right) =>
        left.name.localeCompare(right.name),
      );
      return { ...current, blocklists: nextLists };
    });
  }

  function addCustomBlocklistToProfile() {
    const name = customProfileListName.trim();
    const url = customProfileListUrl.trim();
    if (!name || !url) {
      pushToast(
        "List details required",
        "Enter both a list name and a GitHub URL before adding it.",
        "error",
      );
      return;
    }

    if (
      !(
        url.includes("github.com") ||
        url.includes("raw.githubusercontent.com")
      )
    ) {
      pushToast(
        "GitHub URL required",
        "Manual lists should point at a GitHub or raw GitHub blocklist URL.",
        "error",
      );
      return;
    }

    const nextList: BlockProfileListRecord = {
      id:
        name
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, "-")
          .replace(/^-+|-+$/g, "") || `custom-${Date.now()}`,
      name,
      url,
      kind: "custom",
      family: "custom",
    };

    setBlockProfileDraft((current) => ({
      ...current,
      blocklists: [
        ...current.blocklists.filter((entry) => entry.url !== url),
        nextList,
      ].sort((left, right) => left.name.localeCompare(right.name)),
    }));
    setCustomProfileListName("");
    setCustomProfileListUrl("");
  }

  function removeBlocklistFromProfile(id: string) {
    setBlockProfileDraft((current) => ({
      ...current,
      blocklists: current.blocklists.filter((entry) => entry.id !== id),
    }));
  }

  async function handleBlockProfileSave() {
    if (!blockProfileDraft.name.trim()) {
      pushToast(
        "Name required",
        "Give the block profile a friendly name before saving.",
        "error",
      );
      return;
    }

    setBusyAction("block-profile-save");
    try {
      const updatedProfiles = await api.upsertBlockProfile({
        id: blockProfileDraft.id || undefined,
        emoji: blockProfileDraft.emoji,
        name: blockProfileDraft.name,
        description: blockProfileDraft.description,
        blocklists: blockProfileDraft.blocklists,
        allowlists: blockProfileAllowlistDraft
          .split(",")
          .map((entry) => entry.trim())
          .filter(Boolean),
      });
      const nextSelectedId =
        updatedProfiles.find(
          (profile) => profile.name === blockProfileDraft.name,
        )?.id ?? blockProfileDraft.id;
      setSettings((current) => ({
        ...current,
        block_profiles: updatedProfiles,
      }));
      setCreatingNewBlockProfile(false);
      setSelectedBlockProfileId(nextSelectedId || null);
      pushToast(
        "Block profile saved",
        `${blockProfileDraft.name} is ready for device assignment.`,
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Block profile save failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  async function handleBlockProfileDelete() {
    if (!selectedBlockProfileId) {
      pushToast(
        "Profile required",
        "Choose a saved profile before deleting it.",
        "error",
      );
      return;
    }

    const profileName = blockProfileDraft.name || "This profile";
    setBusyAction("block-profile-delete");
    try {
      const updatedProfiles = await api.deleteBlockProfile(
        selectedBlockProfileId,
      );
      setSettings((current) => ({
        ...current,
        block_profiles: updatedProfiles,
      }));
      setCreatingNewBlockProfile(updatedProfiles.length === 0);
      setSelectedBlockProfileId(updatedProfiles[0]?.id ?? null);
      if (updatedProfiles.length === 0) {
        setBlockProfileDraft({
          ...emptyBlockProfileDraft,
          updated_at: new Date().toISOString(),
        });
        setBlockProfileAllowlistDraft("");
      }
      pushToast(
        "Block profile deleted",
        `${profileName} was removed.`,
        "success",
      );
      await load();
    } catch (mutationError) {
      pushToast(
        "Block profile delete failed",
        mutationError instanceof Error
          ? mutationError.message
          : "Unknown error",
        "error",
      );
    } finally {
      setBusyAction(null);
    }
  }

  return (
    <section className="grid gap-6 xl:grid-cols-[360px_minmax(0,1fr)]">
      <Card className="overflow-hidden p-0">
        <div className="border-b border-border px-6 py-5">
          <div className="flex items-start justify-between gap-3">
            <div>
              <CardTitle>Profile library</CardTitle>
              <CardDescription className="mt-1">
                Choose a saved profile or start a new one for a different room,
                device, or family routine.
              </CardDescription>
            </div>
            <Button
              variant="secondary"
              size="icon"
              onClick={startNewBlockProfile}
              aria-label="Create profile"
            >
              +
            </Button>
          </div>
        </div>
        <div className="px-4 py-4">
          <div className="grid gap-3">
            {settings.block_profiles.length === 0 ? (
              <EmptyState>
                No saved profiles yet. Start with a family-safe or focus profile
                and then assign it to devices.
              </EmptyState>
            ) : (
              settings.block_profiles.map((profile) => (
                <button
                  key={profile.id}
                  type="button"
                  onClick={() => selectBlockProfile(profile)}
                  className={`rounded-2xl border p-4 text-left transition ${selectedBlockProfileId === profile.id ? "border-primary bg-primary/5 shadow-sm" : "border-border bg-muted/20 hover:bg-muted/40"}`}
                >
                  <div className="flex items-start justify-between gap-3">
                    <div>
                      <div className="text-2xl">
                        {profile.emoji || "\u25CC"}
                      </div>
                      <div className="mt-2 font-medium">{profile.name}</div>
                      <div className="mt-1 text-sm text-muted-foreground">
                        {profile.description || "No summary yet."}
                      </div>
                    </div>
                    <Badge
                      className={
                        selectedBlockProfileId === profile.id
                          ? "bg-primary text-primary-foreground"
                          : "bg-secondary text-secondary-foreground"
                      }
                    >
                      {profile.blocklists.length} sources
                    </Badge>
                  </div>
                  <div className="mt-3 text-xs text-muted-foreground">
                    Updated {new Date(profile.updated_at).toLocaleString()}{" "}
                    &bull; {profile.allowlists.length} allowlist entr
                    {profile.allowlists.length === 1 ? "y" : "ies"}
                  </div>
                </button>
              ))
            )}
          </div>
        </div>
      </Card>

      <Card className="overflow-hidden p-0">
        <div className="border-b border-border px-6 py-5">
          <CardTitle>
            {selectedBlockProfileId ? "Edit profile" : "Create profile"}
          </CardTitle>
          <CardDescription className="mt-1">
            Shape one calm, reusable filtering profile at a time: identity first,
            then list sources, then exceptions.
          </CardDescription>
        </div>
        <div className="space-y-5 px-6 py-6">
          <section className="space-y-4">
            <div>
              <div className="text-sm font-medium text-foreground">
                Profile identity
              </div>
              <div className="mt-1 text-sm text-muted-foreground">
                Give the profile a name the household will understand instantly
                during device assignment.
              </div>
            </div>
            <div className="grid gap-3 sm:grid-cols-[120px_1fr]">
              <Input
                value={blockProfileDraft.emoji}
                onChange={(event) =>
                  setBlockProfileDraft((current) => ({
                    ...current,
                    emoji: event.target.value,
                  }))
                }
                placeholder="Optional emoji"
              />
              <Input
                value={blockProfileDraft.name}
                onChange={(event) =>
                  setBlockProfileDraft((current) => ({
                    ...current,
                    name: event.target.value,
                  }))
                }
                placeholder="Homework time"
              />
            </div>
            <Input
              value={blockProfileDraft.description}
              onChange={(event) =>
                setBlockProfileDraft((current) => ({
                  ...current,
                  description: event.target.value,
                }))
              }
              placeholder="Short summary shown when assigning this profile to devices"
            />
          </section>

          <Separator />

          <section className="space-y-4">
            <div>
              <div className="text-sm font-medium text-foreground">
                Blocklist sources
              </div>
              <div className="mt-1 text-sm text-muted-foreground">
                Pick the OISD sources and optional GitHub lists that define what
                this profile blocks before any device-level exceptions apply.
              </div>
            </div>
            <div className="rounded-2xl border border-border bg-muted/20 p-4">
              <div className="flex flex-col gap-1 sm:flex-row sm:items-end sm:justify-between">
                <div>
                  <div className="font-medium text-foreground">
                    OISD presets
                  </div>
                  <div className="text-sm text-muted-foreground">
                    Pick any combination except the overlapping small/full pair
                    in the same family.
                  </div>
                </div>
                <div className="text-xs text-muted-foreground">
                  Core and NSFW families are kept mutually exclusive
                  automatically.
                </div>
              </div>
              <div className="mt-4 grid gap-3 lg:grid-cols-2">
                {oisdProfileOptions.map((option) => {
                  const enabled = blockProfileDraft.blocklists.some(
                    (entry) => entry.id === option.id,
                  );
                  return (
                    <label
                      key={option.id}
                      className={`rounded-2xl border px-4 py-4 text-sm transition ${enabled ? "border-primary bg-primary/5 shadow-sm" : "border-border bg-background hover:bg-muted/30"}`}
                    >
                      <input
                        type="checkbox"
                        className="sr-only"
                        checked={enabled}
                        onChange={() => togglePresetBlocklist(option)}
                      />
                      <div className="flex items-start justify-between gap-3">
                        <div>
                          <div className="font-medium">{option.name}</div>
                          <div className="mt-1 text-xs text-muted-foreground">
                            {option.id.includes("nsfw")
                              ? "Adult-content focused OISD feed."
                              : "General-purpose OISD protection feed."}
                          </div>
                        </div>
                        <Badge
                          className={
                            enabled
                              ? "bg-primary text-primary-foreground"
                              : "bg-secondary text-secondary-foreground"
                          }
                        >
                          {option.id.includes("small") ? "small" : "full"}
                        </Badge>
                      </div>
                    </label>
                  );
                })}
              </div>
            </div>

            <div className="rounded-2xl border border-border bg-muted/20 p-4">
              <div className="font-medium text-foreground">
                Manual GitHub list
              </div>
              <div className="mt-1 text-sm text-muted-foreground">
                Add a named list from GitHub or raw GitHub and bundle it into
                this profile.
              </div>
              <div className="mt-4 grid gap-3 lg:grid-cols-[0.85fr_1.15fr_auto]">
                <Input
                  value={customProfileListName}
                  onChange={(event) =>
                    setCustomProfileListName(event.target.value)
                  }
                  placeholder="My family blocklist companion"
                />
                <Input
                  value={customProfileListUrl}
                  onChange={(event) =>
                    setCustomProfileListUrl(event.target.value)
                  }
                  placeholder="https://raw.githubusercontent.com/.../domains.txt"
                />
                <Button
                  variant="secondary"
                  onClick={addCustomBlocklistToProfile}
                >
                  Add list
                </Button>
              </div>
            </div>

            <div className="grid gap-3">
              {blockProfileDraft.blocklists.length === 0 ? (
                <div className="rounded-2xl border border-dashed border-border bg-background p-4 text-sm text-muted-foreground">
                  Choose at least one OISD preset or add a custom GitHub list
                  here.
                </div>
              ) : (
                blockProfileDraft.blocklists.map((list) => (
                  <div
                    key={list.id}
                    className="flex flex-col gap-3 rounded-2xl border border-border bg-background p-4 sm:flex-row sm:items-center sm:justify-between"
                  >
                    <div>
                      <div className="font-medium text-foreground">
                        {list.name}
                      </div>
                      <div className="mt-1 break-all text-xs text-muted-foreground">
                        {list.url}
                      </div>
                    </div>
                    <div className="flex items-center gap-2">
                      <Badge className="bg-secondary text-secondary-foreground">
                        {list.kind}
                      </Badge>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => removeBlocklistFromProfile(list.id)}
                      >
                        Remove
                      </Button>
                    </div>
                  </div>
                ))
              )}
            </div>
          </section>

          <Separator />

          <section className="space-y-4">
            <div>
              <div className="text-sm font-medium text-foreground">
                Allowlist exceptions
              </div>
              <div className="mt-1 text-sm text-muted-foreground">
                Add domains that should stay reachable even when one of the
                selected blocklists would normally catch them.
              </div>
            </div>
            <div className="rounded-2xl border border-border bg-muted/20 p-4">
              <Input
                value={blockProfileAllowlistDraft}
                onChange={(event) =>
                  setBlockProfileAllowlistDraft(event.target.value)
                }
                placeholder="school.example, video.example"
              />
            </div>
          </section>

          <Separator />

          <section className="space-y-4">
            <div className="rounded-2xl border border-dashed border-border bg-muted/20 p-4 text-sm text-muted-foreground">
              Device assignment uses the profile name as the runtime override
              today, so keeping names short and obvious still makes the household
              UI easier to scan.
            </div>
            <div className="flex flex-wrap justify-end gap-3">
              <Button variant="ghost" onClick={startNewBlockProfile}>
                Clear editor
              </Button>
              {selectedBlockProfileId ? (
                <Button
                  variant="outline"
                  onClick={() => void handleBlockProfileDelete()}
                  disabled={busyAction === "block-profile-delete"}
                >
                  {busyAction === "block-profile-delete"
                    ? "Deleting..."
                    : "Delete profile"}
                </Button>
              ) : null}
              <Button
                onClick={() => void handleBlockProfileSave()}
                disabled={busyAction === "block-profile-save"}
              >
                {busyAction === "block-profile-save"
                  ? "Saving..."
                  : "Save profile"}
              </Button>
            </div>
          </section>
        </div>
      </Card>
    </section>
  );
}
