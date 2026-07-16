import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { listen } from "@tauri-apps/api/event";
import {
  Circle,
  Mic,
  Radio,
  Trash2,
  ChevronLeft,
  Video,
  Users,
  Check,
  Loader2,
  Download,
  Pencil,
} from "lucide-react";
import type {
  MeetingSummary,
  MeetingDetail,
  MeetingSegmentRecord,
  MeetingCaptureStatus,
  MeetingSpeakerRecord,
  DiarizationStatus,
} from "@/bindings";
import { commands, events } from "@/bindings";
import { CHART_PALETTE } from "@/lib/chartPalette";
import { useSettings } from "../../../hooks/useSettings";
import { Button } from "../../ui/Button";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { ShortcutInput } from "../ShortcutInput";

/**
 * OpenFlow Meetings — capture + on-device transcription (M1) with local speaker
 * diarization (M2).
 *
 * M2 adds speaker-colored transcript labels ("You" for the mic channel by
 * construction; "Speaker 1/2/…" or a per-meeting rename for the diarized remote
 * channel), a diarization status chip (provisional / finalizing / done / off),
 * the diarization settings toggle + model-download card, and the
 * concurrent-dictation refinement (segments spoken *to OpenFlow* during the call
 * are dimmed and can be hidden). Labels can retro-change after the canonical
 * final pass, so the transcript re-renders on `meeting-speakers-updated`.
 */

/** Private flag bit on `meeting_segments.flags` (mirrors the Rust constant). */
const SEGMENT_FLAG_PRIVATE = 1;

/** Fixed, CVD-safe speaker colors — the validated chartPalette order. "You"
 * keeps its own brand style and never draws from this. */
