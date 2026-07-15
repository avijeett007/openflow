import React, { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  BookOpen,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  FolderOpen,
  Pencil,
  Sparkles,
  StopCircle,
  Terminal,
  Wrench,
} from "lucide-react";
import type { AgentRunInfo, RunStatus } from "@/bindings";
import { Button } from "../../ui/Button";
import {
  assembleReadableText,
  parseAgentOutput,
  type ActionCategory,
} from "./parseAgentOutput";

interface AgentRunRowProps {
  run: AgentRunInfo;
  isStopping: boolean;
  onStop: () => void;
  onReveal?: () => void;
  /** Whether the Output section should start expanded (most recent / running runs). */
  defaultExpanded?: boolean;
}

const STATUS_PILL_CLASSES: Record<string, string> = {
  running:
    "bg-gradient-to-r from-logo-primary/20 to-logo-secondary/20 text-logo-primary",
  finishedOk: "bg-green-500/15 text-green-400",
  finishedError: "bg-red-500/15 text-red-400",
  failed: "bg-red-500/15 text-red-400",
  stopped: "bg-mid-gray/15 text-mid-gray",
};

const ACTION_ICON: Record<ActionCategory, React.ElementType> = {
  edit: Pencil,
  read: BookOpen,
  bash: Terminal,
  other: Wrench,
};

const basename = (path: string): string =>
  path.split(/[/\\]/).filter(Boolean).pop() ?? path;

const formatElapsed = (ms: number): string => {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
};

/** Collapsible section header shared by Instruction / Output. */
const SectionHeader: React.FC<{
  label: string;
  isOpen: boolean;
  onToggle: () => void;
  toggleLabel: string;
  right?: React.ReactNode;
}> = ({ label, isOpen, onToggle, toggleLabel, right }) => (
  <div className="flex items-center justify-between gap-2 px-4 py-2">
    <button
      type="button"
      onClick={onToggle}
      title={toggleLabel}
      className="inline-flex items-center gap-1 text-xs font-medium text-mid-gray hover:text-logo-primary transition-colors cursor-pointer"
    >
      {isOpen ? (
        <ChevronDown className="h-3.5 w-3.5" />
      ) : (
        <ChevronRight className="h-3.5 w-3.5" />
      )}
      {label}
    </button>
    {right}
  </div>
);

