import React from "react";
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

export const Sidebar: React.FC<SidebarProps> = ({
  activeSection,
  onSectionChange,
}) => {
  const { t } = useTranslation();
  const { settings, updateSetting } = useSettings();

  const advancedMode = settings?.advanced_mode ?? false;

  const availableSections = Object.entries(SECTIONS_CONFIG)
    .filter(([id, config]) =>
      isSectionVisible(id as SidebarSection, config, settings),
    )
    .map(([id, config]) => ({ id: id as SidebarSection, ...config }));

  return (
    <div className="flex flex-col w-40 h-full border-e border-mid-gray/20 px-2 text-text">
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
    </div>
  );
};
