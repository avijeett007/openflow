import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { LineChart as LineChartIcon } from "lucide-react";
import {
  Area,
  AreaChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import type { OverTimePoint } from "@/bindings";
import { formatCompactNumber } from "@/lib/utils/format";
import {
  CHART_AXIS_COLOR,
  CHART_GRID_COLOR,
  CHART_TOOLTIP_LABEL_STYLE,
  CHART_TOOLTIP_STYLE,
  CHART_VIOLET,
} from "@/lib/chartPalette";
import { ModeToggle } from "../model-setup/ModeToggle";
import { EmptyState } from "./EmptyState";

type Metric = "dictations" | "words";

interface OverTimeChartProps {
  data: OverTimePoint[];
}

export const OverTimeChart: React.FC<OverTimeChartProps> = ({ data }) => {
  const { t, i18n } = useTranslation();
  const [metric, setMetric] = useState<Metric>("dictations");

  const dateFormatter = useMemo(
    () =>
      new Intl.DateTimeFormat(i18n.language, {
        month: "short",
        day: "numeric",
      }),
    [i18n.language],
  );

  const formatDateLabel = (isoDate: string) => {
    // isoDate is YYYY-MM-DD; parse as UTC to avoid off-by-one from local TZ.
    const parsed = new Date(`${isoDate}T00:00:00Z`);
    if (Number.isNaN(parsed.getTime())) return isoDate;
    return dateFormatter.format(parsed);
  };

  const metricOptions = [
    { value: "dictations", label: t("settings.dashboard.overTime.dictations") },
    { value: "words", label: t("settings.dashboard.overTime.words") },
  ];

  const hasData = data.length > 0;

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between px-4 pt-3 gap-3 flex-wrap">
        <h3 className="text-sm font-medium">
          {t("settings.dashboard.overTime.title")}
        </h3>
        <ModeToggle
          value={metric}
          options={metricOptions}
          onChange={(value) => setMetric(value as Metric)}
        />
      </div>

      {hasData ? (
        <div className="h-64 w-full px-2 pb-3">
          <ResponsiveContainer width="100%" height="100%">
            <AreaChart
              data={data}
              margin={{ top: 8, right: 16, left: 0, bottom: 0 }}
            >
              <defs>
                <linearGradient
                  id="dashboardAreaFill"
                  x1="0"
                  y1="0"
                  x2="0"
                  y2="1"
                >
                  <stop
                    offset="0%"
                    stopColor={CHART_VIOLET}
                    stopOpacity={0.28}
                  />
                  <stop
                    offset="100%"
                    stopColor={CHART_VIOLET}
                    stopOpacity={0.02}
                  />
                </linearGradient>
              </defs>
              <CartesianGrid
                vertical={false}
                stroke={CHART_GRID_COLOR}
                strokeWidth={1}
              />
              <XAxis
                dataKey="date"
                tickFormatter={formatDateLabel}
                tick={{ fill: CHART_AXIS_COLOR, fontSize: 12 }}
                axisLine={{ stroke: CHART_GRID_COLOR }}
                tickLine={false}
                minTickGap={24}
              />
              <YAxis
                allowDecimals={false}
                tickFormatter={(value) => formatCompactNumber(Number(value))}
                tick={{ fill: CHART_AXIS_COLOR, fontSize: 12 }}
                axisLine={false}
                tickLine={false}
                width={40}
              />
              <Tooltip
                labelFormatter={(label) => formatDateLabel(String(label))}
                formatter={(value) => [
                  formatCompactNumber(Number(value)),
                  metric === "dictations"
                    ? t("settings.dashboard.overTime.dictations")
                    : t("settings.dashboard.overTime.words"),
                ]}
                contentStyle={CHART_TOOLTIP_STYLE}
                labelStyle={CHART_TOOLTIP_LABEL_STYLE}
                cursor={{ stroke: CHART_GRID_COLOR, strokeWidth: 1 }}
              />
              <Area
                type="monotone"
                dataKey={metric}
                stroke={CHART_VIOLET}
                strokeWidth={2}
                fill="url(#dashboardAreaFill)"
                activeDot={{
                  r: 4,
                  stroke: "var(--color-background)",
                  strokeWidth: 2,
                }}
              />
            </AreaChart>
          </ResponsiveContainer>
        </div>
      ) : (
        <EmptyState
          icon={LineChartIcon}
          message={t("settings.dashboard.overTime.empty")}
        />
      )}
    </div>
  );
};
