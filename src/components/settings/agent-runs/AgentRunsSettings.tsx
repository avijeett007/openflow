import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Trash2 } from "lucide-react";
import type { AgentRunInfo, RunStatus } from "@/bindings";
import { commands, events } from "@/bindings";
import { Button } from "../../ui/Button";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { AgentRunRow } from "./AgentRunRow";

/**
 * Flow OS increment 2 "mini Mission Control": a live view of local CLI
 * coding-agent runs. Seeds from `list_agent_runs` on mount, then keeps itself
 * current by subscribing to the `agent-run-output` / `agent-run-status`
 * events (same subscribe/unlisten-on-unmount pattern as
 * `HistorySettings`'s `historyUpdatePayload` listener). A run that starts
 * while this panel is open won't be in the initial snapshot, so on an event
 * for an unrecognized `run_id` we re-fetch the full list instead of dropping
 * the update.
 */
export const AgentRunsSettings: React.FC = () => {
  const { t } = useTranslation();
  const [runs, setRuns] = useState<AgentRunInfo[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [isClearing, setIsClearing] = useState(false);
  const [stoppingIds, setStoppingIds] = useState<Set<string>>(new Set());
  const knownIdsRef = useRef<Set<string>>(new Set());

  const refresh = useCallback(async () => {
    const list = await commands.listAgentRuns();
    const sorted = [...list].sort((a, b) => b.started_at_ms - a.started_at_ms);
    knownIdsRef.current = new Set(sorted.map((run) => run.run_id));
    setRuns(sorted);
  }, []);

  useEffect(() => {
    let cancelled = false;
    setIsLoading(true);
    void refresh().finally(() => {
      if (!cancelled) setIsLoading(false);
    });

    const unlistenOutput = events.agentRunOutput.listen((event) => {
      const { run_id, chunk } = event.payload;
      if (!knownIdsRef.current.has(run_id)) {
        void refresh();
        return;
      }
      setRuns((prev) =>
        prev.map((run) =>
          run.run_id === run_id ? { ...run, output: run.output + chunk } : run,
        ),
      );
    });

    const unlistenStatus = events.agentRunStatus.listen((event) => {
      const { run_id, status } = event.payload;
      if (!knownIdsRef.current.has(run_id)) {
        void refresh();
        return;
      }
      setRuns((prev) =>
        prev.map((run) => (run.run_id === run_id ? { ...run, status } : run)),
      );
    });

    return () => {
      cancelled = true;
      unlistenOutput.then((fn) => fn());
      unlistenStatus.then((fn) => fn());
    };
  }, [refresh]);

  const handleStop = async (runId: string) => {
    setStoppingIds((prev) => new Set(prev).add(runId));
    try {
      const result = await commands.stopAgentRun(runId);
      if (result.status === "error") {
        toast.error(t("settings.agentRuns.stopError", { error: result.error }));
      }
    } finally {
      setStoppingIds((prev) => {
        const next = new Set(prev);
        next.delete(runId);
        return next;
      });
    }
  };

  const handleClearFinished = async () => {
    setIsClearing(true);
    try {
      const result = await commands.clearFinishedAgentRuns();
      if (result.status === "error") {
        toast.error(
          t("settings.agentRuns.clearError", { error: result.error }),
        );
        return;
      }
      await refresh();
    } finally {
      setIsClearing(false);
    }
  };

  const handleReveal = async (path: string) => {
    try {
      await revealItemInDir(path);
    } catch (err) {
      toast.error(t("settings.agentRuns.revealError", { error: String(err) }));
    }
  };

  const isRunning = (status: RunStatus) => status.status === "running";
  const hasFinishedRuns = runs.some((run) => !isRunning(run.status));

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.agentRuns.title")}>
        <div className="px-4 py-3 flex items-center justify-between gap-3">
          <p className="text-sm text-mid-gray">
            {t("settings.agentRuns.intro")}
          </p>
          <Button
            type="button"
            variant="secondary"
            size="sm"
            onClick={() => void handleClearFinished()}
            disabled={isClearing || !hasFinishedRuns}
            className="inline-flex shrink-0 items-center gap-1.5"
          >
            <Trash2 className="h-4 w-4" />
            {t("settings.agentRuns.clearFinished")}
          </Button>
        </div>
      </SettingsGroup>

      {!isLoading && runs.length === 0 ? (
        <div className="rounded-lg border border-dashed border-mid-gray/30 px-4 py-8 text-center text-sm text-mid-gray">
          {t("settings.agentRuns.emptyState")}
        </div>
      ) : (
        <div className="space-y-4">
          {runs.map((run) => (
            <AgentRunRow
              key={run.run_id}
              run={run}
              isStopping={stoppingIds.has(run.run_id)}
              onStop={() => void handleStop(run.run_id)}
              onReveal={
                run.output_file
                  ? () => void handleReveal(run.output_file as string)
                  : undefined
              }
            />
          ))}
        </div>
      )}
    </div>
  );
};
