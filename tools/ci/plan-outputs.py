#!/usr/bin/env python3
"""Turn `cargo xtask check --format json` into the step outputs the CI plan job publishes.

    plan-outputs.py REPORT.json EVENT        EVENT is `pull_request` or `merge_group`

Prints `key=value` lines (append them to $GITHUB_OUTPUT). Why a script and not jq in the workflow: it can be run and
tested by hand (tools/tests/plan_outputs.sh), and the policy lives in one readable place.

Policy. A pull request tests only the crates it changed and lints those plus their direct dependents; a PR that widens
the scope to the whole workspace (a lock file or toolchain change) tests everything, sharded. The merge queue tests and
lints everything affected, direct or not, and cross-builds it for Windows.
"""

import json
import sys


def shard_matrix(shards):
    """The job matrix for a list of crate lists: one entry per shard, crates joined with spaces."""
    return {"include": [{"name": str(index + 1), "crates": " ".join(crates)} for index, crates in enumerate(shards)]}


def outputs(report, event):
    """The outputs for `report` (the xtask JSON) on `event`, as an ordered dict of strings."""
    queue = event != "pull_request"
    shards = report["shards_full"] if queue else report["shards_changed"]
    lint = sorted(set(report["full"] + report["link_only"])) if queue else report["direct"]
    return {
        "mode": report["mode"],
        "empty": str(report["empty"]).lower(),
        "has_tests": str(bool(shards)).lower(),
        "test_matrix": json.dumps(shard_matrix(shards), separators=(",", ":")),
        "lint_crates": " ".join(lint),
        # The link gate tests (ADR 0015) protect the Apps that wire crates together; they run on PRs too.
        "link_crates": " ".join(report["link_only"]),
        "has_link_gates": str(bool(report["link_only"])).lower(),
        # Everything affected is cross-built, so a crate reached only through a link-only dependency still builds.
        "cross_crates": " ".join(sorted(set(report["full"] + report["link_only"]))),
        "has_cross": str(bool(report["full"] or report["link_only"])).lower(),
    }


def main(argv):
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    with open(argv[1], encoding="utf-8") as handle:
        report = json.load(handle)
    for key, value in outputs(report, argv[2]).items():
        print(f"{key}={value}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
