import type { PermissionOption, PlanEntry, RunEvent } from "@/bindings";

/**
 * Reduces a run's flat, append-only `RunEvent[]` (from the `agent-run-event`
 * listener) into display rows for `RunEventList`/`PermissionPrompt`. Kept
 * separate from both components so the merge logic — matching a
 * `ToolCallUpdate` to its `ToolCall` by `id`, and a `PermissionResolved` to its
 * `PermissionRequest` by `request_id` — is exercised in exactly one place
 * instead of twice.
 */

export interface ToolCallRow {
  type: "tool_call";
  key: string;
  id: string;
  title: string;
  toolKind: string;
  status: string;
  locations: string[];
  content: string | null;
}

export interface PermissionRow {
  type: "permission";
  key: string;
  requestId: string;
  toolCallId: string | null;
  title: string;
  options: PermissionOption[];
  /** Resolved via the matching `ToolCall`'s `locations`, if any. */
  targetPaths: string[];
  /**
   * The matching `ToolCall`'s `tool_kind` (e.g. `read`, `execute`), if any.
   * Lets the prompt word an "always" answer's scope concretely (an
   * always-allow is recorded PER TOOL KIND — see `permission::decide` — never
   * as blanket authority) instead of leaving it vague.
   */
  toolKind: string | null;
  /**
   * `open` — still awaiting an answer, the turn has not ended.
   * `resolved` — a matching `PermissionResolved` arrived.
   * `abandoned` — the turn ended (or the run went terminal) with no answer.
   */
  state: "open" | "resolved" | "abandoned";
  outcome?: string;
  automatic?: boolean;
}

export type EventRow =
  | { type: "text"; key: string; text: string }
  | { type: "thought"; key: string; text: string }
  | { type: "plan"; key: string; entries: PlanEntry[] }
  | ToolCallRow
  | PermissionRow
  | { type: "turn_end"; key: string; stopReason: string };

/**
 * Whether this run's one turn is over. Deliberately an OR, not an AND: a
 * `TurnEnd` event is the driver's single, unconditional emission site, but
 * `agent-run-status` going terminal is checked too so a permission card can
 * never survive on a stale `events` snapshot — this is the "clear on TurnEnd
 * / terminal status, not on a matching PermissionResolved" contract from
 * Task 8: several turn-ending paths (a crashed child, a closed stream, an
 * abandoned stdin write) end the turn without ever answering a parked
 * prompt.
 */
export const isTurnOver = (
  events: RunEvent[],
  runIsRunning: boolean,
): boolean => !runIsRunning || events.some((e) => e.kind === "turn_end");

export function buildEventRows(
  events: RunEvent[],
  turnOver: boolean,
): EventRow[] {
  const rows: EventRow[] = [];
  const toolIndex = new Map<string, number>();
  const permIndex = new Map<string, number>();

  events.forEach((e, i) => {
    switch (e.kind) {
      case "text": {
        // `Text` is `SessionUpdate::AgentMessageChunk` — a STREAMING DELTA,
        // not a whole message (Task 11 review, Important 4). Merging
        // consecutive chunks into the row directly above avoids a one
        // paragraph reply rendering as dozens of separate `<p>`s. Only merges
        // with the row IMMEDIATELY above (not "the last text row anywhere"),
        // so an unrelated row in between (a tool call, a permission ask)
        // still starts a fresh paragraph.
        const last = rows[rows.length - 1];
        if (last !== undefined && last.type === "text") {
          rows[rows.length - 1] = { ...last, text: last.text + e.text };
        } else {
          rows.push({ type: "text", key: `text-${i}`, text: e.text });
        }
        break;
      }
      case "thought": {
        // Same streaming-delta shape as `Text` above.
        const last = rows[rows.length - 1];
        if (last !== undefined && last.type === "thought") {
          rows[rows.length - 1] = { ...last, text: last.text + e.text };
        } else {
          rows.push({ type: "thought", key: `thought-${i}`, text: e.text });
        }
        break;
      }
      case "plan":
        rows.push({ type: "plan", key: `plan-${i}`, entries: e.entries });
        break;
      case "tool_call": {
        // Some adapters re-send `ToolCall` (rather than `ToolCallUpdate`) for
        // the same id — update the existing row in place rather than
        // pushing a second one with a duplicate React key (Task 11 review,
        // Minor 7, confirmed at runtime).
        const idx = toolIndex.get(e.id);
        const existing = idx !== undefined ? rows[idx] : undefined;
        if (existing !== undefined && existing.type === "tool_call") {
          rows[idx as number] = {
            ...existing,
            title: e.title,
            toolKind: e.tool_kind,
            status: e.status,
            locations: e.locations,
          };
        } else {
          toolIndex.set(e.id, rows.length);
          rows.push({
            type: "tool_call",
            key: `tool-${e.id}`,
            id: e.id,
            title: e.title,
            toolKind: e.tool_kind,
            status: e.status,
            locations: e.locations,
            content: null,
          });
        }
        break;
      }
      case "tool_call_update": {
        const idx = toolIndex.get(e.id);
        const existing = idx !== undefined ? rows[idx] : undefined;
        if (existing !== undefined && existing.type === "tool_call") {
          rows[idx as number] = {
            ...existing,
            status: e.status,
            content: e.content ?? existing.content,
          };
        } else {
          // An update with no matching call shouldn't happen in practice, but
          // never silently drop a status change the agent sent.
          toolIndex.set(e.id, rows.length);
          rows.push({
            type: "tool_call",
            key: `tool-${e.id}`,
            id: e.id,
            title: e.id,
            toolKind: "other",
            status: e.status,
            locations: [],
            content: e.content,
          });
        }
        break;
      }
      case "permission_request": {
        const toolIdx = e.tool_call_id
          ? toolIndex.get(e.tool_call_id)
          : undefined;
        const toolRow = toolIdx !== undefined ? rows[toolIdx] : undefined;
        const targetPaths =
          toolRow !== undefined && toolRow.type === "tool_call"
            ? toolRow.locations
            : [];
        const toolKind =
          toolRow !== undefined && toolRow.type === "tool_call"
            ? toolRow.toolKind
            : null;
        permIndex.set(e.request_id, rows.length);
        rows.push({
          type: "permission",
          key: `perm-${e.request_id}`,
          requestId: e.request_id,
          toolCallId: e.tool_call_id,
          title: e.title,
          options: e.options,
          targetPaths,
          toolKind,
          state: "open",
        });
        break;
      }
      case "permission_resolved": {
        const idx = permIndex.get(e.request_id);
        const existing = idx !== undefined ? rows[idx] : undefined;
        if (existing !== undefined && existing.type === "permission") {
          rows[idx as number] = {
            ...existing,
            state: "resolved",
            outcome: e.outcome,
            automatic: e.automatic,
          };
        }
        break;
      }
      case "turn_end":
        rows.push({
          type: "turn_end",
          key: `end-${i}`,
          stopReason: e.stop_reason,
        });
        break;
    }
  });

  if (!turnOver) return rows;
  // The turn is over: any request still `open` was abandoned — never wait on
  // a `PermissionResolved` that may not be coming.
  return rows.map((r) =>
    r.type === "permission" && r.state === "open"
      ? { ...r, state: "abandoned" as const }
      : r,
  );
}

/** The currently-actionable permission requests, in the order they arrived. */
export function openPermissionRows(rows: EventRow[]): PermissionRow[] {
  return rows.filter(
    (r): r is PermissionRow => r.type === "permission" && r.state === "open",
  );
}
