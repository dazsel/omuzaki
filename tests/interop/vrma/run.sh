#!/usr/bin/env bash
# The VRMA conformance check (#1277): writes the fixtures from the kit's own character, then plays them in three-vrm in
# headless Chromium and compares with Omuzaki's own reading (`check.mjs`). Part of the nightly matrix; by hand:
#
#   tests/interop/vrma/run.sh [FIXTURES_DIR]
#
# Needs node and npm. Uses the Chromium in OMUZAKI_CHROMIUM or PLAYWRIGHT_BROWSERS_PATH, else installs Playwright's.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
OUT=${1:-$(mktemp -d)}

(cd "$ROOT" && cargo run --locked -q -p omuzaki-stage --example vrma_interop -- "$OUT")
cd "$HERE"
PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm ci --no-audit --no-fund --loglevel=error
if [[ -z ${OMUZAKI_CHROMIUM:-} && -z ${PLAYWRIGHT_BROWSERS_PATH:-} ]]; then
  npx playwright install chromium
fi
node check.mjs "$OUT"
