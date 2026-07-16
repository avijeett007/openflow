import type { AiModeKind } from "@/bindings";

// Slug helpers are identical to the agents' (`^[a-z0-9_-]{1,48}$`, enforced
// backend-side by the shared `is_valid_agent_slug`), so we reuse them rather than
// duplicating the logic.
export {
  slugify,
  uniqueAgentId as uniqueModeId,
} from "../agents/agentTemplates";

/**
 * A one-click AI Mode starter. Unlike the built-in "Write" card (which is a view
 * over the existing cleanup settings), these create real `ai_modes` entries.
 * `appRules` are preseeded bundle-ids/names for per-app auto-selection.
 */
export interface ModeTemplate {
  key: string;
  nameKey: string;
  descriptionKey: string;
  kind: AiModeKind;
  /** Editable prompt body (appended to the hidden base prompt backend-side). */
  prompt: string;
  appRules: string[];
}

// Preseeded terminal bundle-ids + names for the Command template (per the design
// contract). Matching is case-insensitive substring both ways, so the short
// names ("iterm"/"terminal"/"warp") also catch localized app names.
const TERMINAL_APP_RULES = [
  "com.googlecode.iterm2",
  "com.apple.Terminal",
  "dev.warp.Warp",
  "net.kovidgoyal.kitty",
  "com.github.wez.wezterm",
  "iterm",
  "terminal",
  "warp",
];

export const MODE_TEMPLATES: ModeTemplate[] = [
  {
    key: "command",
    nameKey: "settings.aiModes.templates.command.name",
    descriptionKey: "settings.aiModes.templates.command.description",
    kind: "command",
    prompt:
      "Target a standard macOS zsh shell and prefer widely-available tools. When a path or filename is not specified, operate on the current directory. Keep the command minimal and avoid destructive operations unless explicitly requested.",
    appRules: TERMINAL_APP_RULES,
  },
  {
    key: "direct",
    nameKey: "settings.aiModes.templates.direct.name",
    descriptionKey: "settings.aiModes.templates.direct.description",
    kind: "direct",
    prompt: "",
    appRules: [],
  },
  {
    key: "translate",
    nameKey: "settings.aiModes.templates.translate.name",
    descriptionKey: "settings.aiModes.templates.translate.description",
    kind: "rewrite",
    // The language is a placeholder the user edits on the card.
    prompt:
      "Translate the text into French. Output only the translation, preserving the original meaning, tone, and formatting. Do not add notes, explanations, or transliterations.",
    appRules: [],
  },
];
