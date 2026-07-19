import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { RadioTower, Trash2, Zap } from "lucide-react";
import type {
  AgentCardSummary,
  AgentDefinition,
  AgentOutputSink,
} from "@/bindings";
import { commands } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Input } from "../../ui/Input";
import { Button } from "../../ui/Button";
import { Dialog } from "../../ui/Dialog";
import { Alert } from "../../ui/Alert";
import { SettingContainer } from "../../ui/SettingContainer";
import { ShortcutInput } from "../ShortcutInput";
import { AgentApiKeyField } from "./AgentApiKeyField";
import { AgentInlineToggle } from "./AgentInlineToggle";

interface RemoteAgentCardProps {
  agent: AgentDefinition;
}

const OUTPUT_SINKS: AgentOutputSink[] = ["panel", "notify", "file"];

/**
 * Remote-agent card (Flow OS increment 3): configures a spec-compliant **A2A**
 * server instead of a local CLI subprocess or a persona-LLM transform. Mirrors
 * `CliAgentCard`'s layout/patterns (header row, `ShortcutInput`,
 * `SettingContainer` rows, optimistic drafts committed via `commands.updateAgent`
 * + `refreshSettings`, Alert-based status). The bearer token reuses
 * `AgentApiKeyField` against keyring scope "agent"/agent id.
 */
