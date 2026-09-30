#!/usr/bin/env bash
# Renders the Sippy motion clip to assets/sippy/motion/.
# Needs node, rsvg-convert, ffmpeg and the Caprasimo font.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
frames="$root/target/sippy-motion/frames"
out="$root/assets/sippy/motion"

rm -rf "$frames"
mkdir -p "$frames" "$out"

node "$root/scripts/render-sippy-motion.mjs" "$frames"
find "$frames" -name '*.svg' -print0 | xargs -0 -P "$(nproc)" -I{} sh -c 'rsvg-convert "$1" -o "${1%.svg}.png"' _ {}

ffmpeg -y -loglevel error -framerate 60 -i "$frames/%04d.png" \
  -c:v libx264 -preset slow -crf 18 -pix_fmt yuv420p -movflags +faststart \
  "$out/sippy-motion.mp4"

ffmpeg -y -loglevel error -framerate 60 -i "$frames/%04d.png" \
  -vf "fps=30,scale=800:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=96[p];[b][p]paletteuse=dither=bayer:bayer_scale=4" \
  "$out/sippy-motion.gif"

ls -lh "$out"
