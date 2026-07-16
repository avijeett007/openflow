import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Plus, Sparkles } from "lucide-react";
import { commands } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Button } from "../../ui/Button";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { AiModeCard } from "./AiModeCard";
import {
  MODE_TEMPLATES,
  slugify,
  uniqueModeId,
  type ModeTemplate,
} from "./modeTemplates";

/**
 * The AI Modes list that sits below the built-in "Write" card in the AI Modes
 * section. Modes are additive `ai_modes` entries; the Write card above is the
 * unchanged cleanup pipeline. Mirrors the Agents section's add-from-template +
 * blank + card-list conventions.
 */
export const AiModesSection: React.FC = () => {
  const { t } = useTranslation();
  const { settings, refreshSettings } = useSettings();
  const [creatingKey, setCreatingKey] = useState<string | null>(null);

  const modes = settings?.ai_modes ?? [];
  const providers = settings?.post_process_providers ?? [];

  const createMode = async (
    creationKey: string,
    name: string,
    kind: ModeTemplate["kind"],
    prompt: string,
    appRules: string[],
  ) => {
    setCreatingKey(creationKey);
    try {
      const existingIds = new Set(modes.map((mode) => mode.id));
      const id = uniqueModeId(slugify(name), existingIds);

      const result = await commands.createAiMode({
        id,
        name,
        binding_id: `mode:${id}`,
        kind,
        prompt,
        provider_id: null,
        model: null,
        app_rules: appRules,
        enabled: true,
      });

      if (result.status === "error") {
        toast.error(
          t("settings.aiModes.addMode.error", { error: result.error }),
        );
        return;
      }

      await refreshSettings();
      toast.success(t("settings.aiModes.addMode.created", { name }));
    } finally {
      setCreatingKey(null);
    }
  };

  const handleAddBlank = () =>
    void createMode(
      "blank",
      t("settings.aiModes.addMode.blank"),
      "rewrite",
      "",
      [],
    );

  const handleAddFromTemplate = (template: ModeTemplate) =>
    void createMode(
      template.key,
      t(template.nameKey),
      template.kind,
      template.prompt,
      template.appRules,
    );

  return (
    <>
      <SettingsGroup title={t("settings.aiModes.addMode.title")}>
        <div className="px-4 py-3 space-y-3">
          <p className="text-sm text-mid-gray">
            {t("settings.aiModes.addMode.intro")}
          </p>
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              variant="secondary"
              size="md"
              onClick={handleAddBlank}
              disabled={creatingKey !== null}
              className="inline-flex items-center gap-1.5"
            >
              <Plus className="h-4 w-4" />
              {t("settings.aiModes.addMode.blank")}
            </Button>
            {MODE_TEMPLATES.map((template) => (
              <Button
                key={template.key}
                type="button"
                variant="secondary"
                size="md"
                onClick={() => handleAddFromTemplate(template)}
                disabled={creatingKey !== null}
                className="inline-flex items-center gap-1.5"
                title={t(template.descriptionKey)}
              >
                <Sparkles className="h-4 w-4" />
                {t(template.nameKey)}
              </Button>
            ))}
          </div>
        </div>
      </SettingsGroup>

      {modes.length === 0 ? (
        <div className="rounded-lg border border-dashed border-mid-gray/30 px-4 py-8 text-center text-sm text-mid-gray">
          {t("settings.aiModes.emptyState")}
        </div>
      ) : (
        <div className="space-y-4">
          {modes.map((mode) => (
            <AiModeCard key={mode.id} mode={mode} providers={providers} />
          ))}
        </div>
      )}
    </>
  );
};
