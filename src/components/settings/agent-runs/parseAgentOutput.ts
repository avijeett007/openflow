/**
 * Parses a CLI coding-agent's raw output into a readable timeline.
 *
 * Claude Code (and compatible agents run with `--output-format stream-json`)
 * emit one JSON object per line:
 *   - `{"type":"system","subtype":"init",...}`      — session start
 *   - `{"type":"assistant","message":{"content":[...]}}` — text / tool_use blocks
 *   - `{"type":"user",...}`                           — tool_result (noisy, collapsed)
 *   - `{"type":"result","result":"...", ...}`         — final summary
 *
 * This module is intentionally defensive: the backend keeps only a rolling
 * ~1MiB tail of the combined stdout+stderr buffer, so the very first line of
 * `output` can be a truncated/partial JSON object. Unparseable lines are
 * skipped silently rather than surfaced as errors. If nothing recognizable
 * as Claude's stream-json shape is found at all (a custom agent, a format
 * change, or genuinely plain-text output), callers should fall back to
 * showing the raw text verbatim — see `structured` on the returned value.
 *
 * Every exported function here is pure (no React, no I/O) so it can be
 * unit-tested in isolation.
 */

export type ActionCategory = "edit" | "read" | "bash" | "other";

export type ParsedTimelineEvent =
  | { id: string; kind: "session"; model?: string; cwd?: string }
  | { id: string; kind: "text"; text: string }
  | {
      id: string;
      kind: "action";
      category: ActionCategory;
      tool: string;
      target: string;
    }
  | { id: string; kind: "result"; text: string; isError: boolean };

export interface ParsedAgentOutput {
  /** False when no recognized stream-json shape was found; callers should render raw text instead. */
  structured: boolean;
  events: ParsedTimelineEvent[];
}

const MAX_TARGET_LENGTH = 160;

/** Collapses a possibly-multiline string to a single truncated line for compact display. */
export const truncateOneLine = (
  input: string,
  maxLength: number = MAX_TARGET_LENGTH,
): string => {
  const oneLine = input.replace(/\s+/g, " ").trim();
  if (oneLine.length <= maxLength) return oneLine;
  return `${oneLine.slice(0, maxLength - 1)}…`;
};

const FILE_PATH_TOOLS = new Set(["Edit", "MultiEdit", "Write"]);

/** Maps a Claude Code `tool_use` block to a category + one-line human summary. */
export const describeToolUse = (
  name: string,
  input: unknown,
): { category: ActionCategory; target: string } => {
  const record =
    input && typeof input === "object"
      ? (input as Record<string, unknown>)
      : {};

  if (FILE_PATH_TOOLS.has(name) || name === "NotebookEdit") {
    const path = record.file_path ?? record.notebook_path;
    return {
      category: "edit",
      target: typeof path === "string" ? path : name,
    };
  }

  if (name === "Read") {
    const path = record.file_path;
    return {
      category: "read",
      target: typeof path === "string" ? path : name,
    };
  }

  if (name === "Bash") {
    const command = record.command;
    return {
      category: "bash",
      target: typeof command === "string" ? truncateOneLine(command) : name,
    };
  }

  // Generic fallback: show the tool name plus a short one-line hint of its
  // input so unrecognized tools (custom MCP tools, future built-ins, etc.)
  // still read as something meaningful rather than being dropped.
  let hint = "";
  try {
    hint = truncateOneLine(JSON.stringify(input ?? {}));
  } catch {
    hint = "";
  }
  return {
    category: "other",
    target: hint && hint !== "{}" ? hint : "",
  };
};

interface ContentBlock {
  type?: string;
  text?: string;
  name?: string;
  input?: unknown;
}

let idCounter = 0;
const nextId = (): string => `evt-${idCounter++}`;

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/**
 * Parses Claude Code stream-json JSONL into a readable timeline of events.
 * Lines that fail to parse as JSON (e.g. a truncated first line from the
 * backend's rolling buffer) are skipped silently. If no line matches a
 * recognized event shape, `structured` is false and the caller should fall
 * back to rendering `raw` verbatim.
 */
export const parseAgentOutput = (raw: string): ParsedAgentOutput => {
  const events: ParsedTimelineEvent[] = [];
  let recognized = false;

  const lines = raw.split("\n");
  for (const rawLine of lines) {
    const line = rawLine.trim();
    if (!line) continue;

    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      // Skip silently: could be a truncated first line, or non-JSON noise.
      continue;
    }

    if (!isRecord(parsed) || typeof parsed.type !== "string") continue;

    switch (parsed.type) {
      case "system": {
        if (parsed.subtype === "init") {
          recognized = true;
          events.push({
            id: nextId(),
            kind: "session",
            model: typeof parsed.model === "string" ? parsed.model : undefined,
            cwd: typeof parsed.cwd === "string" ? parsed.cwd : undefined,
          });
        }
        break;
      }

      case "assistant": {
        const message = parsed.message;
        if (!isRecord(message)) break;
        recognized = true;
        const content = message.content;
        const blocks: ContentBlock[] = Array.isArray(content)
          ? (content as ContentBlock[])
          : typeof content === "string"
            ? [{ type: "text", text: content }]
            : [];

        for (const block of blocks) {
          if (block.type === "text" && typeof block.text === "string") {
            const text = block.text.trim();
            if (text) events.push({ id: nextId(), kind: "text", text });
          } else if (
            block.type === "tool_use" &&
            typeof block.name === "string"
          ) {
            const { category, target } = describeToolUse(
              block.name,
              block.input,
            );
            events.push({
              id: nextId(),
              kind: "action",
              category,
              tool: block.name,
              target,
            });
          }
          // Other block types (thinking, image, etc.) are intentionally ignored.
        }
        break;
      }

      case "user": {
        // Tool results: noisy and rarely useful in a readable summary, so
        // they're collapsed entirely rather than rendered.
        recognized = true;
        break;
      }

      case "result": {
        recognized = true;
        const resultText =
          typeof parsed.result === "string"
            ? parsed.result
            : typeof parsed.error === "string"
              ? parsed.error
              : "";
        events.push({
          id: nextId(),
          kind: "result",
          text: resultText,
          isError: parsed.is_error === true || parsed.subtype === "error",
        });
        break;
      }

      default:
        // Unrecognized `type` — ignore. If every line falls into this
        // bucket, `recognized` stays false and the caller falls back to raw.
        break;
    }
  }

  return { structured: recognized, events };
};

/** Assembles a plain-text, human-readable transcript for the "Copy" action. */
export const assembleReadableText = (events: ParsedTimelineEvent[]): string => {
  const parts: string[] = [];
  for (const event of events) {
    switch (event.kind) {
      case "session":
        parts.push("— Session started —");
        break;
      case "text":
        parts.push(event.text);
        break;
      case "action": {
        const verb =
          event.category === "bash"
            ? "Ran"
            : event.category === "read"
              ? "Read"
              : event.category === "edit"
                ? "Edited"
                : `Used ${event.tool}`;
        const detail = event.category === "other" ? event.target : "";
        parts.push(
          `> ${event.category === "other" ? verb : `${verb} ${event.target || event.tool}`}${detail ? ` (${detail})` : ""}`,
        );
        break;
      }
      case "result":
        parts.push(
          `\n${event.isError ? "Result (error):" : "Result:"}\n${event.text}`,
        );
        break;
    }
  }
  return parts.join("\n\n").trim();
};
