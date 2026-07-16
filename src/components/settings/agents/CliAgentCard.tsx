import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { open } from "@tauri-apps/plugin-dialog";
import {
  ChevronDown,
  ChevronRight,
  FileSearch,
  FlaskConical,
  FolderOpen,
  ScanSearch,
  Trash2,
  X,
} from "lucide-react";
import type {
  AgentCliType,
  AgentDefinition,
  AgentOutputSink,
} from "@/bindings";
import { commands } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Input } from "../../ui/Input";
import { Textarea } from "../../ui/Textarea";
import { Button } from "../../ui/Button";
import { Dialog } from "../../ui/Dialog";
import { Alert } from "../../ui/Alert";
import { SettingContainer } from "../../ui/SettingContainer";
import { Dropdown } from "../../ui/Dropdown";
import { ShortcutInput } from "../ShortcutInput";
import { AgentInlineToggle } from "./AgentInlineToggle";

interface CliAgentCardProps {
  agent: AgentDefinition;
}

const CLI_TYPES: AgentCliType[] = [
  "claude",
  "codex",
  "openclaw",
  "hermes",
  "custom",
];

const OUTPUT_SINKS: AgentOutputSink[] = ["panel", "notify", "file"];

/**
 * CLI-agent card (Flow OS increment 2): configures a real coding-agent
 * subprocess (Claude Code, Codex, ...) instead of the increment-1
 * persona-LLM transform. Mirrors `AgentCard`'s layout/patterns (header row,
 * `ShortcutInput`, `SettingContainer` rows, optimistic drafts committed via
 * `commands.updateAgent` + `refreshSettings`).
 */
