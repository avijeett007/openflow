import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Clock, Gauge, Mic, Type as TypeIcon } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { AnalyticsSummary, OverTimePoint } from "@/bindings";
import { commands } from "@/bindings";
import { formatCompactNumber, formatDurationCompact } from "@/lib/utils/format";
import { Card } from "../../ui/Card";
import { Sparkline } from "./Sparkline";

interface HeroStatsProps {
  summary: AnalyticsSummary | null;
  overTime: OverTimePoint[];
}

type AppState = "idle" | "listening";

/** Local YYYY-MM-DD key, matching the backend's per-day over-time buckets. */
const localDayKey = (date: Date): string => {
  const y = date.getFullYear();
  const m = `${date.getMonth() + 1}`.padStart(2, "0");
  const d = `${date.getDate()}`.padStart(2, "0");
  return `${y}-${m}-${d}`;
};

const SPARK_WINDOW = 14;

interface HeroTileProps {
  icon: LucideIcon;
  label: string;
  value: string;
  spark?: number[];
  gradientId?: string;
}

const HeroTile: React.FC<HeroTileProps> = ({
  icon: Icon,
  label,
  value,
  spark,
  gradientId,
}) => (
  <Card padding="md" className="flex flex-col gap-1.5 min-w-0">
    <div className="flex items-center gap-1.5 text-text/45">
      <Icon className="h-3.5 w-3.5 shrink-0" />
      <span className="text-[11px] font-medium uppercase tracking-wide truncate">
        {label}
      </span>
    </div>
    <span className="text-2xl font-semibold tabular-nums truncate leading-tight">
      {value}
    </span>
    {spark && gradientId ? (
      <Sparkline data={spark} gradientId={gradientId} />
    ) : (
      <div className="h-8" />
    )}
  </Card>
);

export const HeroStats: React.FC<HeroStatsProps> = ({ summary, overTime }) => {
  const { t } = useTranslation();
  const [appState, setAppState] = useState<AppState>("idle");

  // Reuse the existing recording state: poll `is_recording` on a light
  // interval to reflect Idle / Listening in the hero pill. No new backend.
  useEffect(() => {
    let cancelled = false;
    const check = async () => {
      try {
        const recording = await commands.isRecording();
        if (!cancelled) setAppState(recording ? "listening" : "idle");
      } catch {
        if (!cancelled) setAppState("idle");
      }
    };
    void check();
    const interval = setInterval(() => void check(), 1500);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  const greeting = useMemo(() => {
    const hour = new Date().getHours();
    if (hour < 12) return t("settings.missionControl.hero.greetingMorning");
    if (hour < 18) return t("settings.missionControl.hero.greetingAfternoon");
    return t("settings.missionControl.hero.greetingEvening");
  }, [t]);

  const today = useMemo(() => {
    const key = localDayKey(new Date());
    return overTime.find((point) => point.date === key);
  }, [overTime]);

  const dictationSpark = useMemo(
    () => overTime.slice(-SPARK_WINDOW).map((p) => p.dictations),
    [overTime],
  );
  const wordsSpark = useMemo(
    () => overTime.slice(-SPARK_WINDOW).map((p) => p.words),
    [overTime],
  );

  const stateLabel =
    appState === "listening"
      ? t("settings.missionControl.hero.stateListening")
      : t("settings.missionControl.hero.stateIdle");

  return (
    <Card padding="lg" className="rounded-2xl of-rise-in">
      <div className="flex flex-col gap-5 lg:flex-row lg:items-center lg:justify-between">
        {/* Greeting + wordmark + state */}
        <div className="min-w-0">
          <p className="text-sm text-text/55">{greeting}</p>
          <div className="mt-1 flex items-baseline gap-2">
            {/* eslint-disable-next-line i18next/no-literal-string -- brand wordmark */}
            <span className="of-gradient-text text-[32px] font-semibold tracking-tight leading-none">
              OpenFlow
            </span>
          </div>
          <div className="mt-3 inline-flex items-center gap-2 rounded-full border border-of-hairline bg-of-raised px-3 py-1">
            <span className="relative flex h-2 w-2">
              {appState === "listening" && (
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-of-cyan opacity-75" />
              )}
              <span
                className={`relative inline-flex h-2 w-2 rounded-full ${
                  appState === "listening" ? "bg-of-cyan" : "bg-mid-gray"
                }`}
              />
            </span>
            <span className="text-xs font-medium text-text/70">
              {stateLabel}
            </span>
          </div>
        </div>

        {/* Stat tiles */}
        <div className="grid grid-cols-2 gap-3 lg:w-[62%] xl:grid-cols-4">
          <HeroTile
            icon={Mic}
            label={t("settings.missionControl.hero.dictationsToday")}
            value={formatCompactNumber(today?.dictations ?? 0)}
            spark={dictationSpark}
            gradientId="mcHeroDictations"
          />
          <HeroTile
            icon={TypeIcon}
            label={t("settings.missionControl.hero.wordsToday")}
            value={formatCompactNumber(today?.words ?? 0)}
            spark={wordsSpark}
            gradientId="mcHeroWords"
          />
          <HeroTile
            icon={Gauge}
            label={t("settings.missionControl.hero.avgWpm")}
            value={
              summary
                ? t("settings.missionControl.hero.wpmValue", {
                    wpm: Math.round(summary.avg_wpm),
                  })
                : "—"
            }
          />
          <HeroTile
            icon={Clock}
            label={t("settings.missionControl.hero.timeSaved")}
            value={formatDurationCompact(summary?.time_saved_seconds ?? 0)}
          />
        </div>
      </div>
    </Card>
  );
};
