#!/usr/bin/env bash
# Create (idempotently) the GitHub labels the maintenance crew applies. Safe to
# re-run — existing labels are updated to the canonical colour/description.
#
#   ./scripts/agents/setup-labels.sh [--repo owner/name]
#
# Defaults to avijeett007/openflow. Requires `gh` authenticated with repo scope.
set -euo pipefail

REPO="avijeett007/openflow"
if [[ "${1:-}" == "--repo" && -n "${2:-}" ]]; then
  REPO="$2"
fi

# label|color|description
LABELS=(
  "type:bug|d73a4a|A defect in existing behaviour"
  "type:feature|a2eeef|A new capability or enhancement"
  "type:question|d876e3|A question or support request"
  "type:security|b60205|A vulnerability or privacy regression"
  "type:deprecation|e99695|An upstream deprecation to act on"

  "area:rust|dea584|Rust backend / STT / audio (src-tauri)"
  "area:ui|1d76db|React/TypeScript frontend (src)"
  "area:meetings|0e8a16|Meeting capture / diarization"
  "area:updater|5319e7|OTA updates / signing"
  "area:ci|bfd4f2|Build / release / workflows"
  "area:docs|c5def5|openflow.computer documentation"
  "area:agents|fef2c0|The maintenance crew itself"

  "priority:critical|b60205|Data loss, core-path crash, or security/privacy"
  "priority:high|d93f0b|Core feature broken, no workaround"
  "priority:medium|fbca04|Real issue with a workaround"
  "priority:low|0e8a16|Cosmetic / edge-case / nice-to-have"

  "review:approved|0e8a16|Reviewer agent approved (a human still merges)"
  "review:changes-requested|d93f0b|Reviewer agent requested changes"
  "merge-tier:0|c2e0c6|Trivial (docs/tests/patch dep)"
  "merge-tier:1|fbca04|Standard change — one-click founder merge"
  "merge-tier:2|d73a4a|Sensitive — full founder review required"

  "agent-pr|5319e7|PR authored by an agent or Dependabot"
  "agent:fix|0052cc|Founder opt-in: authorize a fix PR for this issue"
  "source:patrol|fef2c0|Filed by the maintenance patrol"
  "needs-founder|e99695|Waiting on the founder"
  "needs-repro|fbca04|Bug report missing a reproduction"
  "skip-agent-review|ededed|Opt this PR out of agent review"
  "deps|0366d6|Dependency update"
  "duplicate|cfd3d7|Duplicate of another issue/PR"
)

for entry in "${LABELS[@]}"; do
  IFS='|' read -r name color desc <<<"$entry"
  if gh label create "$name" --repo "$REPO" --color "$color" --description "$desc" 2>/dev/null; then
    echo "created  $name"
  else
    gh label edit "$name" --repo "$REPO" --color "$color" --description "$desc" >/dev/null
    echo "updated  $name"
  fi
done
echo "Done. Labels ready on $REPO."
