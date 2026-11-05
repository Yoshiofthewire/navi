#!/usr/bin/env bash
# Development only: bake the password guidance; no font parsing at lock time.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
font=${1:-/usr/share/fonts/noto/NotoSans-Regular.ttf}
mkdir -p assets/lock
magick -size 2240x1400 xc:none -font "$font" -gravity North \
  -fill '#D0D4DC' -pointsize 36 -annotate +0+476 'Session locked' \
  -fill '#D0D4DC' -pointsize 44 -annotate +0+932 'Type your password' \
  -fill '#D0D4DC' -pointsize 32 -annotate +0+1012 'Press Enter to unlock' \
  -depth 8 -strip -define png:exclude-chunk=time PNG32:assets/lock/prompts.png
