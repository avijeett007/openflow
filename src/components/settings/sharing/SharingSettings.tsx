import React, { useCallback, useEffect, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { toast } from "sonner";
import { open } from "@tauri-apps/plugin-dialog";
import {
  AlertTriangle,
  FolderOpen,
  Loader2,
  PauseCircle,
  Plus,
  Trash2,
  X,
} from "lucide-react";
import type {
  AgentDefinition,
  ServiceMember,
  ShareGrant,
  SharingStatus,
} from "@/bindings";
import { commands } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { useNavigationStore } from "../../../stores/navigationStore";
import { Alert } from "../../ui/Alert";
import { Button } from "../../ui/Button";
import { Input } from "../../ui/Input";
import { Select, type SelectOption } from "../../ui/Select";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { ToggleSwitch } from "../../ui/ToggleSwitch";

// How often the (push-free) `sharingStatus` snapshot is re-polled. Fact 1:
// `connected` never arrives as an event — this is the only way the UI learns
// the socket came up, dropped, or a session count changed.
const STATUS_POLL_MS = 4000;

const SHAREABLE_KINDS: AgentDefinition["kind"][] = ["cli", "remote"];

/** A grant is only meaningful — and only ever sent to `setShareGrants` — once
 * all three fields are set. `validate_grants` on the Rust side enforces the
 * same rule for the WHOLE list, so filtering to complete rows here is what
 * lets an in-progress "add grant" row sit locally without blocking the save
 * of every other, already-finished grant. */
const isGrantComplete = (grant: ShareGrant): boolean =>
  (grant.agent_id ?? "").trim().length > 0 &&
  (grant.project_path ?? "").trim().length > 0 &&
  (grant.allowed_members ?? []).length > 0;

const emptyGrant = (): ShareGrant => ({
  agent_id: "",
  project_path: "",
  allowed_members: [],
});

/**
 * The Sharing settings section (C2, Task 8) — where an owner decides who may
 * run an agent on THIS machine. Mirrors `ServiceSettings.tsx`'s shape
 * (status polling, a paired/unpaired split) but the stakes here are higher:
 * a grant hands a named teammate shell-equivalent access to a folder on this
 * computer. See the not-encrypted banner below, which is why this file
 * exists — DESIGN-shared-agents §8 requires it in the UI, not only in a
 * design doc.
 */
export const SharingSettings: React.FC = () => {
  const { t } = useTranslation();
  const { settings, refreshSettings } = useSettings();
  const setCurrentSection = useNavigationStore(
    (state) => state.setCurrentSection,
  );

  const [status, setStatus] = useState<SharingStatus | null>(null);
  const [statusLoading, setStatusLoading] = useState(true);
  const [togglePending, setTogglePending] = useState(false);

  const [members, setMembers] = useState<ServiceMember[]>([]);
  const [membersLoading, setMembersLoading] = useState(false);
  const [membersError, setMembersError] = useState<string | null>(null);

  const [inviteCode, setInviteCode] = useState("");
  const [redeeming, setRedeeming] = useState(false);

  // The editable grant list. Seeded from `settings.sharing.grants` once
  // settings load, then owned locally so an in-progress ("add grant", not
  // yet complete) row survives an unrelated settings refresh elsewhere in
  // the app instead of being silently wiped out from under the owner.
  const [draftGrants, setDraftGrants] = useState<ShareGrant[] | null>(null);
  const [grantsPending, setGrantsPending] = useState(false);

  useEffect(() => {
    if (draftGrants === null && settings) {
      setDraftGrants(settings.sharing.grants ?? []);
    }
  }, [draftGrants, settings]);

  const refreshStatus = useCallback(async () => {
    const result = await commands.sharingStatus();
    if (result.status === "ok") setStatus(result.data);
  }, []);

  useEffect(() => {
    let cancelled = false;
    setStatusLoading(true);
    void refreshStatus().finally(() => {
      if (!cancelled) setStatusLoading(false);
    });
    const interval = setInterval(() => void refreshStatus(), STATUS_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [refreshStatus]);

  const refreshMembers = useCallback(async () => {
    setMembersLoading(true);
    setMembersError(null);
    try {
      const result = await commands.listServiceMembers();
      if (result.status === "ok") {
        setMembers(result.data);
      } else {
        setMembersError(result.error);
      }
    } finally {
      setMembersLoading(false);
    }
  }, []);

  useEffect(() => {
    if (status?.service_paired) void refreshMembers();
  }, [status?.service_paired, refreshMembers]);

  const handleToggleSharing = async (enabled: boolean) => {
    setTogglePending(true);
    try {
      const result = await commands.setSharingEnabled(enabled);
      if (result.status === "error") {
        toast.error(result.error);
        return;
      }
      if (!enabled) {
        toast.success(t("settings.sharing.pausedToast"));
      }
      await refreshSettings();
      await refreshStatus();
    } finally {
      setTogglePending(false);
    }
  };

  // Sends only the COMPLETE grants — see `isGrantComplete`'s doc comment.
  const persistDraft = async (next: ShareGrant[]) => {
    setDraftGrants(next);
    setGrantsPending(true);
    try {
      const result = await commands.setShareGrants(
        next.filter(isGrantComplete),
      );
      if (result.status === "error") {
        toast.error(result.error);
        return;
      }
      await refreshSettings();
      await refreshStatus();
    } finally {
      setGrantsPending(false);
    }
  };

  const handleAddGrant = () => {
    void persistDraft([...(draftGrants ?? []), emptyGrant()]);
  };

  const handleRemoveGrant = (index: number) => {
    void persistDraft((draftGrants ?? []).filter((_, i) => i !== index));
  };

  const updateGrant = (index: number, patch: Partial<ShareGrant>) => {
    const next = (draftGrants ?? []).map((g, i) =>
      i === index ? { ...g, ...patch } : g,
    );
    void persistDraft(next);
  };

  const handleChooseFolder = async (index: number) => {
    try {
      const dir = await open({ directory: true });
      if (typeof dir === "string" && dir.length > 0) {
        updateGrant(index, { project_path: dir });
      }
    } catch (err) {
      toast.error(
        t("settings.sharing.grants.folderError", { error: String(err) }),
      );
    }
  };

  const toggleMember = (index: number, memberId: string) => {
    const grant = (draftGrants ?? [])[index];
    if (!grant) return;
    const currentMembers = grant.allowed_members ?? [];
    const has = currentMembers.includes(memberId);
    const nextMembers = has
      ? currentMembers.filter((m) => m !== memberId)
      : [...currentMembers, memberId];
    updateGrant(index, { allowed_members: nextMembers });
  };

  const handleRedeemInvite = async () => {
    const code = inviteCode.trim();
    if (!code) return;
    setRedeeming(true);
    try {
      const result = await commands.redeemServiceInvite(code);
      if (result.status === "error") {
        toast.error(result.error);
        return;
      }
      setInviteCode("");
      toast.success(t("settings.sharing.notMember.redeemedToast"));
      await refreshStatus();
      await refreshMembers();
    } finally {
      setRedeeming(false);
    }
  };

  const agents = settings?.agents ?? [];
  const shareableAgents = agents.filter((a) =>
    SHAREABLE_KINDS.includes(a.kind),
  );
  const agentOptions: SelectOption[] = shareableAgents.map((a) => ({
    value: a.id,
    label: `${a.name} (${
      a.kind === "cli"
        ? t("settings.sharing.grants.agentKindCli")
        : t("settings.sharing.grants.agentKindRemote")
    })`,
  }));

  // Fact 3: `is_member` is a heuristic, not a hard claim. We only ever
  // surface it when it is NEGATIVE — a real refused dial was observed — and
  // say nothing when it is positive, because "true" also covers "never
  // checked yet", and asserting "you are a member" there would overclaim.
  const activeMembers = members.filter((m) => !m.revoked);

  // Fact 2: gate the error banner on `last_error.is_some() && !connected`,
  // computed fresh from the latest poll — never carried in separate local
  // state — so a clean reconnect (which clears `last_error`) makes it
  // disappear on the very next poll instead of lingering.
  const showLastError = Boolean(status?.last_error) && !status?.connected;

  const grants = draftGrants ?? [];

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      {/* Step 1: the disclosure. Persistent, not dismissible, not collapsible,
          rendered above everything else — and rendered regardless of whether
          sharing is on, so it is seen before anyone turns it on, not after. */}
      <div className="rounded-lg border border-yellow-500/30 bg-yellow-500/10 px-4 py-3 flex items-start gap-2 text-sm">
        <AlertTriangle className="h-4 w-4 shrink-0 mt-0.5 text-yellow-500" />
        <p className="text-text">
          <Trans
            i18nKey="settings.sharing.notEncryptedNotice"
            components={{ strong: <strong /> }}
          />
        </p>
      </div>

      <SettingsGroup
        title={t("settings.sharing.title")}
        description={t("settings.sharing.intro")}
      >
        {statusLoading ? (
          <div className="px-4 py-6 flex items-center gap-2 text-sm text-mid-gray">
            <Loader2 className="h-4 w-4 animate-spin" />
            {t("settings.sharing.loading")}
          </div>
        ) : !status?.service_paired ? (
          // Step 2: `service_paired` false ⇒ replace the whole body with a
          // pointer to Settings → Service. Sharing cannot work unpaired, so
          // the toggle below is not worth showing yet.
          <div className="px-4 py-4 space-y-3">
            <p className="text-sm text-mid-gray">
              {t("settings.sharing.notPaired.body")}
            </p>
            <Button
              type="button"
              variant="secondary"
              size="sm"
              onClick={() => setCurrentSection("service")}
            >
              {t("settings.sharing.notPaired.cta")}
            </Button>
          </div>
        ) : (
          <>
            <ToggleSwitch
              checked={status?.enabled ?? false}
              onChange={(checked) => void handleToggleSharing(checked)}
              disabled={togglePending}
              isUpdating={togglePending}
              label={t("settings.sharing.toggle.label")}
              description={t("settings.sharing.toggle.description")}
              descriptionMode="inline"
              grouped
            />

            {(status?.enabled || showLastError || !status?.is_member) && (
              <div className="px-4 py-3 space-y-3">
                {status?.enabled && (
                  <div className="flex items-center justify-between gap-3 text-xs text-mid-gray">
                    <span>
                      {status.connected
                        ? t("settings.sharing.status.connected", {
                            offers: status.offer_count,
                            sessions: status.active_sessions,
                          })
                        : t("settings.sharing.status.reconnecting")}
                    </span>
                    {status.connected && (
                      <Button
                        type="button"
                        variant="danger-ghost"
                        size="sm"
                        onClick={() => void handleToggleSharing(false)}
                        disabled={togglePending}
                        className="inline-flex shrink-0 items-center gap-1.5"
                      >
                        <PauseCircle className="h-4 w-4" />
                        {t("settings.sharing.pauseButton")}
                      </Button>
                    )}
                  </div>
                )}

                {showLastError && (
                  <Alert variant="warning" contained>
                    {t("settings.sharing.status.lastError", {
                      error: status?.last_error,
                    })}
                  </Alert>
                )}

                {/* Fact 3 rendered: only ever a negative claim, never a
                    positive one — see the doc comment on `activeMembers`
                    above. */}
                {!status?.is_member && (
                  <Alert variant="warning" contained>
                    <div className="space-y-2">
                      <p>{t("settings.sharing.notMember.notice")}</p>
                      <div className="flex items-center gap-2">
                        <Input
                          type="text"
                          value={inviteCode}
                          onChange={(e) => setInviteCode(e.target.value)}
                          placeholder={t(
                            "settings.sharing.notMember.invitePlaceholder",
                          )}
                          variant="compact"
                          className="flex-1"
                        />
                        <Button
                          type="button"
                          variant="secondary"
                          size="sm"
                          onClick={() => void handleRedeemInvite()}
                          disabled={redeeming || inviteCode.trim().length === 0}
                        >
                          {redeeming ? (
                            <Loader2 className="h-4 w-4 animate-spin" />
                          ) : (
                            t("settings.sharing.notMember.redeem")
                          )}
                        </Button>
                      </div>
                    </div>
                  </Alert>
                )}
              </div>
            )}
          </>
        )}
      </SettingsGroup>

      {/* Step 3: grants — only meaningful once paired. */}
      {status?.service_paired && (
        <SettingsGroup
          title={t("settings.sharing.grants.title")}
          description={
            shareableAgents.length === 0
              ? t("settings.sharing.grants.noAgents")
              : undefined
          }
        >
          <div className="px-4 py-4 space-y-4">
            {grants.length === 0 && (
              <p className="text-sm text-mid-gray">
                {t("settings.sharing.grants.empty")}
              </p>
            )}

            {grants.map((grant, index) => {
              const complete = isGrantComplete(grant);
              return (
                <div
                  key={index}
                  className="rounded-lg border border-mid-gray/20 p-3 space-y-3"
                >
                  <div className="flex items-center justify-between gap-3">
                    <Select
                      value={grant.agent_id || null}
                      options={agentOptions}
                      placeholder={t(
                        "settings.sharing.grants.agentPlaceholder",
                      )}
                      onChange={(value) =>
                        updateGrant(index, { agent_id: value ?? "" })
                      }
                      isClearable={false}
                      className="flex-1"
                    />
                    <Button
                      type="button"
                      variant="danger-ghost"
                      size="sm"
                      onClick={() => handleRemoveGrant(index)}
                      aria-label={t("settings.sharing.grants.remove")}
                      title={t("settings.sharing.grants.remove")}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>

                  <div>
                    <div className="flex items-center gap-2">
                      <span
                        className="max-w-[240px] truncate text-sm text-mid-gray"
                        title={grant.project_path || undefined}
                      >
                        {grant.project_path ||
                          t("settings.sharing.grants.folderPlaceholder")}
                      </span>
                      <Button
                        type="button"
                        variant="secondary"
                        size="sm"
                        onClick={() => void handleChooseFolder(index)}
                        className="inline-flex shrink-0 items-center gap-1.5"
                      >
                        <FolderOpen className="h-4 w-4" />
                        {t("settings.sharing.grants.folderChoose")}
                      </Button>
                      {grant.project_path && (
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          onClick={() =>
                            updateGrant(index, { project_path: "" })
                          }
                          aria-label={t("settings.sharing.grants.folderClear")}
                          title={t("settings.sharing.grants.folderClear")}
                        >
                          <X className="h-4 w-4" />
                        </Button>
                      )}
                    </div>
                    <p className="text-xs text-mid-gray mt-1">
                      {t("settings.sharing.grants.folderHint")}
                    </p>
                  </div>

                  <div>
                    <p className="text-xs font-medium text-mid-gray mb-1">
                      {t("settings.sharing.grants.membersLabel")}
                    </p>
                    {membersLoading ? (
                      <p className="text-xs text-mid-gray">
                        {t("settings.sharing.grants.membersLoading")}
                      </p>
                    ) : membersError ? (
                      <p className="text-xs text-red-400">
                        {t("settings.sharing.grants.membersError", {
                          error: membersError,
                        })}
                      </p>
                    ) : activeMembers.length === 0 ? (
                      <p className="text-xs text-mid-gray">
                        {t("settings.sharing.grants.membersEmpty")}
                      </p>
                    ) : (
                      <div className="flex flex-wrap gap-2">
                        {activeMembers.map((member) => {
                          const checked = (
                            grant.allowed_members ?? []
                          ).includes(member.member_id);
                          return (
                            <label
                              key={member.member_id}
                              className="flex items-center gap-1.5 text-xs px-2 py-1 rounded-md border border-mid-gray/20 cursor-pointer select-none"
                            >
                              <input
                                type="checkbox"
                                checked={checked}
                                onChange={() =>
                                  toggleMember(index, member.member_id)
                                }
                              />
                              {member.display_name}
                            </label>
                          );
                        })}
                      </div>
                    )}
                  </div>

                  {!complete && (
                    <p className="text-xs text-yellow-500">
                      {t("settings.sharing.grants.incomplete")}
                    </p>
                  )}
                </div>
              );
            })}

            <Button
              type="button"
              variant="secondary"
              size="sm"
              onClick={handleAddGrant}
              disabled={grantsPending || shareableAgents.length === 0}
              className="inline-flex items-center gap-1.5"
            >
              <Plus className="h-4 w-4" />
              {t("settings.sharing.grants.add")}
            </Button>
          </div>
        </SettingsGroup>
      )}
    </div>
  );
};
