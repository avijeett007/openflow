import React from "react";
import { useAnalyticsData } from "../dashboard/useAnalyticsData";
import { HeroStats } from "./HeroStats";
import { LiveActivity } from "./LiveActivity";
import { AgentsRail } from "./AgentsRail";
import { AnalyticsStrip } from "./AnalyticsStrip";
import { RecentDictations } from "./RecentDictations";

/**
 * Mission Control — Flow OS increment 3. The new "AI OS" home view: live agent
 * activity, connected agents, and dictation analytics in one place.
 *
 * Data sourcing (no duplicated SQL / no new backend):
 *  - Hero + Analytics strip: the EXISTING `useAnalyticsData` hook (same
 *    `get_analytics_*` commands the Dashboard uses), all-time range.
 *  - Live activity: `list_agent_runs` + `agent-run-output`/`agent-run-status`
 *    events (same wiring as the Agent Runs panel).
 *  - Agents rail: `settings.agents` + `update_agent`.
 *  - Recent dictations: `get_history_entries` + `history-update-payload`.
 */
export const MissionControlView: React.FC = () => {
  // All-time range — the hero derives "today" from the per-day series and the
  // strip shows the full trend, matching the Dashboard's default semantics.
  const { summary, overTime, byApp, keywords } = useAnalyticsData(null);

  return (
    <div className="mx-auto w-full max-w-5xl space-y-5">
      <HeroStats summary={summary} overTime={overTime} />
      <LiveActivity />
      <AgentsRail />
      <AnalyticsStrip
        summary={summary}
        overTime={overTime}
        byApp={byApp}
        keywords={keywords}
      />
      <RecentDictations />
    </div>
  );
};
