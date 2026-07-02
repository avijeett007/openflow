import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { Mic } from "lucide-react";
import type { BackendTestResult, Result } from "@/bindings";
import { Button } from "../../ui/Button";

interface BackendTestPanelProps {
  onTest: () => Promise<Result<BackendTestResult, string>>;
  disabled?: boolean;
  /** Shows a "speak now" hint while the test runs (STT records ~2s of audio). */
  showSpeakHint?: boolean;
}

export const BackendTestPanel: React.FC<BackendTestPanelProps> = React.memo(
  ({ onTest, disabled = false, showSpeakHint = false }) => {
    const { t } = useTranslation();
    const [isTesting, setIsTesting] = useState(false);
    const [result, setResult] = useState<BackendTestResult | null>(null);
    const [error, setError] = useState<string | null>(null);

    const handleTest = async () => {
      setIsTesting(true);
      setResult(null);
      setError(null);
      try {
        const response = await onTest();
        if (response.status === "ok") {
          setResult(response.data);
        } else {
          setError(response.error);
        }
      } finally {
        setIsTesting(false);
      }
    };

    return (
      <div className="flex flex-col gap-2">
        <div className="flex items-center gap-3">
          <Button
            onClick={handleTest}
            variant="secondary"
            size="md"
            disabled={disabled || isTesting}
          >
            {isTesting
              ? t("settings.modelSetup.test.testing")
              : t("settings.modelSetup.test.button")}
          </Button>
          {isTesting && showSpeakHint && (
            <span className="flex items-center gap-1.5 text-sm text-mid-gray">
              <Mic className="w-4 h-4 animate-pulse text-logo-primary" />
              {t("settings.modelSetup.test.speakNow")}
            </span>
          )}
        </div>
        {result &&
          (result.ok ? (
            <p className="text-sm text-green-400">
              {t("settings.modelSetup.test.resultOk", {
                text: result.text,
                ms: result.latency_ms,
              })}
            </p>
          ) : (
            <p className="text-sm text-red-400">{result.message}</p>
          ))}
        {error && <p className="text-sm text-red-400">{error}</p>}
      </div>
    );
  },
);

BackendTestPanel.displayName = "BackendTestPanel";
