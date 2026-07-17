#!/usr/bin/env bash
# PR-budget breaker for the maintenance crew. Exits 0 if the number of OPEN PRs
# labelled `agent-pr` is under the cap, non-zero if the budget is exhausted.
#
# Phase 2's fix-dispatch workflow calls this and only grants PR-creation tools when
# it returns 0 — so the cap is ENFORCED (a prompt-injected issue cannot make the
# agent open a PR past the limit), not merely requested.
#
#   ./scripts/agents/pr-budget.sh [--repo owner/name] [--cap N]
set -euo pipefail

REPO="avijeett007/openflow"
CAP="${AGENT_PR_BUDGET:-5}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) REPO="$2"; shift 2 ;;
    --cap)  CAP="$2";  shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

open_agent_prs="$(gh pr list --repo "$REPO" --state open --label agent-pr --limit 100 --json number --jq 'length')"

if (( open_agent_prs < CAP )); then
  echo "PR budget OK: ${open_agent_prs}/${CAP} open agent PRs"
  exit 0
else
  echo "PR budget EXHAUSTED: ${open_agent_prs}/${CAP} open agent PRs"
  exit 1
fi
