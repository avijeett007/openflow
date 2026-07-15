import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Mic } from "lucide-react";
import type { HistoryEntry } from "@/bindings";
import { commands, events } from "@/bindings";
import { useNavigationStore } from "@/stores/navigationStore";
import { formatDateTime } from "@/utils/dateFormat";
import { Card } from "../../ui/Card";
import { ModuleHeader } from "./ModuleHeader";
import { MissionControlEmptyState } from "./MissionControlEmptyState";

const RECENT_LIMIT = 5;

/** First non-empty line of a dictation; respects privacy (nulled text → dash). */
const firstLine = (entry: HistoryEntry): string => {
  const raw = entry.transcription_text?.trim();
  if (raw) {
    const line = raw.split("\n").find((l) => l.trim().length > 0);
    if (line) return line.trim();
  }
  const title = entry.title?.trim();
  return title && title.length > 0 ? title : "—";
};

export const RecentDictations: React.FC = () => {
  const { t, i18n } = useTranslation();
  const setCurrentSection = useNavigationStore((s) => s.setCurrentSection);
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const result = await commands.getHistoryEntries(null, RECENT_LIMIT);
        if (!cancelled && result.status === "ok") {
          setEntries(result.data.entries);
        }
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    };
    void load();

    // Keep in sync with the transcription pipeline (same event as History).
    const unlisten = events.historyUpdatePayload.listen((event) => {
      const payload = event.payload;
      if (payload.action === "added") {
        setEntries((prev) => [payload.entry, ...prev].slice(0, RECENT_LIMIT));
      } else if (payload.action === "updated") {
        setEntries((prev) =>
          prev.map((e) => (e.id === payload.entry.id ? payload.entry : e)),
        );
      }
    });

    return () => {
      cancelled = true;
      unlisten.then((fn) => fn());
    };
  }, []);

  return (
    <section>
      <ModuleHeader
        title={t("settings.missionControl.recent.title")}
        actionLabel={
          entries.length > 0
            ? t("settings.missionControl.recent.viewAll")
            : undefined
        }
        onAction={
          entries.length > 0 ? () => setCurrentSection("history") : undefined
        }
      />
      <Card padding="sm">
        {isLoading ? (
          <div className="flex items-center justify-center py-10">
            <div className="h-6 w-6 animate-spin rounded-full border-2 border-of-violet border-t-transparent" />
          </div>
        ) : entries.length === 0 ? (
          <MissionControlEmptyState
            icon={Mic}
            message={t("settings.missionControl.recent.empty")}
          />
        ) : (
          <div className="flex flex-col">
            {entries.map((entry) => (
              <button
                key={entry.id}
                type="button"
                onClick={() => setCurrentSection("history")}
                className="flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left hover:bg-of-raised transition-colors cursor-pointer"
              >
                <span className="w-32 shrink-0 truncate text-xs tabular-nums text-text/45">
                  {formatDateTime(String(entry.timestamp), i18n.language)}
                </span>
                <span className="min-w-0 flex-1 truncate text-sm text-text/80">
                  {firstLine(entry)}
                </span>
              </button>
            ))}
          </div>
        )}
      </Card>
    </section>
  );
};
