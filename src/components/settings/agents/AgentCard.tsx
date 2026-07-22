import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Trash2 } from "lucide-react";
import type {
  AgentDefinition,
  AgentOutputMode,
  PostProcessProvider,
} from "@/bindings";
import { commands } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Input } from "../../ui/Input";
import { Textarea } from "../../ui/Textarea";
import { Button } from "../../ui/Button";
import { Dialog } from "../../ui/Dialog";
import { SettingContainer } from "../../ui/SettingContainer";
import { Dropdown } from "../../ui/Dropdown";
import { ShortcutInput } from "../ShortcutInput";
import { AgentApiKeyField } from "./AgentApiKeyField";
import { AgentTestPanel } from "./AgentTestPanel";
import { AgentInlineToggle } from "./AgentInlineToggle";
import { CliAgentCard } from "./CliAgentCard";
import { RemoteAgentCard } from "./RemoteAgentCard";

interface AgentCardProps {
  agent: AgentDefinition;
  providers: PostProcessProvider[];
}

export const AgentCard: React.FC<AgentCardProps> = ({ agent, providers }) => {
  const { t } = useTranslation();
  const { refreshSettings } = useSettings();

  // CLI agents (increment 2) render an entirely different card - the
  // prompt-agent fields below (provider/model/system prompt/output mode)
  // don't apply to them. `kind` defaults to "prompt" for every agent stored
  // before this discriminator existed, so this branch never affects
  // increment-1 agents.
  if (agent.kind === "cli") {
    return <CliAgentCard agent={agent} />;
  }

  // Remote (A2A) agents (increment 3) render their own card — the prompt-agent
  // fields below don't apply. Like `cli`, `kind` defaults to "prompt" for every
  // agent stored before this discriminator existed, so this never affects
  // increment-1 agents.
  if (agent.kind === "remote") {
    return <RemoteAgentCard agent={agent} />;
  }

  const [pending, setPending] = useState<Record<string, boolean>>({});
  const [nameDraft, setNameDraft] = useState(agent.name);
  const [promptDraft, setPromptDraft] = useState(agent.system_prompt ?? "");
  const [modelDraft, setModelDraft] = useState(agent.model ?? "");
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  useEffect(() => setNameDraft(agent.name), [agent.name]);
  useEffect(
    () => setPromptDraft(agent.system_prompt ?? ""),
    [agent.system_prompt],
  );
  useEffect(() => setModelDraft(agent.model ?? ""), [agent.model]);

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

  const commitPrompt = () => {
    const trimmed = promptDraft;
    if (trimmed === (agent.system_prompt ?? "")) return;
    void persist({ system_prompt: trimmed }, "system_prompt");
  };

  const commitModel = () => {
    const trimmed = modelDraft.trim();
    if (trimmed === (agent.model ?? "")) return;
    void persist({ model: trimmed }, "model");
  };

  const providerOptions = providers.map((provider) => ({
    value: provider.id,
    label: provider.label,
  }));

  const outputModeOptions = [
    { value: "inject", label: t("settings.agents.card.outputMode.inject") },
    {
      value: "clipboard",
      label: t("settings.agents.card.outputMode.clipboard"),
    },
  ];

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
        title={t("settings.agents.card.provider.label")}
        description={t("settings.agents.card.provider.description")}
        descriptionMode="tooltip"
        grouped
        layout="horizontal"
      >
        <Dropdown
          options={providerOptions}
          selectedValue={agent.provider_id}
          onSelect={(value) => void persist({ provider_id: value }, "provider")}
          disabled={isPending("provider")}
          className="min-w-[220px]"
        />
      </SettingContainer>

      <SettingContainer
        title={t("settings.agents.card.model.label")}
        description={t("settings.agents.card.model.description")}
        descriptionMode="tooltip"
        grouped
        layout="horizontal"
      >
        <Input
          type="text"
          variant="compact"
          value={modelDraft}
          disabled={isPending("model")}
          onChange={(event) => setModelDraft(event.target.value)}
          onBlur={commitModel}
          placeholder={t("settings.agents.card.model.placeholder")}
          className="min-w-[220px]"
          aria-label={t("settings.agents.card.model.label")}
        />
      </SettingContainer>

      <SettingContainer
        title={t("settings.agents.card.apiKey.label")}
        description={t("settings.agents.card.apiKey.description")}
        descriptionMode="tooltip"
        grouped
        layout="horizontal"
      >
        <AgentApiKeyField agentId={agent.id} />
      </SettingContainer>

      <SettingContainer
        title={t("settings.agents.card.outputMode.label")}
        description={t("settings.agents.card.outputMode.description")}
        descriptionMode="tooltip"
        grouped
        layout="horizontal"
      >
        <Dropdown
          options={outputModeOptions}
          selectedValue={agent.output_mode ?? "inject"}
          onSelect={(value) =>
            void persist(
              { output_mode: value as AgentOutputMode },
              "output_mode",
            )
          }
          disabled={isPending("output_mode")}
          className="min-w-[220px]"
        />
      </SettingContainer>

      <SettingContainer
        title={t("settings.agents.card.systemPrompt.label")}
        description={t("settings.agents.card.systemPrompt.description")}
        descriptionMode="tooltip"
        grouped
        layout="stacked"
      >
        <Textarea
          value={promptDraft}
          disabled={isPending("system_prompt")}
          onChange={(event) => setPromptDraft(event.target.value)}
          onBlur={commitPrompt}
          placeholder={t("settings.agents.card.systemPrompt.placeholder")}
          className="w-full"
        />
      </SettingContainer>

      <SettingContainer
        title={t("settings.agents.card.test.label")}
        description={t("settings.agents.card.test.description")}
        descriptionMode="tooltip"
        grouped
        layout="stacked"
      >
        <AgentTestPanel agentId={agent.id} />
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
