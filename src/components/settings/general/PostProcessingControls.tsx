import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { SettingContainer } from "../../ui/SettingContainer";
import { Dropdown } from "../../ui/Dropdown";
import { useSettings } from "../../../hooks/useSettings";

// Sentinel dropdown value for the built-in "Write" mode (today's cleanup path).
// `default_ai_mode_id` stores `null` for it; the Dropdown needs a string value.
const WRITE_VALUE = "__write__";

/**
 * Phase D1: General-tab post-processing controls.
 *
 * - "AI post-processing" master toggle bound to the EXISTING `post_process_enabled`
 *   (one source of truth — the same setting the AI Modes / Model Setup UIs edit).
 * - "Default mode" selector → `default_ai_mode_id` (null = built-in Write cleanup).
 * - Optional light, non-AI filler filter (`basic_filler_filter`).
 *
 * When the master is OFF, the main hotkey injects raw transcription; AI modes with
 * their own hotkeys or app-rules still fire (explicit user intent) — stated in the
 * copy below.
 */
export const PostProcessingControls: React.FC = () => {
  const { t } = useTranslation();
  const { settings, getSetting, updateSetting, isUpdating } = useSettings();

  const enabled = getSetting("post_process_enabled") || false;
  const defaultModeId = getSetting("default_ai_mode_id") ?? null;
  const fillerFilter = getSetting("basic_filler_filter") || false;
  const modes = settings?.ai_modes ?? [];

  const modeOptions = [
    {
      value: WRITE_VALUE,
      label: t("settings.general.postProcessing.writeMode"),
    },
    ...modes.map((mode) => ({ value: mode.id, label: mode.name })),
  ];

  const handleDefaultModeSelect = (value: string) => {
    updateSetting("default_ai_mode_id", value === WRITE_VALUE ? null : value);
  };

  return (
    <>
      <ToggleSwitch
        checked={enabled}
        onChange={(next) => updateSetting("post_process_enabled", next)}
        isUpdating={isUpdating("post_process_enabled")}
        label={t("settings.general.postProcessing.master.label")}
        description={t("settings.general.postProcessing.master.description")}
        descriptionMode="tooltip"
        grouped={true}
      />

      <SettingContainer
        title={t("settings.general.postProcessing.defaultMode.label")}
        description={t(
          "settings.general.postProcessing.defaultMode.description",
        )}
        descriptionMode="tooltip"
        layout="horizontal"
        grouped={true}
      >
        <Dropdown
          selectedValue={defaultModeId ?? WRITE_VALUE}
          options={modeOptions}
          onSelect={handleDefaultModeSelect}
          disabled={!enabled || isUpdating("default_ai_mode_id")}
          className="min-w-[220px]"
        />
      </SettingContainer>

      <ToggleSwitch
        checked={fillerFilter}
        onChange={(next) => updateSetting("basic_filler_filter", next)}
        isUpdating={isUpdating("basic_filler_filter")}
        label={t("settings.general.postProcessing.fillerFilter.label")}
        description={t(
          "settings.general.postProcessing.fillerFilter.description",
        )}
        descriptionMode="tooltip"
        grouped={true}
      />
    </>
  );
};
