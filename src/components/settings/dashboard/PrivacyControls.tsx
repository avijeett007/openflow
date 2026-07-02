import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { ask } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import { commands, type AnalyticsPrivacy } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { Button } from "../../ui/Button";
import { ModeToggle } from "../model-setup/ModeToggle";

interface PrivacyControlsProps {
  onCleared: () => void;
}

const PRIVACY_MODES: AnalyticsPrivacy[] = ["full", "keywords_only", "off"];

export const PrivacyControls: React.FC<PrivacyControlsProps> = ({
  onCleared,
}) => {
  const { t } = useTranslation();
  const { settings, refreshSettings } = useSettings();
  const [isUpdatingPrivacy, setIsUpdatingPrivacy] = useState(false);
  const [isClearing, setIsClearing] = useState(false);

  const currentMode: AnalyticsPrivacy = settings?.analytics_privacy ?? "full";

  const privacyOptions = PRIVACY_MODES.map((mode) => ({
    value: mode,
    label: t(`settings.dashboard.privacy.modes.${mode}.label`),
  }));

  const handlePrivacyChange = async (mode: string) => {
    if (mode === currentMode) return;
    setIsUpdatingPrivacy(true);
    try {
      const result = await commands.setAnalyticsPrivacy(
        mode as AnalyticsPrivacy,
      );
      if (result.status === "ok") {
        await refreshSettings();
      } else {
        toast.error(t("settings.dashboard.privacy.updateError"));
      }
    } catch (error) {
      console.error("Failed to update analytics privacy:", error);
      toast.error(t("settings.dashboard.privacy.updateError"));
    } finally {
      setIsUpdatingPrivacy(false);
    }
  };

  const handleClearAnalytics = async () => {
    const confirmed = await ask(t("settings.dashboard.privacy.clearConfirm"), {
      title: t("settings.dashboard.privacy.clearTitle"),
      kind: "warning",
    });
    if (!confirmed) return;

    setIsClearing(true);
    try {
      const result = await commands.clearAnalytics();
      if (result.status === "ok") {
        toast.success(t("settings.dashboard.privacy.clearSuccess"));
        onCleared();
      } else {
        toast.error(t("settings.dashboard.privacy.clearError"));
      }
    } catch (error) {
      console.error("Failed to clear analytics data:", error);
      toast.error(t("settings.dashboard.privacy.clearError"));
    } finally {
      setIsClearing(false);
    }
  };

  return (
    <div className="space-y-4 px-4 py-4">
      <div className="space-y-2">
        <h3 className="text-sm font-medium">
          {t("settings.dashboard.privacy.title")}
        </h3>
        <p className="text-sm text-text/60">
          {t("settings.dashboard.privacy.description")}
        </p>
      </div>

      <ModeToggle
        value={currentMode}
        options={privacyOptions}
        onChange={handlePrivacyChange}
        disabled={isUpdatingPrivacy}
      />

      <p className="text-xs text-text/50 max-w-xl">
        {t(`settings.dashboard.privacy.modes.${currentMode}.description`)}
      </p>

      <div className="pt-2 border-t border-mid-gray/20">
        <div className="flex items-center justify-between gap-3 pt-3 flex-wrap">
          <div>
            <p className="text-sm font-medium">
              {t("settings.dashboard.privacy.clearTitle")}
            </p>
            <p className="text-xs text-text/50">
              {t("settings.dashboard.privacy.clearDescription")}
            </p>
          </div>
          <Button
            variant="danger"
            size="sm"
            onClick={handleClearAnalytics}
            disabled={isClearing}
          >
            {isClearing
              ? t("settings.dashboard.privacy.clearing")
              : t("settings.dashboard.privacy.clearButton")}
          </Button>
        </div>
      </div>
    </div>
  );
};
