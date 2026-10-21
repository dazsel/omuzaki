#!/usr/bin/env python3
"""The `gate` job's verdict: pass when every job it waits for passed or was legitimately skipped.

    NEEDS=<toJSON(needs)> gate-aggregate.py

Exit 0 to pass, 1 to fail. A skipped job passes (a draft, a docs-only change, a job that runs only on `main`), a failed
or cancelled one does not, and lint must have run once plan succeeded. `gate` is a summary for the agent that landed the
change, not a required check (ADR 0067). It lives in a script so tests/gate_aggregate.sh can run every combination
(tools/tests).
"""

import json
import os
import sys


def failures(needs):
    """The sorted names of what makes the gate fail; empty when it passes."""
    results = {name: job["result"] for name, job in needs.items()}
    bad = [name for name, result in results.items() if result not in ("success", "skipped")]
    if results.get("plan") == "success" and results.get("lint") != "success":
        bad.append("lint")
    return sorted(set(bad))


def main():
    needs = json.loads(os.environ["NEEDS"])
    for name, job in needs.items():
        print(f"{name}: {job['result']}")
    bad = failures(needs)
    if bad:
        print("gate failed: " + ", ".join(bad))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
