import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { Radio } from "lucide-react";
import type { AgentRunInfo, RunStatus } from "@/bindings";
import { commands, events } from "@/bindings";
import { useNavigationStore } from "@/stores/navigationStore";
import { Card } from "../../ui/Card";
import {
  assembleReadableText,
  parseAgentOutput,
} from "../agent-runs/parseAgentOutput";
import { ModuleHeader } from "./ModuleHeader";
import { MissionControlEmptyState } from "./MissionControlEmptyState";

const MAX_ROWS = 6;

const basename = (path: string): string =>
  path.split(/[/\\]/).filter(Boolean).pop() ?? path;

const formatElapsed = (ms: number): string => {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
};

const isRunning = (status: RunStatus) => status.status === "running";

/** Reserved status colours — never used as chart series colours (contract §2). */
const statusPill = (
  status: RunStatus,
  t: (k: string, o?: Record<string, unknown>) => string,
): { label: string; classes: string; pulse: boolean } => {
  switch (status.status) {
    case "running":
      return {
        label: t("settings.missionControl.liveActivity.running"),
        classes: "bg-of-cyan/15 text-of-cyan",
        pulse: true,
      };
    case "finished":
      return status.code === 0
        ? {
            label: t("settings.missionControl.liveActivity.done"),
            classes: "bg-green-500/15 text-green-500",
            pulse: false,
          }
        : {
            label: t("settings.missionControl.liveActivity.failed"),
            classes: "bg-red-500/15 text-red-400",
            pulse: false,
          };
    case "failed":
      return {
        label: t("settings.missionControl.liveActivity.failed"),
        classes: "bg-red-500/15 text-red-400",
        pulse: false,
      };
    case "stopped":
      return {
        label: t("settings.missionControl.liveActivity.stopped"),
        classes: "bg-mid-gray/15 text-mid-gray",
        pulse: false,
      };
  }
};

/** Last readable, non-empty output line for a running run's live ticker. */
const lastOutputLine = (output: string): string => {
  if (!output) return "";
  const parsed = parseAgentOutput(output);
  const text = parsed.structured
    ? assembleReadableText(parsed.events) || output
    : output;
  const lines = text
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);
  return lines[lines.length - 1] ?? "";
};

interface LiveActivityRowProps {
  run: AgentRunInfo;
  onOpen: () => void;
}

const LiveActivityRow: React.FC<LiveActivityRowProps> = ({ run, onOpen }) => {
  const { t } = useTranslation();
  const [, setTick] = useState(0);
  const running = isRunning(run.status);

  // Live elapsed counter for running rows.
  useEffect(() => {
    if (!running) return;
    const interval = setInterval(() => setTick((n) => n + 1), 1000);
    return () => clearInterval(interval);
  }, [running]);

  const pill = statusPill(run.status, t);
  const project = run.project_path
    ? basename(run.project_path)
    : t("settings.missionControl.liveActivity.noProject");
  const ticker = running ? lastOutputLine(run.output) : "";
  const elapsed = formatElapsed(Date.now() - run.started_at_ms);

  return (
    <button
      type="button"
      onClick={onOpen}
      className="w-full text-left rounded-lg px-3 py-2.5 hover:bg-of-raised transition-colors cursor-pointer"
    >
      <div className="flex items-center gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 min-w-0">
            <span className="font-medium text-sm truncate">
              {run.agent_name}
            </span>
            <span className="text-xs text-text/40 truncate">{project}</span>
          </div>
          {ticker && (
            <p className="mt-0.5 truncate font-mono text-xs text-text/50">
              {ticker}
            </p>
          )}
        </div>
        <span
          className={`shrink-0 inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-[11px] font-medium ${pill.classes}`}
        >
          {pill.pulse && (
            <span className="relative flex h-1.5 w-1.5">
              <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-of-cyan opacity-75" />
              <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-of-cyan" />
            </span>
          )}
          {pill.label}
        </span>
        {running && (
          <span className="shrink-0 w-10 text-right text-xs tabular-nums text-text/50">
            {elapsed}
          </span>
        )}
      </div>
    </button>
  );
};

/**
 * The AI-OS centerpiece: a live view of local agent runs. Reuses the exact
 * wiring of the Agent Runs panel — `list_agent_runs` seed + `agent-run-output`
 * / `agent-run-status` event subscription.
 */
export const LiveActivity: React.FC = () => {
  const { t } = useTranslation();
  const setCurrentSection = useNavigationStore((s) => s.setCurrentSection);
  const [runs, setRuns] = useState<AgentRunInfo[]>([]);
  const [isLoading, setIsLoading] = useState(true);
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

  // Running runs first, then the most recent finished/stopped/failed, capped.
  const ordered = useMemo(() => {
    const running = runs.filter((r) => isRunning(r.status));
    const done = runs.filter((r) => !isRunning(r.status));
    return [...running, ...done].slice(0, MAX_ROWS);
  }, [runs]);

  const runningCount = runs.filter((r) => isRunning(r.status)).length;

  return (
    <section>
      <ModuleHeader
        title={t("settings.missionControl.liveActivity.title")}
        actionLabel={
          runs.length > 0
            ? t("settings.missionControl.liveActivity.viewAll")
            : undefined
        }
        onAction={
          runs.length > 0 ? () => setCurrentSection("agentRuns") : undefined
        }
        right={
          runningCount > 0 ? (
            <span className="inline-flex items-center gap-1.5 rounded-full bg-of-cyan/15 px-2 py-0.5 text-[11px] font-medium text-of-cyan">
              <span className="relative flex h-1.5 w-1.5">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-of-cyan opacity-75" />
                <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-of-cyan" />
              </span>
              {t("settings.missionControl.liveActivity.runningCount", {
                count: runningCount,
              })}
            </span>
          ) : null
        }
      />
      <Card padding="sm">
        {isLoading ? (
          <div className="flex items-center justify-center py-10">
            <div className="h-6 w-6 animate-spin rounded-full border-2 border-of-violet border-t-transparent" />
          </div>
        ) : ordered.length === 0 ? (
          <MissionControlEmptyState
            icon={Radio}
            message={t("settings.missionControl.liveActivity.empty")}
            actionLabel={t("settings.missionControl.liveActivity.createAgent")}
            onAction={() => setCurrentSection("agents")}
          />
        ) : (
          <div className="flex flex-col">
            {ordered.map((run) => (
              <LiveActivityRow
                key={run.run_id}
                run={run}
                onOpen={() => setCurrentSection("agentRuns")}
              />
            ))}
          </div>
        )}
      </Card>
    </section>
  );
};
