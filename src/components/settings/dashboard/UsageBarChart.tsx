import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { BarChart2 } from "lucide-react";
import {
  Bar,
  BarChart,
  Cell,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { formatCompactNumber } from "@/lib/utils/format";
import { ModeToggle } from "../model-setup/ModeToggle";
import { EmptyState } from "./EmptyState";

type Metric = "dictations" | "words";

export interface UsageBarDatum {
  name: string;
  dictations: number;
  words: number;
}

interface UsageBarChartProps {
  title: string;
  emptyMessage: string;
  data: UsageBarDatum[];
}

const MAX_BARS = 8;
const CHART_AXIS_COLOR =
  "color-mix(in srgb, var(--color-text) 55%, transparent)";

/** Horizontal bar chart shared by the "by app" and "by project" sections. */
export const UsageBarChart: React.FC<UsageBarChartProps> = ({
  title,
  emptyMessage,
  data,
}) => {
  const { t } = useTranslation();
  const [metric, setMetric] = useState<Metric>("dictations");

  const metricOptions = [
    { value: "dictations", label: t("settings.dashboard.overTime.dictations") },
    { value: "words", label: t("settings.dashboard.overTime.words") },
  ];

  const sorted = useMemo(() => {
    return [...data].sort((a, b) => b[metric] - a[metric]).slice(0, MAX_BARS);
  }, [data, metric]);

  // Taller rows for fewer bars keep the chart from looking sparse/oversized.
  const rowHeight = 34;
  const chartHeight = Math.max(sorted.length * rowHeight + 24, 80);

  const hasData = sorted.length > 0;

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between px-4 pt-3 gap-3 flex-wrap">
        <h3 className="text-sm font-medium">{title}</h3>
        {hasData && (
          <ModeToggle
            value={metric}
            options={metricOptions}
            onChange={(value) => setMetric(value as Metric)}
          />
        )}
      </div>

      {hasData ? (
        <div className="w-full px-2 pb-3" style={{ height: chartHeight }}>
          <ResponsiveContainer width="100%" height="100%">
            <BarChart
              data={sorted}
              layout="vertical"
              margin={{ top: 4, right: 24, left: 8, bottom: 4 }}
              barCategoryGap={8}
            >
              <XAxis type="number" hide allowDecimals={false} />
              <YAxis
                type="category"
                dataKey="name"
                width={120}
                tick={{ fill: CHART_AXIS_COLOR, fontSize: 12 }}
                axisLine={false}
                tickLine={false}
                tickFormatter={(value: string) =>
                  value.length > 18 ? `${value.slice(0, 17)}…` : value
                }
              />
              <Tooltip
                cursor={{
                  fill: "color-mix(in srgb, var(--color-mid-gray) 10%, transparent)",
                }}
                formatter={(value) => [
                  formatCompactNumber(Number(value)),
                  metric === "dictations"
                    ? t("settings.dashboard.overTime.dictations")
                    : t("settings.dashboard.overTime.words"),
                ]}
                contentStyle={{
                  backgroundColor: "var(--color-background)",
                  border:
                    "1px solid color-mix(in srgb, var(--color-mid-gray) 40%, transparent)",
                  borderRadius: 8,
                  fontSize: 12,
                }}
                labelStyle={{ color: "var(--color-text)" }}
              />
              <Bar
                dataKey={metric}
                radius={[0, 4, 4, 0]}
                maxBarSize={20}
                label={{
                  position: "right",
                  fill: "var(--color-text)",
                  fontSize: 12,
                  formatter: (value: React.ReactNode) =>
                    formatCompactNumber(Number(value)),
                }}
              >
                {sorted.map((entry) => (
                  <Cell key={entry.name} fill="var(--color-logo-primary)" />
                ))}
              </Bar>
            </BarChart>
          </ResponsiveContainer>
        </div>
      ) : (
        <EmptyState icon={BarChart2} message={emptyMessage} />
      )}
    </div>
  );
};
