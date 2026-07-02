import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { RefreshCcw } from "lucide-react";
import { commands, type SttApiStyle, type SttBackendMode } from "@/bindings";
import { Dropdown, SettingContainer, SettingsGroup } from "@/components/ui";
import { Input } from "../../ui/Input";
import { Button } from "../../ui/Button";
import { ResetButton } from "../../ui/ResetButton";
import { ModeToggle } from "./ModeToggle";
import { ApiKeyRow } from "./ApiKeyRow";
import { BackendTestPanel } from "./BackendTestPanel";
import { useSttSetup } from "./useSttSetup";

const uniqueOptions = (values: (string | undefined)[]) => {
  const seen = new Set<string>();
  const options: { value: string; label: string }[] = [];
  for (const value of values) {
    const trimmed = value?.trim();
    if (!trimmed || seen.has(trimmed)) continue;
    seen.add(trimmed);
    options.push({ value: trimmed, label: trimmed });
  }
  return options;
};

export const SttSetupCard: React.FC = () => {
  const { t } = useTranslation();
  const stt = useSttSetup();

  const [urlDraft, setUrlDraft] = useState(stt.selfhostedUrl);
  useEffect(() => setUrlDraft(stt.selfhostedUrl), [stt.selfhostedUrl]);

  useEffect(() => {
    if (stt.mode === "remote" && stt.providerId) {
      void stt.fetchRemoteModels(stt.providerId);
    }
    // Only refetch when the mode or provider actually changes.
  }, [stt.mode, stt.providerId]);

  const modeOptions = [
    { value: "local", label: t("settings.modelSetup.mode.local") },
    { value: "self_hosted", label: t("settings.modelSetup.mode.selfHosted") },
    { value: "remote", label: t("settings.modelSetup.mode.remote") },
  ];

  const apiStyleOptions = [
    {
      value: "openai_compatible",
      label: t(
        "settings.modelSetup.stt.selfHosted.apiStyle.options.openaiCompatible",
      ),
    },
    {
      value: "deepgram",
      label: t("settings.modelSetup.stt.selfHosted.apiStyle.options.deepgram"),
    },
  ];

  const providerOptions = useMemo(
    () => stt.providers.map((p) => ({ value: p.id, label: p.label })),
    [stt.providers],
  );

  const remoteModelOptions = useMemo(
    () =>
      uniqueOptions([
        ...stt.remoteModelOptions,
        stt.remoteModel,
        stt.provider?.default_model,
      ]),
    [stt.remoteModelOptions, stt.remoteModel, stt.provider],
  );

  const selfhostedModelOptions = useMemo(
    () => uniqueOptions([...stt.selfhostedModelOptions, stt.selfhostedModel]),
    [stt.selfhostedModelOptions, stt.selfhostedModel],
  );

  const handleUrlBlur = () => {
    const trimmed = urlDraft.trim();
    if (trimmed && trimmed !== stt.selfhostedUrl) {
      void stt.setSelfhostedUrl(trimmed);
    }
  };

  const handleProviderChange = (providerId: string) => {
    void stt.setProvider(providerId);
  };

  return (
    <SettingsGroup title={t("settings.modelSetup.stt.title")}>
      <SettingContainer
        title={t("settings.modelSetup.mode.title")}
        description={t("settings.modelSetup.mode.description")}
        descriptionMode="tooltip"
        layout="horizontal"
        grouped
      >
        <ModeToggle
          value={stt.mode}
          options={modeOptions}
          onChange={(mode) => void stt.setMode(mode as SttBackendMode)}
        />
      </SettingContainer>

      {stt.mode === "local" && (
        <SettingContainer
          title={t("settings.modelSetup.stt.local.title")}
          description={t("settings.modelSetup.stt.local.description")}
          descriptionMode="inline"
          layout="stacked"
          grouped
        >
          <p className="text-sm">
            {stt.selectedModelName
              ? t("settings.modelSetup.stt.local.currentModel", {
                  model: stt.selectedModelName,
                })
              : t("settings.modelSetup.stt.local.noModel")}
          </p>
        </SettingContainer>
      )}

      {stt.mode === "self_hosted" && (
        <>
          <SettingContainer
            title={t("settings.modelSetup.stt.selfHosted.url.title")}
            description={t(
              "settings.modelSetup.stt.selfHosted.url.description",
            )}
            descriptionMode="tooltip"
            layout="horizontal"
            grouped
          >
            <Input
              type="text"
              value={urlDraft}
              onChange={(event) => setUrlDraft(event.target.value)}
              onBlur={handleUrlBlur}
              placeholder={t(
                "settings.modelSetup.stt.selfHosted.url.placeholder",
              )}
              variant="compact"
              className="min-w-[320px]"
            />
          </SettingContainer>

          <SettingContainer
            title={t("settings.modelSetup.stt.selfHosted.apiStyle.title")}
            description={t(
              "settings.modelSetup.stt.selfHosted.apiStyle.description",
            )}
            descriptionMode="tooltip"
            layout="horizontal"
            grouped
          >
            <Dropdown
              options={apiStyleOptions}
              selectedValue={stt.selfhostedApiStyle}
              onSelect={(value) =>
                void stt.setSelfhostedApiStyle(value as SttApiStyle)
              }
              className="min-w-[220px]"
            />
          </SettingContainer>

          <SettingContainer
            title={t("settings.modelSetup.stt.selfHosted.apiKey.title")}
            description={t(
              "settings.modelSetup.stt.selfHosted.apiKey.description",
            )}
            descriptionMode="tooltip"
            layout="horizontal"
            grouped
          >
            <ApiKeyRow
              scope="stt"
              provider="selfhosted"
              placeholder={t(
                "settings.modelSetup.stt.selfHosted.apiKey.placeholder",
              )}
            />
          </SettingContainer>

          <SettingContainer
            title={t("settings.modelSetup.stt.selfHosted.model.title")}
            description={t(
              "settings.modelSetup.stt.selfHosted.model.description",
            )}
            descriptionMode="tooltip"
            layout="stacked"
            grouped
          >
            <div className="flex flex-col gap-2">
              <div>
                <Button
                  onClick={() => void stt.validateSelfhosted()}
                  variant="secondary"
                  size="md"
                  disabled={stt.isValidatingSelfhosted}
                >
                  {stt.isValidatingSelfhosted
                    ? t("settings.modelSetup.stt.selfHosted.validating")
                    : t("settings.modelSetup.stt.selfHosted.validate")}
                </Button>
                {stt.selfhostedValidationError && (
                  <p className="text-sm text-red-400 mt-1">
                    {stt.selfhostedValidationError}
                  </p>
                )}
              </div>
              <Dropdown
                options={selfhostedModelOptions}
                selectedValue={stt.selfhostedModel || null}
                onSelect={(value) => void stt.setSelfhostedModel(value)}
                placeholder={t(
                  "settings.modelSetup.stt.selfHosted.model.placeholder",
                )}
                className="min-w-[320px]"
              />
            </div>
          </SettingContainer>
        </>
      )}

      {stt.mode === "remote" && (
        <>
          <SettingContainer
            title={t("settings.modelSetup.stt.remote.provider.title")}
            description={t(
              "settings.modelSetup.stt.remote.provider.description",
            )}
            descriptionMode="tooltip"
            layout="horizontal"
            grouped
          >
            <Dropdown
              options={providerOptions}
              selectedValue={stt.providerId}
              onSelect={handleProviderChange}
              className="min-w-[220px]"
            />
          </SettingContainer>

          <SettingContainer
            title={t("settings.modelSetup.stt.remote.apiKey.title")}
            description={t("settings.modelSetup.stt.remote.apiKey.description")}
            descriptionMode="tooltip"
            layout="horizontal"
            grouped
          >
            <ApiKeyRow
              scope="stt"
              provider={stt.providerId}
              placeholder={t(
                "settings.modelSetup.stt.remote.apiKey.placeholder",
              )}
            />
          </SettingContainer>

          <SettingContainer
            title={t("settings.modelSetup.stt.remote.model.title")}
            description={t("settings.modelSetup.stt.remote.model.description")}
            descriptionMode="tooltip"
            layout="stacked"
            grouped
          >
            <div className="flex items-center gap-2">
              <Dropdown
                options={remoteModelOptions}
                selectedValue={stt.remoteModel || null}
                onSelect={(value) =>
                  void stt.setRemoteModel(stt.providerId, value)
                }
                placeholder={t(
                  "settings.modelSetup.stt.remote.model.placeholder",
                )}
                className="min-w-[320px]"
              />
              <ResetButton
                onClick={() => void stt.fetchRemoteModels(stt.providerId)}
                disabled={stt.isFetchingRemoteModels}
                ariaLabel={t("settings.postProcessing.api.model.refreshModels")}
                className="flex h-10 w-10 items-center justify-center"
              >
                <RefreshCcw
                  className={`h-4 w-4 ${stt.isFetchingRemoteModels ? "animate-spin" : ""}`}
                />
              </ResetButton>
            </div>
          </SettingContainer>
        </>
      )}

      <SettingContainer
        title={t("settings.modelSetup.test.title")}
        description={t("settings.modelSetup.test.sttDescription")}
        descriptionMode="tooltip"
        layout="stacked"
        grouped
      >
        <BackendTestPanel
          onTest={() => commands.testSttBackend()}
          showSpeakHint
        />
      </SettingContainer>
    </SettingsGroup>
  );
};
