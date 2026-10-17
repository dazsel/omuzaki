#!/usr/bin/env python3
"""The `gate` job's verdict: pass when every job it waits for passed or was legitimately skipped.

    NEEDS=<toJSON(needs)> EVENT=<event name> HOSTED=<true|false> gate-aggregate.py

Exit 0 to pass, 1 to fail. A skipped job passes (a draft, a docs-only change, a job that only runs in the queue), a
failed or cancelled one does not. On top of that:
  - lint must have run once plan succeeded, unless plan skipped the run because the push only merged main;
  - a pull request must have a successful `self-gate`;
  - a `workflow_dispatch` run is the hosted-tests path: it needs `hosted_tests=true`, a successful plan and, when the plan
    has tests, a successful test job. A skipped test job would pass the generic rule above, so it is checked here.
It lives in a script so tests/gate_aggregate.sh can run every combination (tools/tests).
"""

import json
import os
import sys


def failures(needs, event, hosted):
    """The sorted names of what makes the gate fail; empty when it passes."""
    results = {name: job["result"] for name, job in needs.items()}
    bad = [name for name, result in results.items() if result not in ("success", "skipped")]
    plan = needs["plan"]
    merge_only = plan["outputs"].get("skip") == "true"
    if results.get("plan") == "success" and results.get("lint") != "success" and not merge_only:
        bad.append("lint")
    if results.get("plan") == "success" and event == "pull_request" and results.get("self-gate") != "success":
        bad.append("self-gate")
    if event == "workflow_dispatch":
        if not hosted or results.get("plan") != "success":
            bad.append("hosted_tests")
        elif plan["outputs"].get("has_tests") == "true" and results.get("test") != "success":
            bad.append("test")
    return sorted(set(bad))


def main():
    needs = json.loads(os.environ["NEEDS"])
    for name, job in needs.items():
        print(f"{name}: {job['result']}")
    bad = failures(needs, os.environ["EVENT"], os.environ.get("HOSTED") == "true")
    if bad:
        print("gate failed: " + ", ".join(bad))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
