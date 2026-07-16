import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Trash2 } from "lucide-react";
import type { AiMode, AiModeKind, PostProcessProvider } from "@/bindings";
import { commands } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Input } from "../../ui/Input";
import { Textarea } from "../../ui/Textarea";
import { Button } from "../../ui/Button";
import { Dialog } from "../../ui/Dialog";
import { SettingContainer } from "../../ui/SettingContainer";
import { Dropdown } from "../../ui/Dropdown";
import { ShortcutInput } from "../ShortcutInput";
import { AgentInlineToggle } from "../agents/AgentInlineToggle";
import { AppRulesEditor } from "./AppRulesEditor";
import { AiModeTestPanel } from "./AiModeTestPanel";

interface AiModeCardProps {
  mode: AiMode;
  providers: PostProcessProvider[];
}

/** Sentinel for "inherit the default cleanup provider" in the provider dropdown. */
const INHERIT = "__inherit__";

export const AiModeCard: React.FC<AiModeCardProps> = ({ mode, providers }) => {
  const { t } = useTranslation();
  const { refreshSettings } = useSettings();

  const [pending, setPending] = useState<Record<string, boolean>>({});
  const [nameDraft, setNameDraft] = useState(mode.name);
  const [promptDraft, setPromptDraft] = useState(mode.prompt ?? "");
  const [modelDraft, setModelDraft] = useState(mode.model ?? "");
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  useEffect(() => setNameDraft(mode.name), [mode.name]);
  useEffect(() => setPromptDraft(mode.prompt ?? ""), [mode.prompt]);
  useEffect(() => setModelDraft(mode.model ?? ""), [mode.model]);

  const isPending = (field: string) => pending[field] ?? false;

  const persist = async (
    patch: Partial<AiMode>,
    field: string,
  ): Promise<boolean> => {
    setPending((prev) => ({ ...prev, [field]: true }));
    try {
      const result = await commands.updateAiMode({ ...mode, ...patch });
      if (result.status === "error") {
        toast.error(
          t("settings.aiModes.card.update.error", { error: result.error }),
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
      setNameDraft(mode.name);
      return;
    }
    if (trimmed === mode.name) return;
    void persist({ name: trimmed }, "name");
  };

  const commitPrompt = () => {
    if (promptDraft === (mode.prompt ?? "")) return;
    void persist({ prompt: promptDraft }, "prompt");
  };

  const commitModel = () => {
    const trimmed = modelDraft.trim();
    if (trimmed === (mode.model ?? "")) return;
    void persist({ model: trimmed || null }, "model");
  };

  const kindOptions = [
    { value: "rewrite", label: t("settings.aiModes.card.kind.rewrite") },
    { value: "command", label: t("settings.aiModes.card.kind.command") },
    { value: "direct", label: t("settings.aiModes.card.kind.direct") },
  ];

  const providerOptions = [
    { value: INHERIT, label: t("settings.aiModes.card.provider.inherit") },
    ...providers.map((provider) => ({
      value: provider.id,
      label: provider.label,
    })),
  ];

  const isDirect = mode.kind === "direct";

  const handleDelete = async () => {
    setIsDeleting(true);
    try {
      const result = await commands.deleteAiMode(mode.id);
      if (result.status === "error") {
        toast.error(
          t("settings.aiModes.card.delete.error", { error: result.error }),
        );
        return;
      }
      await refreshSettings();
      toast.success(
        t("settings.aiModes.card.delete.success", { name: mode.name }),
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
          placeholder={t("settings.aiModes.card.name.placeholder")}
          className="flex-1 min-w-0 font-semibold"
          aria-label={t("settings.aiModes.card.name.label")}
        />
        <AgentInlineToggle
          checked={mode.enabled ?? true}
          disabled={isPending("enabled")}
          onChange={(checked) => void persist({ enabled: checked }, "enabled")}
          label={t("settings.aiModes.card.enabled.label")}
        />
        <Button
          type="button"
          variant="danger-ghost"
          size="sm"
          onClick={() => setConfirmingDelete(true)}
          aria-label={t("settings.aiModes.card.delete.button")}
          title={t("settings.aiModes.card.delete.button")}
          className="shrink-0"
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>

      <SettingContainer
        title={t("settings.aiModes.card.kind.label")}
        description={t("settings.aiModes.card.kind.description")}
        descriptionMode="tooltip"
        grouped
        layout="horizontal"
      >
        <Dropdown
          options={kindOptions}
          selectedValue={mode.kind ?? "rewrite"}
          onSelect={(value) =>
            void persist({ kind: value as AiModeKind }, "kind")
          }
          disabled={isPending("kind")}
          className="min-w-[220px]"
        />
      </SettingContainer>

      <ShortcutInput shortcutId={mode.binding_id} grouped />

      <SettingContainer
        title={t("settings.aiModes.card.appRules.label")}
        description={t("settings.aiModes.card.appRules.description")}
        descriptionMode="tooltip"
        grouped
        layout="stacked"
      >
        <AppRulesEditor
          rules={mode.app_rules ?? []}
          disabled={isPending("app_rules")}
          onChange={(rules) => void persist({ app_rules: rules }, "app_rules")}
        />
      </SettingContainer>

      {!isDirect && (
        <>
          <SettingContainer
            title={t("settings.aiModes.card.provider.label")}
            description={t("settings.aiModes.card.provider.description")}
            descriptionMode="tooltip"
            grouped
            layout="horizontal"
          >
            <Dropdown
              options={providerOptions}
              selectedValue={mode.provider_id ?? INHERIT}
              onSelect={(value) =>
                void persist(
                  { provider_id: value === INHERIT ? null : value },
                  "provider",
                )
              }
              disabled={isPending("provider")}
              className="min-w-[220px]"
            />
          </SettingContainer>

          <SettingContainer
            title={t("settings.aiModes.card.model.label")}
            description={t("settings.aiModes.card.model.description")}
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
              placeholder={t("settings.aiModes.card.model.placeholder")}
              className="min-w-[220px]"
              aria-label={t("settings.aiModes.card.model.label")}
            />
          </SettingContainer>

          <SettingContainer
            title={t("settings.aiModes.card.prompt.label")}
            description={t("settings.aiModes.card.prompt.description")}
            descriptionMode="tooltip"
            grouped
            layout="stacked"
          >
            <Textarea
              value={promptDraft}
              disabled={isPending("prompt")}
              onChange={(event) => setPromptDraft(event.target.value)}
              onBlur={commitPrompt}
              placeholder={t("settings.aiModes.card.prompt.placeholder")}
              className="w-full"
            />
          </SettingContainer>

          <SettingContainer
            title={t("settings.aiModes.card.test.label")}
            description={t("settings.aiModes.card.test.description")}
            descriptionMode="tooltip"
            grouped
            layout="stacked"
          >
            <AiModeTestPanel modeId={mode.id} />
          </SettingContainer>
        </>
      )}

      {isDirect && (
        <div className="px-4 py-3 text-sm text-mid-gray">
          {t("settings.aiModes.card.directHint")}
        </div>
      )}

      <Dialog
        open={confirmingDelete}
        onOpenChange={setConfirmingDelete}
        title={t("settings.aiModes.card.delete.confirmTitle", {
          name: mode.name,
        })}
        description={t("settings.aiModes.card.delete.confirmDescription")}
        closeLabel={t("settings.aiModes.card.delete.cancel")}
        footer={
          <>
            <Button
              type="button"
              variant="secondary"
              size="md"
              onClick={() => setConfirmingDelete(false)}
              disabled={isDeleting}
            >
              {t("settings.aiModes.card.delete.cancel")}
            </Button>
            <Button
              type="button"
              variant="danger"
              size="md"
              onClick={handleDelete}
              disabled={isDeleting}
            >
              {t("settings.aiModes.card.delete.confirm")}
            </Button>
          </>
        }
      >
        <></>
      </Dialog>
    </div>
  );
};
