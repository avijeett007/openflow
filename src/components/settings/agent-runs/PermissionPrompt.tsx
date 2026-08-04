import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { ShieldAlert } from "lucide-react";
import { commands } from "@/bindings";
import type { PermissionOption } from "@/bindings";
import { Button } from "../../ui/Button";
import type { PermissionRow } from "./runEventRows";

/** The four outcomes `respond_agent_permission` accepts. */
type Outcome = "allow_once" | "allow_always" | "deny_once" | "deny_always";

/**
 * Maps an agent-supplied option's `kind` (ACP's own `allow_once |
 * allow_always | reject_once | reject_always`) to the outcome vocabulary the
 * `respond_agent_permission` command accepts. A robust `includes`/`startsWith`
 * check rather than an exact match table, since every kind ACP defines fits
 * this shape and an unrecognized future kind should still resolve sensibly
 * rather than silently drop the button.
 *
 * This is a GUESS for anything outside those four kinds, and the backend knows
 * it: the exact `option_id` travels alongside and is what actually gets sent,
 * and `PermissionResolved`'s recorded outcome is derived from THAT option's
 * kind, not from this return value. Otherwise an agent offering `kind:
 * "approve"` would be correctly allowed while the audit trail said "Denied".
 */
const toOutcome = (kind: string): Outcome => {
  const always = kind.includes("always");
  const allow = kind.startsWith("allow");
  if (allow) return always ? "allow_always" : "allow_once";
  return always ? "deny_always" : "deny_once";
};

/**
 * Button emphasis. Only ACP's own vocabulary earns a colour: `allow*` reads as
 * the affirmative action, `reject*`/`deny*` as the destructive one. Anything
 * else is styled NEUTRALLY rather than as a denial — painting an unrecognised
 * option red tells the user it refuses when we have no idea whether it does.
 */
const variantFor = (
  kind: string,
): "primary-soft" | "danger-ghost" | "ghost" => {
  if (kind.startsWith("allow")) return "primary-soft";
  if (kind.startsWith("reject") || kind.startsWith("deny"))
    return "danger-ghost";
  return "ghost";
};

interface PermissionPromptProps {
  runId: string;
  requests: PermissionRow[];
}

/**
 * Non-modal, pinned to the bottom of the run — a modal would cover the very
 * output the user needs in order to decide whether to allow the action.
 * Renders exactly one button per agent-supplied option (never inventing one
 * the agent didn't offer); once a matching `PermissionResolved` (or the turn
 * ending) lands, the parent's `openPermissionRows` no longer includes this
 * request and the card disappears on its own — no local "resolved" state to
 * fall out of sync with.
 */
export const PermissionPrompt: React.FC<PermissionPromptProps> = ({
  runId,
  requests,
}) => {
  const { t } = useTranslation();
  const [pendingIds, setPendingIds] = useState<Set<string>>(new Set());

  if (requests.length === 0) return null;

  // Sends the EXACT option the user clicked (Task 11 review, Important 5):
  // collapsing to just `outcome`'s 4-value vocabulary discarded which of
  // possibly several same-kind options was actually pressed, so the backend
  // could only guess (always the first of that kind) — a real trust failure
  // for a feature that exists to make an agent's actions legible. `outcome`
  // still travels alongside it: the backend derives the once/always +
  // allow/deny bookkeeping from it, and falls back to kind-based selection
  // if `option.option_id` ever fails to match (see
  // `AgentRunManager::respond_permission`'s doc comment).
  const respond = async (requestId: string, option: PermissionOption) => {
    setPendingIds((prev) => new Set(prev).add(requestId));
    try {
      const result = await commands.respondAgentPermission(
        runId,
        requestId,
        toOutcome(option.kind),
        option.option_id,
      );
      if (result.status === "error") {
        toast.error(
          t("settings.agentRuns.acp.permission.respondError", {
            error: result.error,
          }),
        );
      }
    } finally {
      setPendingIds((prev) => {
        const next = new Set(prev);
        next.delete(requestId);
        return next;
      });
    }
  };

  return (
    <div className="px-4 pb-3 space-y-2">
      {requests.map((req) => {
        const busy = pendingIds.has(req.requestId);
        const hasAlwaysOption = req.options.some((o) =>
          o.kind.includes("always"),
        );
        return (
          <div
            key={req.requestId}
            className="rounded-lg border border-logo-primary/40 bg-logo-primary/5 p-3 space-y-2"
          >
            <div className="flex items-start gap-2">
              <ShieldAlert className="h-4 w-4 mt-0.5 shrink-0 text-logo-primary" />
              <div className="min-w-0 flex-1">
                <p className="text-sm font-medium break-words">{req.title}</p>
                {req.targetPaths.length > 0 && (
                  <p className="text-xs text-mid-gray break-words font-mono mt-0.5">
                    {t("settings.agentRuns.acp.permission.target", {
                      path: req.targetPaths.join(", "),
                    })}
                  </p>
                )}
                <p className="text-xs text-mid-gray mt-0.5">
                  {t("settings.agentRuns.acp.permission.waiting")}
                </p>
              </div>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              {req.options.map((option) => (
                <Button
                  key={option.option_id}
                  type="button"
                  variant={variantFor(option.kind)}
                  size="sm"
                  disabled={busy}
                  onClick={() => void respond(req.requestId, option)}
                >
                  {option.name}
                </Button>
              ))}
            </div>
            {hasAlwaysOption && (
              <p className="text-xs text-mid-gray/80 italic">
                {/* The copy must match what the backend will actually do. An
                    "always" is remembered PER TOOL KIND, and only for a kind
                    that distinguishes something: an absent kind or ACP's
                    catch-all `other` is deliberately NOT persisted (see
                    `agent_run::is_persistable_kind`), because Claude Code
                    files every MCP tool under `other` and one click would
                    otherwise pre-authorise all of them. Promising a scope we
                    then refuse to honour is worse than promising none. */}
                {req.alwaysPersists && req.toolKind
                  ? t("settings.agentRuns.acp.permission.alwaysScopeKind", {
                      kind: req.toolKind,
                    })
                  : t("settings.agentRuns.acp.permission.alwaysScopeOnce")}
              </p>
            )}
          </div>
        );
      })}
    </div>
  );
};
