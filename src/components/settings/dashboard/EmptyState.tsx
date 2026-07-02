import React from "react";
import type { LucideIcon } from "lucide-react";

interface EmptyStateProps {
  icon?: LucideIcon;
  message: string;
}

/** Friendly placeholder shown by any dashboard chart/list with no data yet. */
export const EmptyState: React.FC<EmptyStateProps> = ({
  icon: Icon,
  message,
}) => {
  return (
    <div className="flex flex-col items-center justify-center gap-2 py-10 text-center text-text/50">
      {Icon && <Icon className="w-6 h-6 text-text/30" />}
      <p className="text-sm">{message}</p>
    </div>
  );
};
