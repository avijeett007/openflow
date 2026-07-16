import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Activity,
  BarChart3,
  Bot,
  Cog,
  Ear,
  FlaskConical,
  History,
  Home,
  Info,
  Sparkles,
  Cpu,
  SlidersHorizontal,
  BookA,
  Radar,
  Video,
} from "lucide-react";
import { useSettings } from "../hooks/useSettings";
import openflowLogo from "../assets/openflow-logo.png";
import {
  GeneralSettings,
  AdvancedSettings,
  HistorySettings,
  DebugSettings,
  AboutSettings,
  PostProcessingSettings,
  DictionarySettings,
  ModelsSettings,
  ModelSetupSettings,
  DashboardSettings,
  HandsFreeSettings,
  AgentsSettings,
  AgentRunsSettings,
  MissionControlSettings,
  MeetingsSettings,
} from "./settings";

export type SidebarSection = keyof typeof SECTIONS_CONFIG;

interface IconProps {
  width?: number | string;
  height?: number | string;
  size?: number | string;
  className?: string;
  [key: string]: any;
}

interface SectionConfig {
  labelKey: string;
  icon: React.ComponentType<IconProps>;
  component: React.ComponentType;
  enabled: (settings: any) => boolean;
}

export const SECTIONS_CONFIG = {
  missionControl: {
    labelKey: "sidebar.missionControl",
    icon: Radar,
    component: MissionControlSettings,
    enabled: () => true,
  },
  general: {
    labelKey: "sidebar.general",
    icon: Home,
    component: GeneralSettings,
    enabled: () => true,
  },
  models: {
    labelKey: "sidebar.models",
    icon: Cpu,
    component: ModelsSettings,
    enabled: () => true,
  },
  modelSetup: {
    labelKey: "sidebar.modelSetup",
    icon: SlidersHorizontal,
    component: ModelSetupSettings,
    enabled: () => true,
  },
  handsFree: {
    labelKey: "sidebar.handsFree",
    icon: Ear,
    component: HandsFreeSettings,
    enabled: () => true,
  },
  advanced: {
    labelKey: "sidebar.advanced",
    icon: Cog,
    component: AdvancedSettings,
    enabled: () => true,
  },
  history: {
    labelKey: "sidebar.history",
    icon: History,
    component: HistorySettings,
    enabled: () => true,
  },
  dashboard: {
    labelKey: "sidebar.dashboard",
    icon: BarChart3,
    component: DashboardSettings,
    enabled: () => true,
  },
  dictionary: {
    labelKey: "sidebar.dictionary",
    icon: BookA,
    component: DictionarySettings,
    enabled: () => true,
  },
  agents: {
    labelKey: "sidebar.agents",
    icon: Bot,
    component: AgentsSettings,
    enabled: () => true,
  },
  agentRuns: {
    labelKey: "sidebar.agentRuns",
    icon: Activity,
    component: AgentRunsSettings,
    enabled: () => true,
  },
  meetings: {
    labelKey: "sidebar.meetings",
    icon: Video,
    component: MeetingsSettings,
    enabled: () => true,
  },
  postprocessing: {
    labelKey: "sidebar.postProcessing",
    icon: Sparkles,
    component: PostProcessingSettings,
    enabled: (settings) => settings?.post_process_enabled ?? false,
  },
  debug: {
    labelKey: "sidebar.debug",
    icon: FlaskConical,
    component: DebugSettings,
    enabled: (settings) => settings?.debug_mode ?? false,
  },
  about: {
    labelKey: "sidebar.about",
    icon: Info,
    component: AboutSettings,
    enabled: () => true,
  },
} as const satisfies Record<string, SectionConfig>;

// BASIC_SECTIONS: the core speech-to-text sections a normal (Basic-mode) user
// sees. Everything else is hidden until "Advanced mode" is turned on in the
// sidebar footer. Edit this array to change what counts as "essential".
// (`about` is intentionally NOT listed here — it is help/info and is ALWAYS
// visible in both Basic and Advanced mode; see visibility logic below.)
const BASIC_SECTIONS: SidebarSection[] = [
  "missionControl",
  "general",
  "models",
  "modelSetup",
];

// Sections badged as experimental in the sidebar (a small "Experimental" tag is
// shown next to the label). These are advanced-only, still-maturing features.
const EXPERIMENTAL_SECTIONS: SidebarSection[] = ["handsFree"];

// A section is visible iff:
//   - it is `about` (always shown, both modes), OR
//   - Advanced mode is on AND its own enabled() predicate passes, OR
//   - it is a BASIC_SECTIONS id AND its own enabled() predicate passes.
// The enabled() predicate is always honoured, so e.g. `postprocessing` still
// also requires `post_process_enabled` and `debug` still requires `debug_mode`.
export const isSectionVisible = (
  id: SidebarSection,
  config: SectionConfig,
  settings: any,
): boolean => {
  if (id === "about") return true;
  if (!config.enabled(settings)) return false;
  return (settings?.advanced_mode ?? false) || BASIC_SECTIONS.includes(id);
};

