import React from "react";
import type { LucideIcon } from "lucide-react";

interface StatTileProps {
  label: string;
  value: string;
  icon: LucideIcon;
}

/** A single KPI card for the dashboard's stat-tile row. */
export const StatTile: React.FC<StatTileProps> = ({
  label,
  value,
  icon: Icon,
}) => {
  return (
    <div className="flex flex-col gap-2 rounded-lg border border-mid-gray/20 bg-background p-4 min-w-0">
      <div className="flex items-center gap-2 text-text/50">
        <Icon className="w-4 h-4 shrink-0" />
        <span className="text-xs font-medium uppercase tracking-wide truncate">
          {label}
        </span>
      </div>
      <span className="text-2xl font-semibold truncate">{value}</span>
    </div>
  );
};
