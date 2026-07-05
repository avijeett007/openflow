import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Download, Settings2, Trash2, Upload, X } from "lucide-react";
import type { DictionaryEntry } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Button } from "../../ui/Button";
import { Input } from "../../ui/Input";
import { SettingsGroup } from "../../ui/SettingsGroup";

// ---------------------------------------------------------------------------
// Helpers (pure, no React) — kept module-local, not exported as primitives.
// ---------------------------------------------------------------------------

const normalize = (value: string) => value.trim().toLowerCase();

const dedupeAliases = (aliases: string[]): string[] => {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const alias of aliases) {
    const trimmed = alias.trim();
    if (!trimmed) continue;
    const key = trimmed.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(trimmed);
  }
  return out;
};

const wordsToEntries = (words: string[]): DictionaryEntry[] => {
  const seen = new Set<string>();
  const out: DictionaryEntry[] = [];
  for (const raw of words) {
    const word = raw.trim();
    if (!word) continue;
    const key = word.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({
      word,
      sounds_like: [],
      replace_exact: false,
      case_sensitive: false,
    });
  }
  return out;
};

const sanitizeEntry = (raw: unknown): DictionaryEntry | null => {
  if (!raw || typeof raw !== "object") return null;
  const candidate = raw as Record<string, unknown>;
  if (typeof candidate.word !== "string") return null;
  const word = candidate.word.trim();
  if (!word) return null;
  const soundsLike = Array.isArray(candidate.sounds_like)
    ? dedupeAliases(
        candidate.sounds_like.filter(
          (item): item is string => typeof item === "string",
        ),
      )
    : [];
  return {
    word,
    sounds_like: soundsLike,
    replace_exact: candidate.replace_exact === true,
    case_sensitive: candidate.case_sensitive === true,
  };
};

// Accepts either a dictionary JSON (array of entries, an array of plain
// strings, or an object wrapping a `dictionary` array) OR a plain
// newline/comma-separated word list.
const parseImport = (text: string): DictionaryEntry[] => {
  const trimmed = text.trim();
  if (!trimmed) return [];

  try {
    const data: unknown = JSON.parse(trimmed);
    if (Array.isArray(data)) {
      if (data.every((item) => typeof item === "string")) {
        return wordsToEntries(data as string[]);
      }
      return data
        .map(sanitizeEntry)
        .filter((entry): entry is DictionaryEntry => entry !== null);
    }
    if (data && typeof data === "object") {
      const wrapped = (data as Record<string, unknown>).dictionary;
      if (Array.isArray(wrapped)) {
        return wrapped
          .map(sanitizeEntry)
          .filter((entry): entry is DictionaryEntry => entry !== null);
      }
      const single = sanitizeEntry(data);
      return single ? [single] : [];
    }
  } catch {
    // Not JSON — fall through to plain word-list parsing.
  }

  return wordsToEntries(trimmed.split(/[\n,]+/));
};

// Merge imported entries into the existing list: matching words (case
// insensitive) are overwritten by the import, new words are appended.
const mergeDictionary = (
  existing: DictionaryEntry[],
  imported: DictionaryEntry[],
): DictionaryEntry[] => {
  const result = existing.map((entry) => ({ ...entry }));
  const indexByWord = new Map<string, number>();
  result.forEach((entry, index) =>
    indexByWord.set(normalize(entry.word), index),
  );

  for (const entry of imported) {
    const key = normalize(entry.word);
    const existingIndex = indexByWord.get(key);
    if (existingIndex !== undefined) {
      result[existingIndex] = entry;
    } else {
      indexByWord.set(key, result.length);
      result.push(entry);
    }
  }
  return result;
};

// ---------------------------------------------------------------------------
// Inline sub-components
// ---------------------------------------------------------------------------

interface InlineToggleProps {
  checked: boolean;
  onChange: (value: boolean) => void;
  disabled?: boolean;
  label: string;
}