export const AgentRunRow: React.FC<AgentRunRowProps> = ({
  run,
  isStopping,
  onStop,
  onReveal,
  defaultExpanded = true,
}) => {
  const { t } = useTranslation();
  const outputRef = useRef<HTMLDivElement>(null);
  const [, setTick] = useState(0);
  const [outputOpen, setOutputOpen] = useState(defaultExpanded);
  const [instructionOpen, setInstructionOpen] = useState(true);
  const [copiedReadable, setCopiedReadable] = useState(false);
  const [copiedRaw, setCopiedRaw] = useState(false);

  const isRunning = run.status.status === "running";

  // Live-updating elapsed counter for running rows.
  useEffect(() => {
    if (!isRunning) return;
    const interval = setInterval(() => setTick((prev) => prev + 1), 1000);
    return () => clearInterval(interval);
  }, [isRunning]);

  const parsed = useMemo(() => parseAgentOutput(run.output), [run.output]);
  const readableText = useMemo(() => {
    if (!parsed.structured) return run.output;
    return assembleReadableText(parsed.events) || run.output;
  }, [parsed, run.output]);

  // Auto-scroll the streamed output view as new chunks arrive, while running
  // and the section is open.
  useEffect(() => {
    if (!outputOpen) return;
    const node = outputRef.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [run.output, outputOpen]);

  const badge = ((status: RunStatus) => {
    switch (status.status) {
      case "running":
        return {
          key: "running",
          label: t("settings.agentRuns.status.running"),
          pulse: true,
        };
      case "finished":
        return {
          key: status.code === 0 ? "finishedOk" : "finishedError",
          label: t("settings.agentRuns.status.finished", {
            code: status.code,
          }),
          pulse: false,
        };
      case "failed":
        return {
          key: "failed",
          label: t("settings.agentRuns.status.failed", {
            error: status.error,
          }),
          pulse: false,
        };
      case "stopped":
        return {
          key: "stopped",
          label: t("settings.agentRuns.status.stopped"),
          pulse: false,
        };
    }
  })(run.status);

  const elapsedMs = Date.now() - run.started_at_ms;
  const startedLabel = new Date(run.started_at_ms).toLocaleTimeString();

  const copy = async (text: string, onDone: (copied: boolean) => void) => {
    try {
      await navigator.clipboard.writeText(text);
      onDone(true);
      setTimeout(() => onDone(false), 1500);
    } catch (err) {
      toast.error(t("settings.agentRuns.copyError", { error: String(err) }));
    }
  };

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
          className={`shrink-0 inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium ${STATUS_PILL_CLASSES[badge.key]}`}
        >
          {badge.pulse && (
            <span className="relative flex h-1.5 w-1.5">
              <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-logo-primary opacity-75" />
              <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-logo-primary" />
            </span>
          )}
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
        <div>
          <SectionHeader
            label={t("settings.agentRuns.instruction.label")}
            isOpen={instructionOpen}
            onToggle={() => setInstructionOpen((prev) => !prev)}
            toggleLabel={
              instructionOpen
                ? t("settings.agentRuns.instruction.hide")
                : t("settings.agentRuns.instruction.show")
            }
          />
          {instructionOpen && (
            <div className="px-4 pb-3 -mt-1">
              <p className="text-sm whitespace-pre-wrap break-words">
                {run.instruction}
              </p>
            </div>
          )}
        </div>
      )}

      <div>
        <SectionHeader
          label={t("settings.agentRuns.output.label")}
          isOpen={outputOpen}
          onToggle={() => setOutputOpen((prev) => !prev)}
          toggleLabel={
            outputOpen
              ? t("settings.agentRuns.output.hide")
              : t("settings.agentRuns.output.show")
          }
          right={
            run.output && (
              <div className="flex items-center gap-1.5">
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={() => void copy(readableText, setCopiedReadable)}
                  className="inline-flex items-center gap-1"
                >
                  {copiedReadable ? (
                    <Check className="h-3.5 w-3.5" />
                  ) : (
                    <Copy className="h-3.5 w-3.5" />
                  )}
                  {copiedReadable
                    ? t("settings.agentRuns.copied")
                    : t("settings.agentRuns.copyReadable")}
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={() => void copy(run.output, setCopiedRaw)}
                  className="inline-flex items-center gap-1"
                >
                  {copiedRaw
                    ? t("settings.agentRuns.copied")
                    : t("settings.agentRuns.copyRaw")}
                </Button>
              </div>
            )
          }
        />

        {outputOpen && (
          <div className="px-4 pb-3">
            <div
              ref={outputRef}
              className="max-h-96 overflow-y-auto rounded-md border border-mid-gray/20 bg-mid-gray/5 p-3 space-y-2"
            >
              {!run.output ? (
                <p className="text-xs text-mid-gray font-mono">
                  {t("settings.agentRuns.output.empty")}
                </p>
              ) : !parsed.structured ? (
                <pre className="font-mono text-xs whitespace-pre-wrap break-words">
                  {run.output}
                </pre>
              ) : (
                parsed.events.map((event) => {
                  switch (event.kind) {
                    case "session":
                      return (
                        <div
                          key={event.id}
                          className="inline-flex items-center gap-1.5 rounded-full bg-mid-gray/10 px-2 py-0.5 text-xs text-mid-gray"
                          title={
                            event.model || event.cwd
                              ? t("settings.agentRuns.session.tooltip", {
                                  model: event.model ?? "—",
                                  cwd: event.cwd ?? "—",
                                })
                              : undefined
                          }
                        >
                          <Sparkles className="h-3 w-3 text-logo-primary" />
                          {t("settings.agentRuns.session.started")}
                        </div>
                      );
                    case "text":
                      return (
                        <p
                          key={event.id}
                          className="text-sm whitespace-pre-wrap break-words"
                        >
                          {event.text}
                        </p>
                      );
                    case "action": {
                      const Icon = ACTION_ICON[event.category];
                      const label =
                        event.category === "bash"
                          ? t("settings.agentRuns.actions.ran", {
                              command: event.target || event.tool,
                            })
                          : event.category === "read"
                            ? t("settings.agentRuns.actions.read", {
                                target: event.target || event.tool,
                              })
                            : event.category === "edit"
                              ? t("settings.agentRuns.actions.edited", {
                                  target: event.target || event.tool,
                                })
                              : t("settings.agentRuns.actions.used", {
                                  tool: event.target
                                    ? `${event.tool} (${event.target})`
                                    : event.tool,
                                });
                      return (
                        <div
                          key={event.id}
                          className="flex items-start gap-1.5 font-mono text-xs text-text/80"
                        >
                          <Icon className="h-3.5 w-3.5 mt-0.5 shrink-0 text-logo-secondary" />
                          <span className="whitespace-pre-wrap break-words">
                            {label}
                          </span>
                        </div>
                      );
                    }
                    case "result":
                      return (
                        <div
                          key={event.id}
                          className={`rounded-md border p-3 ${
                            event.isError
                              ? "border-red-500/30 bg-red-500/10"
                              : "border-green-500/30 bg-green-500/10"
                          }`}
                        >
                          <p
                            className={`text-xs font-semibold mb-1 ${
                              event.isError ? "text-red-400" : "text-green-400"
                            }`}
                          >
                            {t("settings.agentRuns.result.label")}
                          </p>
                          <p className="text-sm whitespace-pre-wrap break-words">
                            {event.text}
                          </p>
                        </div>
                      );
                  }
                })
              )}
            </div>
          </div>
        )}
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
