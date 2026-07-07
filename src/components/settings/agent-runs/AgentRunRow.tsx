import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { FolderOpen, StopCircle } from "lucide-react";
import type { AgentRunInfo, RunStatus } from "@/bindings";
import { Button } from "../../ui/Button";

interface AgentRunRowProps {
  run: AgentRunInfo;
  isStopping: boolean;
  onStop: () => void;
  onReveal?: () => void;
}

const STATUS_BADGE_CLASSES: Record<string, string> = {
  running: "bg-blue-500/15 text-blue-400",
  finishedOk: "bg-green-500/15 text-green-400",
  finishedError: "bg-yellow-500/15 text-yellow-400",
  failed: "bg-red-500/15 text-red-400",
  stopped: "bg-mid-gray/15 text-mid-gray",
};

const basename = (path: string): string =>
  path.split(/[/\\]/).filter(Boolean).pop() ?? path;

const formatElapsed = (ms: number): string => {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
};

export const AgentRunRow: React.FC<AgentRunRowProps> = ({
  run,
  isStopping,
  onStop,
  onReveal,
}) => {
  const { t } = useTranslation();
  const outputRef = useRef<HTMLDivElement>(null);
  const [, setTick] = useState(0);

  const isRunning = run.status.status === "running";

  // Live-updating elapsed counter for running rows.
  useEffect(() => {
    if (!isRunning) return;
    const interval = setInterval(() => setTick((prev) => prev + 1), 1000);
    return () => clearInterval(interval);
  }, [isRunning]);

  // Auto-scroll the streamed output view as new chunks arrive.
  useEffect(() => {
    const node = outputRef.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [run.output]);

  const badge = ((status: RunStatus) => {
    switch (status.status) {
      case "running":
        return {
          key: "running",
          label: t("settings.agentRuns.status.running"),
        };
      case "finished":
        return {
          key: status.code === 0 ? "finishedOk" : "finishedError",
          label: t("settings.agentRuns.status.finished", {
            code: status.code,
          }),
        };
      case "failed":
        return {
          key: "failed",
          label: t("settings.agentRuns.status.failed", {
            error: status.error,
          }),
        };
      case "stopped":
        return {
          key: "stopped",
          label: t("settings.agentRuns.status.stopped"),
        };
    }
  })(run.status);

  const elapsedMs = Date.now() - run.started_at_ms;
  const startedLabel = new Date(run.started_at_ms).toLocaleTimeString();

  return (
    <div className="bg-background border border-mid-gray/20 rounded-lg divide-y divide-mid-gray/20">
      <div className="flex items-center gap-3 px-4 py-3">
        <div className="min-w-0 flex-1">
          <p className="font-semibold text-sm truncate">{run.agent_name}</p>
          <p
            className="text-xs text-mid-gray truncate"
            title={run.project_path || undefined}
          >
            {run.project_path
              ? basename(run.project_path)
              : t("settings.agentRuns.noProject")}
          </p>
        </div>

        <span
          className={`shrink-0 rounded-full px-2 py-0.5 text-xs font-medium ${STATUS_BADGE_CLASSES[badge.key]}`}
        >
          {badge.label}
        </span>

        <div className="shrink-0 text-xs text-mid-gray text-end">
          <div>{t("settings.agentRuns.startedAt", { time: startedLabel })}</div>
          {isRunning && (
            <div>
              {t("settings.agentRuns.elapsed", {
                value: formatElapsed(elapsedMs),
              })}
            </div>
          )}
        </div>

        {isRunning && (
          <Button
            type="button"
            variant="danger-ghost"
            size="sm"
            onClick={onStop}
            disabled={isStopping}
            className="inline-flex shrink-0 items-center gap-1.5"
          >
            <StopCircle className="h-4 w-4" />
            {isStopping
              ? t("settings.agentRuns.stopping")
              : t("settings.agentRuns.stop")}
          </Button>
        )}
      </div>

      {run.instruction && (
        <div className="px-4 py-2">
          <p className="text-xs font-medium text-mid-gray mb-1">
            {t("settings.agentRuns.instruction.label")}
          </p>
          <p className="text-sm whitespace-pre-wrap break-words">
            {run.instruction}
          </p>
        </div>
      )}

      <div className="px-4 py-2">
        <p className="text-xs font-medium text-mid-gray mb-1">
          {t("settings.agentRuns.output.label")}
        </p>
        <div
          ref={outputRef}
          className="max-h-64 overflow-y-auto rounded-md border border-mid-gray/20 bg-mid-gray/5 p-3 font-mono text-xs whitespace-pre-wrap break-words"
        >
          {run.output}
        </div>
      </div>

      {run.output_file && (
        <div className="flex items-center gap-2 px-4 py-2">
          <p
            className="min-w-0 flex-1 truncate text-xs text-mid-gray"
            title={run.output_file}
          >
            {t("settings.agentRuns.outputFile.label", {
              path: run.output_file,
            })}
          </p>
          {onReveal && (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={onReveal}
              className="inline-flex shrink-0 items-center gap-1.5"
            >
              <FolderOpen className="h-4 w-4" />
              {t("settings.agentRuns.outputFile.reveal")}
            </Button>
          )}
        </div>
      )}
    </div>
  );
};
