import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  AppWindow,
  Clock,
  Flame,
  Gauge,
  Mic,
  Type as TypeIcon,
} from "lucide-react";
import { formatCompactNumber, formatDurationCompact } from "@/lib/utils/format";
import { ModeToggle } from "../model-setup/ModeToggle";
import { StatTile } from "./StatTile";
import { OverTimeChart } from "./OverTimeChart";
import { UsageBarChart } from "./UsageBarChart";
import { TopKeywordsList } from "./TopKeywordsList";
import { PrivacyControls } from "./PrivacyControls";
import { useAnalyticsData, type AnalyticsRangeDays } from "./useAnalyticsData";

const RANGE_VALUES: { value: string; days: AnalyticsRangeDays }[] = [
  { value: "7", days: 7 },
  { value: "30", days: 30 },
  { value: "90", days: 90 },
  { value: "all", days: null },
];

interface SectionProps {
  title: string;
  children: React.ReactNode;
}

const Section: React.FC<SectionProps> = ({ title, children }) => (
  <div className="space-y-2">
    <div className="px-4">
      <h2 className="text-xs font-medium text-mid-gray uppercase tracking-wide">
        {title}
      </h2>
    </div>
    <div className="bg-background border border-mid-gray/20 rounded-lg overflow-visible">
      {children}
    </div>
  </div>
);

export const DashboardSettings: React.FC = () => {
  const { t } = useTranslation();
  const [rangeValue, setRangeValue] = useState<string>("30");

  const rangeDays = useMemo(
    () => RANGE_VALUES.find((r) => r.value === rangeValue)?.days ?? 30,
    [rangeValue],
  );

  const {
    summary,
    overTime,
    byApp,
    byProject,
    keywords,
    isLoading,
    isFetching,
    refetch,
  } = useAnalyticsData(rangeDays);

  const rangeOptions = RANGE_VALUES.map((r) => ({
    value: r.value,
    label: t(`settings.dashboard.range.${r.value}`),
  }));

  const byAppData = useMemo(
    () =>
      byApp.map((a) => ({
        name: a.app,
        dictations: a.dictations,
        words: a.words,
      })),
    [byApp],
  );
  const byProjectData = useMemo(
    () =>
      byProject.map((p) => ({
        name: p.project,
        dictations: p.dictations,
        words: p.words,
      })),
    [byProject],
  );

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="flex items-center justify-between gap-3 flex-wrap">
        <div>
          <h1 className="text-xl font-semibold mb-2">
            {t("settings.dashboard.title")}
          </h1>
          <p className="text-sm text-text/60">
            {t("settings.dashboard.description")}
          </p>
        </div>
        <ModeToggle
          value={rangeValue}
          options={rangeOptions}
          onChange={setRangeValue}
        />
      </div>

      {isLoading ? (
        <div className="flex items-center justify-center py-16">
          <div className="w-8 h-8 border-2 border-logo-primary border-t-transparent rounded-full animate-spin" />
        </div>
      ) : (
        <div
          className={`space-y-6 transition-opacity ${isFetching ? "opacity-60" : "opacity-100"}`}
        >
          {/* KPI row */}
          <div className="grid grid-cols-2 sm:grid-cols-3 gap-3">
            <StatTile
              icon={Mic}
              label={t("settings.dashboard.kpis.totalDictations")}
              value={formatCompactNumber(summary?.total_dictations ?? 0)}
            />
            <StatTile
              icon={TypeIcon}
              label={t("settings.dashboard.kpis.totalWords")}
              value={formatCompactNumber(summary?.total_words ?? 0)}
            />
            <StatTile
              icon={Gauge}
              label={t("settings.dashboard.kpis.avgWpm")}
              value={
                summary
                  ? t("settings.dashboard.kpis.wpmValue", {
                      wpm: Math.round(summary.avg_wpm),
                    })
                  : "—"
              }
            />
            <StatTile
              icon={Clock}
              label={t("settings.dashboard.kpis.timeSaved")}
              value={formatDurationCompact(summary?.time_saved_seconds ?? 0)}
            />
            <StatTile
              icon={Flame}
              label={t("settings.dashboard.kpis.streak")}
              value={t("settings.dashboard.kpis.streakValue", {
                count: summary?.current_streak_days ?? 0,
              })}
            />
            <StatTile
              icon={AppWindow}
              label={t("settings.dashboard.kpis.appsUsed")}
              value={formatCompactNumber(summary?.active_apps_count ?? 0)}
            />
          </div>

          <Section title={t("settings.dashboard.overTime.sectionTitle")}>
            <OverTimeChart data={overTime} />
          </Section>

          <Section title={t("settings.dashboard.byApp.sectionTitle")}>
            <UsageBarChart
              title={t("settings.dashboard.byApp.title")}
              emptyMessage={t("settings.dashboard.byApp.empty")}
              data={byAppData}
            />
          </Section>

          <Section title={t("settings.dashboard.byProject.sectionTitle")}>
            <UsageBarChart
              title={t("settings.dashboard.byProject.title")}
              emptyMessage={t("settings.dashboard.byProject.empty")}
              data={byProjectData}
            />
          </Section>

          <Section title={t("settings.dashboard.keywords.sectionTitle")}>
            <div className="px-4 pt-3">
              <h3 className="text-sm font-medium">
                {t("settings.dashboard.keywords.title")}
              </h3>
            </div>
            <TopKeywordsList data={keywords} />
          </Section>

          <Section title={t("settings.dashboard.privacy.sectionTitle")}>
            <PrivacyControls onCleared={refetch} />
          </Section>
        </div>
      )}
    </div>
  );
};
