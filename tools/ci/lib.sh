#!/usr/bin/env bash
# Shared helpers for the CI scripts (pr-check.sh, nightly-check.sh). Source this file; don't run it.
#
# Why stages instead of `set -e`: one CI run should report everything that is red (fmt AND clippy AND the Windows build),
# not stop at the first failure and make the author fix and re-push three times. Each stage is timed, its log is grouped
# in the Actions UI, and a one-screen summary (with the tail of every failure) goes to the job summary page.
#
# shellcheck shell=bash

CI_TITLE=""
CI_NOTES=()       # "key: value" lines shown under the summary heading
CI_NAMES=()       # one entry per stage, in run order
CI_RESULTS=()     # pass | FAIL | skipped
CI_DETAILS=()     # seconds taken, or the reason a stage was skipped
CI_TAILS=()       # last lines of a failed stage's output (empty otherwise)
CI_CURRENT=""     # stage in progress, so an aborted run can say where it stopped
CI_FAILED=0
CI_SUMMARY_DONE=0
CI_START=$SECONDS
CI_LOG_DIR=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/omuzaki-ci.XXXXXX")

# Start a run: remember the title, make signals run the EXIT trap (so the summary is written even when a PR is
# superseded and the runner cancels the job), and write the summary on the way out.
ci_begin() {
  CI_TITLE=$1
  trap 'exit 143' TERM
  trap 'exit 130' INT
  trap ci_summary EXIT
}

# ci_note KEY VALUE: a line of context under the summary heading (mode, base, slot).
ci_note() { CI_NOTES+=("$1: $2"); }

# stage NAME COMMAND...: run COMMAND, show its output in a collapsible group, record pass/FAIL and the time taken.
stage() {
  local name=$1 log rc start=$SECONDS
  shift
  log="$CI_LOG_DIR/stage-${#CI_NAMES[@]}.log"
  CI_CURRENT=$name
  echo "::group::$name"
  echo "+ $*"
  "$@" 2>&1 | tee "$log"
  rc=${PIPESTATUS[0]}
  echo "::endgroup::"
  CI_CURRENT=""
  CI_NAMES+=("$name")
  CI_DETAILS+=("$((SECONDS - start))s")
  if ((rc == 0)); then
    CI_RESULTS+=("pass")
    CI_TAILS+=("")
  else
    CI_FAILED=1
    CI_RESULTS+=("FAIL")
    CI_TAILS+=("$(ci_strip_ansi <"$log" | tail -n 40 | cut -c1-240)")
    echo "::error title=$name failed::exit status $rc after $((SECONDS - start))s (see the '$name' group above)"
  fi
}

# skip NAME REASON: record a stage that had nothing to do (shown in the summary so a skip is never silent).
skip() {
  CI_NAMES+=("$1")
  CI_RESULTS+=("skipped")
  CI_DETAILS+=("$2")
  CI_TAILS+=("")
}

ci_strip_ansi() { sed -e 's/\x1b\[[0-9;]*[A-Za-z]//g'; }

# The run's verdict as the exit code; the EXIT trap prints the summary.
ci_finish() { exit "$CI_FAILED"; }

# Write the markdown summary to the job summary page (stdout when run by hand). Runs once, from the EXIT trap.
ci_summary() {
  ((CI_SUMMARY_DONE)) && return
  CI_SUMMARY_DONE=1
  local i verdict out=${GITHUB_STEP_SUMMARY:-/dev/stdout}
  if [[ -n $CI_CURRENT ]]; then
    CI_FAILED=1
    CI_NAMES+=("$CI_CURRENT")
    CI_RESULTS+=("aborted")
    CI_DETAILS+=("cancelled or killed after $((SECONDS - CI_START))s in total")
    CI_TAILS+=("")
  fi
  ((CI_FAILED)) && verdict=FAILED || verdict=passed
  {
    echo "## $CI_TITLE: $verdict"
    echo
    for i in "${!CI_NOTES[@]}"; do echo "- ${CI_NOTES[i]}"; done
    echo "- Total: $((SECONDS - CI_START))s"
    echo
    echo "| Stage | Result | Time or reason |"
    echo "|---|---|---|"
    for i in "${!CI_NAMES[@]}"; do echo "| ${CI_NAMES[i]} | ${CI_RESULTS[i]} | ${CI_DETAILS[i]} |"; done
    for i in "${!CI_NAMES[@]}"; do
      [[ -n ${CI_TAILS[i]} ]] || continue
      echo
      echo "<details open><summary>${CI_NAMES[i]}: last lines of output</summary>"
      echo
      echo '```text'
      echo "${CI_TAILS[i]}"
      echo '```'
      echo "</details>"
    done
  } >>"$out"
  rm -rf "$CI_LOG_DIR"
}

# xtask_affected BASE: print the crates affected since BASE, one per line (nothing when none). Fails while xtask has no
# `check` command or is broken, which is how the scripts decide between xtask and whole-workspace mode.
# Why probe with the real command: any other "is xtask ready" test (a --help flag, a file name) can drift from it.
xtask_affected() { cargo xtask check --affected --list --base "$1" 2>/dev/null; }

# Fails when Cargo.lock needs changes, i.e. someone edited a manifest without committing the lockfile. Every later stage
# also passes --locked; this names the problem up front instead of as a confusing build error.
lockfile_current() { cargo metadata --locked --format-version 1 >/dev/null; }

# golden_skip_reason: prints why the golden-image check has nothing to check on this runner and succeeds. Fails, printing
# nothing, when the check should run. Two cases skip: the golden store (an external asset store) is not reachable from
# this runner, and no golden manifest has been committed yet, so nothing has been blessed. A store that IS reachable,
# with manifests present, always runs, so a wrong or broken manifest folder fails the PR instead of skipping.
golden_skip_reason() {
  local store=${OMUZAKI_GOLDENS:-} manifests=${OMUZAKI_GOLDEN_MANIFESTS:-tests/goldens}
  if [[ -z $store ]]; then
    echo "OMUZAKI_GOLDENS is not set on this runner, so the golden store is not reachable here"
  elif [[ ! -d $store ]]; then
    echo "the golden store $store is not reachable from this runner (is the assets folder mounted?)"
  elif ! compgen -G "$manifests/*.toml" >/dev/null; then
    echo "no golden manifest in $manifests yet, so no blessed image to check"
  else
    return 1
  fi
}

# golden_verify: every image the manifests name must be in the store and undamaged (`omuzaki-test verify`). Exit 1
# lists each missing or corrupt case by name; exit 2 (a harness problem) fails the PR too. OMUZAKI_TEST_BIN runs a prebuilt
# binary, as the golden-comment step does.
golden_verify() {
  if [[ -n ${OMUZAKI_TEST_BIN:-} ]]; then
    "$OMUZAKI_TEST_BIN" verify
  else
    cargo run --locked -q -p omuzaki-test -- verify
  fi
}
