import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { AlertTriangle, Check } from "lucide-react";
import { commands, type HandsFreeReadiness } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { useNavigationStore } from "../../../stores/navigationStore";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { SettingContainer } from "../../ui/SettingContainer";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { Slider } from "../../ui/Slider";
import { Input } from "../../ui/Input";
import { Button } from "../../ui/Button";
import { Alert } from "../../ui/Alert";

export const HandsFreeSettings: React.FC = () => {
  const { t } = useTranslation();
  const { settings, refreshSettings } = useSettings();
  const setCurrentSection = useNavigationStore(
    (state) => state.setCurrentSection,
  );

  const handsFreeEnabled = settings?.hands_free_enabled ?? false;

  const [isTogglingEnabled, setIsTogglingEnabled] = useState(false);

  // Whether the selected local STT model is present on disk. Wake-word detection
  // always needs one (even for remote-STT users), so we warn at the toggle when
  // it's missing rather than failing silently in the background loop.
  const [readiness, setReadiness] = useState<HandsFreeReadiness | null>(null);
  // Latest runtime failure surfaced by the background loop's `hands-free-error`
  // event (payload: "model" | "transcription" | "microphone").
  const [runtimeError, setRuntimeError] = useState<string | null>(null);

  const refreshReadiness = async () => {
    const result = await commands.getHandsFreeReadiness();
    if (result.status === "ok") {
      setReadiness(result.data);
    }
  };

  // Check readiness on mount, and whenever the persisted enabled/model changes.
  useEffect(() => {
    void refreshReadiness();
  }, [settings?.hands_free_enabled, settings?.selected_model]);

  // Surface runtime failures from the background listener as an error banner.
  useEffect(() => {
    const unlisten = listen<string>("hands-free-error", (event) => {
      setRuntimeError(event.payload);
      // A runtime failure may reflect a newly-missing model — re-check so the
      // model-missing banner stays accurate.
      void refreshReadiness();
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  const modelMissing =
    handsFreeEnabled && readiness !== null && !readiness.local_model_available;

  const runtimeErrorMessage = (payload: string): string => {
    switch (payload) {
      case "model":
        return t("settings.handsFree.runtimeError.model");
      case "microphone":
        return t("settings.handsFree.runtimeError.microphone");
      default:
        return t("settings.handsFree.runtimeError.transcription");
    }
  };

  const [wakeWordInput, setWakeWordInput] = useState("");
  const [isSavingWakeWord, setIsSavingWakeWord] = useState(false);
  const [wakeWordSaved, setWakeWordSaved] = useState(false);
  const [wakeWordError, setWakeWordError] = useState<string | null>(null);

  const [sensitivity, setSensitivity] = useState(0.8);
  const [listenSeconds, setListenSeconds] = useState(10);
  const [silenceSeconds, setSilenceSeconds] = useState(2.5);

  // Keep local editable state in sync with the persisted settings whenever
  // they change (initial load, or an update from elsewhere).
  useEffect(() => {
    if (settings) {
      setWakeWordInput(settings.wake_word ?? "");
      setSensitivity(settings.wake_word_sensitivity ?? 0.8);
      setListenSeconds(settings.wake_word_listen_seconds ?? 10);
      setSilenceSeconds((settings.wake_word_silence_timeout_ms ?? 2500) / 1000);
    }
  }, [
    settings?.wake_word,
    settings?.wake_word_sensitivity,
    settings?.wake_word_listen_seconds,
    settings?.wake_word_silence_timeout_ms,
  ]);

  const handleToggleEnabled = async (enabled: boolean) => {
    setIsTogglingEnabled(true);
    // Disabling stops the listener, so any stale runtime error no longer applies.
    if (!enabled) {
      setRuntimeError(null);
    }
    try {
      await commands.setHandsFreeEnabled(enabled);
      await refreshSettings();
      await refreshReadiness();
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

  const handleListenSecondsChange = async (value: number) => {
    setListenSeconds(value);
    try {
      await commands.setWakeWordListenSeconds(value);
      await refreshSettings();
    } catch {
      // Keep the optimistic value; re-syncs on the next settings refresh.
    }
  };

  const handleSilenceSecondsChange = async (value: number) => {
    setSilenceSeconds(value);
    try {
      await commands.setWakeWordSilenceTimeoutSeconds(value);
      await refreshSettings();
    } catch {
      // Keep the optimistic value; re-syncs on the next settings refresh.
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

      {modelMissing && (
        <div className="flex items-start gap-3 p-4 rounded-lg bg-yellow-500/10 border border-yellow-500/30">
          <AlertTriangle className="w-5 h-5 shrink-0 mt-0.5 text-yellow-500" />
          <div className="flex-1 space-y-2">
            <p className="text-sm font-semibold text-yellow-400">
              {t("settings.handsFree.modelMissing.title")}
            </p>
            <p className="text-sm text-yellow-400/90">
              {t("settings.handsFree.modelMissing.body")}
            </p>
            <Button
              onClick={() => setCurrentSection("models")}
              variant="secondary"
              size="sm"
            >
              {t("settings.handsFree.modelMissing.cta")}
            </Button>
          </div>
        </div>
      )}

      {runtimeError && !(modelMissing && runtimeError === "model") && (
        <Alert variant="error">{runtimeErrorMessage(runtimeError)}</Alert>
      )}

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

        <Slider
          value={listenSeconds}
          onChange={handleListenSecondsChange}
          min={3}
          max={60}
          step={1}
          label={t("settings.handsFree.listenWindow.label")}
          description={t("settings.handsFree.listenWindow.description")}
          descriptionMode="inline"
          grouped
          disabled={!handsFreeEnabled}
          formatValue={(v) =>
            t("settings.handsFree.listenWindow.unit", { count: v })
          }
        />

        <Slider
          value={silenceSeconds}
          onChange={handleSilenceSecondsChange}
          min={1}
          max={10}
          step={0.5}
          label={t("settings.handsFree.silenceTimeout.label")}
          description={t("settings.handsFree.silenceTimeout.description")}
          descriptionMode="inline"
          grouped
          disabled={!handsFreeEnabled}
          formatValue={(v) =>
            t("settings.handsFree.silenceTimeout.unit", { count: v })
          }
        />
      </SettingsGroup>

      <p className="text-xs text-text/50 px-1">
        {t("settings.handsFree.tapToToggleHint")}
      </p>
    </div>
  );
};
