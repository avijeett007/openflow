import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Bot, Plus, Terminal } from "lucide-react";
import type { AgentCliType, AgentDefinition } from "@/bindings";
import { commands } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Button } from "../../ui/Button";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { AgentCard } from "./AgentCard";
import { AGENT_TEMPLATES, slugify, uniqueAgentId } from "./agentTemplates";

/** Default CLI type for a freshly-created CLI agent - `claude` is the
 * verified/installed integration, so it gives the best out-of-the-box
 * experience; the user can switch types on the card afterwards. */
const DEFAULT_CLI_TYPE: AgentCliType = "claude";

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

  const handleAddCli = async () => {
    setCreatingKey("cli");
    try {
      const name = t("settings.agents.addAgent.cliAgent");
      const existingIds = new Set(agents.map((agent) => agent.id));
      const id = uniqueAgentId(slugify(name), existingIds);

      // Prefill the command template/prompt-delivery and auto-detect the
      // binary for the default CLI type, same as changing the "Agent type"
      // dropdown on the card does - so a freshly-created CLI agent is ready
      // to run immediately instead of needing the user to touch the
      // dropdown first.
      let commandTemplate = "";
      let promptVia: AgentDefinition["prompt_via"] = "stdin";
      try {
        const defaults = await commands.getCliAgentDefaults(DEFAULT_CLI_TYPE);
        commandTemplate = defaults.command_template;
        promptVia = defaults.prompt_via;
      } catch {
        // Leave blank; the card's "Agent type" dropdown lets the user
        // retry by reselecting a type.
      }

      let binaryPath = "";
      const detected = await commands.detectAgentBinary(DEFAULT_CLI_TYPE);
      if (detected.status === "ok") {
        binaryPath = detected.data;
      }

      const result = await commands.createAgent({
        id,
        name,
        binding_id: `agent:${id}`,
        provider_id: providers[0]?.id ?? "",
        kind: "cli",
        cli_type: DEFAULT_CLI_TYPE,
        binary_path: binaryPath,
        command_template: commandTemplate,
        project_path: "",
        output_sinks: ["panel"],
        prompt_via: promptVia,
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

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.agents.title")}>
        <div className="px-4 py-3 space-y-3">
          <p className="text-sm text-mid-gray">{t("settings.agents.intro")}</p>
        </div>
      </SettingsGroup>

      <SettingsGroup title={t("settings.agents.addAgent.title")}>
        <div className="px-4 py-3 space-y-4">
          <div>
            <p className="text-xs font-medium text-mid-gray uppercase tracking-wide mb-2">
              {t("settings.agents.addAgent.promptSectionTitle")}
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
          </div>

          <div>
            <p className="text-xs font-medium text-mid-gray uppercase tracking-wide mb-2">
              {t("settings.agents.addAgent.cliSectionTitle")}
            </p>
            <div className="flex flex-wrap gap-2">
              <Button
                type="button"
                variant="secondary"
                size="md"
                onClick={() => void handleAddCli()}
                disabled={creatingKey !== null}
                className="inline-flex items-center gap-1.5"
                title={t("settings.agents.addAgent.cliDescription")}
              >
                <Terminal className="h-4 w-4" />
                {t("settings.agents.addAgent.cliAgent")}
              </Button>
            </div>
          </div>
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
