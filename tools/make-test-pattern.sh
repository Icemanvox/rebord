#!/bin/sh
# Mire HEVC de 3 s pour le mode --synthetic : 1280×720, 30 i/s, sans images B,
# VPS/SPS/PPS répétés, une seule image clé en tête, Main 8 bits 4:2:0.
set -e
cd "$(dirname "$0")/.."
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=3" \
  -vf "drawtext=text='Rebord %{n}':fontsize=64:fontcolor=white:box=1:boxcolor=black@0.6:x=40:y=40" \
  -pix_fmt yuv420p -c:v libx265 -preset medium -crf 32 \
  -x265-params "bframes=0:keyint=-1:min-keyint=90:scenecut=0:repeat-headers=1:info=0:aud=0:log-level=error" \
  -f hevc assets/test-pattern.hevc
ls -l assets/test-pattern.hevc