function speakerColor(localSpeaker: number): string {
  return CHART_PALETTE[localSpeaker % CHART_PALETTE.length];
}

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

  // Diarization availability + model download state.
  const [diar, setDiar] = useState<DiarizationStatus | null>(null);
  const [modelsInstalled, setModelsInstalled] = useState<boolean>(true);
  const [modelSizeMb, setModelSizeMb] = useState<number>(0);
  const [downloadPct, setDownloadPct] = useState<number | null>(null);

  // Speaker-name lookup for the live view (from the open detail, if any).
  const liveSpeakerNames = useMemo(() => {
    if (!detail || selectedId === null) return {} as Record<number, string>;
    const map: Record<number, string> = {};
    for (const s of detail.speakers ?? []) map[s.local_speaker] = s.name;
    return map;
  }, [detail, selectedId]);

  const refreshList = useCallback(async () => {
    const result = await commands.listMeetings();
    if (result.status === "ok") setMeetings(result.data);
  }, []);

  const refreshDiar = useCallback(async () => {
    const [s, m] = await Promise.all([
      commands.getDiarizationStatus(),
      commands.getDiarizationModelsStatus(),
    ]);
    if (s.status === "ok") setDiar(s.data);
    if (m.status === "ok") {
      setModelsInstalled(m.data.installed);
      setModelSizeMb(m.data.size_mb);
    }
  }, []);

  const refreshStatus = useCallback(async () => {
    const result = await commands.getMeetingCaptureStatus();
    if (result.status === "ok") {
      setStatus(result.data);
      if (result.data.active && startedAtRef.current === null) {
        startedAtRef.current = Date.now();
      }
      if (!result.data.active) startedAtRef.current = null;
    }
  }, []);

  // Initial load + live subscriptions.
  useEffect(() => {
    let cancelled = false;
    setIsLoading(true);
    void Promise.all([refreshList(), refreshStatus(), refreshDiar()]).finally(
      () => {
        if (!cancelled) setIsLoading(false);
      },
    );

    const unlistenState = events.meetingState.listen((event) => {
      const { status: s } = event.payload;
      if (s === "recording") {
        startedAtRef.current = Date.now();
        setLiveSegments([]);
        void refreshStatus();
      } else {
        // processing | done | failed
        if (s !== "processing") {
          startedAtRef.current = null;
          setLevels({ mic: 0, system: 0 });
        }
        void refreshStatus();
        void refreshList();
        // A processing→done transition can carry new speaker labels; reload the
        // open detail so the transcript firms up.
        setSelectedId((cur) => {
          if (cur !== null) void reloadDetail(cur);
          return cur;
        });
      }
    });

    const unlistenSegment = events.meetingSegmentEvent.listen((event) => {
      setLiveSegments((prev) => [...prev, event.payload.segment]);
      setDetail((prev) =>
        prev && prev.meeting.id === event.payload.meeting_id
          ? { ...prev, segments: [...prev.segments, event.payload.segment] }
          : prev,
      );
    });

    const unlistenLevels = events.meetingLevels.listen((event) => {
      setLevels({ mic: event.payload.mic, system: event.payload.system });
    });

    // Diarization relabeled some segments (provisional cycle or final pass) —
    // reload the open detail so past turns re-render with new speaker labels.
    const unlistenSpeakers = events.meetingSpeakersUpdated.listen((event) => {
      setSelectedId((cur) => {
        if (cur === event.payload.meeting_id) void reloadDetail(cur);
        return cur;
      });
    });

    const unlistenDownload = listen<{
      stage: string;
      percentage: number;
      error?: string | null;
    }>("diarization-model-progress", (event) => {
      const { stage, percentage, error } = event.payload;
      if (stage === "done") {
        setDownloadPct(null);
        setModelsInstalled(true);
        void refreshDiar();
      } else if (stage === "error") {
        setDownloadPct(null);
        toast.error(
          t("settings.meetings.diarization.downloadError", {
            error: error ?? "",
          }),
        );
      } else {
        setDownloadPct(Math.round(percentage));
      }
    });

    return () => {
      cancelled = true;
      unlistenState.then((fn) => fn());
      unlistenSegment.then((fn) => fn());
      unlistenLevels.then((fn) => fn());
      unlistenSpeakers.then((fn) => fn());
      unlistenDownload.then((fn) => fn());
    };
  }, [refreshList, refreshStatus, refreshDiar]);

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

  const reloadDetail = async (id: number) => {
    const result = await commands.getMeeting(id);
    if (result.status === "ok" && result.data) setDetail(result.data);
  };

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
    await reloadDetail(id);
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

  // Enabling diarization triggers the one-time model download (never at
  // startup). Disabling just flips the setting; models stay on disk.
  const handleToggleDiarization = async (checked: boolean) => {
    await updateSetting("meetings_diarization", checked);
    void refreshDiar();
    if (checked && !modelsInstalled && downloadPct === null) {
      void handleDownloadModels();
    }
  };

  const handleDownloadModels = async () => {
    setDownloadPct(0);
    const result = await commands.downloadDiarizationModels();
    if (result.status === "error") {
      setDownloadPct(null);
      toast.error(
        t("settings.meetings.diarization.downloadError", {
          error: result.error,
        }),
      );
    }
  };

  // ---- detail view ----
  if (selectedId !== null && detail) {
    return (
      <MeetingDetailView
        detail={detail}
        onBack={closeDetail}
        onDelete={() => void handleDelete(detail.meeting.id)}
        onReload={() => void reloadDetail(detail.meeting.id)}
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
                  <DiarizationChip diar={diar} live />
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
                    { defaultValue: t("settings.meetings.notice.mic_only") },
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
                    <TranscriptTurn
                      key={seg.id}
                      segment={seg}
                      speakerNames={liveSpeakerNames}
                    />
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

      {/* Speaker diarization (M2) */}
      <SettingsGroup title={t("settings.meetings.diarization.title")}>
        <ToggleSwitch
          checked={settings?.meetings_diarization ?? true}
          onChange={(checked) => void handleToggleDiarization(checked)}
          label={t("settings.meetings.diarization.enable")}
          description={t("settings.meetings.diarization.enableHint")}
          descriptionMode="inline"
          grouped
        />
        <DiarizationModelCard
          installed={modelsInstalled}
          sizeMb={modelSizeMb}
          downloadPct={downloadPct}
          onDownload={() => void handleDownloadModels()}
        />
        <ToggleSwitch
          checked={settings?.meetings_diarization_provisional ?? false}
          onChange={(checked) => {
            void updateSetting("meetings_diarization_provisional", checked);
            void refreshDiar();
          }}
          label={t("settings.meetings.diarization.provisional")}
          description={t("settings.meetings.diarization.provisionalHint")}
          descriptionMode="inline"
          grouped
        />
      </SettingsGroup>

      {/* Capture hotkey */}
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
  onReload: () => void;
}> = ({ detail, onBack, onDelete, onReload }) => {
  const { t } = useTranslation();
  const [hidePrivate, setHidePrivate] = useState(false);

  const speakerNames = useMemo(() => {
    const map: Record<number, string> = {};
    for (const s of detail.speakers ?? []) map[s.local_speaker] = s.name;
    return map;
  }, [detail.speakers]);

  // Distinct remote speakers actually present in the transcript.
  const speakers = useMemo(() => {
    const set = new Set<number>();
    for (const seg of detail.segments) {
      if (seg.channel === "system" && seg.local_speaker !== null) {
        set.add(seg.local_speaker);
      }
    }
    return [...set].sort((a, b) => a - b);
  }, [detail.segments]);

  const hasPrivate = detail.segments.some(
    (s) => ((s.flags ?? 0) & SEGMENT_FLAG_PRIVATE) !== 0,
  );

  const segments = hidePrivate
    ? detail.segments.filter(
        (s) => ((s.flags ?? 0) & SEGMENT_FLAG_PRIVATE) === 0,
      )
    : detail.segments;

  const diarStatus: "processing" | "done" | "none" =
    detail.meeting.status === "processing"
      ? "processing"
      : detail.meeting.diarized
        ? "done"
        : "none";

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
        <div className="px-4 py-3 space-y-3">
          {/* Diarization status + speaker chips (rename inline) */}
          <div className="flex items-center justify-between gap-2 flex-wrap">
            <DetailDiarizationChip status={diarStatus} />
            {hasPrivate && (
              <label className="inline-flex items-center gap-1.5 text-xs text-mid-gray cursor-pointer">
                <input
                  type="checkbox"
                  checked={hidePrivate}
                  onChange={(e) => setHidePrivate(e.target.checked)}
                  className="accent-logo-primary"
                />
                {t("settings.meetings.diarization.hidePrivate")}
              </label>
            )}
          </div>

          {speakers.length > 0 && (
            <div className="flex flex-wrap gap-2">
              {speakers.map((s) => (
                <SpeakerChip
                  key={s}
                  meetingId={detail.meeting.id}
                  localSpeaker={s}
                  name={speakerNames[s]}
                  onRenamed={onReload}
                />
              ))}
            </div>
          )}

          {segments.length === 0 ? (
            <p className="text-sm text-mid-gray">
              {t("settings.meetings.noTranscript")}
            </p>
          ) : (
            <div className="space-y-2">
              {segments.map((seg) => (
                <TranscriptTurn
                  key={seg.id}
                  segment={seg}
                  speakerNames={speakerNames}
                />
              ))}
            </div>
          )}
        </div>
      </SettingsGroup>
    </div>
  );
};

