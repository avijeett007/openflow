import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { LogOut, MessagesSquare, Send, Trash2 } from "lucide-react";
import type { AgentRunInfo, RunStatus } from "@/bindings";
import { commands, events } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { useAgentRunEventsStore } from "../../../stores/agentRunEventsStore";
import { Button } from "../../ui/Button";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { Textarea } from "../../ui/Textarea";
import { AgentRunRow } from "./AgentRunRow";

/** One conversation: every run sharing an ACP `session_id`, oldest first. A run with no `session_id` (raw CLI, remote, or an ACP run before grouping applies) is its own one-run thread. */
interface RunThread {
  key: string;
  runs: AgentRunInfo[];
}

/**
 * Task 11: runs sharing a `session_id` are one ACP conversation. `runs`
 * arrives newest-first (by `started_at_ms`, same sort `refresh` already
 * applies); each thread's OWN runs are reversed to read oldest-first like a
 * conversation, while the returned thread list stays ordered by each
 * thread's newest activity — identical to today's flat ordering whenever no
 * run shares a session (every thread is a singleton, so this is a no-op).
 */
function groupRunsIntoThreads(runs: AgentRunInfo[]): RunThread[] {
  const bySession = new Map<string, AgentRunInfo[]>();
  const sessionOrder: string[] = [];
  const threads: RunThread[] = [];

  for (const run of runs) {
    if (run.session_id) {
      // Namespaced by agent id, not the bare `session_id` (Task 11 review,
      // Minor 8): `session_id` is whatever the agent's own `session/new`
      // returned, with no cross-agent uniqueness guarantee — two different
      // custom ACP agents both handing back `"1"` must NOT merge into one
      // thread, since the follow-up box targets a single `agent_id` and
      // sending it to the wrong agent would be a real, silent misdirection.
      const key = `${run.agent_id}::${run.session_id}`;
      if (!bySession.has(key)) {
        bySession.set(key, []);
        sessionOrder.push(key);
      }
      bySession.get(key)?.push(run);
    } else {
      threads.push({ key: run.run_id, runs: [run] });
    }
  }
  for (const sessionKey of sessionOrder) {
    const sessionRuns = bySession.get(sessionKey) ?? [];
    threads.push({ key: sessionKey, runs: [...sessionRuns].reverse() });
  }

  threads.sort((a, b) => {
    const aNewest = a.runs[a.runs.length - 1].started_at_ms;
    const bNewest = b.runs[b.runs.length - 1].started_at_ms;
    return bNewest - aNewest;
  });
  return threads;
}

/** The follow-up textarea + Send button shown under the newest run of a thread whose warm session can still take another turn. */
const FollowUpBox: React.FC<{ onSubmit: (text: string) => Promise<void> }> = ({
  onSubmit,
}) => {
  const { t } = useTranslation();
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);

  const submit = async () => {
    const trimmed = text.trim();
    if (!trimmed || sending) return;
    setSending(true);
    try {
      await onSubmit(trimmed);
      setText("");
    } finally {
      setSending(false);
    }
  };

  return (
    <div className="px-1 space-y-2">
      <Textarea
        variant="compact"
        value={text}
        onChange={(e) => setText(e.target.value)}
        placeholder={t("settings.agentRuns.acp.followUp.placeholder")}
        disabled={sending}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            void submit();
          }
        }}
      />
      <div className="flex justify-end">
        <Button
          type="button"
          variant="primary-soft"
          size="sm"
          disabled={sending || !text.trim()}
          onClick={() => void submit()}
          className="inline-flex items-center gap-1.5"
        >
          <Send className="h-3.5 w-3.5" />
          {sending
            ? t("settings.agentRuns.acp.followUp.sending")
            : t("settings.agentRuns.acp.followUp.send")}
        </Button>
      </div>
    </div>
  );
};

