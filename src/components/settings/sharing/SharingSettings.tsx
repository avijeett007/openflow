import React, { useCallback, useEffect, useRef, useState } from "react";
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

/**
 * Only `cli`. C2 v1 rides the shipped raw-CLI driver (DESIGN-shared-agents §2),
 * and that is what makes the copy above the grant list literally true: a CLI
 * agent runs as a subprocess on this machine, in the folder the grant names.
 *
 * A `remote` (A2A) agent does not. `AgentRunManager::start` routes it to
 * `drive_remote_run`, which uses neither `cwd` nor `argv` — the work happens at
 * the owner's remote endpoint, under the owner's credentials, billed to the
 * owner's account, and the grant's folder bounds nothing at all. The Rust host
 * refuses one too (`relay::grants::is_shareable_kind`); this is the half that
 * keeps it out of the picker.
 */
const SHAREABLE_KINDS: AgentDefinition["kind"][] = ["cli"];

/** A grant is only meaningful — and only ever sent to `setShareGrants` — once
 * all three fields are set. `validate_grants` on the Rust side enforces the
 * same rule for the WHOLE list, so filtering to complete rows here is what
 * lets an in-progress "add grant" row sit locally without blocking the save
 * of every other, already-finished grant. */
const isGrantComplete = (grant: ShareGrant): boolean =>
  (grant.id ?? "").trim().length > 0 &&
  (grant.agent_id ?? "").trim().length > 0 &&
  (grant.project_path ?? "").trim().length > 0 &&
  (grant.allowed_members ?? []).length > 0;

/** A new grant gets its identity here and keeps it for life. The published
 * relay `action_id` is keyed on it, so it is what lets one agent be shared
 * twice — two folders, two member lists — without the two grants collapsing
 * into a single offer. */
const emptyGrant = (): ShareGrant => ({
  id: crypto.randomUUID(),
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

  // The synchronous mirror of `draftGrants`. A handler that reads
  // `draftGrants` reads the render closure, so two interactions in one render
  // frame (tick a teammate, then tick a second one) both start from the same
  // list and the second discards the first — and because `persistDraft` writes
  // straight through to `setShareGrants`, that loss is PERSISTED, not merely
  // visual. Every mutation therefore composes off this ref, which each write
  // advances immediately, before React has re-rendered.
  const grantsRef = useRef<ShareGrant[]>([]);

  useEffect(() => {
    if (draftGrants === null && settings) {
      const seeded = settings.sharing.grants ?? [];
      grantsRef.current = seeded;
      setDraftGrants(seeded);
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
    grantsRef.current = next;
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

  /** The one way grants change: compose off the latest list, never off the
   * render closure. Grants are addressed by their own id — the same id the
   * relay `action_id` is keyed on — so an edit lands on the grant the owner
   * touched even while another one is being added or removed. */
  const mutateGrants = (mutate: (prev: ShareGrant[]) => ShareGrant[]) => {
    void persistDraft(mutate(grantsRef.current));
  };

  const handleAddGrant = () => {
    mutateGrants((prev) => [...prev, emptyGrant()]);
  };

  const handleRemoveGrant = (grantId: string) => {
    mutateGrants((prev) => prev.filter((g) => (g.id ?? "") !== grantId));
  };

  const updateGrant = (grantId: string, patch: Partial<ShareGrant>) => {
    mutateGrants((prev) =>
      prev.map((g) => ((g.id ?? "") === grantId ? { ...g, ...patch } : g)),
    );
  };

  const handleChooseFolder = async (grantId: string) => {
    try {
      const dir = await open({ directory: true });
      if (typeof dir === "string" && dir.length > 0) {
        updateGrant(grantId, { project_path: dir });
      }
    } catch (err) {
      toast.error(
        t("settings.sharing.grants.folderError", { error: String(err) }),
      );
    }
  };

  const toggleMember = (grantId: string, memberId: string) => {
    mutateGrants((prev) =>
      prev.map((g) => {
        if ((g.id ?? "") !== grantId) return g;
        const current = g.allowed_members ?? [];
        return {
          ...g,
          allowed_members: current.includes(memberId)
            ? current.filter((m) => m !== memberId)
            : [...current, memberId],
        };
      }),
    );
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
    label: `${a.name} (${t("settings.sharing.grants.agentKindCli")})`,
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

            {grants.map((grant) => {
              const complete = isGrantComplete(grant);
              const grantId = grant.id ?? "";
              return (
                <div
                  key={grantId}
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
                        updateGrant(grantId, { agent_id: value ?? "" })
                      }
                      isClearable={false}
                      className="flex-1"
                    />
                    <Button
                      type="button"
                      variant="danger-ghost"
                      size="sm"
                      onClick={() => handleRemoveGrant(grantId)}
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
                        onClick={() => void handleChooseFolder(grantId)}
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
                            updateGrant(grantId, { project_path: "" })
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
                                  toggleMember(grantId, member.member_id)
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
