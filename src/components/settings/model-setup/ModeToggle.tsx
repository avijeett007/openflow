import React from "react";

export interface ModeToggleOption {
  value: string;
  label: string;
}

interface ModeToggleProps {
  value: string;
  options: ModeToggleOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
}

export const ModeToggle: React.FC<ModeToggleProps> = React.memo(
  ({ value, options, onChange, disabled = false }) => {
    return (
      <div
        className={`inline-flex items-center gap-1 rounded-lg border border-mid-gray/30 bg-mid-gray/10 p-1 ${
          disabled ? "opacity-50" : ""
        }`}
        role="radiogroup"
      >
        {options.map((option) => (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={value === option.value}
            disabled={disabled}
            onClick={() => onChange(option.value)}
            className={`px-3 py-1.5 text-sm font-medium rounded-md transition-colors cursor-pointer disabled:cursor-not-allowed ${
              value === option.value
                ? "bg-background-ui text-white"
                : "text-text/70 hover:bg-mid-gray/20"
            }`}
          >
            {option.label}
          </button>
        ))}
      </div>
    );
  },
);

ModeToggle.displayName = "ModeToggle";
