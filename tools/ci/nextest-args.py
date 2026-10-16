#!/usr/bin/env python3
"""Turn one shard item of a test job's `parts` into the nextest arguments that run it.

    nextest-args.py ITEM [--list FILE]    prints one argument per line (read them with `mapfile -t`)

FILE is the output of `cargo nextest list -p NAME --list-type binaries-only --message-format json`. With it, binaries
that no longer exist are dropped from the filters, because nextest (0.9.148, checked) refuses a filterset that names a
test binary it cannot find, with `=`, glob or any other matcher, and that would fail every item of the crate if the
isolated binary were renamed. An `NAME@=BIN` item whose binary is gone exits with status 10 and prints nothing: there
is nothing to run, and the partitions already run the renamed binary since they no longer skip anything.

The planner (xtask/src/report.rs) labels a share of a crate that is too heavy for one job:

    NAME@K/N            partition K of N of the crate's tests         -p NAME --partition hash:K/N
    NAME@K/N!BIN,BIN    the same, over the tests outside those test binaries
                                                                       ... -E 'not (binary(=BIN) | binary(=BIN))'
    NAME@=BIN           only the tests of one test binary              -p NAME -E 'binary(=BIN)' --no-tests=pass

A crate's isolated binaries run as `NAME@=BIN` items and its partitions skip them, so together they run every test once.
nextest partitions after it filters, so a test's partition does not depend on which binaries are skipped. The script
lives apart from ci.yml so the mapping is tested, against real nextest when it is installed
(tools/tests/nextest_args.sh). Standard library only.
"""

import json
import re
import sys

NAME = r"[A-Za-z0-9_-]+"
BINARY = r"[A-Za-z0-9_-]+"
PART = re.compile(rf"^(?P<name>{NAME})@(?P<index>[1-9][0-9]*)/(?P<parts>[1-9][0-9]*)(?:!(?P<skip>{BINARY}(?:,{BINARY})*))?$")
ONLY = re.compile(rf"^(?P<name>{NAME})@=(?P<binary>{BINARY})$")


NOTHING_TO_RUN = 10


def nextest_args(item, existing=None):
    """The nextest arguments for `item`; None when the label is not one the planner writes; [] when it is an isolated
    binary that is not among `existing` (the binary names nextest lists; None means do not check)."""
    only = ONLY.match(item)
    if only:
        if existing is not None and only["binary"] not in existing:
            return []
        # The binary exists but may select no tests (they moved, an empty file stayed): nextest exits 4 for that unless told.
        return ["-p", only["name"], "-E", f"binary(={only['binary']})", "--no-tests=pass"]
    part = PART.match(item)
    if not part or int(part["index"]) > int(part["parts"]):
        return None
    args = ["-p", part["name"], "--partition", f"hash:{part['index']}/{part['parts']}"]
    skip = part["skip"].split(",") if part["skip"] else []
    if existing is not None:
        skip = [binary for binary in skip if binary in existing]
    if skip:
        skipped = " | ".join(f"binary(={binary})" for binary in skip)
        args += ["-E", f"not ({skipped})" if len(skip) > 1 else f"not {skipped}"]
    return args


def existing_binaries(path):
    """The binary names in the JSON of `cargo nextest list --list-type binaries-only --message-format json`."""
    with open(path, encoding="utf-8") as handle:
        return {binary["binary-name"] for binary in json.load(handle)["rust-binaries"].values()}


def main(argv):
    if len(argv) not in (2, 4) or (len(argv) == 4 and argv[2] != "--list"):
        print(__doc__, file=sys.stderr)
        return 2
    existing = existing_binaries(argv[3]) if len(argv) == 4 else None
    args = nextest_args(argv[1], existing)
    if args is None:
        print(f"nextest-args.py: not a shard item: {argv[1]!r}", file=sys.stderr)
        return 2
    if not args:
        print(f"nextest-args.py: {argv[1]}: no such test binary any more, nothing to run", file=sys.stderr)
        return NOTHING_TO_RUN
    print("\n".join(args))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
