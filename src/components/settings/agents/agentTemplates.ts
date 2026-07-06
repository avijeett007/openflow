import type { AgentOutputMode } from "@/bindings";

// ---------------------------------------------------------------------------
// Id helpers
// ---------------------------------------------------------------------------

const MAX_ID_LENGTH = 48;

/**
 * Turns a display name into a slug matching the backend's accepted agent id
 * pattern (`^[a-z0-9_-]{1,48}$`, enforced in `create_agent`). Falls back to
 * "agent" if the name has no ASCII alphanumeric characters at all.
 */
export const slugify = (name: string): string => {
  const slug = name
    .normalize("NFKD")
    .replace(/[\u0300-\u036f]/g, "") // strip combining diacritics
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, MAX_ID_LENGTH);
  return slug || "agent";
};

/**
 * Appends a numeric suffix until `base` no longer collides with an existing
 * agent id, keeping the result within the backend's 48-character limit.
 */
export const uniqueAgentId = (
  base: string,
  existingIds: Set<string>,
): string => {
  if (!existingIds.has(base)) return base;

  let counter = 2;
  let candidate = base;
  while (existingIds.has(candidate)) {
    const suffix = `-${counter}`;
    candidate = `${base.slice(0, MAX_ID_LENGTH - suffix.length)}${suffix}`;
    counter += 1;
  }
  return candidate;
};

// ---------------------------------------------------------------------------
// Starter templates
// ---------------------------------------------------------------------------

export interface AgentTemplate {
  key: string;
  nameKey: string;
  descriptionKey: string;
  systemPrompt: string;
  outputMode: AgentOutputMode;
}

export const AGENT_TEMPLATES: AgentTemplate[] = [
  {
    key: "formalRewriter",
    nameKey: "settings.agents.addAgent.templates.formalRewriter.name",
    descriptionKey:
      "settings.agents.addAgent.templates.formalRewriter.description",
    systemPrompt:
      "You are a formal writing assistant. Rewrite the user's dictated text in a polished, professional, and formal tone. Preserve the original meaning and intent, fix grammar and awkward phrasing, and remove filler words and false starts. Do not add new information, commentary, or explanations - respond with only the rewritten text.",
    outputMode: "inject",
  },
  {
    key: "commitMessage",
    nameKey: "settings.agents.addAgent.templates.commitMessage.name",
    descriptionKey:
      "settings.agents.addAgent.templates.commitMessage.description",
    systemPrompt:
      'You turn a spoken description of a code change into a single Conventional Commits message. Output only the commit message: a "type(scope): subject" line (feat, fix, refactor, docs, test, chore, etc.), optionally followed by a blank line and a short body if the description warrants more detail. Keep the subject line under 72 characters, written in the imperative mood, with no trailing period. Do not include any explanation, markdown formatting, or quotes - respond with only the commit message text.',
    outputMode: "clipboard",
  },
];
