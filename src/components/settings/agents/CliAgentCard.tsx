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
  AcpAgentTest,
  AcpPermissionPolicy,
  AgentCliType,
  AgentDefinition,
  AgentOutputSink,
  CliProtocol,
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
import { ModeToggle } from "../model-setup/ModeToggle";
import { AgentInlineToggle } from "./AgentInlineToggle";
import { ACP_DEFAULT_TEMPLATES, supportsAcpProtocol } from "./agentTemplates";

interface CliAgentCardProps {
  agent: AgentDefinition;
}

const CLI_TYPES: AgentCliType[] = [
  "claude",
  "codex",
  "openclaw",
  "hermes",
  "kimi",
  "custom",
];

const OUTPUT_SINKS: AgentOutputSink[] = ["panel", "notify", "file"];

const ACP_PERMISSION_POLICIES: AcpPermissionPolicy[] = [
  "ask",
  "auto_edits",
  "auto_all",
];

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

  // ---- ACP mode ----
  const [acpCommandTemplateDraft, setAcpCommandTemplateDraft] = useState(
    agent.acp_command_template ?? "",
  );
  const [acpIdleTimeoutDraft, setAcpIdleTimeoutDraft] = useState(
    String(agent.acp_idle_timeout_secs ?? 600),
  );
  const [acpTestResult, setAcpTestResult] = useState<AcpAgentTest | null>(null);
  const [acpTestError, setAcpTestError] = useState<string | null>(null);

  useEffect(() => setNameDraft(agent.name), [agent.name]);
  useEffect(
    () => setBinaryPathDraft(agent.binary_path ?? ""),
    [agent.binary_path],
  );
  useEffect(
    () => setCommandTemplateDraft(agent.command_template ?? ""),
    [agent.command_template],
  );
  useEffect(
    () => setAcpCommandTemplateDraft(agent.acp_command_template ?? ""),
    [agent.acp_command_template],
  );
  useEffect(
    () => setAcpIdleTimeoutDraft(String(agent.acp_idle_timeout_secs ?? 600)),
    [agent.acp_idle_timeout_secs],
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

  const commitAcpCommandTemplate = () => {
    if (acpCommandTemplateDraft === (agent.acp_command_template ?? "")) return;
    void persist(
      { acp_command_template: acpCommandTemplateDraft },
      "acp_command_template",
    );
  };

  const commitAcpIdleTimeout = () => {
    const parsed = Number.parseInt(acpIdleTimeoutDraft, 10);
    const next = Number.isNaN(parsed) || parsed < 0 ? 0 : parsed;
    if (next === (agent.acp_idle_timeout_secs ?? 600)) {
      setAcpIdleTimeoutDraft(String(next));
      return;
    }
    void persist({ acp_idle_timeout_secs: next }, "acp_idle_timeout_secs");
  };

  const handleProtocolChange = async (value: string) => {
    const protocol = value as CliProtocol;
    if (protocol === (agent.cli_protocol ?? "raw")) return;
    setPending((prev) => ({ ...prev, cli_protocol: true }));
    setAcpTestResult(null);
    setAcpTestError(null);
    try {
      const patch: Partial<AgentDefinition> = { cli_protocol: protocol };
      // Prefill the ACP command template the FIRST time the agent switches
      // to ACP mode (never overwrite an already-customized template).
      // `command_template` (one-shot mode) is deliberately never touched
      // here - the two templates are separate fields precisely so toggling
      // back and forth never loses either mode's configuration.
      if (protocol === "acp" && !(agent.acp_command_template ?? "").trim()) {
        const effectiveCliType = agent.cli_type ?? "custom";
        const preset = ACP_DEFAULT_TEMPLATES[effectiveCliType];
        if (preset) {
          patch.acp_command_template = preset.argv;
        }
      }
      await persist(patch, "cli_protocol");
    } finally {
      setPending((prev) => ({ ...prev, cli_protocol: false }));
    }
  };

  const handleTestAcpAgent = async () => {
    setPending((prev) => ({ ...prev, acpTest: true }));
    setAcpTestResult(null);
    setAcpTestError(null);
    try {
      // Commit any unsaved ACP command-template edit first so the handshake
      // actually exercises what's about to be tested, not stale state.
      if (acpCommandTemplateDraft !== (agent.acp_command_template ?? "")) {
        const ok = await persist(
          { acp_command_template: acpCommandTemplateDraft },
          "acp_command_template",
        );
        if (!ok) return;
      }
      const result = await commands.testAcpAgent(agent.id);
      if (result.status === "error") {
        setAcpTestError(result.error);
        return;
      }
      setAcpTestResult(result.data);
    } catch (err) {
      setAcpTestError(String(err));
    } finally {
      setPending((prev) => ({ ...prev, acpTest: false }));
    }
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
    setAcpTestResult(null);
    setAcpTestError(null);
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

  // ---- ACP mode ----
  const effectiveCliType: AgentCliType = agent.cli_type ?? "custom";
  const acpOffered = supportsAcpProtocol(effectiveCliType);
  const protocol: CliProtocol = agent.cli_protocol ?? "raw";
  const isAcp = acpOffered && protocol === "acp";
  const acpPreset = ACP_DEFAULT_TEMPLATES[effectiveCliType];
  // The program actually launched in ACP mode: the cli_type's hint binary
  // wins when one exists (Claude/Codex/Kimi); otherwise it falls back to
  // `binary_path`, mirroring `resolve_acp_binary` on the backend exactly.
  // This is what makes the field honest - a user who left `binary_path` set
  // to the raw `claude` binary must see that a DIFFERENT program (npx ...)
  // is what actually gets spawned in ACP mode.
  const resolvedAcpBinary =
    acpPreset?.binary ??
    (binaryPathDraft.trim() ||
      t("settings.agents.acp.resolvedProgram.binaryUnset"));
  const resolvedAcpProgram =
    `${resolvedAcpBinary} ${acpCommandTemplateDraft}`.trim();

  const permissionOptions = ACP_PERMISSION_POLICIES.map((policy) => ({
    value: policy,
    label: t(`settings.agents.acp.permission.options.${policy}`),
  }));
  const permissionPolicy: AcpPermissionPolicy =
    agent.acp_permission_policy ?? "ask";

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
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          autoComplete="off"
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
              autoCorrect="off"
              autoCapitalize="off"
              spellCheck={false}
              autoComplete="off"
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

      {acpOffered && (
        <SettingContainer
          title={t("settings.agents.acp.protocol.label")}
          description={t("settings.agents.acp.protocol.description")}
          descriptionMode="tooltip"
          grouped
          layout="horizontal"
        >
          <ModeToggle
            value={protocol}
            options={[
              {
                value: "raw",
                label: t("settings.agents.acp.protocol.raw"),
              },
              {
                value: "acp",
                label: t("settings.agents.acp.protocol.acp"),
              },
            ]}
            onChange={(value) => void handleProtocolChange(value)}
            disabled={isPending("cli_type") || isPending("cli_protocol")}
          />
        </SettingContainer>
      )}

      {isAcp && (
        <>
          <SettingContainer
            title={t("settings.agents.acp.command.label")}
            description={t("settings.agents.acp.command.description")}
            descriptionMode="tooltip"
            grouped
            layout="stacked"
          >
            <div className="space-y-2">
              <Textarea
                value={acpCommandTemplateDraft}
                disabled={isPending("acp_command_template")}
                onChange={(event) =>
                  setAcpCommandTemplateDraft(event.target.value)
                }
                onBlur={commitAcpCommandTemplate}
                placeholder={t("settings.agents.acp.command.placeholder")}
                className="w-full font-mono"
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                autoComplete="off"
              />
              <p className="text-xs text-mid-gray">
                {t("settings.agents.acp.resolvedProgram.label")}{" "}
                <code className="rounded bg-mid-gray/10 px-1 py-0.5 font-mono">
                  {resolvedAcpProgram}
                </code>
              </p>
              {/* Verified live: codex-acp advertises `authMethods` and refuses
                  `session/new` with "Authentication required" until the CLI
                  itself has been logged in. OpenFlow does not implement ACP's
                  `authenticate`, so this preset simply cannot work first-run —
                  say so here rather than let the user discover it as a failed
                  run. */}
              {effectiveCliType === "codex" && (
                <p className="text-xs text-mid-gray/80 italic">
                  {t("settings.agents.acp.resolvedProgram.codexLoginNote")}
                </p>
              )}
            </div>
          </SettingContainer>

          <SettingContainer
            title={t("settings.agents.acp.permission.label")}
            description={t("settings.agents.acp.permission.description")}
            descriptionMode="tooltip"
            grouped
            layout="stacked"
          >
            <div className="space-y-2">
              <Dropdown
                options={permissionOptions}
                selectedValue={permissionPolicy}
                onSelect={(value) =>
                  void persist(
                    {
                      acp_permission_policy: value as AcpPermissionPolicy,
                    },
                    "acp_permission_policy",
                  )
                }
                disabled={isPending("acp_permission_policy")}
                className="min-w-[260px]"
              />
              {permissionPolicy === "auto_all" && (
                <Alert variant="warning" contained>
                  {t("settings.agents.acp.permission.autoAllWarning")}
                </Alert>
              )}
            </div>
          </SettingContainer>

          <SettingContainer
            title={t("settings.agents.acp.idleTimeout.label")}
            description={t("settings.agents.acp.idleTimeout.description")}
            descriptionMode="tooltip"
            grouped
            layout="horizontal"
          >
            <div className="flex items-center gap-2">
              <Input
                type="number"
                min="0"
                variant="compact"
                value={acpIdleTimeoutDraft}
                disabled={isPending("acp_idle_timeout_secs")}
                onChange={(event) => setAcpIdleTimeoutDraft(event.target.value)}
                onBlur={commitAcpIdleTimeout}
                className="w-24"
                aria-label={t("settings.agents.acp.idleTimeout.label")}
              />
              <span className="text-sm text-mid-gray">
                {t("settings.agents.acp.idleTimeout.seconds")}
              </span>
            </div>
          </SettingContainer>

          <SettingContainer
            title={t("settings.agents.acp.test.label")}
            description={t("settings.agents.acp.test.description")}
            descriptionMode="tooltip"
            grouped
            layout="stacked"
          >
            <div className="space-y-2">
              <Button
                type="button"
                variant="secondary"
                size="md"
                onClick={() => void handleTestAcpAgent()}
                disabled={isPending("acpTest")}
                className="inline-flex shrink-0 items-center gap-1.5"
              >
                <FlaskConical className="h-4 w-4" />
                {isPending("acpTest")
                  ? t("settings.agents.acp.test.testing")
                  : t("settings.agents.acp.test.run")}
              </Button>
              {acpTestError && (
                <Alert variant="error" contained>
                  {t("settings.agents.acp.test.error", {
                    error: acpTestError,
                  })}
                </Alert>
              )}
              {acpTestResult && (
                <Alert variant="success" contained>
                  {t("settings.agents.acp.test.ok", {
                    name: acpTestResult.agent_name,
                    version: acpTestResult.agent_version,
                    protocolVersion: acpTestResult.protocol_version,
                  })}
                </Alert>
              )}
            </div>
          </SettingContainer>
        </>
      )}

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
                // Hardening against macOS text substitution: a hand-typed
                // `--yolo` was silently mangled into an em-dash ("—yolo") by
                // WebKit's smart-dashes substitution, which the CLI then
                // rejected as an unknown command. autoCorrect/autoComplete
                // "off" disable WebKit's substitution behavior (smart
                // quotes/dashes, autocomplete) on this field; the prefilled
                // per-type template (see handleCliTypeChange) means most users
                // never hand-type flags here at all.
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                autoComplete="off"
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
