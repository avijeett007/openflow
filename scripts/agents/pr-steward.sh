#!/usr/bin/env bash
# Nightly LOCAL PR steward for OpenFlow — runs on this Intel Mac (the only machine
# with the real build env). Collects the night's dependency PRs, stacks the safe
# ones onto one integration branch testing after each, and opens ONE integration PR
# left ready for the founder's final check. It NEVER merges, tags, or releases —
# those tools are withheld below, so it cannot ship even if told to.
#
# Drive it with launchd (recommended on macOS — keychain/GUI context is available)
# or cron. See scripts/agents/install-pr-steward.sh.
#
#   ./scripts/agents/pr-steward.sh            # run now
#   STEWARD_MODEL=sonnet ./scripts/agents/pr-steward.sh
set -uo pipefail

# --- Resolve repo root (script lives in <repo>/scripts/agents) ---------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

# --- Toolchain + the Intel build env (OPENFLOW-NOTES.md) ---------------------
# cron/launchd start from a stripped PATH; put cargo/bun/gh/claude on it, and set
# the dynamic-ORT + cmake-policy vars this Intel Mac needs to compile src-tauri.
export PATH="$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:$PATH"
export CMAKE_POLICY_VERSION_MINIMUM="${CMAKE_POLICY_VERSION_MINIMUM:-3.5}"
export ORT_LIB_LOCATION="${ORT_LIB_LOCATION:-/usr/local/opt/onnxruntime/lib}"
export ORT_PREFER_DYNAMIC_LINK="${ORT_PREFER_DYNAMIC_LINK:-1}"

# --- Config ------------------------------------------------------------------
STEWARD_MODEL="${STEWARD_MODEL:-opus}"
STEWARD_BUDGET_USD="${STEWARD_BUDGET_USD:-20}"
STEWARD_MAX_CANDIDATES="${STEWARD_MAX_CANDIDATES:-15}"
export STEWARD_MAX_CANDIDATES
LOG_DIR="${STEWARD_LOG_DIR:-$HOME/.openflow-steward/logs}"
mkdir -p "$LOG_DIR"
STAMP="$(date +%Y-%m-%dT%H-%M-%S)"
LOG="$LOG_DIR/steward-$STAMP.log"

log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG"; }

# --- Kill switch -------------------------------------------------------------
if [[ -f agents/KILL-SWITCH || -f agents/KILL-STEWARD ]]; then
  log "KILL switch present — steward halted."
  exit 0
fi

# --- Preconditions -----------------------------------------------------------
for bin in claude gh cargo bun git; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    log "FATAL: '$bin' not on PATH — aborting."
    exit 1
  fi
done
if ! gh auth status >/dev/null 2>&1; then
  log "FATAL: gh not authenticated — aborting."
  exit 1
fi

# Start from a clean, up-to-date main so a half-finished previous run can't taint us.
if [[ -n "$(git status --porcelain)" ]]; then
  log "Working tree dirty — stashing before run."
  git stash push -u -m "pr-steward-$STAMP" >>"$LOG" 2>&1 || true
fi
git fetch origin main >>"$LOG" 2>&1
git switch main >>"$LOG" 2>&1 && git reset --hard origin/main >>"$LOG" 2>&1

log "Steward starting (model=$STEWARD_MODEL, budget=\$$STEWARD_BUDGET_USD, max=$STEWARD_MAX_CANDIDATES)"

# --- The prompt --------------------------------------------------------------
PROMPT="You are the pr-steward. Read your full instructions from
.claude/agents/pr-steward.md and follow them EXACTLY, then execute a full run now.

Environment: this is the Intel Mac with the real build env already exported
(CMAKE_POLICY_VERSION_MINIMUM, ORT_LIB_LOCATION, ORT_PREFER_DYNAMIC_LINK). Repo is
avijeett007/openflow. Budget: at most \$STEWARD_MAX_CANDIDATES candidate PRs.

Do the whole flow: collect tonight's dependency/agent PRs, LEVEL them (exclude L2:
majors, core-dictation deps, updater/capabilities/workflows), build one integration
branch by stacking the safe candidates and running cargo test + lint after each
(test-and-drop), run one full 'bun run tauri build' on the converged branch, then
open ONE integration PR labelled needs-founder that is READY FOR FINAL HUMAN CHECK,
trigger build.yml on that branch for 3-platform artifacts, and update the tracker
issue. Do NOT merge, approve, tag, or release anything. Leave the working tree on a
clean main checkout when done. Finish with a concise summary of included/excluded PRs."

# --- Tool policy -------------------------------------------------------------
# Allow: read/inspect, run tests/build/lint, git ops on the integration branch,
# gh PR/issue/label/api, and dispatching build.yml (artifacts only, never publishes).
ALLOW=(
  "Read" "Grep" "Glob"
  "Bash(cd:*)" "Bash(ls:*)" "Bash(cat:*)"
  "Bash(git fetch:*)" "Bash(git switch:*)" "Bash(git checkout:*)" "Bash(git merge:*)"
  "Bash(git reset:*)" "Bash(git branch:*)" "Bash(git status)" "Bash(git status:*)"
  "Bash(git log:*)" "Bash(git diff:*)" "Bash(git push:*)" "Bash(git rev-parse:*)" "Bash(git stash:*)"
  "Bash(cargo test:*)" "Bash(cargo build:*)" "Bash(cargo fmt:*)" "Bash(cargo tree:*)"
  "Bash(bun install)" "Bash(bun run:*)" "Bash(bun x:*)"
  "Bash(gh pr list:*)" "Bash(gh pr view:*)" "Bash(gh pr diff:*)" "Bash(gh pr checks:*)"
  "Bash(gh pr create:*)" "Bash(gh pr edit:*)" "Bash(gh pr comment:*)"
  "Bash(gh issue list:*)" "Bash(gh issue view:*)" "Bash(gh issue create:*)" "Bash(gh issue comment:*)" "Bash(gh issue edit:*)"
  "Bash(gh label:*)" "Bash(gh api:*)" "Bash(gh workflow run build.yml:*)"
)
# Deny (defense in depth): anything that ships or merges. The steward must never
# tag, release, merge, approve, dispatch a non-build workflow, or delete files.
DENY=(
  "Bash(git tag:*)" "Bash(git push origin --tags)" "Bash(gh release:*)"
  "Bash(gh pr merge:*)" "Bash(gh pr review:*)" "Bash(rm:*)"
  "Write" "Edit" "MultiEdit"
)

log "Invoking Claude Code (headless)…"
claude -p "$PROMPT" \
  --model "$STEWARD_MODEL" \
  --allowedTools "${ALLOW[@]}" \
  --disallowedTools "${DENY[@]}" \
  --permission-mode default \
  --max-budget-usd "$STEWARD_BUDGET_USD" \
  --output-format text 2>&1 | tee -a "$LOG"
CODE="${PIPESTATUS[0]}"

# --- Always leave the machine on a clean main -------------------------------
git switch main >>"$LOG" 2>&1 || true
git reset --hard origin/main >>"$LOG" 2>&1 || true

log "Steward finished (exit=$CODE). Log: $LOG"
exit "$CODE"