const InlineToggle: React.FC<InlineToggleProps> = ({
  checked,
  onChange,
  disabled = false,
  label,
}) => (
  <label
    className={`flex items-center justify-between gap-3 text-sm ${
      disabled ? "cursor-not-allowed opacity-60" : "cursor-pointer"
    }`}
  >
    <span>{label}</span>
    <span className="relative inline-flex shrink-0">
      <input
        type="checkbox"
        className="sr-only peer"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
      />
      <span className="w-9 h-5 bg-mid-gray/20 rounded-full transition-colors after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-white after:rounded-full after:h-4 after:w-4 after:transition-all peer-checked:bg-background-ui peer-checked:after:translate-x-4" />
    </span>
  </label>
);

interface AliasChipsProps {
  aliases: string[];
  onChange: (aliases: string[]) => void;
  disabled?: boolean;
  placeholder: string;
}

const AliasChips: React.FC<AliasChipsProps> = ({
  aliases,
  onChange,
  disabled = false,
  placeholder,
}) => {
  const { t } = useTranslation();
  const [draft, setDraft] = useState("");

  const commit = () => {
    const parts = draft.split(",");
    const next = dedupeAliases([...aliases, ...parts]);
    onChange(next);
    setDraft("");
  };

  const removeAlias = (alias: string) => {
    onChange(aliases.filter((item) => item !== alias));
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter" || event.key === ",") {
      event.preventDefault();
      commit();
    } else if (
      event.key === "Backspace" &&
      draft === "" &&
      aliases.length > 0
    ) {
      event.preventDefault();
      onChange(aliases.slice(0, -1));
    }
  };

  return (
    <div
      className={`flex flex-wrap items-center gap-1 rounded-md border border-mid-gray/40 bg-mid-gray/10 px-2 py-1 min-h-[34px] ${
        disabled ? "opacity-60" : "focus-within:border-logo-primary"
      }`}
    >
      {aliases.map((alias) => (
        <span
          key={alias}
          className="inline-flex items-center gap-1 rounded bg-logo-primary/15 px-1.5 py-0.5 text-xs font-medium"
        >
          <span>{alias}</span>
          <button
            type="button"
            onClick={() => removeAlias(alias)}
            disabled={disabled}
            className="text-mid-gray hover:text-red-400 disabled:cursor-not-allowed"
            aria-label={t("settings.dictionary.removeAlias", { alias })}
          >
            <X className="h-3 w-3" />
          </button>
        </span>
      ))}
      <input
        type="text"
        value={draft}
        disabled={disabled}
        onChange={(event) => setDraft(event.target.value)}
        onKeyDown={handleKeyDown}
        onBlur={() => draft.trim() && commit()}
        placeholder={aliases.length === 0 ? placeholder : ""}
        className="flex-1 min-w-[80px] bg-transparent text-sm outline-none placeholder:text-mid-gray/60"
      />
    </div>
  );
};

interface EntryRowProps {
  entry: DictionaryEntry;
  disabled: boolean;
  onCommitWord: (word: string) => boolean;
  onChangeAliases: (aliases: string[]) => void;
  onToggleOption: (
    option: "case_sensitive" | "replace_exact",
    value: boolean,
  ) => void;
  onDelete: () => void;
}

