import React from "react";
import type { LucideIcon } from "lucide-react";

interface MissionControlEmptyStateProps {
  icon: LucideIcon;
  message: string;
  actionLabel?: string;
  onAction?: () => void;
}

/**
 * Consistent Mission Control empty state: icon + one-line message + optional
 * action button. Mirrors the pattern the design contract asks for across the
 * app (Mission Control, Agent Runs, History, Dictionary).
 */
export const MissionControlEmptyState: React.FC<
  MissionControlEmptyStateProps
> = ({ icon: Icon, message, actionLabel, onAction }) => (
  <div className="flex flex-col items-center justify-center gap-3 py-10 text-center">
    <div className="flex h-11 w-11 items-center justify-center rounded-full bg-of-violet/10">
      <Icon className="h-5 w-5 text-of-violet" />
    </div>
    <p className="text-sm text-text/60 max-w-xs">{message}</p>
    {actionLabel && onAction && (
      <button
        type="button"
        onClick={onAction}
        className="mt-1 inline-flex items-center gap-1.5 rounded-lg border border-of-hairline bg-of-raised px-3 py-1.5 text-xs font-medium text-text hover:border-of-violet/40 transition-colors cursor-pointer"
      >
        {actionLabel}
      </button>
    )}
  </div>
);
