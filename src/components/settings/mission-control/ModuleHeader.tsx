import React from "react";
import { ArrowRight } from "lucide-react";

interface ModuleHeaderProps {
  title: string;
  /** Optional right-aligned "see more" affordance. */
  actionLabel?: string;
  onAction?: () => void;
  /** Optional trailing content (e.g. a live count pill). */
  right?: React.ReactNode;
}

/**
 * One consistent section-header pattern for every Mission Control module:
 * uppercase micro-label on the left, an optional link/action on the right.
 */
export const ModuleHeader: React.FC<ModuleHeaderProps> = ({
  title,
  actionLabel,
  onAction,
  right,
}) => (
  <div className="flex items-center justify-between gap-3 mb-2 px-0.5">
    <h2 className="text-[11px] font-semibold uppercase tracking-wide text-text/50">
      {title}
    </h2>
    <div className="flex items-center gap-2">
      {right}
      {actionLabel && onAction && (
        <button
          type="button"
          onClick={onAction}
          className="inline-flex items-center gap-1 text-xs font-medium text-of-violet hover:text-of-violet/80 transition-colors cursor-pointer"
        >
          {actionLabel}
          <ArrowRight className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  </div>
);
