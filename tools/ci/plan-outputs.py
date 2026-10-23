#!/usr/bin/env python3
"""Turn `cargo xtask check --format json` into the step outputs the CI plan job publishes.

    plan-outputs.py REPORT.json

Prints `key=value` lines (append them to $GITHUB_OUTPUT). Why a script and not jq in the workflow: it can be run and
tested by hand (tools/tests/plan_outputs.sh), and the policy lives in one readable place.

Policy (ADR 0067, spine first). Every run tests and lints everything the change affects, in shards, on GitHub's
runners: a pull request, a push to `main` and a manual dispatch alike. No run is a required gate; the results inform
the agent that landed the change, and a red run on `main` is the first thing its author fixes.
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


def outputs(report):
    """The outputs for `report` (the xtask JSON), as an ordered dict of strings."""
    shards = report["shards_full"]
    lint = sorted(set(report["full"] + report["link_only"]))
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
        # The workspace-scan test targets (xtask/src/scans.rs) read every crate, so a change anywhere can break them:
        # they run on every change, an empty scope included (#2139, #2163). An older report without the field has none.
        "scan_tests": " ".join(f"{scan['krate']}:{scan['test']}" for scan in report.get("scans", [])),
        "has_scans": str(bool(report.get("scans"))).lower(),
    }


def main(argv):
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    with open(argv[1], encoding="utf-8") as handle:
        report = json.load(handle)
    for key, value in outputs(report).items():
        print(f"{key}={value}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