const EntryRow: React.FC<EntryRowProps> = ({
  entry,
  disabled,
  onCommitWord,
  onChangeAliases,
  onToggleOption,
  onDelete,
}) => {
  const { t } = useTranslation();
  const [wordDraft, setWordDraft] = useState(entry.word);
  const [showOptions, setShowOptions] = useState(false);

  useEffect(() => {
    setWordDraft(entry.word);
  }, [entry.word]);

  const commitWord = () => {
    const trimmed = wordDraft.trim();
    if (trimmed === entry.word) {
      setWordDraft(entry.word);
      return;
    }
    if (!trimmed || !onCommitWord(trimmed)) {
      setWordDraft(entry.word);
    }
  };

  const hasActiveOptions = entry.case_sensitive || entry.replace_exact;

  return (
    <div className="px-4 py-3">
      <div className="flex items-start gap-3">
        <Input
          type="text"
          variant="compact"
          value={wordDraft}
          disabled={disabled}
          onChange={(event) => setWordDraft(event.target.value)}
          onBlur={commitWord}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              event.currentTarget.blur();
            }
          }}
          className="w-40 shrink-0"
          aria-label={t("settings.dictionary.columns.word")}
        />
        <div className="flex-1 min-w-0">
          <AliasChips
            aliases={entry.sounds_like ?? []}
            onChange={onChangeAliases}
            disabled={disabled}
            placeholder={t("settings.dictionary.aliases.placeholder")}
          />
        </div>
        <Button
          type="button"
          variant={showOptions || hasActiveOptions ? "primary-soft" : "ghost"}
          size="sm"
          onClick={() => setShowOptions((value) => !value)}
          disabled={disabled}
          aria-expanded={showOptions}
          aria-label={t("settings.dictionary.options.title")}
          title={t("settings.dictionary.options.title")}
          className="shrink-0"
        >
          <Settings2 className="h-4 w-4" />
        </Button>
        <Button
          type="button"
          variant="danger-ghost"
          size="sm"
          onClick={onDelete}
          disabled={disabled}
          aria-label={t("settings.dictionary.delete")}
          title={t("settings.dictionary.delete")}
          className="shrink-0"
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>

      {showOptions && (
        <div className="mt-3 space-y-2 rounded-md bg-mid-gray/5 p-3">
          <InlineToggle
            checked={entry.case_sensitive ?? false}
            disabled={disabled}
            onChange={(value) => onToggleOption("case_sensitive", value)}
            label={t("settings.dictionary.options.caseSensitive")}
          />
          <InlineToggle
            checked={entry.replace_exact ?? false}
            disabled={disabled}
            onChange={(value) => onToggleOption("replace_exact", value)}
            label={t("settings.dictionary.options.replaceExact")}
          />
        </div>
      )}
    </div>
  );
};

// ---------------------------------------------------------------------------
// Main section
// ---------------------------------------------------------------------------

