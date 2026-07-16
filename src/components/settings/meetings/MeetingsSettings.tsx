import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Circle, Mic, Radio, Trash2, ChevronLeft, Video } from "lucide-react";
import type {
  MeetingSummary,
  MeetingDetail,
  MeetingSegmentRecord,
  MeetingCaptureStatus,
} from "@/bindings";
import { commands, events } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Button } from "../../ui/Button";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { ShortcutInput } from "../ShortcutInput";

/**
 * OpenFlow Meetings (M1) — capture + on-device transcription.
 *
 * A meetings list plus a live capture view. During a capture the transcript
 * streams in via the `meeting-segment` event (You = mic, Them = system audio),
 * with two-channel level meters from `meeting-levels`. Follows the AgentRuns
 * subscribe/unlisten-on-unmount pattern. Speaker separation (M2) and calendar
 * (M4) are out of scope here — segments carry only the channel label.
 */
export const MeetingsSettings: React.FC = () => {
  const { t } = useTranslation();
  const { settings, updateSetting } = useSettings();

  const [meetings, setMeetings] = useState<MeetingSummary[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [status, setStatus] = useState<MeetingCaptureStatus | null>(null);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [detail, setDetail] = useState<MeetingDetail | null>(null);
  const [liveSegments, setLiveSegments] = useState<MeetingSegmentRecord[]>([]);
  const [levels, setLevels] = useState<{ mic: number; system: number }>({
    mic: 0,
    system: 0,
  });
  const [elapsed, setElapsed] = useState(0);
  const startedAtRef = useRef<number | null>(null);
  const [busy, setBusy] = useState(false);

  const refreshList = useCallback(async () => {
    const result = await commands.listMeetings();
    if (result.status === "ok") {
      setMeetings(result.data);
    }
  }, []);

  const refreshStatus = useCallback(async () => {
    const result = await commands.getMeetingCaptureStatus();
    if (result.status === "ok") {
      setStatus(result.data);
      if (result.data.active && startedAtRef.current === null) {
        startedAtRef.current = Date.now();
      }
      if (!result.data.active) {
        startedAtRef.current = null;
      }
    }
  }, []);

  // Initial load + live subscriptions.
  useEffect(() => {
    let cancelled = false;
    setIsLoading(true);
    void Promise.all([refreshList(), refreshStatus()]).finally(() => {
      if (!cancelled) setIsLoading(false);
    });

    const unlistenState = events.meetingState.listen((event) => {
      const { status: s } = event.payload;
      if (s === "recording") {
        startedAtRef.current = Date.now();
        setLiveSegments([]);
        void refreshStatus();
      } else {
        // done | failed
        startedAtRef.current = null;
        setLevels({ mic: 0, system: 0 });
        void refreshStatus();
        void refreshList();
      }
    });

    const unlistenSegment = events.meetingSegmentEvent.listen((event) => {
      setLiveSegments((prev) => [...prev, event.payload.segment]);
      // If viewing the detail of the meeting being captured, fold it in too.
      setDetail((prev) =>
        prev && prev.meeting.id === event.payload.meeting_id
          ? { ...prev, segments: [...prev.segments, event.payload.segment] }
          : prev,
      );
    });

    const unlistenLevels = events.meetingLevels.listen((event) => {
      setLevels({ mic: event.payload.mic, system: event.payload.system });
    });

    return () => {
      cancelled = true;
      unlistenState.then((fn) => fn());
      unlistenSegment.then((fn) => fn());
      unlistenLevels.then((fn) => fn());
    };
  }, [refreshList, refreshStatus]);

  // Elapsed-time ticker while capturing.
  useEffect(() => {
    if (!status?.active) {
      setElapsed(0);
      return;
    }
    const id = setInterval(() => {
      if (startedAtRef.current !== null) {
        setElapsed(Math.floor((Date.now() - startedAtRef.current) / 1000));
      }
    }, 1000);
    return () => clearInterval(id);
  }, [status?.active]);

  const handleStart = async () => {
    setBusy(true);
    try {
      const result = await commands.startMeetingCapture(null);
      if (result.status === "error") {
        toast.error(t("settings.meetings.startError", { error: result.error }));
      } else {
        setLiveSegments([]);
        await refreshStatus();
      }
    } finally {
      setBusy(false);
    }
  };

  const handleStop = async () => {
    setBusy(true);
    try {
      const result = await commands.stopMeetingCapture();
      if (result.status === "error") {
        toast.error(t("settings.meetings.stopError", { error: result.error }));
      } else {
        await refreshStatus();
        await refreshList();
      }
    } finally {
      setBusy(false);
    }
  };

  const openDetail = async (id: number) => {
    setSelectedId(id);
    const result = await commands.getMeeting(id);
    if (result.status === "ok" && result.data) {
      setDetail(result.data);
    }
  };

  const closeDetail = () => {
    setSelectedId(null);
    setDetail(null);
  };

  const handleDelete = async (id: number) => {
    const result = await commands.deleteMeeting(id);
    if (result.status === "error") {
      toast.error(t("settings.meetings.deleteError", { error: result.error }));
      return;
    }
    if (selectedId === id) closeDetail();
    await refreshList();
  };

  // ---- detail view ----
  if (selectedId !== null && detail) {
    return (
      <MeetingDetailView
        detail={detail}
        onBack={closeDetail}
        onDelete={() => void handleDelete(detail.meeting.id)}
      />
    );
  }

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      {/* Capture control + live status */}
      <SettingsGroup
        title={t("settings.meetings.title")}
        description={t("settings.meetings.intro")}
      >
        <div className="px-4 py-3 space-y-3">
          {status?.active ? (
            <div className="space-y-3">
              <div className="flex items-center justify-between gap-3">
                <div className="flex items-center gap-2 text-sm">
                  <Circle className="h-3 w-3 fill-red-500 text-red-500 animate-pulse" />
                  <span className="font-medium">
                    {t("settings.meetings.capturing")}
                  </span>
                  <span className="text-mid-gray tabular-nums">
                    {formatElapsed(elapsed)}
                  </span>
                </div>
                <Button
                  type="button"
                  variant="secondary"
                  size="sm"
                  onClick={() => void handleStop()}
                  disabled={busy}
                >
                  {t("settings.meetings.stop")}
                </Button>
              </div>
              {status.mic_only && (
                <p className="text-xs text-amber-600 dark:text-amber-400">
                  {t(
                    `settings.meetings.notice.${status.notice ?? "mic_only"}`,
                    {
                      defaultValue: t("settings.meetings.notice.mic_only"),
                    },
                  )}
                </p>
              )}
              <LevelMeter
                label={t("settings.meetings.you")}
                icon={<Mic className="h-3.5 w-3.5" />}
                level={levels.mic}
              />
              <LevelMeter
                label={t("settings.meetings.them")}
                icon={<Radio className="h-3.5 w-3.5" />}
                level={status.mic_only ? 0 : levels.system}
                muted={status.mic_only}
              />
              {liveSegments.length > 0 && (
                <div className="mt-2 max-h-48 overflow-y-auto rounded-md border border-mid-gray/20 p-2 space-y-1">
                  {liveSegments.map((seg) => (
                    <TranscriptTurn key={seg.id} segment={seg} />
                  ))}
                </div>
              )}
            </div>
          ) : (
            <div className="flex items-center justify-between gap-3">
              <span className="text-xs text-mid-gray">
                {t("settings.meetings.idleHint")}
              </span>
              <Button
                type="button"
                variant="primary"
                size="sm"
                onClick={() => void handleStart()}
                disabled={busy || !settings?.meetings_enabled}
                className="inline-flex items-center gap-1.5"
              >
                <Video className="h-4 w-4" />
                {t("settings.meetings.start")}
              </Button>
            </div>
          )}
        </div>
      </SettingsGroup>

      {/* Detection settings */}
      <SettingsGroup title={t("settings.meetings.settingsTitle")}>
        <ToggleSwitch
          checked={settings?.meeting_auto_detect ?? true}
          onChange={(checked) => {
            void updateSetting("meeting_auto_detect", checked);
          }}
          label={t("settings.meetings.autoDetect")}
          description={t("settings.meetings.autoDetectHint")}
          descriptionMode="inline"
          grouped
        />
      </SettingsGroup>

      {/* Capture hotkey — a global shortcut to start/stop capture without
          opening the app. Essential for Google Meet, which runs in a browser
          tab and so can never trigger bundle-id auto-detection. */}
      <SettingsGroup title={t("settings.meetings.captureHotkeyTitle")}>
        <ShortcutInput
          shortcutId="meeting_capture"
          descriptionMode="inline"
          grouped
        />
      </SettingsGroup>

      {/* Meetings list */}
      {!isLoading && meetings.length === 0 ? (
        <div className="rounded-lg border border-dashed border-mid-gray/30 px-4 py-8 text-center text-sm text-mid-gray">
          {t("settings.meetings.emptyState")}
        </div>
      ) : (
        <div className="space-y-2">
          {meetings.map((m) => (
            <button
              key={m.id}
              type="button"
              onClick={() => void openDetail(m.id)}
              className="w-full text-left rounded-lg border border-mid-gray/20 px-4 py-3 hover:bg-mid-gray/5 transition-colors flex items-center justify-between gap-3"
            >
              <div className="min-w-0">
                <p className="text-sm font-medium truncate">{m.title}</p>
                <p className="text-xs text-mid-gray">
                  {t("settings.meetings.rowMeta", {
                    duration: formatElapsed(Math.floor(m.duration_ms / 1000)),
                    count: m.segment_count,
                  })}
                  {m.status !== "done" ? ` · ${m.status}` : ""}
                </p>
              </div>
              <Trash2
                className="h-4 w-4 shrink-0 text-mid-gray hover:text-red-500"
                onClick={(e) => {
                  e.stopPropagation();
                  void handleDelete(m.id);
                }}
              />
            </button>
          ))}
        </div>
      )}
    </div>
  );
};

