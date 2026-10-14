#!/usr/bin/env bash
# Prints the face parity table (Rust painter against the JS prototype's isolated face sections) and writes it with
# a reference / painter / diff picture per compared case to the assets folder ($OMUZAKI_ASSETS, default assets-local). The pictures are of the sample character.
#
#   tools/face-parity-report.sh [output-dir]       # default is $OMUZAKI_ASSETS/goldens/rust/face
#
# Sections without an enforcement test (crates/omuzaki-test/tests/face_parity_<section>.rs) only print here.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=${1:-${OMUZAKI_ASSETS:-assets-local}/goldens/rust/face}
OMUZAKI_FACE_PARITY_OUT=$OUT timeout 1200 cargo test -p omuzaki-test --lib -- --ignored --nocapture face_parity::sections::tests::write_the_report
echo "report: $OUT/report.md"
