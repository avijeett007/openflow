import React from "react";

interface AgentInlineToggleProps {
  checked: boolean;
  onChange: (value: boolean) => void;
  disabled?: boolean;
  label: string;
}

/**
 * Small labeled toggle switch shared by the prompt-agent and CLI-agent cards
 * (enabled/disabled, header row). Extracted from `AgentCard` so `CliAgentCard`
 * doesn't have to duplicate it.
 */
export const AgentInlineToggle: React.FC<AgentInlineToggleProps> = ({
  checked,
  onChange,
  disabled = false,
  label,
}) => (
  <label
    className={`flex items-center gap-2 text-sm shrink-0 ${
      disabled ? "cursor-not-allowed opacity-60" : "cursor-pointer"
    }`}
  >
    <span>{label}</span>
    <span className="relative inline-flex shrink-0">
      <input
        type="checkbox"
        className="sr-only peer"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
      />
      <span className="w-9 h-5 bg-mid-gray/20 rounded-full transition-colors after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-white after:rounded-full after:h-4 after:w-4 after:transition-all peer-checked:bg-background-ui peer-checked:after:translate-x-4" />
    </span>
  </label>
);
