import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check } from "lucide-react";
import { commands } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { SettingContainer } from "../../ui/SettingContainer";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { Slider } from "../../ui/Slider";
import { Input } from "../../ui/Input";
import { Button } from "../../ui/Button";

export const HandsFreeSettings: React.FC = () => {
  const { t } = useTranslation();
  const { settings, refreshSettings } = useSettings();

  const handsFreeEnabled = settings?.hands_free_enabled ?? false;

  const [isTogglingEnabled, setIsTogglingEnabled] = useState(false);

  const [wakeWordInput, setWakeWordInput] = useState("");
  const [isSavingWakeWord, setIsSavingWakeWord] = useState(false);
  const [wakeWordSaved, setWakeWordSaved] = useState(false);
  const [wakeWordError, setWakeWordError] = useState<string | null>(null);

  const [sensitivity, setSensitivity] = useState(0.8);

  // Keep local editable state in sync with the persisted settings whenever
  // they change (initial load, or an update from elsewhere).
  useEffect(() => {
    if (settings) {
      setWakeWordInput(settings.wake_word ?? "");
      setSensitivity(settings.wake_word_sensitivity ?? 0.8);
    }
  }, [settings?.wake_word, settings?.wake_word_sensitivity]);

  const handleToggleEnabled = async (enabled: boolean) => {
    setIsTogglingEnabled(true);
    try {
      await commands.setHandsFreeEnabled(enabled);
      await refreshSettings();
    } finally {
      setIsTogglingEnabled(false);
    }
  };

  const handleSaveWakeWord = async () => {
    const trimmed = wakeWordInput.trim();
    if (!trimmed) {
      setWakeWordError(t("settings.handsFree.wakeWord.emptyError"));
      return;
    }
    setIsSavingWakeWord(true);
    setWakeWordError(null);
    setWakeWordSaved(false);
    try {
      const result = await commands.setWakeWord(trimmed);
      if (result.status === "ok") {
        await refreshSettings();
        setWakeWordSaved(true);
      } else {
        setWakeWordError(result.error);
      }
    } finally {
      setIsSavingWakeWord(false);
    }
  };

  const handleSensitivityChange = async (value: number) => {
    setSensitivity(value);
    try {
      await commands.setWakeWordSensitivity(value);
      await refreshSettings();
    } catch {
      // Keep the optimistic local value; the slider will re-sync once
      // settings are refreshed elsewhere.
    }
  };

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="mb-2">
        <h1 className="text-xl font-semibold mb-2">
          {t("settings.handsFree.title")}
        </h1>
        <p className="text-sm text-text/60">
          {t("settings.handsFree.description")}
        </p>
      </div>

      <SettingsGroup>
        <ToggleSwitch
          checked={handsFreeEnabled}
          onChange={handleToggleEnabled}
          isUpdating={isTogglingEnabled}
          label={t("settings.handsFree.enable.label")}
          description={t("settings.handsFree.enable.description")}
          descriptionMode="inline"
          grouped
        />
      </SettingsGroup>

      <SettingsGroup>
        <SettingContainer
          title={t("settings.handsFree.wakeWord.label")}
          description={t("settings.handsFree.wakeWord.description")}
          descriptionMode="inline"
          layout="stacked"
          grouped
          disabled={!handsFreeEnabled}
        >
          <div className="flex items-center gap-2">
            <Input
              type="text"
              value={wakeWordInput}
              onChange={(e) => {
                setWakeWordInput(e.target.value);
                setWakeWordSaved(false);
                setWakeWordError(null);
              }}
              placeholder={t("settings.handsFree.wakeWord.placeholder")}
              variant="compact"
              disabled={isSavingWakeWord}
              className="flex-1 max-w-60"
            />
            <Button
              onClick={handleSaveWakeWord}
              variant="secondary"
              size="md"
              disabled={isSavingWakeWord || !wakeWordInput.trim()}
            >
              {t("settings.handsFree.wakeWord.save")}
            </Button>
          </div>
          {wakeWordSaved && !wakeWordError && (
            <span className="flex items-center gap-1 text-xs text-green-400 mt-1.5">
              <Check className="w-3.5 h-3.5" />
              {t("settings.handsFree.wakeWord.saved")}
            </span>
          )}
          {wakeWordError && (
            <span className="text-xs text-red-400 mt-1.5 block">
              {wakeWordError}
            </span>
          )}
        </SettingContainer>

        <Slider
          value={sensitivity}
          onChange={handleSensitivityChange}
          min={0.5}
          max={0.95}
          step={0.05}
          label={t("settings.handsFree.sensitivity.label")}
          description={t("settings.handsFree.sensitivity.description")}
          descriptionMode="inline"
          grouped
          disabled={!handsFreeEnabled}
          formatValue={(v) => v.toFixed(2)}
        />
      </SettingsGroup>

      <p className="text-xs text-text/50 px-1">
        {t("settings.handsFree.tapToToggleHint")}
      </p>
    </div>
  );
};
