#!/usr/bin/env python3
"""Turn `cargo xtask check --format json` into the step outputs the CI plan job publishes.

    plan-outputs.py REPORT.json EVENT        EVENT is `pull_request` or `merge_group`

Prints `key=value` lines (append them to $GITHUB_OUTPUT). Why a script and not jq in the workflow: it can be run and
tested by hand (tools/tests/plan_outputs.sh), and the policy lives in one readable place.

Policy. A pull request runs no tests: the session that wrote it runs `cargo xtask check --affected` on its own machine
and attests it (tools/ci/self-gate.py). It lints the crates it changed plus their direct dependents and runs the link
gates. The merge queue tests and lints everything affected, direct or not, in shards, and cross-builds it for Windows;
it builds the exact commit that will land, so it is the authoritative run.
"""

import json
import sys


def shard_matrix(shards):
    """The job matrix for a list of shards: one entry per shard.

    A shard item is a package name, or `name@k/n` for partition k of n of a package too heavy for one job. `crates`
    joins the whole packages with spaces; `parts` joins the partitioned ones (the test job runs those with
    `cargo nextest run --partition hash:k/n`).
    """
    include = []
    for index, items in enumerate(shards):
        whole = [item for item in items if "@" not in item]
        parts = [item for item in items if "@" in item]
        include.append({"name": str(index + 1), "crates": " ".join(whole), "parts": " ".join(parts)})
    return {"include": include}


def outputs(report, event):
    """The outputs for `report` (the xtask JSON) on `event`, as an ordered dict of strings."""
    queue = event != "pull_request"
    shards = report["shards_full"] if queue else []
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
        # The CLI's product configuration (`omuzaki-stage/wgpu`) is link-gated whenever the CLI is in scope at all: in
        # full scope its shard tests run default features only, so nothing else checks that configuration.
        "cli_in_scope": str("omuzaki-cli" in report["full"] or "omuzaki-cli" in report["link_only"]).lower(),
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