export const DictionarySettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const fileInputRef = useRef<HTMLInputElement>(null);

  const [newWord, setNewWord] = useState("");
  const [newAliases, setNewAliases] = useState<string[]>([]);

  const dictionary = getSetting("dictionary") ?? [];
  const disabled = isUpdating("dictionary");

  const persist = (next: DictionaryEntry[]) =>
    updateSetting("dictionary", next);

  const wordExists = (word: string, exceptIndex = -1) =>
    dictionary.some(
      (entry, index) =>
        index !== exceptIndex && normalize(entry.word) === normalize(word),
    );

  const handleAdd = () => {
    const word = newWord.trim();
    if (!word) {
      toast.error(t("settings.dictionary.emptyWord"));
      return;
    }
    if (wordExists(word)) {
      toast.error(t("settings.dictionary.duplicate", { word }));
      return;
    }
    persist([
      ...dictionary,
      {
        word,
        sounds_like: dedupeAliases(newAliases),
        replace_exact: false,
        case_sensitive: false,
      },
    ]);
    setNewWord("");
    setNewAliases([]);
  };

  const commitWord = (index: number, word: string): boolean => {
    if (wordExists(word, index)) {
      toast.error(t("settings.dictionary.duplicate", { word }));
      return false;
    }
    persist(
      dictionary.map((entry, i) => (i === index ? { ...entry, word } : entry)),
    );
    return true;
  };

  const changeAliases = (index: number, aliases: string[]) => {
    persist(
      dictionary.map((entry, i) =>
        i === index ? { ...entry, sounds_like: aliases } : entry,
      ),
    );
  };

  const toggleOption = (
    index: number,
    option: "case_sensitive" | "replace_exact",
    value: boolean,
  ) => {
    persist(
      dictionary.map((entry, i) =>
        i === index ? { ...entry, [option]: value } : entry,
      ),
    );
  };

  const removeEntry = (index: number) => {
    persist(dictionary.filter((_, i) => i !== index));
  };

  const handleExport = () => {
    if (dictionary.length === 0) {
      toast.error(t("settings.dictionary.importExport.nothingToExport"));
      return;
    }
    const blob = new Blob([JSON.stringify(dictionary, null, 2)], {
      type: "application/json",
    });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = "openflow-dictionary.json";
    document.body.appendChild(anchor);
    anchor.click();
    document.body.removeChild(anchor);
    URL.revokeObjectURL(url);
  };

  const handleImportClick = () => {
    fileInputRef.current?.click();
  };

  const handleFileSelected = async (
    event: React.ChangeEvent<HTMLInputElement>,
  ) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;

    try {
      const text = await file.text();
      const imported = parseImport(text);
      if (imported.length === 0) {
        toast.error(t("settings.dictionary.importExport.importError"));
        return;
      }
      await persist(mergeDictionary(dictionary, imported));
      toast.success(
        t("settings.dictionary.importExport.importSuccess", {
          count: imported.length,
        }),
      );
    } catch (error) {
      console.error("Failed to import dictionary:", error);
      toast.error(t("settings.dictionary.importExport.importError"));
    }
  };

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.dictionary.title")}>
        <div className="px-4 py-3 space-y-3">
          <p className="text-sm text-mid-gray">
            {t("settings.dictionary.intro")}
          </p>

          <div className="space-y-2">
            <div className="flex gap-2">
              <Input
                type="text"
                variant="compact"
                value={newWord}
                disabled={disabled}
                onChange={(event) => setNewWord(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    handleAdd();
                  }
                }}
                placeholder={t("settings.dictionary.word.placeholder")}
                className="flex-1"
                aria-label={t("settings.dictionary.columns.word")}
              />
              <Button
                onClick={handleAdd}
                disabled={disabled || !newWord.trim()}
                variant="primary"
                size="md"
                className="shrink-0"
              >
                {t("settings.dictionary.add")}
              </Button>
            </div>
            <AliasChips
              aliases={newAliases}
              onChange={setNewAliases}
              disabled={disabled}
              placeholder={t("settings.dictionary.aliases.placeholder")}
            />
          </div>

          <div className="flex flex-wrap gap-2 pt-1">
            <Button
              onClick={handleImportClick}
              disabled={disabled}
              variant="secondary"
              size="md"
              className="inline-flex items-center gap-1.5"
            >
              <Upload className="h-4 w-4" />
              {t("settings.dictionary.importExport.import")}
            </Button>
            <Button
              onClick={handleExport}
              disabled={disabled}
              variant="secondary"
              size="md"
              className="inline-flex items-center gap-1.5"
            >
              <Download className="h-4 w-4" />
              {t("settings.dictionary.importExport.export")}
            </Button>
            <input
              ref={fileInputRef}
              type="file"
              accept=".json,.txt,application/json,text/plain"
              onChange={handleFileSelected}
              className="hidden"
            />
          </div>
        </div>
      </SettingsGroup>

      {dictionary.length === 0 ? (
        <div className="rounded-lg border border-dashed border-mid-gray/30 px-4 py-8 text-center text-sm text-mid-gray">
          {t("settings.dictionary.emptyState")}
        </div>
      ) : (
        <div className="space-y-2">
          <div className="flex items-center gap-3 px-4 text-xs font-medium uppercase tracking-wide text-mid-gray">
            <span className="w-40 shrink-0">
              {t("settings.dictionary.columns.word")}
            </span>
            <span className="flex-1">
              {t("settings.dictionary.columns.aliases")}
            </span>
          </div>
          <div className="bg-background border border-mid-gray/20 rounded-lg divide-y divide-mid-gray/20">
            {dictionary.map((entry, index) => (
              <EntryRow
                key={entry.word}
                entry={entry}
                disabled={disabled}
                onCommitWord={(word) => commitWord(index, word)}
                onChangeAliases={(aliases) => changeAliases(index, aliases)}
                onToggleOption={(option, value) =>
                  toggleOption(index, option, value)
                }
                onDelete={() => removeEntry(index)}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
};
