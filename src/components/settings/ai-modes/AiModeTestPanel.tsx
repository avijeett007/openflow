import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { FlaskConical } from "lucide-react";
import { commands } from "@/bindings";
import { Button } from "../../ui/Button";
import { Input } from "../../ui/Input";
import { Alert } from "../../ui/Alert";

interface AiModeTestPanelProps {
  modeId: string;
}

/**
 * "Test" affordance for an AI Mode card: runs the mode's transform over a sample
 * transcript via `commands.testAiMode` and shows the output + timing, without
 * touching the hotkey/injection pipeline. Mirrors the agent card's test panel.
 */
export const AiModeTestPanel: React.FC<AiModeTestPanelProps> = ({ modeId }) => {
  const { t } = useTranslation();
  const [sampleText, setSampleText] = useState("");
  const [isRunning, setIsRunning] = useState(false);
  const [result, setResult] = useState<{
    output: string;
    latencyMs: number;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleRun = async () => {
    const trimmed = sampleText.trim();
    if (!trimmed || isRunning) return;

    setIsRunning(true);
    setError(null);
    setResult(null);
    try {
      const response = await commands.testAiMode(modeId, trimmed);
      if (response.status === "error") {
        setError(response.error);
        return;
      }
      setResult({
        output: response.data.output,
        latencyMs: response.data.latency_ms,
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setIsRunning(false);
    }
  };

  return (
    <div className="space-y-2">
      <div className="flex gap-2">
        <Input
          type="text"
          variant="compact"
          value={sampleText}
          onChange={(event) => setSampleText(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              handleRun();
            }
          }}
          placeholder={t("settings.aiModes.card.test.sampleText.placeholder")}
          className="flex-1"
          aria-label={t("settings.aiModes.card.test.label")}
        />
        <Button
          type="button"
          variant="secondary"
          size="md"
          onClick={handleRun}
          disabled={isRunning || !sampleText.trim()}
          className="inline-flex shrink-0 items-center gap-1.5"
        >
          <FlaskConical className="h-4 w-4" />
          {isRunning
            ? t("settings.aiModes.card.test.running")
            : t("settings.aiModes.card.test.run")}
        </Button>
      </div>

      {error && (
        <Alert variant="error" contained>
          {t("settings.aiModes.card.test.error", { error })}
        </Alert>
      )}

      {result && (
        <div className="space-y-1 rounded-md border border-mid-gray/20 bg-mid-gray/5 p-3">
          <p className="whitespace-pre-wrap text-sm">{result.output}</p>
          <p className="text-xs text-mid-gray">
            {t("settings.aiModes.card.test.latency", {
              ms: result.latencyMs,
            })}
          </p>
        </div>
      )}
    </div>
  );
};
