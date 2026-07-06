import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Bot, Plus } from "lucide-react";
import type { AgentDefinition } from "@/bindings";
import { commands } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Button } from "../../ui/Button";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { AgentCard } from "./AgentCard";
import { AGENT_TEMPLATES, slugify, uniqueAgentId } from "./agentTemplates";

export const AgentsSettings: React.FC = () => {
  const { t } = useTranslation();
  const { settings, refreshSettings } = useSettings();
  const [creatingKey, setCreatingKey] = useState<string | null>(null);

  const agents = settings?.agents ?? [];
  const providers = settings?.post_process_providers ?? [];

  const createAgent = async (
    creationKey: string,
    name: string,
    systemPrompt: string,
    outputMode: AgentDefinition["output_mode"],
  ) => {
    setCreatingKey(creationKey);
    try {
      const existingIds = new Set(agents.map((agent) => agent.id));
      const id = uniqueAgentId(slugify(name), existingIds);
      const providerId = providers[0]?.id ?? "";

      const result = await commands.createAgent({
        id,
        name,
        binding_id: `agent:${id}`,
        provider_id: providerId,
        model: "",
        system_prompt: systemPrompt,
        output_mode: outputMode,
        enabled: true,
      });

      if (result.status === "error") {
        toast.error(
          t("settings.agents.addAgent.error", { error: result.error }),
        );
        return;
      }

      await refreshSettings();
      toast.success(t("settings.agents.addAgent.created", { name }));
    } finally {
      setCreatingKey(null);
    }
  };

  const handleAddBlank = () =>
    void createAgent(
      "blank",
      t("settings.agents.addAgent.blank"),
      "",
      "inject",
    );

  const handleAddFromTemplate = (template: (typeof AGENT_TEMPLATES)[number]) =>
    void createAgent(
      template.key,
      t(template.nameKey),
      template.systemPrompt,
      template.outputMode,
    );

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.agents.title")}>
        <div className="px-4 py-3 space-y-3">
          <p className="text-sm text-mid-gray">{t("settings.agents.intro")}</p>
        </div>
      </SettingsGroup>

      <SettingsGroup title={t("settings.agents.addAgent.title")}>
        <div className="px-4 py-3 flex flex-wrap gap-2">
          <Button
            type="button"
            variant="secondary"
            size="md"
            onClick={handleAddBlank}
            disabled={creatingKey !== null}
            className="inline-flex items-center gap-1.5"
          >
            <Plus className="h-4 w-4" />
            {t("settings.agents.addAgent.blank")}
          </Button>
          {AGENT_TEMPLATES.map((template) => (
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
              <Bot className="h-4 w-4" />
              {t(template.nameKey)}
            </Button>
          ))}
        </div>
      </SettingsGroup>

      {agents.length === 0 ? (
        <div className="rounded-lg border border-dashed border-mid-gray/30 px-4 py-8 text-center text-sm text-mid-gray">
          {t("settings.agents.emptyState")}
        </div>
      ) : (
        <div className="space-y-4">
          {agents.map((agent) => (
            <AgentCard key={agent.id} agent={agent} providers={providers} />
          ))}
        </div>
      )}
    </div>
  );
};
