#!/usr/bin/env bash
# Fetches the pinned Spout2 SDK source (BSD-2-Clause, https://github.com/leadedge/Spout2) into third_party/Spout2, where
# spout-out/build.rs looks for it (or set SPOUT2_DIR). The source is not vendored: it stays out of git, and the pin below
# is what we tested. Run once per checkout.
set -euo pipefail

PIN=6b68a5b4fdeca6130b851e5f9366467a844860e5   # "Version 2.007.018", 2026-10-10
dest=${1:-"$(cd "$(dirname "$0")/.." && pwd)/third_party/Spout2"}

if [[ -d $dest/.git ]] && [[ $(git -C "$dest" rev-parse HEAD) == "$PIN" ]]; then
  echo "Spout2 already at $PIN in $dest"
  exit 0
fi
mkdir -p "$dest"
git -C "$dest" init -q
git -C "$dest" remote add origin https://github.com/leadedge/Spout2.git 2>/dev/null || true
timeout 300 git -C "$dest" fetch -q --depth 1 origin "$PIN"
git -C "$dest" checkout -q FETCH_HEAD
echo "Spout2 $(git -C "$dest" rev-parse --short HEAD) checked out in $dest"