interface SidebarProps {
  activeSection: SidebarSection;
  onSectionChange: (section: SidebarSection) => void;
}

// Resizable sidebar width — a pure UI preference persisted in localStorage
// (not a Rust/tauri-store setting; nothing here affects the dictation
// pipeline or is worth round-tripping through the backend).
const SIDEBAR_WIDTH_STORAGE_KEY = "openflow.sidebarWidth";
const MIN_SIDEBAR_WIDTH = 176;
const MAX_SIDEBAR_WIDTH = 320;
const DEFAULT_SIDEBAR_WIDTH = 208;
const SIDEBAR_WIDTH_KEYBOARD_STEP = 8;

const clampSidebarWidth = (width: number): number =>
  Math.min(MAX_SIDEBAR_WIDTH, Math.max(MIN_SIDEBAR_WIDTH, width));

const readStoredSidebarWidth = (): number => {
  if (typeof window === "undefined") return DEFAULT_SIDEBAR_WIDTH;
  try {
    const stored = window.localStorage.getItem(SIDEBAR_WIDTH_STORAGE_KEY);
    if (!stored) return DEFAULT_SIDEBAR_WIDTH;
    const parsed = Number(stored);
    if (!Number.isFinite(parsed)) return DEFAULT_SIDEBAR_WIDTH;
    return clampSidebarWidth(parsed);
  } catch {
    return DEFAULT_SIDEBAR_WIDTH;
  }
};

const persistSidebarWidth = (width: number) => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(SIDEBAR_WIDTH_STORAGE_KEY, String(width));
  } catch {
    // Best-effort only — a resize that fails to persist just resets to the
    // default next launch, which is harmless.
  }
};