export const RemoteAgentCard: React.FC<RemoteAgentCardProps> = ({ agent }) => {
  const { t } = useTranslation();
  const { refreshSettings } = useSettings();

  const [pending, setPending] = useState<Record<string, boolean>>({});
  const [nameDraft, setNameDraft] = useState(agent.name);
  const [urlDraft, setUrlDraft] = useState(agent.remote_url ?? "");
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  const [card, setCard] = useState<AgentCardSummary | null>(null);
  const [fetchError, setFetchError] = useState<string | null>(null);

  useEffect(() => setNameDraft(agent.name), [agent.name]);
  useEffect(() => setUrlDraft(agent.remote_url ?? ""), [agent.remote_url]);

  const isPending = (field: string) => pending[field] ?? false;

  const persist = async (
    patch: Partial<AgentDefinition>,
    field: string,
  ): Promise<boolean> => {
    setPending((prev) => ({ ...prev, [field]: true }));
    try {
      const result = await commands.updateAgent({ ...agent, ...patch });
      if (result.status === "error") {
        toast.error(
          t("settings.agents.card.update.error", { error: result.error }),
        );
        return false;
      }
      await refreshSettings();
      return true;
    } finally {
      setPending((prev) => ({ ...prev, [field]: false }));
    }
  };

  const commitName = () => {
    const trimmed = nameDraft.trim();
    if (!trimmed) {
      setNameDraft(agent.name);
      return;
    }
    if (trimmed === agent.name) return;
    void persist({ name: trimmed }, "name");
  };

  const commitUrl = () => {
    const trimmed = urlDraft.trim();
    if (trimmed === (agent.remote_url ?? "")) return;
    // Changing the URL invalidates the cached endpoint/card metadata.
    void persist(
      {
        remote_url: trimmed,
        remote_endpoint: "",
        remote_card_name: "",
        remote_card_version: "",
        remote_streaming: false,
      },
      "remote_url",
    );
    setCard(null);
    setFetchError(null);
  };

  const handleFetchCard = async () => {
    // Persist any pending URL edit first so the backend fetches the right one.
    const trimmed = urlDraft.trim();
    if (trimmed !== (agent.remote_url ?? "")) {
      const ok = await persist(
        {
          remote_url: trimmed,
          remote_endpoint: "",
          remote_card_name: "",
          remote_card_version: "",
          remote_streaming: false,
        },
        "remote_url",
      );
      if (!ok) return;
    }
    setPending((prev) => ({ ...prev, fetch: true }));
    setFetchError(null);
    try {
      const result = await commands.fetchRemoteAgentCard(agent.id);
      if (result.status === "error") {
        setCard(null);
        setFetchError(result.error);
        return;
      }
      setCard(result.data);
      await refreshSettings();
    } catch (err) {
      setCard(null);
      setFetchError(String(err));
    } finally {
      setPending((prev) => ({ ...prev, fetch: false }));
    }
  };

  const handleDelete = async () => {
    setIsDeleting(true);
    try {
      const result = await commands.deleteAgent(agent.id);
      if (result.status === "error") {
        toast.error(
          t("settings.agents.card.delete.error", { error: result.error }),
        );
        return;
      }
      await refreshSettings();
      toast.success(
        t("settings.agents.card.delete.success", { name: agent.name }),
      );
      setConfirmingDelete(false);
    } finally {
      setIsDeleting(false);
    }
  };

  const toggleOutputSink = (sink: AgentOutputSink) => {
    const current = agent.output_sinks ?? ["panel"];
    const has = current.includes(sink);
    let next: AgentOutputSink[];
    if (has) {
      // An agent must always land its output somewhere (defaults to Panel).
      if (current.length <= 1) return;
      next = current.filter((s) => s !== sink);
    } else {
      next = [...current, sink];
    }
    void persist({ output_sinks: next }, "output_sinks");
  };

  const activeOutputSinks = agent.output_sinks ?? ["panel"];

  // Prefer the just-fetched summary; fall back to what's cached on the agent so
  // a previously-fetched card still shows after a reload.
  const cardName = card?.name ?? agent.remote_card_name ?? "";
  const cardVersion = card?.version ?? agent.remote_card_version ?? "";
  const cardStreaming = card?.streaming ?? agent.remote_streaming ?? false;
  const cardEndpoint = card?.endpoint ?? agent.remote_endpoint ?? "";
  const hasCard = cardName.length > 0 || cardEndpoint.length > 0;

  return (
    <div className="bg-background border border-mid-gray/20 rounded-lg divide-y divide-mid-gray/20">
      <div className="flex items-center gap-3 px-4 py-3">
        <Input
          type="text"
          variant="compact"
          value={nameDraft}
          disabled={isPending("name")}
          onChange={(event) => setNameDraft(event.target.value)}
          onBlur={commitName}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              event.currentTarget.blur();
            }
          }}
          placeholder={t("settings.agents.card.name.placeholder")}
          className="flex-1 min-w-0 font-semibold"
          aria-label={t("settings.agents.card.name.label")}
        />
        <AgentInlineToggle
          checked={agent.enabled ?? true}
          disabled={isPending("enabled")}
          onChange={(checked) => void persist({ enabled: checked }, "enabled")}
          label={t("settings.agents.card.enabled.label")}
        />
        <Button
          type="button"
          variant="danger-ghost"
          size="sm"
          onClick={() => setConfirmingDelete(true)}
          aria-label={t("settings.agents.card.delete.button")}
          title={t("settings.agents.card.delete.button")}
          className="shrink-0"
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>

      <ShortcutInput shortcutId={agent.binding_id} grouped />

      <SettingContainer
        title={t("settings.agents.card.remote.url.label")}
        description={t("settings.agents.card.remote.url.description")}
        descriptionMode="tooltip"
        grouped
        layout="stacked"
      >
        <div className="flex gap-2">
          <Input
            type="text"
            variant="compact"
            value={urlDraft}
            disabled={isPending("remote_url")}
            onChange={(event) => setUrlDraft(event.target.value)}
            onBlur={commitUrl}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                event.currentTarget.blur();
              }
            }}
            placeholder={t("settings.agents.card.remote.url.placeholder")}
            className="flex-1 min-w-0"
            aria-label={t("settings.agents.card.remote.url.label")}
          />
          <Button
            type="button"
            variant="secondary"
            size="md"
            onClick={() => void handleFetchCard()}
            disabled={isPending("fetch") || !urlDraft.trim()}
            className="inline-flex shrink-0 items-center gap-1.5"
          >
            <RadioTower className="h-4 w-4" />
            {isPending("fetch")
              ? t("settings.agents.card.remote.fetch.fetching")
              : t("settings.agents.card.remote.fetch.button")}
          </Button>
        </div>
      </SettingContainer>

      <SettingContainer
        title={t("settings.agents.card.remote.token.label")}
        description={t("settings.agents.card.remote.token.description")}
        descriptionMode="tooltip"
        grouped
        layout="horizontal"
      >
        <AgentApiKeyField agentId={agent.id} />
      </SettingContainer>

      <SettingContainer
        title={t("settings.agents.card.remote.fetch.label")}
        description={t("settings.agents.card.remote.fetch.description")}
        descriptionMode="tooltip"
        grouped
        layout="stacked"
      >
        <div className="space-y-2">
          {fetchError && (
            <Alert variant="error" contained>
              {t("settings.agents.card.remote.fetch.error", {
                error: fetchError,
              })}
            </Alert>
          )}
          {!fetchError && hasCard && (
            <Alert variant="success" contained>
              <div className="space-y-1.5">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-semibold">{cardName}</span>
                  {cardVersion && (
                    <span className="text-xs text-mid-gray">
                      {t("settings.agents.card.remote.fetch.version", {
                        version: cardVersion,
                      })}
                    </span>
                  )}
                  {cardStreaming && (
                    <span className="inline-flex items-center gap-1 rounded-full bg-background-ui/60 px-2 py-0.5 text-xs">
                      <Zap className="h-3 w-3" />
                      {t("settings.agents.card.remote.fetch.streaming")}
                    </span>
                  )}
                </div>
                {cardEndpoint && (
                  <p
                    className="truncate text-xs text-mid-gray"
                    title={cardEndpoint}
                  >
                    {t("settings.agents.card.remote.fetch.endpoint", {
                      endpoint: cardEndpoint,
                    })}
                  </p>
                )}
                {card?.auth_schemes && card.auth_schemes.length > 0 && (
                  <p className="text-xs text-mid-gray">
                    {t("settings.agents.card.remote.fetch.auth", {
                      schemes: card.auth_schemes.join(", "),
                    })}
                  </p>
                )}
                {card?.skills && card.skills.length > 0 && (
                  <div className="flex flex-wrap gap-1.5 pt-0.5">
                    {card.skills.map((skill) => (
                      <span
                        key={skill.id}
                        className="rounded-full bg-background-ui/60 px-2 py-0.5 text-xs"
                        title={skill.id}
                      >
                        {skill.name}
                      </span>
                    ))}
                  </div>
                )}
              </div>
            </Alert>
          )}
          {!fetchError && !hasCard && (
            <p className="text-sm text-mid-gray">
              {t("settings.agents.card.remote.fetch.notFetched")}
            </p>
          )}
        </div>
      </SettingContainer>

      <SettingContainer
        title={t("settings.agents.card.remote.outputSinks.label")}
        description={t("settings.agents.card.remote.outputSinks.description")}
        descriptionMode="tooltip"
        grouped
        layout="horizontal"
      >
        <div className="flex items-center gap-3">
          {OUTPUT_SINKS.map((sink) => (
            <label
              key={sink}
              className="flex items-center gap-1.5 text-sm cursor-pointer"
            >
              <input
                type="checkbox"
                checked={activeOutputSinks.includes(sink)}
                disabled={isPending("output_sinks")}
                onChange={() => toggleOutputSink(sink)}
                className="h-4 w-4 rounded border-mid-gray/80 accent-background-ui"
              />
              {t(`settings.agents.card.remote.outputSinks.${sink}`)}
            </label>
          ))}
        </div>
      </SettingContainer>

      <Dialog
        open={confirmingDelete}
        onOpenChange={setConfirmingDelete}
        title={t("settings.agents.card.delete.confirmTitle", {
          name: agent.name,
        })}
        description={t("settings.agents.card.delete.confirmDescription")}
        closeLabel={t("settings.agents.card.delete.cancel")}
        footer={
          <>
            <Button
              type="button"
              variant="secondary"
              size="md"
              onClick={() => setConfirmingDelete(false)}
              disabled={isDeleting}
            >
              {t("settings.agents.card.delete.cancel")}
            </Button>
            <Button
              type="button"
              variant="danger"
              size="md"
              onClick={handleDelete}
              disabled={isDeleting}
            >
              {t("settings.agents.card.delete.confirm")}
            </Button>
          </>
        }
      >
        <></>
      </Dialog>
    </div>
  );
};
