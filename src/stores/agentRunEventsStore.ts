import { create } from "zustand";
import type { RunEvent } from "@/bindings";

/**
 * Structured `agent-run-event` payloads, keyed by `run_id`. Deliberately a
 * module-level store rather than component state (Task 11 review, Critical
 * 2): `AgentRunsSettings` unmounts on every tab switch (the app renders only
 * the active settings section — see `App.tsx`'s `renderSettingsContent`), so
 * a listener owned by that component loses every event, including a parked
 * `PermissionRequest`, while the user is looking at any other tab. There is
 * deliberately no timeout on a parked prompt (see `agent_run.rs`'s
 * `respond_permission` doc comment) — Stop is the only other way out — so
 * losing the card here would make an in-progress turn unanswerable for as
 * long as the user stayed off this one tab.
 *
 * This store is created once at module load and its listener (see
 * `AgentRunEventListener`, mounted once at the App root — same pattern as
 * `MeetingDetectionListener`) is registered once for the app's whole
 * lifetime, so events keep accumulating regardless of which settings section
 * is mounted.
 */
interface AgentRunEventsStore {
  eventsByRun: Record<string, RunEvent[]>;
  appendEvent: (runId: string, event: RunEvent) => void;
  /** Drops every run's events except the given ids — called after a refresh so a run no longer in the registry (e.g. after "Clear finished") doesn't accumulate forever. */
  pruneToRunIds: (runIds: string[]) => void;
}

export const useAgentRunEventsStore = create<AgentRunEventsStore>((set) => ({
  eventsByRun: {},
  appendEvent: (runId, event) =>
    set((state) => ({
      eventsByRun: {
        ...state.eventsByRun,
        [runId]: [...(state.eventsByRun[runId] ?? []), event],
      },
    })),
  pruneToRunIds: (runIds) =>
    set((state) => {
      const keep = new Set(runIds);
      const next: Record<string, RunEvent[]> = {};
      let changed = false;
      for (const [runId, events] of Object.entries(state.eventsByRun)) {
        if (keep.has(runId)) {
          next[runId] = events;
        } else {
          changed = true;
        }
      }
      return changed ? { eventsByRun: next } : state;
    }),
}));
