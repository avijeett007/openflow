import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Bot, FolderGit2, Plus, Terminal } from "lucide-react";
import type { AgentDefinition } from "@/bindings";
import { commands } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { useOsType } from "@/hooks/useOsType";
import { useNavigationStore } from "@/stores/navigationStore";
import { formatKeyCombination } from "@/lib/utils/keyboard";
import { Card } from "../../ui/Card";
import { AgentInlineToggle } from "../agents/AgentInlineToggle";
import { ModuleHeader } from "./ModuleHeader";
import { MissionControlEmptyState } from "./MissionControlEmptyState";

const basename = (path: string): string =>
  path.split(/[/\\]/).filter(Boolean).pop() ?? path;

interface AgentRailCardProps {
  agent: AgentDefinition;
  hotkey: string;
}

const AgentRailCard: React.FC<AgentRailCardProps> = ({ agent, hotkey }) => {
  const { t } = useTranslation();
  const { refreshSettings } = useSettings();
  const [pending, setPending] = useState(false);

  const isCli = agent.kind === "cli";

  const toggleEnabled = async (enabled: boolean) => {
    setPending(true);
    try {
      const result = await commands.updateAgent({ ...agent, enabled });
      if (result.status === "error") {
        toast.error(
          t("settings.missionControl.agents.toggleError", {
            error: result.error,
          }),
        );
        return;
      }
      await refreshSettings();
    } finally {
      setPending(false);
    }
  };

  return (
    <Card
      padding="md"
      raised
      className="flex w-56 shrink-0 flex-col gap-3 snap-start"
    >
      <div className="flex items-start justify-between gap-2">
        <div className="flex items-center gap-2 min-w-0">
          <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-lg bg-of-violet/12 text-of-violet">
            {isCli ? (
              <Terminal className="h-4 w-4" />
            ) : (
              <Bot className="h-4 w-4" />
            )}
          </span>
          <span className="font-medium text-sm truncate">{agent.name}</span>
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-1.5">
        <span className="inline-flex items-center rounded-md border border-of-hairline px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-text/55">
          {isCli
            ? t("settings.missionControl.agents.kindCli")
            : t("settings.missionControl.agents.kindPrompt")}
        </span>
        {hotkey ? (
          <span className="inline-flex items-center rounded-md bg-mid-gray/15 px-1.5 py-0.5 font-mono text-[10px] text-text/70">
            {hotkey}
          </span>
        ) : (
          <span className="text-[10px] text-text/40">
            {t("settings.missionControl.agents.noHotkey")}
          </span>
        )}
      </div>

      {isCli && agent.project_path ? (
        <div
          className="flex items-center gap-1.5 text-xs text-text/50 min-w-0"
          title={agent.project_path}
        >
          <FolderGit2 className="h-3.5 w-3.5 shrink-0" />
          <span className="truncate">{basename(agent.project_path)}</span>
        </div>
      ) : (
        <div className="h-4" />
      )}

      <div className="mt-auto flex items-center justify-end border-t border-of-hairline pt-2.5">
        <AgentInlineToggle
          checked={agent.enabled ?? true}
          disabled={pending}
          onChange={(checked) => void toggleEnabled(checked)}
          label={t("settings.missionControl.agents.enabled")}
        />
      </div>
    </Card>
  );
};

export const AgentsRail: React.FC = () => {
  const { t } = useTranslation();
  const { settings } = useSettings();
  const osType = useOsType();
  const setCurrentSection = useNavigationStore((s) => s.setCurrentSection);

  const agents = settings?.agents ?? [];
  const bindings = settings?.bindings ?? {};

  return (
    <section>
      <ModuleHeader
        title={t("settings.missionControl.agents.title")}
        actionLabel={t("settings.missionControl.agents.manage")}
        onAction={() => setCurrentSection("agents")}
      />
      {agents.length === 0 ? (
        <Card padding="sm">
          <MissionControlEmptyState
            icon={Bot}
            message={t("settings.missionControl.agents.empty")}
            actionLabel={t("settings.missionControl.agents.createAgent")}
            onAction={() => setCurrentSection("agents")}
          />
        </Card>
      ) : (
        <div className="flex gap-3 overflow-x-auto pb-2 snap-x">
          {agents.map((agent) => {
            const binding = bindings[agent.binding_id]?.current_binding ?? "";
            return (
              <AgentRailCard
                key={agent.id}
                agent={agent}
                hotkey={formatKeyCombination(binding, osType)}
              />
            );
          })}
          <button
            type="button"
            onClick={() => setCurrentSection("agents")}
            className="flex w-40 shrink-0 flex-col items-center justify-center gap-2 rounded-xl border border-dashed border-of-hairline text-text/50 hover:border-of-violet/50 hover:text-of-violet transition-colors cursor-pointer snap-start"
          >
            <Plus className="h-5 w-5" />
            <span className="text-xs font-medium">
              {t("settings.missionControl.agents.newAgent")}
            </span>
          </button>
        </div>
      )}
    </section>
  );
};
