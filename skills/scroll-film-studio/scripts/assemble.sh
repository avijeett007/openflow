#!/bin/zsh
# assemble.sh <assets-dir> <frames-out-dir> <clip1> <clip2> ...  (clip names, no .mp4, in order)
#
# Concats the chained clips (dropping the duplicate junction frame on clips 2+), encodes the
# master with -fps_mode vfr (CFR padding causes frozen scrub zones), extracts ~300 JPEG frames
# at 1280px for the canvas scrubber, and prints the final-frame seam colour for the handoff.
# Mechanical lane — no model. Requires: ffmpeg, xxd.
set -e -o pipefail
setopt null_glob 2>/dev/null || true
A=$1; F=$2; shift 2
if [[ -z "$A" || -z "$F" || $# -lt 2 ]]; then
  echo "usage: assemble.sh <assets-dir> <frames-out-dir> <clip1> <clip2> ... (>=2 clips)"; exit 1
fi
for CLIP in "$@"; do [[ -r "$A/$CLIP.mp4" ]] || { echo "missing clip: $A/$CLIP.mp4"; exit 1; }; done
mkdir -p "$F"

INPUTS=(); FILTER=""; N=0
for CLIP in "$@"; do
  INPUTS+=(-i "$A/$CLIP.mp4")
  if (( N == 0 )); then FILTER+="[${N}:v]setpts=PTS-STARTPTS[v${N}];"
  else FILTER+="[${N}:v]select='gte(n\\,1)',setpts=PTS-STARTPTS[v${N}];"; fi
  N=$((N+1))
done
CONCAT=""; for ((i=0;i<N;i++)); do CONCAT+="[v${i}]"; done
FILTER+="${CONCAT}concat=n=${N}:v=1:a=0[out]"

ffmpeg -y -v error "${INPUTS[@]}" -filter_complex "$FILTER" -map "[out]" \
  -fps_mode vfr -c:v libx264 -crf 16 -preset slow -pix_fmt yuv420p "$A/master.mp4"
echo "master: $(ffprobe -v error -select_streams v -show_entries stream=width,height,nb_frames -of csv=p=0 "$A/master.mp4")"

rm -f "$F"/f_*.jpg
ffmpeg -v error -i "$A/master.mp4" -vf "select='not(mod(n\\,2))',scale=1280:-2" -vsync vfr -q:v 4 "$F/f_%04d.jpg"
FRAMES=("$F"/f_*.jpg)
COUNT=${#FRAMES[@]}
if (( COUNT == 0 )); then echo "FAILED — no frames were extracted"; exit 1; fi
echo "frames: $COUNT at 1280w, $(du -sh "$F" | cut -f1)  ->  set FRAME_COUNT=$COUNT in the engine"

LAST=${FRAMES[-1]}
SEAM=$(ffmpeg -v error -i "$LAST" -vf "crop=iw:ih*0.12:0:ih*0.88,scale=1:1" -frames:v 1 -f rawvideo -pix_fmt rgb24 - | xxd -p | cut -c1-6)
if [[ -z "$SEAM" ]]; then echo "warning: seam colour could not be sampled — sample $LAST manually"; else
echo "seam colour of $(basename $LAST): #$SEAM   (start the after-film section background here)"; fi
