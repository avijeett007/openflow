#!/bin/zsh
# chain-step.sh <assets-dir> <clip-name> <start-image> <prompt> [prev-last-png] [resolution]
#
# Generates ONE Higgsfield Seedance clip chained from <start-image>, waits, downloads,
# extracts its first + last frame, and SSIM-gates the junction against <prev-last-png>.
# Mechanical lane — no model. Requires: higgsfield CLI (logged in), ffmpeg, python3, curl.
#
#   resolution defaults to 1080p. Use 480p for cheap draft passes (mode auto-switches to fast).
set -e -o pipefail
DIR=$1; NAME=$2; START=$3; PROMPT=$4; PREV=$5; RES=${6:-1080p}
if [[ -z "$DIR" || -z "$NAME" || -z "$START" || -z "$PROMPT" ]]; then
  echo "usage: chain-step.sh <assets-dir> <clip-name> <start-image> <prompt> [prev-last-png] [resolution]"; exit 1
fi
[[ -r "$START" ]] || { echo "start image not readable: $START"; exit 1; }
for bin in higgsfield ffmpeg python3 curl; do command -v $bin >/dev/null || { echo "missing dependency: $bin"; exit 1; }; done
MODE=std; [[ "$RES" == "480p" || "$RES" == "720p" ]] && MODE=fast
mkdir -p "$DIR"

echo "[$NAME] creating job ($RES/$MODE, audio off)..."
CREATE=$(higgsfield generate create seedance_2_0 \
  --prompt "$PROMPT" \
  --start-image "$START" \
  --duration 5 --resolution "$RES" --mode "$MODE" --generate-audio false \
  --json 2>&1)
ID=$(echo "$CREATE" | python3 -c "import sys,re;s=sys.stdin.read();m=re.findall(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}',s);print(m[0] if m else '')")
if [[ -z "$ID" ]]; then
  echo "[$NAME] FAILED — could not parse a job id from create output (not guessing from job list;"
  echo "[$NAME] that could attach to an unrelated job). Raw output tail:"
  echo "$CREATE" | tail -5; exit 1
fi
echo "[$NAME] job $ID — waiting..."

WAIT=$(higgsfield generate wait "$ID" --timeout 15m --interval 5s --json 2>&1)
URL=$(echo "$WAIT" | python3 -c "import sys,re;s=sys.stdin.read();m=re.findall(r'https://[^\"\s]+\.mp4[^\"\s]*',s);print(m[-1] if m else '')")
if [[ -z "$URL" ]]; then
  echo "[$NAME] FAILED — no mp4 url (server-side failures are unbilled; just retry)."
  echo "$WAIT" | tail -5; exit 1
fi

curl -fsSL -o "$DIR/$NAME.mp4" "$URL"
ffmpeg -y -v error -i "$DIR/$NAME.mp4" -vf "select=eq(n\,0)" -frames:v 1 -update 1 -q:v 1 "$DIR/$NAME-first.png"
ffmpeg -y -v error -sseof -0.05 -i "$DIR/$NAME.mp4" -update 1 -q:v 1 "$DIR/$NAME-last.png"
echo "[$NAME] downloaded: $(ffprobe -v error -select_streams v -show_entries stream=width,height,nb_frames -of csv=p=0 "$DIR/$NAME.mp4")"

if [[ -n "$PREV" ]]; then
  SSIM=$( (ffmpeg -i "$PREV" -i "$DIR/$NAME-first.png" -lavfi ssim -f null - 2>&1 || true) | grep -o 'All:[0-9.]*' | cut -d: -f2)
  ffmpeg -y -v error -i "$PREV" -i "$DIR/$NAME-first.png" -filter_complex "[0][1]hstack" "$DIR/$NAME-junction-compare.jpg" || true
  if [[ -z "$SSIM" ]]; then echo "[$NAME] JUNCTION SSIM could not be computed — inspect $NAME-junction-compare.jpg"; exit 2; fi
  echo "[$NAME] JUNCTION SSIM vs $(basename $PREV): $SSIM   (side-by-side: $NAME-junction-compare.jpg)"
  PASS=$(python3 -c "print(1 if float('$SSIM') >= 0.80 else 0)")
  if [[ "$PASS" == "0" ]]; then
    echo "[$NAME] JUNCTION REVIEW REQUIRED (<0.80). SSIM under-reads on stochastic texture (clouds/"
    echo "[$NAME] particles/caustics can be seamless at 0.6) — inspect the side-by-side. A structural"
    echo "[$NAME] change (new horizon/objects/grade) is a real fail: regenerate with 'Continue the exact"
    echo "[$NAME] same shot from the reference frame, identical framing, identical colour grade. Do not"
    echo "[$NAME] change the colour grade.'"
    exit 2   # exit 2 = downloaded fine, junction needs a human/model eye before proceeding
  fi
fi
echo "[$NAME] DONE"
