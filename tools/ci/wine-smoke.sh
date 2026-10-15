#!/usr/bin/env bash
# Smoke test for the Windows zip: unpack it and run `omuzaki.exe --version` under Wine.
#
# Usage: tools/ci/wine-smoke.sh [<zip>]
#   default zip: the one `cargo xtask dist` wrote (target/dist/omuzaki-<version>-windows-x64.zip)
# Prints a skip line and exits 0 when Wine is not installed. Exits 1 when the exe does not run or reports another version.
set -uo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# The version every crate shares: `version = "..."` under [workspace.package].
version=$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$ROOT/Cargo.toml" | head -n 1)
[[ -n $version ]] || { echo "wine-smoke: cannot read the workspace version" >&2; exit 1; }

if ! command -v wine >/dev/null 2>&1; then
  echo "wine-smoke: skipped, wine is not installed"
  exit 0
fi

zip=${1:-$ROOT/target/dist/omuzaki-$version-windows-x64.zip}
[[ -f $zip ]] || { echo "wine-smoke: no zip at $zip (run: cargo xtask dist --target x86_64-pc-windows-gnu --no-models)" >&2; exit 1; }

# Under ~/work, as the machine's other scratch folders are.
mkdir -p "$HOME/work" || { echo "wine-smoke: cannot create $HOME/work" >&2; exit 1; }
tmp=$(mktemp -d "$HOME/work/wine-smoke.XXXXXX") || { echo "wine-smoke: cannot create a temp folder" >&2; exit 1; }
trap 'rm -rf "$tmp"' EXIT

unzip -q "$zip" -d "$tmp/unpacked" || { echo "wine-smoke: cannot unpack $zip" >&2; exit 1; }
# A private prefix, so the run neither reads nor changes the user's own Wine setup.
export WINEPREFIX="$tmp/prefix" WINEDEBUG=-all WINEDLLOVERRIDES="mscoree,mshtml="
output=$(cd "$tmp/unpacked" && timeout 300 wine omuzaki.exe --version 2>&1)
status=$?
output=${output//$'\r'/}
echo "$output"
[[ $status -eq 0 ]] || { echo "wine-smoke: FAIL, omuzaki.exe --version exited $status" >&2; exit 1; }
if ! grep -qE "(^|[^0-9.])${version//./\\.}([^0-9.]|$)" <<<"$output"; then
  echo "wine-smoke: FAIL, expected version $version" >&2
  exit 1
fi
echo "wine-smoke: ok, omuzaki.exe reports $version"