const MeetingDetailView: React.FC<{
  detail: MeetingDetail;
  onBack: () => void;
  onDelete: () => void;
}> = ({ detail, onBack, onDelete }) => {
  const { t } = useTranslation();
  return (
    <div className="max-w-3xl w-full mx-auto space-y-4">
      <div className="flex items-center justify-between gap-3">
        <button
          type="button"
          onClick={onBack}
          className="inline-flex items-center gap-1 text-sm text-mid-gray hover:text-foreground"
        >
          <ChevronLeft className="h-4 w-4" />
          {t("settings.meetings.back")}
        </button>
        <Button type="button" variant="secondary" size="sm" onClick={onDelete}>
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
      <SettingsGroup title={detail.meeting.title}>
        <div className="px-4 py-3">
          {detail.segments.length === 0 ? (
            <p className="text-sm text-mid-gray">
              {t("settings.meetings.noTranscript")}
            </p>
          ) : (
            <div className="space-y-2">
              {detail.segments.map((seg) => (
                <TranscriptTurn key={seg.id} segment={seg} />
              ))}
            </div>
          )}
        </div>
      </SettingsGroup>
    </div>
  );
};

const TranscriptTurn: React.FC<{
  segment: MeetingSegmentRecord;
}> = ({ segment }) => {
  const { t } = useTranslation();
  const isMic = segment.channel === "mic";
  return (
    <div className="flex gap-2 text-sm">
      <span className="shrink-0 text-xs tabular-nums text-mid-gray mt-0.5 w-10">
        {formatElapsed(Math.floor(segment.t_start_ms / 1000))}
      </span>
      <span
        className={`shrink-0 font-medium w-12 ${
          isMic ? "text-logo-primary" : "text-emerald-600 dark:text-emerald-400"
        }`}
      >
        {isMic ? t("settings.meetings.you") : t("settings.meetings.them")}
      </span>
      <span className="min-w-0">{segment.text}</span>
    </div>
  );
};

const LevelMeter: React.FC<{
  label: string;
  icon: React.ReactNode;
  level: number;
  muted?: boolean;
}> = ({ label, icon, level, muted }) => {
  const pct = Math.min(100, Math.round(level * 300));
  return (
    <div className="flex items-center gap-2">
      <span className="flex items-center gap-1 text-xs text-mid-gray w-14 shrink-0">
        {icon}
        {label}
      </span>
      <div className="flex-1 h-1.5 rounded-full bg-mid-gray/15 overflow-hidden">
        <div
          className={`h-full rounded-full transition-all duration-75 ${
            muted ? "bg-mid-gray/40" : "bg-logo-primary"
          }`}
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
};

function formatElapsed(seconds: number): string {
  const s = Math.max(0, seconds);
  const m = Math.floor(s / 60);
  const rem = s % 60;
  return `${m}:${rem.toString().padStart(2, "0")}`;
}