export const Sidebar: React.FC<SidebarProps> = ({
  activeSection,
  onSectionChange,
}) => {
  const { t, i18n } = useTranslation();
  const { settings, updateSetting } = useSettings();

  const advancedMode = settings?.advanced_mode ?? false;

  const availableSections = Object.entries(SECTIONS_CONFIG)
    .filter(([id, config]) =>
      isSectionVisible(id as SidebarSection, config, settings),
    )
    .map(([id, config]) => ({ id: id as SidebarSection, ...config }));

  const [width, setWidth] = useState<number>(readStoredSidebarWidth);
  const [isDragging, setIsDragging] = useState(false);
  const dragStateRef = useRef<{
    pointerId: number;
    startX: number;
    startWidth: number;
    // The sidebar sits on the inline-end side of its resize handle in both
    // directions (it's the first flex child, so `dir` flips which physical
    // side that is). Flip the drag delta's sign in RTL so "drag away from
    // the content area" always widens the sidebar.
    dirSign: 1 | -1;
  } | null>(null);

  const handlePointerMove = useCallback((e: PointerEvent) => {
    const dragState = dragStateRef.current;
    if (!dragState || e.pointerId !== dragState.pointerId) return;
    const delta = (e.clientX - dragState.startX) * dragState.dirSign;
    setWidth(clampSidebarWidth(dragState.startWidth + delta));
  }, []);

  const endDrag = useCallback(() => {
    dragStateRef.current = null;
    setIsDragging(false);
    document.body.style.removeProperty("user-select");
    window.removeEventListener("pointermove", handlePointerMove);
    window.removeEventListener("pointerup", endDrag);
    window.removeEventListener("pointercancel", endDrag);
    setWidth((current) => {
      persistSidebarWidth(current);
      return current;
    });
  }, [handlePointerMove]);

  const handlePointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      // Only the primary button/touch starts a resize.
      if (e.button !== 0 && e.pointerType === "mouse") return;
      dragStateRef.current = {
        pointerId: e.pointerId,
        startX: e.clientX,
        startWidth: width,
        dirSign: i18n.dir() === "rtl" ? -1 : 1,
      };
      setIsDragging(true);
      document.body.style.userSelect = "none";
      window.addEventListener("pointermove", handlePointerMove);
      window.addEventListener("pointerup", endDrag);
      window.addEventListener("pointercancel", endDrag);
    },
    [width, i18n, handlePointerMove, endDrag],
  );

  const handleDoubleClick = useCallback(() => {
    setWidth(DEFAULT_SIDEBAR_WIDTH);
    persistSidebarWidth(DEFAULT_SIDEBAR_WIDTH);
  }, []);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      const isRtl = i18n.dir() === "rtl";
      // "Widen" step: ArrowRight widens in LTR, ArrowLeft widens in RTL —
      // mirrors which physical direction is "away from the content area".
      const widenKey = isRtl ? "ArrowLeft" : "ArrowRight";
      const narrowKey = isRtl ? "ArrowRight" : "ArrowLeft";
      let next: number | null = null;
      if (e.key === widenKey) {
        next = clampSidebarWidth(width + SIDEBAR_WIDTH_KEYBOARD_STEP);
      } else if (e.key === narrowKey) {
        next = clampSidebarWidth(width - SIDEBAR_WIDTH_KEYBOARD_STEP);
      } else if (e.key === "Home") {
        next = MIN_SIDEBAR_WIDTH;
      } else if (e.key === "End") {
        next = MAX_SIDEBAR_WIDTH;
      } else {
        return;
      }
      e.preventDefault();
      setWidth(next);
      persistSidebarWidth(next);
    },
    [width, i18n],
  );

  // Clean up any window listeners if the component unmounts mid-drag.
  useEffect(() => {
    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", endDrag);
      window.removeEventListener("pointercancel", endDrag);
      document.body.style.removeProperty("user-select");
    };
  }, [handlePointerMove, endDrag]);

  return (
    <div
      className={`relative flex flex-col shrink-0 h-full border-e border-mid-gray/20 px-2 text-text ${
        isDragging ? "" : "transition-[width] duration-150 ease-out"
      }`}
      style={{ width: `${width}px` }}
    >
      {/* Fixed header: logo mark + wordmark */}
      <div className="m-4 flex items-center gap-1.5 select-none shrink-0">
        <img
          src={openflowLogo}
          alt=""
          aria-hidden="true"
          className="w-6 h-6 rounded-md shrink-0"
        />
        <div className="flex items-baseline gap-0.5">
          {/* eslint-disable-next-line i18next/no-literal-string -- brand wordmark, not translatable content */}
          <span className="text-xl font-extrabold tracking-tight text-logo-primary">
            Open
          </span>
          {/* eslint-disable-next-line i18next/no-literal-string -- brand wordmark, not translatable content */}
          <span className="text-xl font-extrabold tracking-tight text-text">
            Flow
          </span>
        </div>
      </div>

      {/* Scrollable section list — only this region scrolls */}
      <div className="flex-1 min-h-0 overflow-y-auto flex flex-col w-full items-center gap-1 pt-2 border-t border-mid-gray/20">
        {availableSections.map((section) => {
          const Icon = section.icon;
          const isActive = activeSection === section.id;
          const isExperimental = EXPERIMENTAL_SECTIONS.includes(section.id);

          return (
            <div
              key={section.id}
              className={`flex gap-2 items-center p-2 w-full rounded-lg cursor-pointer transition-colors ${
                isActive
                  ? "bg-logo-primary/80 text-white"
                  : "hover:bg-mid-gray/20 hover:opacity-100 opacity-85"
              }`}
              onClick={() => onSectionChange(section.id)}
            >
              <Icon width={24} height={24} className="shrink-0" />
              <div className="flex flex-col min-w-0">
                <p
                  className="text-sm font-medium truncate"
                  title={t(section.labelKey)}
                >
                  {t(section.labelKey)}
                </p>
                {isExperimental && (
                  <span className="text-[9px] leading-tight font-semibold uppercase tracking-wide text-logo-primary">
                    {t("common.experimental")}
                  </span>
                )}
              </div>
            </div>
          );
        })}
      </div>

      {/* Fixed footer: Advanced mode toggle (always visible in both modes) */}
      <div className="shrink-0 w-full border-t border-mid-gray/20 py-3">
        <label
          className="flex items-center justify-between gap-2 cursor-pointer px-1"
          title={t("sidebar.advancedModeTooltip")}
        >
          <span className="text-xs font-medium truncate">
            {t("sidebar.advancedMode")}
          </span>
          <input
            type="checkbox"
            className="sr-only peer"
            checked={advancedMode}
            onChange={(e) => updateSetting("advanced_mode", e.target.checked)}
          />
          <div className="relative w-9 h-5 bg-mid-gray/20 peer-focus:outline-none peer-focus:ring-2 peer-focus:ring-logo-primary rounded-full peer peer-checked:after:translate-x-full rtl:peer-checked:after:-translate-x-full after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-4 after:w-4 after:transition-all peer-checked:bg-background-ui shrink-0"></div>
        </label>
      </div>

      {/* Resize handle — sits on the sidebar's inline-end edge (the border
          that touches the content area, in both LTR and RTL). A slim hit
          area keeps the visual footprint hairline-subtle while still being
          comfortably grabbable. */}
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label={t("sidebar.resizeHandle")}
        aria-valuenow={width}
        aria-valuemin={MIN_SIDEBAR_WIDTH}
        aria-valuemax={MAX_SIDEBAR_WIDTH}
        tabIndex={0}
        className={`absolute inset-y-0 -end-[3px] w-1.5 cursor-col-resize touch-none select-none rounded-full focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary/70 ${
          isDragging
            ? "bg-logo-primary/60"
            : "bg-transparent hover:bg-logo-primary/30"
        }`}
        onPointerDown={handlePointerDown}
        onDoubleClick={handleDoubleClick}
        onKeyDown={handleKeyDown}
      />
    </div>
  );
};