export const CliAgentCard: React.FC<CliAgentCardProps> = ({ agent }) => {
  const { t } = useTranslation();
  const { refreshSettings } = useSettings();

  const [pending, setPending] = useState<Record<string, boolean>>({});
  const [nameDraft, setNameDraft] = useState(agent.name);
  const [binaryPathDraft, setBinaryPathDraft] = useState(
    agent.binary_path ?? "",
  );
  const [commandTemplateDraft, setCommandTemplateDraft] = useState(
    agent.command_template ?? "",
  );
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  const [binaryTestResult, setBinaryTestResult] = useState<{
    ok: boolean;
    output: string;
  } | null>(null);
  const [binaryTestError, setBinaryTestError] = useState<string | null>(null);
  // Inline "not found" notice shown after a failed auto-detect so the user
  // isn't left with a silently-stale path (they can type one or Browse).
  const [detectNotFound, setDetectNotFound] = useState(false);

  useEffect(() => setNameDraft(agent.name), [agent.name]);
  useEffect(
    () => setBinaryPathDraft(agent.binary_path ?? ""),
    [agent.binary_path],
  );
  useEffect(
    () => setCommandTemplateDraft(agent.command_template ?? ""),
    [agent.command_template],
  );

  const isPending = (field: string) => pending[field] ?? false;

  const persist = async (
    patch: Partial<AgentDefinition>,
    field: string,
  ): Promise<boolean> => {
    setPending((prev) => ({ ...prev, [field]: true }));
    try {
      const result = await commands.updateAgent({ ...agent, ...patch });
      if (result.status === "error") {
        toast.error(
          t("settings.agents.card.update.error", { error: result.error }),
        );
        return false;
      }
      await refreshSettings();
      return true;
    } finally {
      setPending((prev) => ({ ...prev, [field]: false }));
    }
  };

  const commitName = () => {
    const trimmed = nameDraft.trim();
    if (!trimmed) {
      setNameDraft(agent.name);
      return;
    }
    if (trimmed === agent.name) return;
    void persist({ name: trimmed }, "name");
  };

  const commitBinaryPath = () => {
    const trimmed = binaryPathDraft.trim();
    if (trimmed === (agent.binary_path ?? "")) return;
    void persist({ binary_path: trimmed }, "binary_path");
  };

  const commitCommandTemplate = () => {
    if (commandTemplateDraft === (agent.command_template ?? "")) return;
    void persist(
      { command_template: commandTemplateDraft },
      "command_template",
    );
  };

  const handleDelete = async () => {
    setIsDeleting(true);
    try {
      const result = await commands.deleteAgent(agent.id);
      if (result.status === "error") {
        toast.error(
          t("settings.agents.card.delete.error", { error: result.error }),
        );
        return;
      }
      await refreshSettings();
      toast.success(
        t("settings.agents.card.delete.success", { name: agent.name }),
      );
      setConfirmingDelete(false);
    } finally {
      setIsDeleting(false);
    }
  };

  const handleCliTypeChange = async (value: string) => {
    const cliType = value as AgentCliType;
    setPending((prev) => ({ ...prev, cli_type: true }));
    setBinaryTestResult(null);
    setBinaryTestError(null);
    setDetectNotFound(false);
    try {
      // Always refresh the template + delivery for the new type…
      const patch: Partial<AgentDefinition> = { cli_type: cliType };

      try {
        const defaults = await commands.getCliAgentDefaults(cliType);
        patch.command_template = defaults.command_template;
        patch.prompt_via = defaults.prompt_via;
      } catch (err) {
        toast.error(
          t("settings.agents.card.cli.agentType.defaultsError", {
            error: String(err),
          }),
        );
      }

      // …and re-run detection. On success adopt the new path; on failure CLEAR
      // the stale one (it belonged to the previous type) and flag "not found"
      // so the path field never silently points at the wrong binary.
      const detected = await commands.detectAgentBinary(cliType);
      if (detected.status === "ok") {
        patch.binary_path = detected.data;
        setDetectNotFound(false);
      } else {
        patch.binary_path = "";
        setDetectNotFound(cliType !== "custom");
      }

      await persist(patch, "cli_type");
    } finally {
      setPending((prev) => ({ ...prev, cli_type: false }));
    }
  };

  const handleDetectBinary = async () => {
    const cliType = agent.cli_type ?? "custom";
    setPending((prev) => ({ ...prev, detect: true }));
    setBinaryTestResult(null);
    setBinaryTestError(null);
    setDetectNotFound(false);
    try {
      const detected = await commands.detectAgentBinary(cliType);
      if (detected.status === "error") {
        // Clear any stale path and surface an inline, actionable notice.
        setBinaryPathDraft("");
        setDetectNotFound(true);
        await persist({ binary_path: "" }, "binary_path");
        return;
      }
      setBinaryPathDraft(detected.data);
      await persist({ binary_path: detected.data }, "binary_path");
    } finally {
      setPending((prev) => ({ ...prev, detect: false }));
    }
  };

  const handleBrowseBinary = async () => {
    try {
      // Start in the Homebrew bin dir when the field is empty; otherwise near
      // the current path. The picker falls back gracefully if it doesn't exist.
      const current = binaryPathDraft.trim();
      const defaultPath =
        current.length > 0
          ? current.slice(0, current.lastIndexOf("/") + 1) || undefined
          : "/opt/homebrew/bin";
      const picked = await open({ directory: false, defaultPath });
      if (typeof picked === "string" && picked.length > 0) {
        setBinaryPathDraft(picked);
        setDetectNotFound(false);
        setBinaryTestResult(null);
        setBinaryTestError(null);
        await persist({ binary_path: picked }, "binary_path");
      }
    } catch (err) {
      toast.error(
        t("settings.agents.card.cli.binaryPath.browseError", {
          error: String(err),
        }),
      );
    }
  };

  const handleTestBinary = async () => {
    const target = binaryPathDraft.trim();
    if (!target) return;
    setPending((prev) => ({ ...prev, test: true }));
    setBinaryTestResult(null);
    setBinaryTestError(null);
    try {
      const result = await commands.testAgentBinary(target);
      if (result.status === "error") {
        setBinaryTestError(result.error);
        return;
      }
      // A classified, actionable failure (e.g. a broken Codex install) renders
      // its own fix message instead of the raw spawn/stderr text.
      if (result.data.hint === "codex_vendor_missing") {
        setBinaryTestError(
          t("settings.agents.card.cli.binaryPath.hint.codexVendorMissing"),
        );
        return;
      }
      setBinaryTestResult(result.data);
    } catch (err) {
      setBinaryTestError(String(err));
    } finally {
      setPending((prev) => ({ ...prev, test: false }));
    }
  };

  const handleChooseFolder = async () => {
    try {
      const dir = await open({ directory: true });
      if (typeof dir === "string" && dir.length > 0) {
        await persist({ project_path: dir }, "project_path");
      }
    } catch (err) {
      toast.error(
        t("settings.agents.card.cli.projectPath.error", {
          error: String(err),
        }),
      );
    }
  };

  const handleClearFolder = () =>
    void persist({ project_path: "" }, "project_path");

  const toggleOutputSink = (sink: AgentOutputSink) => {
    const current = agent.output_sinks ?? ["panel"];
    const has = current.includes(sink);
    let next: AgentOutputSink[];
    if (has) {
      // Never allow the last sink to be unchecked - an agent must always
      // land its output somewhere (defaults to Panel).
      if (current.length <= 1) return;
      next = current.filter((s) => s !== sink);
    } else {
      next = [...current, sink];
    }
    void persist({ output_sinks: next }, "output_sinks");
  };

  const cliTypeOptions = CLI_TYPES.map((type) => ({
    value: type,
    label: t(`settings.agents.card.cli.agentType.options.${type}`),
  }));

  const activeOutputSinks = agent.output_sinks ?? ["panel"];
  const projectPath = agent.project_path ?? "";

  return (
    <div className="bg-background border border-mid-gray/20 rounded-lg divide-y divide-mid-gray/20">
      <div className="flex items-center gap-3 px-4 py-3">
        <Input
          type="text"
          variant="compact"
          value={nameDraft}
          disabled={isPending("name")}
          onChange={(event) => setNameDraft(event.target.value)}
          onBlur={commitName}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              event.currentTarget.blur();
            }
          }}
          placeholder={t("settings.agents.card.name.placeholder")}
          className="flex-1 min-w-0 font-semibold"
          aria-label={t("settings.agents.card.name.label")}
        />
        <AgentInlineToggle
          checked={agent.enabled ?? true}
          disabled={isPending("enabled")}
          onChange={(checked) => void persist({ enabled: checked }, "enabled")}
          label={t("settings.agents.card.enabled.label")}
        />
        <Button
          type="button"
          variant="danger-ghost"
          size="sm"
          onClick={() => setConfirmingDelete(true)}
          aria-label={t("settings.agents.card.delete.button")}
          title={t("settings.agents.card.delete.button")}
          className="shrink-0"
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>

      <ShortcutInput shortcutId={agent.binding_id} grouped />

      <SettingContainer
        title={t("settings.agents.card.cli.agentType.label")}
        description={t("settings.agents.card.cli.agentType.description")}
        descriptionMode="tooltip"
        grouped
        layout="horizontal"
      >
        <Dropdown
          options={cliTypeOptions}
          selectedValue={agent.cli_type ?? "custom"}
          onSelect={(value) => void handleCliTypeChange(value)}
          disabled={isPending("cli_type")}
          className="min-w-[220px]"
        />
      </SettingContainer>

      <SettingContainer
        title={t("settings.agents.card.cli.binaryPath.label")}
        description={t("settings.agents.card.cli.binaryPath.description")}
        descriptionMode="tooltip"
        grouped
        layout="stacked"
      >
        <div className="space-y-2">
          <div className="flex gap-2">
            <Input
              type="text"
              variant="compact"
              value={binaryPathDraft}
              disabled={isPending("binary_path")}
              onChange={(event) => {
                setBinaryPathDraft(event.target.value);
                if (detectNotFound) setDetectNotFound(false);
              }}
              onBlur={commitBinaryPath}
              placeholder={t("settings.agents.card.cli.binaryPath.placeholder")}
              className="flex-1 min-w-0"
              aria-label={t("settings.agents.card.cli.binaryPath.label")}
            />
            <Button
              type="button"
              variant="secondary"
              size="md"
              onClick={handleDetectBinary}
              disabled={isPending("detect")}
              className="inline-flex shrink-0 items-center gap-1.5"
            >
              <ScanSearch className="h-4 w-4" />
              {isPending("detect")
                ? t("settings.agents.card.cli.binaryPath.detecting")
                : t("settings.agents.card.cli.binaryPath.detect")}
            </Button>
            <Button
              type="button"
              variant="secondary"
              size="md"
              onClick={handleBrowseBinary}
              disabled={isPending("binary_path")}
              className="inline-flex shrink-0 items-center gap-1.5"
            >
              <FileSearch className="h-4 w-4" />
              {t("settings.agents.card.cli.binaryPath.browse")}
            </Button>
            <Button
              type="button"
              variant="secondary"
              size="md"
              onClick={handleTestBinary}
              disabled={isPending("test") || !binaryPathDraft.trim()}
              className="inline-flex shrink-0 items-center gap-1.5"
            >
              <FlaskConical className="h-4 w-4" />
              {isPending("test")
                ? t("settings.agents.card.cli.binaryPath.testing")
                : t("settings.agents.card.cli.binaryPath.test")}
            </Button>
          </div>

          {detectNotFound && (
            <Alert variant="warning" contained>
              {t("settings.agents.card.cli.binaryPath.notFound")}
            </Alert>
          )}
          {binaryTestError && (
            <Alert variant="error" contained>
              {t("settings.agents.card.cli.binaryPath.testError", {
                error: binaryTestError,
              })}
            </Alert>
          )}
          {binaryTestResult && (
            <Alert
              variant={binaryTestResult.ok ? "success" : "warning"}
              contained
            >
              {binaryTestResult.ok
                ? t("settings.agents.card.cli.binaryPath.testOk", {
                    output: binaryTestResult.output,
                  })
                : t("settings.agents.card.cli.binaryPath.testFailed", {
                    output: binaryTestResult.output,
                  })}
            </Alert>
          )}
        </div>
      </SettingContainer>

      <SettingContainer
        title={t("settings.agents.card.cli.projectPath.label")}
        description={t("settings.agents.card.cli.projectPath.description")}
        descriptionMode="tooltip"
        grouped
        layout="horizontal"
      >
        <div className="flex items-center gap-2">
          <span
            className="max-w-[220px] truncate text-sm text-mid-gray"
            title={projectPath || undefined}
          >
            {projectPath ||
              t("settings.agents.card.cli.projectPath.placeholder")}
          </span>
          <Button
            type="button"
            variant="secondary"
            size="sm"
            onClick={handleChooseFolder}
            disabled={isPending("project_path")}
            className="inline-flex shrink-0 items-center gap-1.5"
          >
            <FolderOpen className="h-4 w-4" />
            {t("settings.agents.card.cli.projectPath.choose")}
          </Button>
          {projectPath && (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={handleClearFolder}
              disabled={isPending("project_path")}
              aria-label={t("settings.agents.card.cli.projectPath.clear")}
              title={t("settings.agents.card.cli.projectPath.clear")}
              className="shrink-0"
            >
              <X className="h-4 w-4" />
            </Button>
          )}
        </div>
      </SettingContainer>

      <div className="px-4 py-2">
        <button
          type="button"
          onClick={() => setShowAdvanced((prev) => !prev)}
          className="flex items-center gap-1.5 text-sm font-medium text-mid-gray hover:text-text transition-colors cursor-pointer"
        >
          {showAdvanced ? (
            <ChevronDown className="h-4 w-4" />
          ) : (
            <ChevronRight className="h-4 w-4" />
          )}
          {showAdvanced
            ? t("settings.agents.card.cli.commandTemplate.hide")
            : t("settings.agents.card.cli.commandTemplate.show")}
        </button>
        {showAdvanced && (
          <div className="mt-3">
            <SettingContainer
              title={t("settings.agents.card.cli.commandTemplate.label")}
              description={t(
                "settings.agents.card.cli.commandTemplate.description",
              )}
              descriptionMode="tooltip"
              grouped
              layout="stacked"
            >
              <Textarea
                value={commandTemplateDraft}
                disabled={isPending("command_template")}
                onChange={(event) =>
                  setCommandTemplateDraft(event.target.value)
                }
                onBlur={commitCommandTemplate}
                placeholder={t(
                  "settings.agents.card.cli.commandTemplate.placeholder",
                )}
                className="w-full font-mono"
              />
            </SettingContainer>
          </div>
        )}
      </div>

      <SettingContainer
        title={t("settings.agents.card.cli.outputSinks.label")}
        description={t("settings.agents.card.cli.outputSinks.description")}
        descriptionMode="tooltip"
        grouped
        layout="horizontal"
      >
        <div className="flex items-center gap-3">
          {OUTPUT_SINKS.map((sink) => (
            <label
              key={sink}
              className="flex items-center gap-1.5 text-sm cursor-pointer"
            >
              <input
                type="checkbox"
                checked={activeOutputSinks.includes(sink)}
                disabled={isPending("output_sinks")}
                onChange={() => toggleOutputSink(sink)}
                className="h-4 w-4 rounded border-mid-gray/80 accent-background-ui"
              />
              {t(`settings.agents.card.cli.outputSinks.${sink}`)}
            </label>
          ))}
        </div>
      </SettingContainer>

      <Dialog
        open={confirmingDelete}
        onOpenChange={setConfirmingDelete}
        title={t("settings.agents.card.delete.confirmTitle", {
          name: agent.name,
        })}
        description={t("settings.agents.card.delete.confirmDescription")}
        closeLabel={t("settings.agents.card.delete.cancel")}
        footer={
          <>
            <Button
              type="button"
              variant="secondary"
              size="md"
              onClick={() => setConfirmingDelete(false)}
              disabled={isDeleting}
            >
              {t("settings.agents.card.delete.cancel")}
            </Button>
            <Button
              type="button"
              variant="danger"
              size="md"
              onClick={handleDelete}
              disabled={isDeleting}
            >
              {t("settings.agents.card.delete.confirm")}
            </Button>
          </>
        }
      >
        <></>
      </Dialog>
    </div>
  );
};
