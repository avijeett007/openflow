import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { Plus, X, MonitorSmartphone } from "lucide-react";
import type { RunningApp } from "@/bindings";
import { commands } from "@/bindings";
import { Input } from "../../ui/Input";
import { Button } from "../../ui/Button";

interface AppRulesEditorProps {
  rules: string[];
  onChange: (rules: string[]) => void;
  disabled?: boolean;
}

/**
 * "Activate when using [apps]" editor for an AI Mode card: a chip list of
 * bundle-id / app-name substrings, a free-text add field, and a picker populated
 * from the currently-running apps (best-effort). Matching is case-insensitive
 * substring both ways backend-side, so either a bundle id or a plain name works.
 */
export const AppRulesEditor: React.FC<AppRulesEditorProps> = ({
  rules,
  onChange,
  disabled = false,
}) => {
  const { t } = useTranslation();
  const [draft, setDraft] = useState("");
  const [running, setRunning] = useState<RunningApp[] | null>(null);
  const [loadingApps, setLoadingApps] = useState(false);

  const addRule = (value: string) => {
    const trimmed = value.trim();
    if (!trimmed) return;
    // Case-insensitive dedupe.
    if (rules.some((r) => r.toLowerCase() === trimmed.toLowerCase())) return;
    onChange([...rules, trimmed]);
  };

  const removeRule = (value: string) => {
    onChange(rules.filter((r) => r !== value));
  };

  const handleAddDraft = () => {
    addRule(draft);
    setDraft("");
  };

  const loadRunningApps = async () => {
    if (running || loadingApps) return;
    setLoadingApps(true);
    try {
      const apps = await commands.getRunningApps();
      setRunning(apps);
    } catch {
      setRunning([]);
    } finally {
      setLoadingApps(false);
    }
  };

  return (
    <div className="space-y-2">
      {rules.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {rules.map((rule) => (
            <span
              key={rule}
              className="inline-flex items-center gap-1 rounded-full bg-mid-gray/10 px-2.5 py-1 text-xs"
            >
              <span className="max-w-[220px] truncate">{rule}</span>
              <button
                type="button"
                onClick={() => removeRule(rule)}
                disabled={disabled}
                aria-label={t("settings.aiModes.card.appRules.remove", {
                  rule,
                })}
                className="text-mid-gray hover:text-text disabled:opacity-50"
              >
                <X className="h-3 w-3" />
              </button>
            </span>
          ))}
        </div>
      )}

      <div className="flex gap-2">
        <Input
          type="text"
          variant="compact"
          value={draft}
          disabled={disabled}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              handleAddDraft();
            }
          }}
          placeholder={t("settings.aiModes.card.appRules.placeholder")}
          className="flex-1 min-w-0"
          aria-label={t("settings.aiModes.card.appRules.label")}
        />
        <Button
          type="button"
          variant="secondary"
          size="md"
          onClick={handleAddDraft}
          disabled={disabled || !draft.trim()}
          className="inline-flex shrink-0 items-center gap-1.5"
        >
          <Plus className="h-4 w-4" />
          {t("settings.aiModes.card.appRules.add")}
        </Button>
      </div>

      <div>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={() => void loadRunningApps()}
          disabled={disabled || loadingApps}
          className="inline-flex items-center gap-1.5 text-xs"
        >
          <MonitorSmartphone className="h-3.5 w-3.5" />
          {loadingApps
            ? t("settings.aiModes.card.appRules.loadingApps")
            : t("settings.aiModes.card.appRules.pickRunning")}
        </Button>

        {running && running.length > 0 && (
          <div className="mt-2 flex flex-wrap gap-1.5">
            {running.map((app) => {
              const already = rules.some(
                (r) => r.toLowerCase() === app.bundle_id.toLowerCase(),
              );
              return (
                <button
                  key={app.bundle_id}
                  type="button"
                  disabled={disabled || already}
                  onClick={() => addRule(app.bundle_id)}
                  title={app.bundle_id}
                  className="inline-flex items-center gap-1 rounded-full border border-mid-gray/20 px-2.5 py-1 text-xs hover:bg-mid-gray/10 disabled:opacity-40"
                >
                  <Plus className="h-3 w-3" />
                  {app.name}
                </button>
              );
            })}
          </div>
        )}
        {running && running.length === 0 && (
          <p className="mt-2 text-xs text-mid-gray">
            {t("settings.aiModes.card.appRules.noApps")}
          </p>
        )}
      </div>
    </div>
  );
};
