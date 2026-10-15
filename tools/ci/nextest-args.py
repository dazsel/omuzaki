#!/usr/bin/env python3
"""Turn one shard item of a test job's `parts` into the nextest arguments that run it.

    nextest-args.py ITEM        prints one argument per line (read them with `mapfile -t`)

The planner (xtask/src/report.rs) labels a share of a crate that is too heavy for one job:

    NAME@K/N            partition K of N of the crate's tests         -p NAME --partition hash:K/N
    NAME@K/N!BIN,BIN    the same, over the tests outside those test binaries
                                                                       ... -E 'not (binary(=BIN) | binary(=BIN))'
    NAME@=BIN           only the tests of one test binary              -p NAME -E 'binary(=BIN)' --no-tests=pass

A crate's isolated binaries run as `NAME@=BIN` items and its partitions skip them, so together they run every test once.
nextest partitions after it filters, so a test's partition does not depend on which binaries are skipped. An isolated
item passes when its binary no longer exists (it was renamed): the partitions then run the renamed binary, because the
skip list names a binary that is gone. The script lives apart from ci.yml so the mapping is tested
(tools/tests/nextest_args.sh). Standard library only.
"""

import re
import sys

NAME = r"[A-Za-z0-9_-]+"
BINARY = r"[A-Za-z0-9_-]+"
PART = re.compile(rf"^(?P<name>{NAME})@(?P<index>[1-9][0-9]*)/(?P<parts>[1-9][0-9]*)(?:!(?P<skip>{BINARY}(?:,{BINARY})*))?$")
ONLY = re.compile(rf"^(?P<name>{NAME})@=(?P<binary>{BINARY})$")


def nextest_args(item):
    """The nextest arguments for `item`, or None when the label is not one the planner writes."""
    only = ONLY.match(item)
    if only:
        return ["-p", only["name"], "-E", f"binary(={only['binary']})", "--no-tests=pass"]
    part = PART.match(item)
    if not part or int(part["index"]) > int(part["parts"]):
        return None
    args = ["-p", part["name"], "--partition", f"hash:{part['index']}/{part['parts']}"]
    if part["skip"]:
        skipped = " | ".join(f"binary(={binary})" for binary in part["skip"].split(","))
        args += ["-E", f"not ({skipped})" if "," in part["skip"] else f"not {skipped}"]
    return args


def main(argv):
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    args = nextest_args(argv[1])
    if args is None:
        print(f"nextest-args.py: not a shard item: {argv[1]!r}", file=sys.stderr)
        return 2
    print("\n".join(args))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
