import React from "react";
import { Area, AreaChart, ResponsiveContainer } from "recharts";
import { CHART_VIOLET } from "@/lib/chartPalette";

interface SparklineProps {
  data: number[];
  /** Unique id so multiple sparklines don't share one gradient def. */
  gradientId: string;
}

/**
 * A tiny 32px violet sparkline (no axes, no grid, no tooltip) for hero stat
 * tiles. Single-series → violet only, per the chart colour rules.
 */
export const Sparkline: React.FC<SparklineProps> = ({ data, gradientId }) => {
  if (data.length < 2) return null;
  const points = data.map((value, index) => ({ index, value }));

  return (
    <div className="h-8 w-full" aria-hidden="true">
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart
          data={points}
          margin={{ top: 2, right: 0, left: 0, bottom: 0 }}
        >
          <defs>
            <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={CHART_VIOLET} stopOpacity={0.35} />
              <stop offset="100%" stopColor={CHART_VIOLET} stopOpacity={0.02} />
            </linearGradient>
          </defs>
          <Area
            type="monotone"
            dataKey="value"
            stroke={CHART_VIOLET}
            strokeWidth={2}
            fill={`url(#${gradientId})`}
            isAnimationActive={false}
            dot={false}
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
};
