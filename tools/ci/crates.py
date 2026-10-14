#!/usr/bin/env python3
"""Workspace queries for the CI scripts. Reads `cargo metadata`; standard library only.

    crates.py members                 every workspace crate, one name per line
    crates.py wasm [NAME ...]         the crates among NAME ... (default: all members) that must build for WebAssembly

Why a script: deciding which crates cross-build is data in each crate's own Cargo.toml, so adding a crate never edits a
shared CI list (spec v3 section 39 rule 11). A crate that cannot target WebAssembly (audio capture, native video
encoders, the desktop shell) declares it once:

    [package.metadata.omuzaki]
    wasm = false
"""

import json
import subprocess
import sys

# Repo tooling that only ever runs on the build machine.
HOST_ONLY = {"xtask"}


def workspace_packages():
    """Return the workspace's own packages (no dependencies, so no network access) from `cargo metadata`."""
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)["packages"]


def targets_wasm(package):
    """True unless the crate is host-only tooling or opts out with `package.metadata.omuzaki.wasm = false`."""
    if package["name"] in HOST_ONLY:
        return False
    return ((package.get("metadata") or {}).get("omuzaki") or {}).get("wasm", True) is not False


def main(argv):
    """Entry point: `argv` is the command line after the script name."""
    if not argv or argv[0] not in ("members", "wasm"):
        print(__doc__, file=sys.stderr)
        return 2
    packages = workspace_packages()
    names = [p["name"] for p in packages]
    if argv[0] == "wasm":
        wanted = set(argv[1:]) or set(names)
        names = [p["name"] for p in packages if targets_wasm(p) and p["name"] in wanted]
    # One name per line, and nothing at all for an empty list: `mapfile` would read a lone newline as one empty name.
    for name in sorted(names):
        print(name)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
