#!/usr/bin/env bash
# Measure the spec §10 budgets and write a markdown and a JSON report (G-BUD). Runnable on its own:
#
#   tools/ci/budget-report.sh [--out DIR] [--previous report.json] [--class CLASS] [--strict]
#
# --out       folder for report.md and report.json (default: target/budget)
# --previous  last night's report.json, for the regression check (more than 10% worse is a regression)
# --class     what the numbers are, in the spec's §10 sense: reported (default), relative, measured or scaled.
# --strict    exit non-zero on the findings the class gates on (relative: a budgeted metric more than 10% worse than
#             --previous; an error when no budgeted metric was compared or the session differs). Without it the run only
#             reports. Numbers of class `reported` (any cloud runner) never gate, so --strict with the default class
#             cannot fail on a slow machine (§10).
#
# Environment (the provenance a gating class states; the report prints it):
#   OMUZAKI_BUDGET_TIER, _OS_BUILD, _GPU, _BACKEND   required by --class measured and scaled
#   OMUZAKI_BUDGET_SCALING_RULE                      also required by --class scaled
#   OMUZAKI_BUDGET_SESSION                           required by --class relative --strict, equal in --previous
#
# The nightly step calls this script with --previous pointing at the artifact of the last green night.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
out="$ROOT/target/budget"
previous=""
strict=0
class=reported

while [[ $# -gt 0 ]]; do
  case $1 in
    --out) out=${2:?--out needs a folder}; shift 2 ;;
    --previous) previous=${2:?--previous needs a file}; shift 2 ;;
    --class) class=${2:?--class needs a name}; shift 2 ;;
    --strict) strict=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ -n $previous && ! -f $previous ]]; then
  echo "previous report not found: $previous" >&2
  exit 2
fi

# The test runs from the crate folder, so hand it absolute paths.
out=$(realpath -m "$out")
[[ -z $previous ]] || previous=$(realpath "$previous")
cd "$ROOT"
OMUZAKI_BUDGET_OUT=$out \
OMUZAKI_BUDGET_PREVIOUS=$previous \
OMUZAKI_BUDGET_CLASS=$class \
OMUZAKI_BUDGET_STRICT=$strict \
  timeout 1800 cargo test --locked -p omuzaki-test --lib --release budget::runner::tests::nightly_from_env \
  -- --ignored --exact --nocapture
echo "report: $out/report.md, $out/report.json"
