import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { RefreshCcw } from "lucide-react";
import { commands } from "@/bindings";
import {
  Dropdown,
  SettingContainer,
  SettingsGroup,
  ToggleSwitch,
} from "@/components/ui";
import { ResetButton } from "../../ui/ResetButton";
import { ModeToggle } from "./ModeToggle";
import { ApiKeyRow } from "./ApiKeyRow";
import { BackendTestPanel } from "./BackendTestPanel";
import { BaseUrlField } from "../PostProcessingSettingsApi/BaseUrlField";
import { ModelSelect } from "../PostProcessingSettingsApi/ModelSelect";
import type { ModelOption } from "../PostProcessingSettingsApi/types";
import {
  useCleanupSetup,
  LOCAL_OLLAMA_URL,
  type CleanupMode,
} from "./useCleanupSetup";
import { useSettings } from "../../../hooks/useSettings";

export const CleanupSetupCard: React.FC = () => {
  const { t } = useTranslation();
  const cleanup = useCleanupSetup();
  const { refreshSettings } = useSettings();
  const [isTogglingEnabled, setIsTogglingEnabled] = useState(false);

  const modeOptions = [
    { value: "local", label: t("settings.modelSetup.mode.local") },
    { value: "self_hosted", label: t("settings.modelSetup.mode.selfHosted") },
    { value: "remote", label: t("settings.modelSetup.mode.remote") },
  ];

  const remoteProviderOptions = useMemo(
    () => cleanup.remoteProviders.map((p) => ({ value: p.id, label: p.label })),
    [cleanup.remoteProviders],
  );

  const modelOptions = useMemo<ModelOption[]>(() => {
    const seen = new Set<string>();
    const options: ModelOption[] = [];
    const upsert = (value: string | null | undefined) => {
      const trimmed = value?.trim();
      if (!trimmed || seen.has(trimmed)) return;
      seen.add(trimmed);
      options.push({ value: trimmed, label: trimmed });
    };
    cleanup.modelOptions.forEach(upsert);
    upsert(cleanup.model);
    return options;
  }, [cleanup.modelOptions, cleanup.model]);

  const handleToggleEnabled = async (nextEnabled: boolean) => {
    setIsTogglingEnabled(true);
    try {
      await cleanup.setEnabled(nextEnabled);
    } finally {
      setIsTogglingEnabled(false);
    }
  };

  const handleApiKeySave = async (key: string) => {
    const result = await commands.changePostProcessApiKeySetting(
      cleanup.providerId,
      key,
    );
    if (result.status === "ok") await refreshSettings();
    return result;
  };

  return (
    <SettingsGroup title={t("settings.modelSetup.cleanup.title")}>
      <ToggleSwitch
        checked={cleanup.enabled}
        onChange={(value) => void handleToggleEnabled(value)}
        isUpdating={isTogglingEnabled}
        label={t("settings.modelSetup.cleanup.enable.label")}
        description={t("settings.modelSetup.cleanup.enable.description")}
        descriptionMode="tooltip"
        grouped
      />

      <SettingContainer
        title={t("settings.modelSetup.mode.title")}
        description={t("settings.modelSetup.mode.description")}
        descriptionMode="tooltip"
        layout="horizontal"
        grouped
        disabled={!cleanup.enabled}
      >
        <ModeToggle
          value={cleanup.mode}
          options={modeOptions}
          onChange={(mode) => void cleanup.setMode(mode as CleanupMode)}
          disabled={!cleanup.enabled}
        />
      </SettingContainer>

      {cleanup.mode === "local" && (
        <SettingContainer
          title={t("settings.modelSetup.cleanup.local.title")}
          description={t("settings.modelSetup.cleanup.local.description", {
            url: LOCAL_OLLAMA_URL,
          })}
          descriptionMode="inline"
          layout="stacked"
          grouped
          disabled={!cleanup.enabled}
        >
          <></>
        </SettingContainer>
      )}

      {cleanup.mode === "self_hosted" && (
        <>
          <SettingContainer
            title={t("settings.modelSetup.cleanup.selfHosted.url.title")}
            description={t(
              "settings.modelSetup.cleanup.selfHosted.url.description",
            )}
            descriptionMode="tooltip"
            layout="horizontal"
            grouped
            disabled={!cleanup.enabled}
          >
            <BaseUrlField
              value={cleanup.baseUrl}
              onBlur={(value) => void cleanup.setBaseUrl(value)}
              placeholder={t(
                "settings.modelSetup.cleanup.selfHosted.url.placeholder",
              )}
              disabled={!cleanup.enabled}
              className="min-w-[320px]"
            />
          </SettingContainer>

          <SettingContainer
            title={t("settings.modelSetup.cleanup.apiKey.title")}
            description={t("settings.modelSetup.cleanup.apiKey.description")}
            descriptionMode="tooltip"
            layout="horizontal"
            grouped
            disabled={!cleanup.enabled}
          >
            <ApiKeyRow
              scope="cleanup"
              provider={cleanup.providerId}
              disabled={!cleanup.enabled}
              onSave={handleApiKeySave}
            />
          </SettingContainer>

          <SettingContainer
            title={t("settings.modelSetup.cleanup.model.title")}
            description={t(
              "settings.modelSetup.cleanup.model.descriptionCustom",
            )}
            descriptionMode="tooltip"
            layout="stacked"
            grouped
            disabled={!cleanup.enabled}
          >
            <div className="flex items-center gap-2">
              <ModelSelect
                value={cleanup.model}
                options={modelOptions}
                disabled={!cleanup.enabled}
                isLoading={cleanup.isFetchingModels}
                placeholder={
                  modelOptions.length > 0
                    ? t(
                        "settings.postProcessing.api.model.placeholderWithOptions",
                      )
                    : t(
                        "settings.postProcessing.api.model.placeholderNoOptions",
                      )
                }
                onSelect={(value) =>
                  void cleanup.setModel(cleanup.providerId, value)
                }
                onCreate={(value) =>
                  void cleanup.setModel(cleanup.providerId, value)
                }
                onBlur={() => {}}
                className="flex-1 min-w-[320px]"
              />
              <ResetButton
                onClick={() => void cleanup.fetchModels(cleanup.providerId)}
                disabled={!cleanup.enabled || cleanup.isFetchingModels}
                ariaLabel={t("settings.postProcessing.api.model.refreshModels")}
                className="flex h-10 w-10 items-center justify-center"
              >
                <RefreshCcw
                  className={`h-4 w-4 ${cleanup.isFetchingModels ? "animate-spin" : ""}`}
                />
              </ResetButton>
            </div>
          </SettingContainer>
        </>
      )}

      {cleanup.mode === "remote" && (
        <>
          <SettingContainer
            title={t("settings.modelSetup.cleanup.remote.provider.title")}
            description={t(
              "settings.modelSetup.cleanup.remote.provider.description",
            )}
            descriptionMode="tooltip"
            layout="horizontal"
            grouped
            disabled={!cleanup.enabled}
          >
            <Dropdown
              options={remoteProviderOptions}
              selectedValue={cleanup.providerId}
              onSelect={(value) => void cleanup.setProvider(value)}
              disabled={!cleanup.enabled}
              className="min-w-[220px]"
            />
          </SettingContainer>

          {cleanup.isAppleProvider ? (
            <SettingContainer
              title={t("settings.modelSetup.cleanup.appleIntelligence.title")}
              description={t(
                "settings.modelSetup.cleanup.appleIntelligence.description",
              )}
              descriptionMode="inline"
              layout="stacked"
              grouped
            >
              <></>
            </SettingContainer>
          ) : (
            <>
              <SettingContainer
                title={t("settings.modelSetup.cleanup.apiKey.title")}
                description={t(
                  "settings.modelSetup.cleanup.apiKey.description",
                )}
                descriptionMode="tooltip"
                layout="horizontal"
                grouped
                disabled={!cleanup.enabled}
              >
                <ApiKeyRow
                  scope="cleanup"
                  provider={cleanup.providerId}
                  disabled={!cleanup.enabled}
                  onSave={handleApiKeySave}
                />
              </SettingContainer>

              <SettingContainer
                title={t("settings.modelSetup.cleanup.model.title")}
                description={t(
                  "settings.modelSetup.cleanup.model.descriptionDefault",
                )}
                descriptionMode="tooltip"
                layout="stacked"
                grouped
                disabled={!cleanup.enabled}
              >
                <div className="flex items-center gap-2">
                  <ModelSelect
                    value={cleanup.model}
                    options={modelOptions}
                    disabled={!cleanup.enabled}
                    isLoading={cleanup.isFetchingModels}
                    placeholder={
                      modelOptions.length > 0
                        ? t(
                            "settings.postProcessing.api.model.placeholderWithOptions",
                          )
                        : t(
                            "settings.postProcessing.api.model.placeholderNoOptions",
                          )
                    }
                    onSelect={(value) =>
                      void cleanup.setModel(cleanup.providerId, value)
                    }
                    onCreate={(value) =>
                      void cleanup.setModel(cleanup.providerId, value)
                    }
                    onBlur={() => {}}
                    className="flex-1 min-w-[320px]"
                  />
                  <ResetButton
                    onClick={() => void cleanup.fetchModels(cleanup.providerId)}
                    disabled={!cleanup.enabled || cleanup.isFetchingModels}
                    ariaLabel={t(
                      "settings.postProcessing.api.model.refreshModels",
                    )}
                    className="flex h-10 w-10 items-center justify-center"
                  >
                    <RefreshCcw
                      className={`h-4 w-4 ${cleanup.isFetchingModels ? "animate-spin" : ""}`}
                    />
                  </ResetButton>
                </div>
              </SettingContainer>
            </>
          )}
        </>
      )}

      <SettingContainer
        title={t("settings.modelSetup.test.title")}
        description={t("settings.modelSetup.test.cleanupDescription")}
        descriptionMode="tooltip"
        layout="stacked"
        grouped
        disabled={!cleanup.enabled}
      >
        <BackendTestPanel
          onTest={() => commands.testCleanupBackend()}
          disabled={!cleanup.enabled}
        />
      </SettingContainer>
    </SettingsGroup>
  );
};
