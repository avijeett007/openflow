import { useState } from "react";
import { commands } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";

export type CleanupMode = "local" | "self_hosted" | "remote";

const CUSTOM_PROVIDER_ID = "custom";
const APPLE_PROVIDER_ID = "apple_intelligence";
export const LOCAL_OLLAMA_URL = "http://localhost:11434/v1";

export const useCleanupSetup = () => {
  const { settings, refreshSettings } = useSettings();

  const [modelOptions, setModelOptions] = useState<Record<string, string[]>>(
    {},
  );
  const [isFetchingModels, setIsFetchingModels] = useState(false);

  const enabled = settings?.post_process_enabled ?? false;
  const providers = settings?.post_process_providers ?? [];
  const providerId = settings?.post_process_provider_id || "openai";
  const provider = providers.find((candidate) => candidate.id === providerId);
  const remoteProviders = providers.filter(
    (candidate) => candidate.id !== CUSTOM_PROVIDER_ID,
  );
  const isAppleProvider = providerId === APPLE_PROVIDER_ID;
  const baseUrl = provider?.base_url ?? "";
  const models = settings?.post_process_models ?? {};
  const model = models[providerId] ?? "";

  const mode: CleanupMode =
    providerId === CUSTOM_PROVIDER_ID
      ? baseUrl.trim() === LOCAL_OLLAMA_URL
        ? "local"
        : "self_hosted"
      : "remote";

  const setEnabled = async (nextEnabled: boolean) => {
    const result = await commands.changePostProcessEnabledSetting(nextEnabled);
    if (result.status === "ok") await refreshSettings();
    return result;
  };

  const setProvider = async (nextProviderId: string) => {
    const result = await commands.setPostProcessProvider(nextProviderId);
    if (result.status === "ok") await refreshSettings();
    return result;
  };

  const setMode = async (nextMode: CleanupMode) => {
    if (!enabled) {
      const enableResult = await commands.changePostProcessEnabledSetting(true);
      if (enableResult.status === "error") return enableResult;
    }

    if (nextMode === "remote") {
      const targetId =
        providerId !== CUSTOM_PROVIDER_ID
          ? providerId
          : remoteProviders[0]?.id || "openai";
      const result = await commands.setPostProcessProvider(targetId);
      if (result.status === "ok") await refreshSettings();
      return result;
    }

    // local / self_hosted both use the "custom" provider
    if (providerId !== CUSTOM_PROVIDER_ID) {
      const providerResult =
        await commands.setPostProcessProvider(CUSTOM_PROVIDER_ID);
      if (providerResult.status === "error") return providerResult;
    }

    if (nextMode === "local") {
      const currentCustomUrl =
        providers.find((candidate) => candidate.id === CUSTOM_PROVIDER_ID)
          ?.base_url ?? "";
      if (currentCustomUrl.trim() !== LOCAL_OLLAMA_URL) {
        const urlResult = await commands.changePostProcessBaseUrlSetting(
          CUSTOM_PROVIDER_ID,
          LOCAL_OLLAMA_URL,
        );
        if (urlResult.status === "error") return urlResult;
      }
    }

    await refreshSettings();
    return { status: "ok", data: null } as const;
  };

  const setBaseUrl = async (url: string) => {
    const result = await commands.changePostProcessBaseUrlSetting(
      CUSTOM_PROVIDER_ID,
      url,
    );
    if (result.status === "ok") await refreshSettings();
    return result;
  };

  const setModel = async (nextProviderId: string, nextModel: string) => {
    const result = await commands.changePostProcessModelSetting(
      nextProviderId,
      nextModel,
    );
    if (result.status === "ok") await refreshSettings();
    return result;
  };

  const fetchModels = async (nextProviderId: string) => {
    setIsFetchingModels(true);
    try {
      const result = await commands.fetchPostProcessModels(nextProviderId);
      if (result.status === "ok") {
        setModelOptions((prev) => ({ ...prev, [nextProviderId]: result.data }));
      }
      return result;
    } finally {
      setIsFetchingModels(false);
    }
  };

  return {
    enabled,
    mode,
    providers,
    remoteProviders,
    providerId,
    provider,
    isAppleProvider,
    baseUrl,
    model,
    modelOptions: modelOptions[providerId] ?? [],
    isFetchingModels,
    setEnabled,
    setMode,
    setProvider,
    setBaseUrl,
    setModel,
    fetchModels,
  };
};