/** An editable per-meeting speaker chip. */
const SpeakerChip: React.FC<{
  meetingId: number;
  localSpeaker: number;
  name?: string;
  onRenamed: () => void;
}> = ({ meetingId, localSpeaker, name, onRenamed }) => {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState(name ?? "");
  const color = speakerColor(localSpeaker);
  const display =
    name ??
    t("settings.meetings.diarization.speakerN", { n: localSpeaker + 1 });

  const save = async () => {
    setEditing(false);
    const result = await commands.renameMeetingSpeaker(
      meetingId,
      localSpeaker,
      value.trim(),
    );
    if (result.status === "error") {
      toast.error(
        t("settings.meetings.diarization.renameError", {
          error: result.error,
        }),
      );
      return;
    }
    onRenamed();
  };

  if (editing) {
    return (
      <span className="inline-flex items-center gap-1 rounded-full border border-mid-gray/30 pl-2 pr-1 py-0.5 text-xs">
        <span
          className="h-2.5 w-2.5 rounded-full shrink-0"
          style={{ backgroundColor: color }}
        />
        <input
          autoFocus
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
            if (e.key === "Escape") setEditing(false);
          }}
          placeholder={t("settings.meetings.diarization.speakerN", {
            n: localSpeaker + 1,
          })}
          className="w-24 bg-transparent outline-none"
        />
        <button
          type="button"
          onClick={() => void save()}
          className="p-0.5 text-mid-gray hover:text-foreground"
          aria-label={t("settings.meetings.diarization.save")}
        >
          <Check className="h-3 w-3" />
        </button>
      </span>
    );
  }

  return (
    <button
      type="button"
      onClick={() => {
        setValue(name ?? "");
        setEditing(true);
      }}
      className="inline-flex items-center gap-1.5 rounded-full border border-mid-gray/30 px-2 py-0.5 text-xs hover:bg-mid-gray/5"
      title={t("settings.meetings.diarization.rename")}
    >
      <span
        className="h-2.5 w-2.5 rounded-full shrink-0"
        style={{ backgroundColor: color }}
      />
      <span style={{ color }}>{display}</span>
      <Pencil className="h-3 w-3 text-mid-gray" />
    </button>
  );
};

/** Live status chip during capture. */
const DiarizationChip: React.FC<{
  diar: DiarizationStatus | null;
  live?: boolean;
}> = ({ diar, live }) => {
  const { t } = useTranslation();
  if (!diar || diar.mode === "off") {
    return (
      <span className="inline-flex items-center gap-1 rounded-full bg-mid-gray/15 px-2 py-0.5 text-[10px] text-mid-gray">
        <Users className="h-2.5 w-2.5" />
        {t("settings.meetings.diarization.chip.off")}
      </span>
    );
  }
  const key = diar.mode === "provisional" ? "provisional" : "finalPass";
  return (
    <span className="inline-flex items-center gap-1 rounded-full bg-logo-primary/15 px-2 py-0.5 text-[10px] text-logo-primary">
      <Users className="h-2.5 w-2.5" />
      {live
        ? t(`settings.meetings.diarization.chip.${key}`)
        : t("settings.meetings.diarization.chip.on")}
    </span>
  );
};

