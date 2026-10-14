#!/usr/bin/env bash
# Runs the Spout loopback matrix the spike's report quotes: each (path, sync, size, rate) in two processes, with a timeout.
# Build first:  cargo build --release -p spout-out --target x86_64-pc-windows-gnu   (from spikes/broadcast-out)
# Run from WSL (Windows exes start through interop):  tools/bench-spout.sh [path-to-spout-out.exe] > results.txt
set -uo pipefail

exe=${1:-"$(cd "$(dirname "$0")/../../.." && pwd)/target/x86_64-pc-windows-gnu/release/spout-out.exe"}

run() {
  echo "=== spout-out loopback $*"
  timeout 240 "$exe" loopback "$@" 2>&1 | grep -E 'sender "|Spout hand-off|received |notices|pixels in receiver RAM|interval between|reads |compared bit'
}

run --path gpu --sync none  --fps 60 --frames 1000
run --path gpu --sync fence --fps 60 --frames 1000
run --path cpu --sync none  --fps 60 --frames 1000
run --path cpu --sync fence --fps 60 --frames 1000
run --path gpu --sync fence --fps 60 --frames 600 --size 3840x2160
run --path gpu --sync fence --fps 0  --frames 3000

# OBS-style receiver: reads the shared texture on its own clock with no sync event and no mutex (what OBS's Spout source
# does). 1000 reads per second is the worst case for tear exposure; 60 per second is what OBS does at 60 fps.
run --path gpu --sync none  --fps 60 --frames 1200 --receiver raw --poll-hz 1000
run --path gpu --sync fence --fps 60 --frames 1200 --receiver raw --poll-hz 1000
run --path cpu --sync fence --fps 60 --frames 1200 --receiver raw --poll-hz 1000
run --path gpu --sync fence --fps 60 --frames 1200 --receiver raw --poll-hz 60
