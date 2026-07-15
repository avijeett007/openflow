import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { BarChart2, Flame, LineChart as LineChartIcon } from "lucide-react";
import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import type {
  AnalyticsSummary,
  AppUsage,
  KeywordCount,
  OverTimePoint,
} from "@/bindings";
import { formatCompactNumber } from "@/lib/utils/format";
import {
  CHART_AXIS_COLOR,
  CHART_GRID_COLOR,
  CHART_TOOLTIP_LABEL_STYLE,
  CHART_TOOLTIP_STYLE,
  CHART_VIOLET,
} from "@/lib/chartPalette";
import { useNavigationStore } from "@/stores/navigationStore";
import { Card } from "../../ui/Card";
import { EmptyState } from "../dashboard/EmptyState";
import { ModuleHeader } from "./ModuleHeader";

interface AnalyticsStripProps {
  summary: AnalyticsSummary | null;
  overTime: OverTimePoint[];
  byApp: AppUsage[];
  keywords: KeywordCount[];
}

const MAX_APPS = 5;
const MAX_KEYWORDS = 6;

export const AnalyticsStrip: React.FC<AnalyticsStripProps> = ({
  summary,
  overTime,
  byApp,
  keywords,
}) => {
  const { t, i18n } = useTranslation();
  const setCurrentSection = useNavigationStore((s) => s.setCurrentSection);

  const dateFormatter = useMemo(
    () =>
      new Intl.DateTimeFormat(i18n.language, {
        month: "short",
        day: "numeric",
      }),
    [i18n.language],
  );

  const formatDateLabel = (isoDate: string) => {
    const parsed = new Date(`${isoDate}T00:00:00Z`);
    if (Number.isNaN(parsed.getTime())) return isoDate;
    return dateFormatter.format(parsed);
  };

  const topApps = useMemo(
    () =>
      [...byApp]
        .sort((a, b) => b.dictations - a.dictations)
        .slice(0, MAX_APPS)
        .map((a) => ({ name: a.app, dictations: a.dictations })),
    [byApp],
  );

  const topKeywords = useMemo(
    () => keywords.slice(0, MAX_KEYWORDS),
    [keywords],
  );

  const hasOverTime = overTime.length > 0;
  const hasApps = topApps.length > 0;

  return (
    <section>
      <ModuleHeader
        title={t("settings.missionControl.analytics.title")}
        actionLabel={t("settings.missionControl.analytics.openDashboard")}
        onAction={() => setCurrentSection("dashboard")}
      />
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        {/* (a) Dictations over time — single series, violet, no legend. */}
        <Card padding="md">
          <h3 className="mb-2 text-sm font-medium">
            {t("settings.missionControl.analytics.overTime")}
          </h3>
          {hasOverTime ? (
            <div className="h-48 w-full">
              <ResponsiveContainer width="100%" height="100%">
                <AreaChart
                  data={overTime}
                  margin={{ top: 6, right: 8, left: 0, bottom: 0 }}
                >
                  <defs>
                    <linearGradient
                      id="mcOverTimeFill"
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
                    tick={{ fill: CHART_AXIS_COLOR, fontSize: 11 }}
                    axisLine={{ stroke: CHART_GRID_COLOR }}
                    tickLine={false}
                    minTickGap={28}
                  />
                  <YAxis
                    allowDecimals={false}
                    tickFormatter={(v) => formatCompactNumber(Number(v))}
                    tick={{ fill: CHART_AXIS_COLOR, fontSize: 11 }}
                    axisLine={false}
                    tickLine={false}
                    width={32}
                  />
                  <Tooltip
                    labelFormatter={(label) => formatDateLabel(String(label))}
                    formatter={(value) => [
                      formatCompactNumber(Number(value)),
                      t("settings.missionControl.analytics.dictations"),
                    ]}
                    contentStyle={CHART_TOOLTIP_STYLE}
                    labelStyle={CHART_TOOLTIP_LABEL_STYLE}
                    cursor={{ stroke: CHART_GRID_COLOR, strokeWidth: 1 }}
                  />
                  <Area
                    type="monotone"
                    dataKey="dictations"
                    stroke={CHART_VIOLET}
                    strokeWidth={2}
                    fill="url(#mcOverTimeFill)"
                    activeDot={{
                      r: 3,
                      stroke: "var(--color-of-surface)",
                      strokeWidth: 2,
                    }}
                  />
                </AreaChart>
              </ResponsiveContainer>
            </div>
          ) : (
            <EmptyState
              icon={LineChartIcon}
              message={t("settings.missionControl.analytics.overTimeEmpty")}
            />
          )}
        </Card>

        {/* (b) Top apps — single hue horizontal bars. */}
        <Card padding="md">
          <h3 className="mb-2 text-sm font-medium">
            {t("settings.missionControl.analytics.topApps")}
          </h3>
          {hasApps ? (
            <div className="h-48 w-full">
              <ResponsiveContainer width="100%" height="100%">
                <BarChart
                  data={topApps}
                  layout="vertical"
                  margin={{ top: 4, right: 28, left: 8, bottom: 4 }}
                  barCategoryGap={6}
                >
                  <XAxis type="number" hide allowDecimals={false} />
                  <YAxis
                    type="category"
                    dataKey="name"
                    width={110}
                    tick={{ fill: CHART_AXIS_COLOR, fontSize: 11 }}
                    axisLine={false}
                    tickLine={false}
                    tickFormatter={(value: string) =>
                      value.length > 16 ? `${value.slice(0, 15)}…` : value
                    }
                  />
                  <Tooltip
                    cursor={{
                      fill: "color-mix(in srgb, var(--color-mid-gray) 10%, transparent)",
                    }}
                    formatter={(value) => [
                      formatCompactNumber(Number(value)),
                      t("settings.missionControl.analytics.dictations"),
                    ]}
                    contentStyle={CHART_TOOLTIP_STYLE}
                    labelStyle={CHART_TOOLTIP_LABEL_STYLE}
                  />
                  <Bar
                    dataKey="dictations"
                    radius={[0, 4, 4, 0]}
                    maxBarSize={14}
                    label={{
                      position: "right",
                      fill: CHART_AXIS_COLOR,
                      fontSize: 11,
                      formatter: (value: React.ReactNode) =>
                        formatCompactNumber(Number(value)),
                    }}
                  >
                    {topApps.map((entry) => (
                      <Cell key={entry.name} fill={CHART_VIOLET} />
                    ))}
                  </Bar>
                </BarChart>
              </ResponsiveContainer>
            </div>
          ) : (
            <EmptyState
              icon={BarChart2}
              message={t("settings.missionControl.analytics.topAppsEmpty")}
            />
          )}
        </Card>
      </div>

      {/* Streak + top keyword chips (quiet). */}
      {(topKeywords.length > 0 || (summary?.current_streak_days ?? 0) > 0) && (
        <div className="mt-3 flex flex-wrap items-center gap-2">
          {(summary?.current_streak_days ?? 0) > 0 && (
            <span className="inline-flex items-center gap-1.5 rounded-full border border-of-hairline bg-of-raised px-2.5 py-1 text-xs font-medium text-text/70">
              <Flame className="h-3.5 w-3.5 text-of-violet" />
              {t("settings.missionControl.analytics.streak", {
                count: summary?.current_streak_days ?? 0,
              })}
            </span>
          )}
          {topKeywords.map((keyword) => (
            <span
              key={keyword.keyword}
              className="inline-flex items-center gap-1.5 rounded-full bg-mid-gray/10 px-2.5 py-1 text-xs text-text/60"
            >
              <span className="truncate max-w-[10rem]">{keyword.keyword}</span>
              <span className="tabular-nums text-text/40">
                {formatCompactNumber(keyword.count)}
              </span>
            </span>
          ))}
        </div>
      )}
    </section>
  );
};
