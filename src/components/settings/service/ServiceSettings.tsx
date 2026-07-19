import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Cloud, Link2, Loader2, ShieldCheck, Unlink } from "lucide-react";
import type { ServiceStatus, ServiceInfo } from "@/bindings";
import { commands } from "@/bindings";
import { Button } from "../../ui/Button";
import { Input } from "../../ui/Input";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { ToggleSwitch } from "../../ui/ToggleSwitch";

/**
 * OpenFlow Service — "Connect to my service".
 *
 * Additive, dormant-by-default integration: pair this device with a self-hosted
 * OpenFlow Service, then opt in (per data type) to sync dictation transcripts
 * and/or usage events. Nothing leaves the machine until the user pairs AND turns
 * a sync on. The device token lives in the OS keyring (backend); this UI only
 * ever sees the URL, device name and opt-in flags.
 */
export const ServiceSettings: React.FC = () => {
  const { t } = useTranslation();

  const [status, setStatus] = useState<ServiceStatus | null>(null);
  const [loading, setLoading] = useState(true);

  // Unpaired-form state.
  const [url, setUrl] = useState("");
  const [setupToken, setSetupToken] = useState("");
  const [pairing, setPairing] = useState(false);
  const [testing, setTesting] = useState(false);
  const [unpairing, setUnpairing] = useState(false);
  const [info, setInfo] = useState<ServiceInfo | null>(null);

  const refresh = useCallback(async () => {
    const result = await commands.serviceStatus();
    if (result.status === "ok") {
      setStatus(result.data);
      // Pre-fill the URL field from any previously-configured value.
      setUrl((prev) => (prev.length === 0 ? result.data.url : prev));
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    void refresh().finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => {
      cancelled = true;
    };
  }, [refresh]);

  const handlePair = async () => {
    setPairing(true);
    setInfo(null);
    try {
      const result = await commands.pairService(url.trim(), setupToken.trim());
      if (result.status === "error") {
        toast.error(result.error);
        return;
      }
      toast.success(
        t("settings.service.pairedToast", { name: result.data.device_name }),
      );
      setSetupToken("");
      await refresh();
    } finally {
      setPairing(false);
    }
  };

  const handleTest = async () => {
    setTesting(true);
    setInfo(null);
    try {
      const result = await commands.testServiceConnection();
      if (result.status === "error") {
        toast.error(result.error);
        return;
      }
      setInfo(result.data);
      toast.success(
        t("settings.service.testOk", { version: result.data.version }),
      );
    } finally {
      setTesting(false);
    }
  };

  const handleUnpair = async () => {
    setUnpairing(true);
    try {
      const result = await commands.unpairService();
      if (result.status === "error") {
        toast.error(result.error);
        return;
      }
      setInfo(null);
      toast.success(t("settings.service.unpairedToast"));
      await refresh();
    } finally {
      setUnpairing(false);
    }
  };

  const handleToggleTranscripts = async (checked: boolean) => {
    const result = await commands.setServiceSyncTranscripts(checked);
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    await refresh();
  };

  const handleToggleUsage = async (checked: boolean) => {
    const result = await commands.setServiceSyncUsage(checked);
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    await refresh();
  };

  const paired = status?.enabled ?? false;

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup
        title={t("settings.service.title")}
        description={t("settings.service.intro")}
      >
        {/* Privacy note — always visible. */}
        <div className="px-4 py-3 flex items-start gap-2 text-xs text-mid-gray border-b border-mid-gray/15">
          <ShieldCheck className="h-4 w-4 shrink-0 mt-0.5 text-logo-primary" />
          <span>{t("settings.service.privacyNote")}</span>
        </div>

        {loading ? (
          <div className="px-4 py-6 flex items-center gap-2 text-sm text-mid-gray">
            <Loader2 className="h-4 w-4 animate-spin" />
            {t("settings.service.loading")}
          </div>
        ) : paired ? (
          // ---- Paired state ----
          <div className="px-4 py-4 space-y-4">
            <div className="flex items-center justify-between gap-3">
              <div className="min-w-0">
                <div className="flex items-center gap-2 text-sm font-medium">
                  <Cloud className="h-4 w-4 text-logo-primary" />
                  {status?.paired_device_name ??
                    t("settings.service.thisDevice")}
                </div>
                <p className="text-xs text-mid-gray truncate mt-0.5">
                  {status?.url}
                </p>
              </div>
              <Button
                type="button"
                variant="danger-ghost"
                size="sm"
                onClick={() => void handleUnpair()}
                disabled={unpairing}
                className="inline-flex items-center gap-1.5"
              >
                {unpairing ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : (
                  <Unlink className="h-4 w-4" />
                )}
                {t("settings.service.unpair")}
              </Button>
            </div>

            <div className="flex items-center gap-3">
              <Button
                type="button"
                variant="secondary"
                size="sm"
                onClick={() => void handleTest()}
                disabled={testing}
                className="inline-flex items-center gap-1.5"
              >
                {testing && <Loader2 className="h-4 w-4 animate-spin" />}
                {t("settings.service.testConnection")}
              </Button>
              {info && (
                <span className="text-xs text-mid-gray">
                  {t("settings.service.infoLine", {
                    version: info.version,
                    edition: info.edition,
                  })}
                </span>
              )}
            </div>
          </div>
        ) : (
          // ---- Unpaired state ----
          <div className="px-4 py-4 space-y-3">
            <label className="block space-y-1">
              <span className="text-xs font-medium text-mid-gray">
                {t("settings.service.urlLabel")}
              </span>
              <Input
                type="text"
                inputMode="url"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                placeholder={t("settings.service.urlPlaceholder")}
                variant="compact"
                className="w-full"
              />
            </label>
            <label className="block space-y-1">
              <span className="text-xs font-medium text-mid-gray">
                {t("settings.service.setupTokenLabel")}
              </span>
              <Input
                type="password"
                value={setupToken}
                onChange={(e) => setSetupToken(e.target.value)}
                placeholder={t("settings.service.setupTokenPlaceholder")}
                variant="compact"
                className="w-full"
              />
            </label>
            <div className="flex items-center gap-2 pt-1">
              <Button
                type="button"
                variant="primary"
                size="sm"
                onClick={() => void handlePair()}
                disabled={
                  pairing ||
                  url.trim().length === 0 ||
                  setupToken.trim().length === 0
                }
                className="inline-flex items-center gap-1.5"
              >
                {pairing ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : (
                  <Link2 className="h-4 w-4" />
                )}
                {t("settings.service.pair")}
              </Button>
            </div>
          </div>
        )}
      </SettingsGroup>

      {/* Sync options — only meaningful once paired. */}
      {paired && (
        <SettingsGroup title={t("settings.service.syncTitle")}>
          <ToggleSwitch
            checked={status?.sync_transcripts ?? false}
            onChange={(checked) => void handleToggleTranscripts(checked)}
            label={t("settings.service.syncTranscripts")}
            description={t("settings.service.syncTranscriptsHint")}
            descriptionMode="inline"
            grouped
          />
          <ToggleSwitch
            checked={status?.sync_usage ?? false}
            onChange={(checked) => void handleToggleUsage(checked)}
            label={t("settings.service.syncUsage")}
            description={t("settings.service.syncUsageHint")}
            descriptionMode="inline"
            grouped
          />
          <div className="px-4 py-3 border-t border-mid-gray/15 flex items-center justify-between gap-3 text-xs text-mid-gray">
            <span>
              {status?.last_sync_at
                ? t("settings.service.lastSync", {
                    when: new Date(status.last_sync_at * 1000).toLocaleString(),
                  })
                : t("settings.service.neverSynced")}
            </span>
            <span className="tabular-nums">
              {t("settings.service.pending", {
                count: status?.pending_count ?? 0,
              })}
            </span>
          </div>
        </SettingsGroup>
      )}
    </div>
  );
};
