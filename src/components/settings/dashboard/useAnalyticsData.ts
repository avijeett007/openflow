import { useCallback, useEffect, useState } from "react";
import { commands } from "@/bindings";
import type {
  AnalyticsSummary,
  AppUsage,
  KeywordCount,
  OverTimePoint,
  ProjectUsage,
} from "@/bindings";

/** Range selector values — `null` means "all time". */
export type AnalyticsRangeDays = 7 | 30 | 90 | null;

const KEYWORDS_LIMIT = 20;

interface AnalyticsData {
  summary: AnalyticsSummary | null;
  overTime: OverTimePoint[];
  byApp: AppUsage[];
  byProject: ProjectUsage[];
  keywords: KeywordCount[];
}

const EMPTY_DATA: AnalyticsData = {
  summary: null,
  overTime: [],
  byApp: [],
  byProject: [],
  keywords: [],
};

interface UseAnalyticsDataReturn extends AnalyticsData {
  /** True only while the very first fetch for the current range is in flight. */
  isLoading: boolean;
  /** True while any (re)fetch is in flight, including background refreshes. */
  isFetching: boolean;
  refetch: () => Promise<void>;
}

/**
 * Fetches every analytics view (summary, time series, breakdowns, keywords)
 * for a given range and refetches whenever the range changes. Previous data
 * is kept on screen while a refetch is in flight (no skeleton flash).
 */
export const useAnalyticsData = (
  rangeDays: AnalyticsRangeDays,
): UseAnalyticsDataReturn => {
  const [data, setData] = useState<AnalyticsData>(EMPTY_DATA);
  const [isLoading, setIsLoading] = useState(true);
  const [isFetching, setIsFetching] = useState(false);

  const fetchAll = useCallback(async () => {
    setIsFetching(true);
    try {
      const [summaryRes, overTimeRes, byAppRes, byProjectRes, keywordsRes] =
        await Promise.all([
          commands.getAnalyticsSummary(rangeDays),
          commands.getDictationsOverTime(rangeDays),
          commands.getAnalyticsByApp(rangeDays),
          commands.getAnalyticsByProject(rangeDays),
          commands.getTopKeywords(rangeDays, KEYWORDS_LIMIT),
        ]);

      setData({
        summary: summaryRes.status === "ok" ? summaryRes.data : null,
        overTime: overTimeRes.status === "ok" ? overTimeRes.data : [],
        byApp: byAppRes.status === "ok" ? byAppRes.data : [],
        byProject: byProjectRes.status === "ok" ? byProjectRes.data : [],
        keywords: keywordsRes.status === "ok" ? keywordsRes.data : [],
      });
    } catch (error) {
      console.error("Failed to load analytics data:", error);
    } finally {
      setIsFetching(false);
      setIsLoading(false);
    }
  }, [rangeDays]);

  useEffect(() => {
    // Previous data (from the prior range) stays mounted while this runs —
    // callers dim via `isFetching` rather than swapping in a skeleton.
    fetchAll();
  }, [fetchAll]);

  return { ...data, isLoading, isFetching, refetch: fetchAll };
};
