import React from "react";
import { useTranslation } from "react-i18next";
import {
  BookOpen,
  Brain,
  CheckCircle2,
  Circle,
  CircleDot,
  Download,
  HelpCircle,
  Move,
  Pencil,
  Search,
  ShieldCheck,
  ShieldX,
  Terminal,
  Trash2,
  Wrench,
} from "lucide-react";
import type { EventRow } from "./runEventRows";

/** ACP tool kind (`read | edit | execute | delete | move | search | fetch | think | other`) → icon. */
const TOOL_ICON: Record<string, React.ElementType> = {
  read: BookOpen,
  edit: Pencil,
  execute: Terminal,
  delete: Trash2,
  move: Move,
  search: Search,
  fetch: Download,
  think: Brain,
};

const PLAN_ICON: Record<string, React.ElementType> = {
  completed: CheckCircle2,
  in_progress: CircleDot,
};

/** `RunEvent::TurnEnd`'s `stop_reason` wire values (`stop_reason_label` in agent_run.rs, plus the driver's own `"failed"`). */
const TURN_END_KEYS: Record<string, string> = {
  completed: "settings.agentRuns.acp.turnEnd.completed",
  cancelled: "settings.agentRuns.acp.turnEnd.cancelled",
  max_steps_reached: "settings.agentRuns.acp.turnEnd.maxSteps",
  request_timeout: "settings.agentRuns.acp.turnEnd.timeout",
  failed: "settings.agentRuns.acp.turnEnd.failed",
};

/** `RunEvent::PermissionResolved`'s `outcome` is always one of these three (see `drive_acp_run`'s `on_event(RunEvent::PermissionResolved …)` call sites) — never the agent-supplied option name. */
const PERMISSION_OUTCOME_KEYS: Record<string, string> = {
  allow: "settings.agentRuns.acp.permission.outcomeAllow",
  deny: "settings.agentRuns.acp.permission.outcomeDeny",
  cancelled: "settings.agentRuns.acp.permission.outcomeCancelled",
};

interface RunEventListProps {
  rows: EventRow[];
}

/**
 * Renders one run's structured `RunEvent` timeline. `ToolCallUpdate`s and
 * `PermissionResolved`s are already merged into their matching row by
 * `buildEventRows` — this component only renders, it never merges.
 *
 * An `open` permission row renders nothing here: the non-modal
 * `PermissionPrompt` (pinned to the bottom of the run) is the only place that
 * shows an actionable card, so a request never appears twice.
 */
export const RunEventList: React.FC<RunEventListProps> = ({ rows }) => {
  const { t } = useTranslation();

  return (
    <div className="space-y-2">
      {rows.map((row) => {
        switch (row.type) {
          case "text":
            return (
              <p
                key={row.key}
                className="text-sm whitespace-pre-wrap break-words"
              >
                {row.text}
              </p>
            );

          case "thought":
            return (
              <details key={row.key} className="text-xs text-mid-gray">
                <summary className="cursor-pointer select-none list-none inline-flex items-center gap-1 hover:text-logo-primary transition-colors">
                  <Brain className="h-3.5 w-3.5 shrink-0" />
                  {t("settings.agentRuns.acp.thought.label")}
                </summary>
                <p className="mt-1 pl-5 whitespace-pre-wrap break-words italic text-mid-gray/90">
                  {row.text}
                </p>
              </details>
            );

          case "plan":
            return (
              <div
                key={row.key}
                className="rounded-md border border-mid-gray/20 p-2.5 space-y-1.5"
              >
                <p className="text-xs font-semibold text-mid-gray">
                  {t("settings.agentRuns.acp.plan.label")}
                </p>
                <ul className="space-y-1">
                  {row.entries.map((entry, i) => {
                    const Icon = PLAN_ICON[entry.status] ?? Circle;
                    const done = entry.status === "completed";
                    return (
                      <li
                        key={`${row.key}-${i}`}
                        className="flex items-start gap-1.5 text-sm"
                      >
                        <Icon
                          className={`h-3.5 w-3.5 mt-0.5 shrink-0 ${
                            done ? "text-green-400" : "text-mid-gray"
                          }`}
                        />
                        <span
                          className={
                            done ? "line-through text-mid-gray" : undefined
                          }
                        >
                          {entry.content}
                        </span>
                      </li>
                    );
                  })}
                </ul>
              </div>
            );

          case "tool_call": {
            const Icon = TOOL_ICON[row.toolKind] ?? Wrench;
            const failed = row.status === "failed";
            const done = row.status === "completed";
            return (
              <div
                key={row.key}
                className="flex items-start gap-1.5 font-mono text-xs text-text/80"
              >
                <Icon
                  className={`h-3.5 w-3.5 mt-0.5 shrink-0 ${
                    failed
                      ? "text-red-400"
                      : done
                        ? "text-green-400"
                        : "text-logo-secondary"
                  }`}
                />
                <span className="whitespace-pre-wrap break-words">
                  {row.locations.length > 0
                    ? `${row.title} — ${row.locations.join(", ")}`
                    : row.title}
                  {failed &&
                    ` (${t("settings.agentRuns.acp.toolCall.failed")})`}
                </span>
              </div>
            );
          }

          case "permission": {
            if (row.state === "open") return null;
            if (row.state === "abandoned") {
              return (
                <div
                  key={row.key}
                  className="flex items-start gap-1.5 text-xs text-mid-gray italic"
                >
                  <HelpCircle className="h-3.5 w-3.5 mt-0.5 shrink-0" />
                  <span>
                    {t("settings.agentRuns.acp.permission.abandoned", {
                      title: row.title,
                    })}
                  </span>
                </div>
              );
            }
            const outcomeKey = row.outcome
              ? PERMISSION_OUTCOME_KEYS[row.outcome]
              : undefined;
            const allowed = row.outcome === "allow";
            return (
              <div key={row.key} className="flex items-start gap-1.5 text-xs">
                {allowed ? (
                  <ShieldCheck className="h-3.5 w-3.5 mt-0.5 shrink-0 text-green-400" />
                ) : (
                  <ShieldX className="h-3.5 w-3.5 mt-0.5 shrink-0 text-red-400" />
                )}
                <span className="text-mid-gray">
                  {t("settings.agentRuns.acp.permission.resolved", {
                    title: row.title,
                    outcome: outcomeKey ? t(outcomeKey) : (row.outcome ?? ""),
                  })}
                  {row.automatic
                    ? ` ${t("settings.agentRuns.acp.permission.automatic")}`
                    : ""}
                </span>
              </div>
            );
          }

          case "turn_end":
            return (
              <div key={row.key} className="flex items-center gap-2 py-1">
                <div className="h-px flex-1 bg-mid-gray/20" />
                <span className="text-[11px] uppercase tracking-wide text-mid-gray/70 shrink-0">
                  {t(
                    TURN_END_KEYS[row.stopReason] ??
                      "settings.agentRuns.acp.turnEnd.other",
                  )}
                </span>
                <div className="h-px flex-1 bg-mid-gray/20" />
              </div>
            );
        }
      })}
    </div>
  );
};
