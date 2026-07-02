import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Trash2 } from "lucide-react";
import { commands, type Result } from "@/bindings";
import { Input } from "../../ui/Input";
import { Button } from "../../ui/Button";

interface ApiKeyRowProps {
  scope: "stt" | "cleanup";
  provider: string;
  disabled?: boolean;
  placeholder?: string;
  /** Override how the key is persisted (e.g. cleanup routes through changePostProcessApiKeySetting). */
  onSave?: (key: string) => Promise<Result<null, string>>;
}

export const ApiKeyRow: React.FC<ApiKeyRowProps> = React.memo(
  ({ scope, provider, disabled = false, placeholder, onSave }) => {
    const { t } = useTranslation();
    const [value, setValue] = useState("");
    const [hasKey, setHasKey] = useState(false);
    const [isSaving, setIsSaving] = useState(false);
    const [isDeleting, setIsDeleting] = useState(false);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
      let cancelled = false;
      setValue("");
      setError(null);
      commands.hasApiKey(scope, provider).then((result) => {
        if (!cancelled && result.status === "ok") {
          setHasKey(result.data);
        }
      });
      return () => {
        cancelled = true;
      };
    }, [scope, provider]);

    const handleSave = async () => {
      const trimmed = value.trim();
      if (!trimmed) return;
      setIsSaving(true);
      setError(null);
      try {
        const result = onSave
          ? await onSave(trimmed)
          : await commands.setApiKey(scope, provider, trimmed);
        if (result.status === "ok") {
          setHasKey(true);
          setValue("");
        } else {
          setError(result.error);
        }
      } finally {
        setIsSaving(false);
      }
    };

    const handleDelete = async () => {
      setIsDeleting(true);
      setError(null);
      try {
        const result = await commands.deleteApiKey(scope, provider);
        if (result.status === "ok") {
          setHasKey(false);
          setValue("");
        } else {
          setError(result.error);
        }
      } finally {
        setIsDeleting(false);
      }
    };

    const busy = disabled || isSaving || isDeleting;

    return (
      <div className="flex flex-col gap-1.5">
        <div className="flex items-center gap-2">
          <Input
            type="password"
            value={value}
            onChange={(event) => setValue(event.target.value)}
            placeholder={
              placeholder ?? t("settings.modelSetup.apiKey.placeholder")
            }
            variant="compact"
            disabled={busy}
            autoComplete="off"
            className="flex-1 min-w-[260px]"
          />
          <Button
            onClick={handleSave}
            variant="secondary"
            size="md"
            disabled={busy || !value.trim()}
          >
            {t("settings.modelSetup.apiKey.save")}
          </Button>
          {hasKey && (
            <button
              type="button"
              onClick={handleDelete}
              disabled={busy}
              aria-label={t("settings.modelSetup.apiKey.delete")}
              className="p-1.5 rounded-md text-red-400 hover:bg-red-500/10 hover:text-red-300 transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
            >
              <Trash2 className="w-4 h-4" />
            </button>
          )}
        </div>
        {hasKey && !error && (
          <span className="flex items-center gap-1 text-xs text-green-400">
            <Check className="w-3.5 h-3.5" />
            {t("settings.modelSetup.apiKey.saved")}
          </span>
        )}
        {error && <span className="text-xs text-red-400">{error}</span>}
      </div>
    );
  },
);

ApiKeyRow.displayName = "ApiKeyRow";
