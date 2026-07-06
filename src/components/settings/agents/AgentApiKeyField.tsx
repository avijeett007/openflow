import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands } from "@/bindings";
import { ApiKeyField } from "../PostProcessingSettingsApi/ApiKeyField";
import { Button } from "../../ui/Button";

interface AgentApiKeyFieldProps {
  agentId: string;
}

/**
 * API key editor for a single agent. Reuses `ApiKeyField` for the input
 * itself, but manages its own presence/loading state: unlike the
 * post-processing key (which lives in plain settings and round-trips through
 * `useSettings`), agent keys live in the OS keychain under scope `"agent"`
 * and the backend never returns the plaintext value - only whether one is
 * set (`has_api_key`). The field therefore always starts blank; typing and
 * blurring writes a new key, and "Clear" removes it so the agent falls back
 * to the cleanup-scope key for its provider (see DESIGN-flow-os.md §3).
 */
export const AgentApiKeyField: React.FC<AgentApiKeyFieldProps> = ({
  agentId,
}) => {
  const { t } = useTranslation();
  const [hasKey, setHasKey] = useState(false);
  const [isChecking, setIsChecking] = useState(true);
  const [isSaving, setIsSaving] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setIsChecking(true);
    commands
      .hasApiKey("agent", agentId)
      .then((result) => {
        if (cancelled) return;
        setHasKey(result.status === "ok" && result.data);
      })
      .finally(() => {
        if (!cancelled) setIsChecking(false);
      });
    return () => {
      cancelled = true;
    };
  }, [agentId]);

  const handleBlur = async (value: string) => {
    const trimmed = value.trim();
    if (!trimmed) return;

    setIsSaving(true);
    try {
      const result = await commands.setApiKey("agent", agentId, trimmed);
      if (result.status === "error") {
        toast.error(
          t("settings.agents.card.apiKey.error", { error: result.error }),
        );
        return;
      }
      setHasKey(true);
      toast.success(t("settings.agents.card.apiKey.saved"));
    } finally {
      setIsSaving(false);
    }
  };

  const handleClear = async () => {
    setIsSaving(true);
    try {
      const result = await commands.deleteApiKey("agent", agentId);
      if (result.status === "error") {
        toast.error(
          t("settings.agents.card.apiKey.error", { error: result.error }),
        );
        return;
      }
      setHasKey(false);
      toast.success(t("settings.agents.card.apiKey.cleared"));
    } finally {
      setIsSaving(false);
    }
  };

  return (
    <div className="flex items-center gap-2">
      <ApiKeyField
        value=""
        onBlur={handleBlur}
        disabled={isChecking || isSaving}
        placeholder={
          hasKey
            ? t("settings.agents.card.apiKey.placeholderConfigured")
            : t("settings.agents.card.apiKey.placeholderEmpty")
        }
      />
      {hasKey && (
        <Button
          type="button"
          variant="danger-ghost"
          size="sm"
          onClick={handleClear}
          disabled={isSaving}
          className="shrink-0"
        >
          {t("settings.agents.card.apiKey.clear")}
        </Button>
      )}
    </div>
  );
};