/**
 * Flow OS increment 2 "mini Mission Control": a live view of local CLI
 * coding-agent runs. Seeds from `list_agent_runs` on mount, then keeps itself
 * current by subscribing to the `agent-run-output` / `agent-run-status`
 * events (same subscribe/unlisten-on-unmount pattern as
 * `HistorySettings`'s `historyUpdatePayload` listener). A run that starts
 * while this panel is open won't be in the initial snapshot, so on an event
 * for an unrecognized `run_id` we re-fetch the full list instead of dropping
 * the update.
 *
 * Task 11 adds structured ACP events, session-based thread grouping, and the
 * follow-up/End session actions for a still-warm ACP session. Structured
 * events are read from `useAgentRunEventsStore`, NOT a listener owned by this
 * component: this panel unmounts on every tab switch (only the active
 * settings section renders — see `App.tsx`), so a listener here would drop
 * every event, including an unanswerable parked `PermissionRequest`, for as
 * long as the user was on any other tab. `AgentRunEventListener` (mounted
 * once at the App root) keeps the store current regardless of which section
 * is showing — see its doc comment (Task 11 review, Critical 2).
 */
export const AgentRunsSettings: React.FC = () => {
  const { t } = useTranslation();
  const { settings } = useSettings();
  const [runs, setRuns] = useState<AgentRunInfo[]>([]);
  const eventsByRun = useAgentRunEventsStore((state) => state.eventsByRun);
  const [isLoading, setIsLoading] = useState(true);
  const [isClearing, setIsClearing] = useState(false);
  const [stoppingIds, setStoppingIds] = useState<Set<string>>(new Set());
  // Epoch ms an agent's session was last manually ended via "End session",
  // keyed by agent id. A thread's follow-up box hides only while its newest
  // run started BEFORE that click; a run that starts afterward (e.g. a fresh
  // hotkey trigger) makes the thread current again on its own.
  const [endedSessionsAt, setEndedSessionsAt] = useState<
    Record<string, number>
  >({});
  const knownIdsRef = useRef<Set<string>>(new Set());

  const refresh = useCallback(async () => {
    const list = await commands.listAgentRuns();
    const sorted = [...list].sort((a, b) => b.started_at_ms - a.started_at_ms);
    knownIdsRef.current = new Set(sorted.map((run) => run.run_id));
    setRuns(sorted);
    // Self-heals the persistent event store: a run that's no longer in the
    // registry (e.g. dropped by "Clear finished") stops accumulating events
    // forever (Task 11 review, Important 4).
    useAgentRunEventsStore
      .getState()
      .pruneToRunIds(sorted.map((run) => run.run_id));
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

  /**
   * Follow-up submit: same `AgentRunManager::start` path a hotkey trigger
   * uses (`send_agent_followup`), so a still-warm session continues its
   * conversation. Deliberately does NOT `refresh()` here (Task 11 review,
   * Important 3): `start` registers the run and returns immediately, before
   * `drive_acp_run` has acquired a session and called `set_session_id` — an
   * immediate refresh would snapshot `session_id: null` AND mark the run
   * `known`, so no later listener would ever refetch it, and the follow-up
   * would render as a detached singleton outside its thread forever. Leaving
   * it `unknown` means the existing output/status listeners' "unrecognized
   * run_id → refetch" branch (unchanged) picks it up on its first event,
   * by which point `session_id` is already set.
   */
  const handleFollowUp = async (agentId: string, instruction: string) => {
    const result = await commands.sendAgentFollowup(agentId, instruction);
    if (result.status === "error") {
      toast.error(
        t("settings.agentRuns.acp.followUp.error", { error: result.error }),
      );
      return;
    }
    // A follow-up after "End session" spawns a fresh session for this agent —
    // recognize the thread as current again rather than leaving it hidden.
    setEndedSessionsAt((prev) => {
      if (!(agentId in prev)) return prev;
      const next = { ...prev };
      delete next[agentId];
      return next;
    });
  };

  const handleEndSession = async (agentId: string) => {
    const result = await commands.endAcpSession(agentId);
    if (result.status === "error") {
      toast.error(
        t("settings.agentRuns.acp.thread.endSessionError", {
          error: result.error,
        }),
      );
      return;
    }
    setEndedSessionsAt((prev) => ({ ...prev, [agentId]: Date.now() }));
    toast.success(t("settings.agentRuns.acp.thread.endSessionSuccess"));
  };

  const isRunning = (status: RunStatus) => status.status === "running";
  const hasFinishedRuns = runs.some((run) => !isRunning(run.status));
  const runningCount = runs.filter((run) => isRunning(run.status)).length;
  const doneCount = runs.length - runningCount;
  const agentsById = new Map((settings?.agents ?? []).map((a) => [a.id, a]));
  const threads = groupRunsIntoThreads(runs);

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup
        title={t("settings.agentRuns.title")}
        description={t("settings.agentRuns.intro")}
      >
        <div className="px-4 py-3 flex items-center justify-between gap-3">
          <div className="flex items-center gap-2 text-xs text-mid-gray min-w-0">
            {runningCount > 0 && (
              <span className="relative flex h-2 w-2 shrink-0">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-logo-primary opacity-75" />
                <span className="relative inline-flex h-2 w-2 rounded-full bg-logo-primary" />
              </span>
            )}
            <span className="truncate">
              {t("settings.agentRuns.summary", {
                total: runs.length,
                running: runningCount,
                done: doneCount,
              })}
            </span>
          </div>
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
          {threads.map((thread) => {
            const isThread =
              thread.runs.length > 1 || thread.runs[0].session_id != null;

            if (!isThread) {
              const run = thread.runs[0];
              const flatIndex = runs.findIndex((r) => r.run_id === run.run_id);
              return (
                <AgentRunRow
                  key={run.run_id}
                  run={run}
                  events={eventsByRun[run.run_id] ?? []}
                  isStopping={stoppingIds.has(run.run_id)}
                  onStop={() => void handleStop(run.run_id)}
                  onReveal={
                    run.output_file
                      ? () => void handleReveal(run.output_file as string)
                      : undefined
                  }
                  defaultExpanded={isRunning(run.status) || flatIndex === 0}
                />
              );
            }

            const newest = thread.runs[thread.runs.length - 1];
            const agent = agentsById.get(newest.agent_id);
            const acpCapable = Boolean(
              agent?.enabled &&
                agent.kind === "cli" &&
                agent.cli_protocol === "acp",
            );
            const turnOver = !isRunning(newest.status);
            const showThreadControls = acpCapable && turnOver;
            const endedAt = endedSessionsAt[newest.agent_id];
            const manuallyEnded =
              endedAt !== undefined && endedAt >= newest.started_at_ms;
            const showFollowUp = showThreadControls && !manuallyEnded;

            return (
              <div
                key={thread.key}
                className="rounded-lg border border-logo-primary/25 bg-logo-primary/[0.03] p-3 space-y-3"
              >
                <div className="flex items-center justify-between gap-2 px-1">
                  <span className="inline-flex items-center gap-1.5 text-xs font-medium text-logo-primary">
                    <MessagesSquare className="h-3.5 w-3.5" />
                    {t("settings.agentRuns.acp.thread.label", {
                      count: thread.runs.length,
                    })}
                  </span>
                  {showThreadControls && (
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => void handleEndSession(newest.agent_id)}
                      className="inline-flex items-center gap-1.5 text-mid-gray"
                    >
                      <LogOut className="h-3.5 w-3.5" />
                      {t("settings.agentRuns.acp.thread.endSession")}
                    </Button>
                  )}
                </div>
                {thread.runs.map((run, idxInThread) => (
                  <AgentRunRow
                    key={run.run_id}
                    run={run}
                    events={eventsByRun[run.run_id] ?? []}
                    isStopping={stoppingIds.has(run.run_id)}
                    onStop={() => void handleStop(run.run_id)}
                    onReveal={
                      run.output_file
                        ? () => void handleReveal(run.output_file as string)
                        : undefined
                    }
                    defaultExpanded={idxInThread === thread.runs.length - 1}
                  />
                ))}
                {showFollowUp && (
                  <FollowUpBox
                    onSubmit={(text) => handleFollowUp(newest.agent_id, text)}
                  />
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
};