/** Detail-view status chip: finalizing / done. */
const DetailDiarizationChip: React.FC<{
  status: "processing" | "done" | "none";
}> = ({ status }) => {
  const { t } = useTranslation();
  if (status === "processing") {
    return (
      <span className="inline-flex items-center gap-1 rounded-full bg-amber-500/15 px-2 py-0.5 text-[11px] text-amber-600 dark:text-amber-400">
        <Loader2 className="h-3 w-3 animate-spin" />
        {t("settings.meetings.diarization.chip.finalizing")}
      </span>
    );
  }
  if (status === "done") {
    return (
      <span className="inline-flex items-center gap-1 rounded-full bg-emerald-500/15 px-2 py-0.5 text-[11px] text-emerald-600 dark:text-emerald-400">
        <Check className="h-3 w-3" />
        {t("settings.meetings.diarization.chip.done")}
      </span>
    );
  }
  return <span />;
};

/** The diarization model-download card. */
const DiarizationModelCard: React.FC<{
  installed: boolean;
  sizeMb: number;
  downloadPct: number | null;
  onDownload: () => void;
}> = ({ installed, sizeMb, downloadPct, onDownload }) => {
  const { t } = useTranslation();
  return (
    <div className="px-4 py-3 border-t border-mid-gray/15">
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0">
          <p className="text-sm">
            {t("settings.meetings.diarization.modelsTitle")}
          </p>
          <p className="text-xs text-mid-gray">
            {installed
              ? t("settings.meetings.diarization.modelsInstalled")
              : t("settings.meetings.diarization.modelsSize", { size: sizeMb })}
          </p>
        </div>
        {installed ? (
          <span className="inline-flex items-center gap-1 text-xs text-emerald-600 dark:text-emerald-400">
            <Check className="h-4 w-4" />
            {t("settings.meetings.diarization.modelsReady")}
          </span>
        ) : downloadPct !== null ? (
          <span className="inline-flex items-center gap-1.5 text-xs text-mid-gray tabular-nums">
            <Loader2 className="h-4 w-4 animate-spin" />
            {downloadPct}%
          </span>
        ) : (
          <Button
            type="button"
            variant="secondary"
            size="sm"
            onClick={onDownload}
            className="inline-flex items-center gap-1.5"
          >
            <Download className="h-4 w-4" />
            {t("settings.meetings.diarization.download")}
          </Button>
        )}
      </div>
      {downloadPct !== null && (
        <div className="mt-2 h-1.5 rounded-full bg-mid-gray/15 overflow-hidden">
          <div
            className="h-full rounded-full bg-logo-primary transition-all"
            style={{ width: `${downloadPct}%` }}
          />
        </div>
      )}
    </div>
  );
};

const TranscriptTurn: React.FC<{
  segment: MeetingSegmentRecord;
  speakerNames: Record<number, string>;
}> = ({ segment, speakerNames }) => {
  const { t } = useTranslation();
  const isMic = segment.channel === "mic";
  const isPrivate = ((segment.flags ?? 0) & SEGMENT_FLAG_PRIVATE) !== 0;

  let label: string;
  let color: string | undefined;
  let className = "";
  if (isMic) {
    label = isPrivate
      ? t("settings.meetings.diarization.youPrivate")
      : t("settings.meetings.you");
    className = "text-logo-primary";
  } else if (segment.local_speaker !== null) {
    label =
      speakerNames[segment.local_speaker] ??
      t("settings.meetings.diarization.speakerN", {
        n: segment.local_speaker + 1,
      });
    color = speakerColor(segment.local_speaker);
  } else {
    // M1 fallback: undiarized remote channel.
    label = t("settings.meetings.them");
    className = "text-emerald-600 dark:text-emerald-400";
  }

  return (
    <div className={`flex gap-2 text-sm ${isPrivate ? "opacity-50" : ""}`}>
      <span className="shrink-0 text-xs tabular-nums text-mid-gray mt-0.5 w-10">
        {formatElapsed(Math.floor(segment.t_start_ms / 1000))}
      </span>
      <span
        className={`shrink-0 font-medium w-16 truncate ${className}`}
        style={color ? { color } : undefined}
        title={label}
      >
        {label}
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
