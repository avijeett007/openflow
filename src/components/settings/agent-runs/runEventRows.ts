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
  // NB: `ToolCallUpdate.content` is deliberately NOT retained. It is
  // `Value::to_string()` of arbitrary agent JSON — bounded only by the
  // transport's 8 MiB per-line cap, and a real run accumulates megabytes of it
  // — while nothing in the panel has ever rendered it (the only `content` the
  // UI draws belongs to a `PlanEntry`). Keeping it meant this store held
  // roughly 10 MB where the backend's own mirror of the same run holds ~3 KB.
  // If a tool-output view is ever built, read it from the event stream at
  // render time rather than reviving this field.
}

export interface PermissionRow {
  type: "permission";
  key: string;
  requestId: string;
  toolCallId: string | null;
  /**
   * The card's headline. From the request itself where the agent supplied one,
   * else the matching `ToolCall`'s — the same fallback as `targetPaths` and
   * `toolKind` below, and for the same reason. `codex-acp` 1.1.9's
   * `session/request_permission.toolCall` is `{toolCallId, kind, status,
   * rawInput}` with **no `title` at all** (legal: the schema types that field as
   * a `ToolCallUpdate`, where only `toolCallId` is required), which rendered an
   * empty headline above an Allow button.
   */
  title: string;
  options: PermissionOption[];
  /**
   * What an Allow would touch. Taken from the permission request's OWN
   * `locations` when the agent supplied them, and only falling back to the
   * matching `ToolCall`'s. ACP permits a permission request with no preceding
   * `tool_call`, so the join can legitimately miss — and a card that asks
   * "Edit file — Allow?" without naming the file is the one failure this
   * feature cannot afford.
   */
  targetPaths: string[];
  /**
   * The tool kind (e.g. `read`, `execute`), from the request itself where the
   * agent stated one, else the matching `ToolCall`'s. Lets the prompt word an
   * "always" answer's scope concretely (an always-allow is recorded PER TOOL
   * KIND — see `permission::decide`) instead of leaving it vague. `null` means
   * the agent named no kind at all (Kimi does this), in which case an "always"
   * is NOT remembered — see `alwaysPersists`.
   */
  toolKind: string | null;
  /**
   * Whether clicking an "always" option will actually be remembered for the
   * session. False when the kind is absent or `"other"` — ACP's catch-all,
   * which Claude Code files every MCP and unrecognised tool under, so
   * remembering it would pre-authorise all of them at once.
   *
   * Decided from the REQUEST's own kind, never the joined `ToolCall`'s, because
   * that is the only thing the backend sees: `agent_run::apply_answer` gates on
   * `is_persistable_kind(parked.tool_kind)` where `parked.tool_kind` is
   * `params.tool_call.kind` straight off the wire. `toolKind` above may fall
   * back to the join so the card can still NAME what it is about; this must
   * not, or the card promises a scope the backend then refuses.
   */
  alwaysPersists: boolean;
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
          });
        }
        break;
      }
      case "tool_call_update": {
        // A `ToolCallUpdate` is a REFINEMENT: `null` on any field means "the
        // agent said nothing about this", never "reset it". Merge, never
        // overwrite. `claude-agent-acp` sends refinements carrying the resolved
        // path and NO status at all; assigning `e.status` unconditionally used
        // to clobber the tool call's real `pending`/`completed` with `""`, and
        // the path itself was not modelled and never arrived.
        const idx = toolIndex.get(e.id);
        const existing = idx !== undefined ? rows[idx] : undefined;
        if (existing !== undefined && existing.type === "tool_call") {
          rows[idx as number] = {
            ...existing,
            status: e.status ?? existing.status,
            title: e.title ?? existing.title,
            toolKind: e.tool_kind ?? existing.toolKind,
            locations: e.locations ?? existing.locations,
          };
        } else {
          // An update with no matching call shouldn't happen in practice, but
          // never silently drop what the agent sent.
          toolIndex.set(e.id, rows.length);
          rows.push({
            type: "tool_call",
            key: `tool-${e.id}`,
            id: e.id,
            title: e.title ?? e.id,
            toolKind: e.tool_kind ?? "other",
            status: e.status ?? "",
            locations: e.locations ?? [],
          });
        }
        break;
      }
      case "permission_request": {
        const toolIdx = e.tool_call_id
          ? toolIndex.get(e.tool_call_id)
          : undefined;
        const toolRow = toolIdx !== undefined ? rows[toolIdx] : undefined;
        const joined =
          toolRow !== undefined && toolRow.type === "tool_call"
            ? toolRow
            : undefined;
        // The REQUEST's own fields win. The join is only a fallback: ACP allows
        // a permission request with no preceding `tool_call`, and a card that
        // cannot name what it is about is worse than no card. `title` joins for
        // the same reason `targetPaths`/`toolKind` do — codex-acp 1.1.9 sends
        // permission requests with no title at all.
        const targetPaths =
          e.locations.length > 0 ? e.locations : (joined?.locations ?? []);
        const toolKind = e.tool_kind || (joined?.toolKind ?? null);
        const title = e.title || (joined?.title ?? "");
        // NOT the joined kind. The backend decides persistence from the
        // request's OWN kind (`is_persistable_kind(parked.tool_kind)`), and it
        // never sees the join — so keying this on `toolKind` made the card
        // promise "always applies to execute actions" for a request whose kind
        // the agent never stated, and nothing was recorded. Kimi 0.31.0 is
        // exactly that case: a kind-ful `tool_call` and a kind-LESS permission
        // request for it. Trimmed to mirror `is_persistable_kind` exactly.
        const ownKind = e.tool_kind.trim();
        permIndex.set(e.request_id, rows.length);
        rows.push({
          type: "permission",
          key: `perm-${e.request_id}`,
          requestId: e.request_id,
          toolCallId: e.tool_call_id,
          title,
          options: e.options,
          targetPaths,
          toolKind,
          alwaysPersists: ownKind !== "" && ownKind !== "other",
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
