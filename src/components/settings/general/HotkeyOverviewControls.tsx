import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { ShortcutInput } from "../ShortcutInput";
import { useSettings } from "../../../hooks/useSettings";

/**
 * Phase D2: "Hotkey overview" controls in the General tab — an enable toggle
 * (`hotkey_overlay_enabled`) plus the `hotkey_overlay` ShortcutInput. The binding
 * ships unbound, so nothing happens until the user assigns a hotkey; holding it
 * then shows a cheat-sheet of every configured hotkey.
 */
export const HotkeyOverviewControls: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();

  const enabled = getSetting("hotkey_overlay_enabled") ?? true;

  return (
    <>
      <ToggleSwitch
        checked={enabled}
        onChange={(next) => updateSetting("hotkey_overlay_enabled", next)}
        isUpdating={isUpdating("hotkey_overlay_enabled")}
        label={t("settings.general.hotkeyOverview.enable.label")}
        description={t("settings.general.hotkeyOverview.enable.description")}
        descriptionMode="tooltip"
        grouped={true}
      />
      <ShortcutInput
        shortcutId="hotkey_overlay"
        descriptionMode="tooltip"
        grouped={true}
        disabled={!enabled}
      />
    </>
  );
};
