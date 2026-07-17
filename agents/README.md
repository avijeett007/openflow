# OpenFlow maintenance crew

A small set of Claude Code agents that keep this repo healthy — reviewing PRs,
triaging issues, patrolling for security/dependency/CI problems, and (Phase 2)
opening opt-in fix PRs. It is deliberately **GitHub-native** (no external PM/chat
tools) and deliberately **cannot ship anything on its own**: `main` is protected
and a `v*` tag publishes an OTA release to every user, so a human always merges and
always tags.

The agents obey [`AGENTS.md`](../AGENTS.md) — above all the **non-breaking
principle**: new work is additive, the core dictation loop is never regressed, and
every change carries a regression argument.

## The crew (Phase 1 — live)

| Agent              | Persona                                                                           | Workflow                       | Trigger                  | Output                                                                         |
| ------------------ | --------------------------------------------------------------------------------- | ------------------------------ | ------------------------ | ------------------------------------------------------------------------------ |
| PR reviewer        | [`.claude/agents/pr-reviewer.md`](../.claude/agents/pr-reviewer.md)               | `agent-pr-review.yml`          | every non-draft PR       | one sticky review + `review:*`, `merge-tier:*`, `area:*` labels. Never merges. |
| Maintenance patrol | [`.claude/agents/maintenance-patrol.md`](../.claude/agents/maintenance-patrol.md) | `agent-maintenance-patrol.yml` | weekly (Mon 07:00 UTC)   | rolling digest issue + a GitHub issue per new finding. Never fixes.            |
| Issue triage       | [`.claude/agents/issue-triage.md`](../.claude/agents/issue-triage.md)             | `agent-issue-triage.yml`       | issue opened             | `type:/area:/priority:` labels + repro request. Never codes.                   |
| @claude            | —                                                                                 | `agent-mention.yml`            | `@claude` in an issue/PR | interactive help on demand.                                                    |
| Dependabot         | —                                                                                 | `.github/dependabot.yml`       | weekly                   | cargo + npm + actions update PRs → reviewed by the PR reviewer.                |
| Dependency scan    | —                                                                                 | `dependency-scan.yml`          | PR + weekly              | deterministic osv-scanner gate (no LLM).                                       |

## Phase 2 (planned, not yet built)

- **fix-dispatch** — on the founder adding an `agent:fix` label to an issue, a fix
  agent investigates and opens ONE budget-capped PR under the non-breaking
  principle. Opt-in only; never auto-triggered, never auto-merged. The
  [`scripts/agents/pr-budget.sh`](../scripts/agents/pr-budget.sh) breaker and the
  `agent:fix` label already exist so this drops in cleanly.
- **docs-writer** — when a user-facing PR merges, update the openflow.computer docs
  via a PR.

## Safety rails

- **Kill switch.** Create a file named `agents/KILL-SWITCH` (any content) and every
  agent workflow halts on its next run. Delete it to resume.
  ```bash
  touch agents/KILL-SWITCH && git add agents/KILL-SWITCH && git commit -m "chore: halt agents" && git push
  ```
- **Token gate.** Every agent workflow no-ops cleanly unless the repo secret
  `CLAUDE_CODE_OAUTH_TOKEN` is set — so merging this crew before the token exists is
  safe (jobs skip, nothing errors).
- **PR budget.** `scripts/agents/pr-budget.sh` caps concurrent open `agent-pr` PRs
  (default 5). Phase 2's fix agent is only granted PR-creation tools when the budget
  is OK, so the cap holds even against a prompt-injected issue.
- **No auto-merge.** Nothing in this crew merges. Ever.

## Prerequisites

1. Repo secret **`CLAUDE_CODE_OAUTH_TOKEN`** (generate with `claude setup-token`).
   Without it the agent workflows skip.
2. **Dependabot alerts** enabled (Settings → Code security) so the patrol's
   security lens can read `dependabot/alerts`.
3. Labels created — run [`scripts/agents/setup-labels.sh`](../scripts/agents/setup-labels.sh) once.
