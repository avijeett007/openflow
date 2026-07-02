import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { Hash } from "lucide-react";
import type { KeywordCount } from "@/bindings";
import { formatCompactNumber } from "@/lib/utils/format";
import { EmptyState } from "./EmptyState";

interface TopKeywordsListProps {
  data: KeywordCount[];
}

/** Ranked list of top keywords, with a subtle magnitude bar behind each row. */
export const TopKeywordsList: React.FC<TopKeywordsListProps> = ({ data }) => {
  const { t } = useTranslation();

  const maxCount = useMemo(
    () => data.reduce((max, k) => Math.max(max, k.count), 0),
    [data],
  );

  if (data.length === 0) {
    return (
      <EmptyState
        icon={Hash}
        message={t("settings.dashboard.keywords.empty")}
      />
    );
  }

  return (
    <div className="px-4 pb-3 pt-1 space-y-1">
      {data.map((keyword, index) => {
        const widthPct = maxCount > 0 ? (keyword.count / maxCount) * 100 : 0;
        return (
          <div
            key={keyword.keyword}
            className="relative flex items-center justify-between gap-3 rounded-md px-2 py-1.5 overflow-hidden"
          >
            <div
              className="absolute inset-y-0 left-0 bg-logo-primary/10"
              style={{ width: `${widthPct}%` }}
              aria-hidden="true"
            />
            <span className="relative flex items-center gap-2 min-w-0 text-sm">
              <span className="text-text/40 w-5 shrink-0 text-right tabular-nums">
                {index + 1}
              </span>
              <span className="truncate">{keyword.keyword}</span>
            </span>
            <span className="relative text-xs font-medium text-text/60 shrink-0 tabular-nums">
              {formatCompactNumber(keyword.count)}
            </span>
          </div>
        );
      })}
    </div>
  );
};
