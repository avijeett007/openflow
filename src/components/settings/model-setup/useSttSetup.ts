import { useState } from "react";
import { commands, type SttApiStyle, type SttBackendMode } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";

const SELFHOSTED_PROVIDER_KEY = "selfhosted";

export const useSttSetup = () => {
  const { settings, refreshSettings } = useSettings();

  const [selfhostedModelOptions, setSelfhostedModelOptions] = useState<
    string[]
  >([]);
  const [remoteModelOptions, setRemoteModelOptions] = useState<
    Record<string, string[]>
  >({});
  const [isValidatingSelfhosted, setIsValidatingSelfhosted] = useState(false);
  const [selfhostedValidationError, setSelfhostedValidationError] = useState<
    string | null
  >(null);
  const [isFetchingRemoteModels, setIsFetchingRemoteModels] = useState(false);

  const mode: SttBackendMode = settings?.stt_backend_mode ?? "local";
  const providers = settings?.stt_providers ?? [];
  const providerId = settings?.stt_provider_id || providers[0]?.id || "groq";
  const provider = providers.find((candidate) => candidate.id === providerId);
  const models = settings?.stt_models ?? {};
  const remoteModel =
    (providerId && models[providerId]) || provider?.default_model || "";
  const selfhostedUrl = settings?.stt_selfhosted_url ?? "";
  const selfhostedModel = settings?.stt_selfhosted_model ?? "";
  const selfhostedApiStyle: SttApiStyle =
    settings?.stt_selfhosted_api_style ?? "openai_compatible";
  const selectedModelName = settings?.selected_model;

  const setMode = async (nextMode: SttBackendMode) => {
    const result = await commands.setSttBackendMode(nextMode);
    if (result.status === "ok") await refreshSettings();
    return result;
  };

  const setProvider = async (nextProviderId: string) => {
    const result = await commands.setSttProvider(nextProviderId);
    if (result.status === "ok") await refreshSettings();
    return result;
  };

  const setRemoteModel = async (nextProviderId: string, model: string) => {
    const result = await commands.changeSttModelSetting(nextProviderId, model);
    if (result.status === "ok") await refreshSettings();
    return result;
  };

  const setSelfhostedUrl = async (url: string) => {
    const result = await commands.changeSttSelfhostedUrlSetting(url);
    if (result.status === "ok") await refreshSettings();
    return result;
  };

  const setSelfhostedModel = async (model: string) => {
    const result = await commands.changeSttSelfhostedModelSetting(model);
    if (result.status === "ok") await refreshSettings();
    return result;
  };

  const setSelfhostedApiStyle = async (style: SttApiStyle) => {
    const result = await commands.setSttSelfhostedApiStyle(style);
    if (result.status === "ok") await refreshSettings();
    return result;
  };

  const validateSelfhosted = async () => {
    setIsValidatingSelfhosted(true);
    setSelfhostedValidationError(null);
    try {
      const result = await commands.listSttModels(SELFHOSTED_PROVIDER_KEY);
      if (result.status === "ok") {
        setSelfhostedModelOptions(result.data);
      } else {
        setSelfhostedValidationError(result.error);
      }
      return result;
    } finally {
      setIsValidatingSelfhosted(false);
    }
  };

  const fetchRemoteModels = async (nextProviderId: string) => {
    setIsFetchingRemoteModels(true);
    try {
      const result = await commands.listSttModels(nextProviderId);
      if (result.status === "ok") {
        setRemoteModelOptions((prev) => ({
          ...prev,
          [nextProviderId]: result.data,
        }));
      }
      return result;
    } finally {
      setIsFetchingRemoteModels(false);
    }
  };

  return {
    mode,
    providers,
    providerId,
    provider,
    remoteModel,
    remoteModelOptions: remoteModelOptions[providerId] ?? [],
    selfhostedUrl,
    selfhostedModel,
    selfhostedApiStyle,
    selfhostedModelOptions,
    selectedModelName,
    isValidatingSelfhosted,
    selfhostedValidationError,
    isFetchingRemoteModels,
    setMode,
    setProvider,
    setRemoteModel,
    setSelfhostedUrl,
    setSelfhostedModel,
    setSelfhostedApiStyle,
    validateSelfhosted,
    fetchRemoteModels,
  };
};
