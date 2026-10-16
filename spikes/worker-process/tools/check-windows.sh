#!/usr/bin/env bash
# Compile-checks the Windows lane from a Linux machine: `cargo check --target x86_64-pc-windows-msvc`, no linking.
# TARGET=x86_64-pc-windows-gnu checks the target the Spout shim is actually built for (see the research page).
#
# spout-out's build script compiles the Spout2 C++ shim whenever the target is Windows, even for `cargo check`. This script
# hands it empty stand-in sources, a stand-in compiler and archiver (they only have to leave an object file and a library behind), so the check needs
# neither the Spout2 source nor a Windows C++ toolchain. It proves the Rust compiles against the Windows APIs; it does not
# prove the shim compiles (the broadcast-out spike's own build does) and it links nothing.
#
#   rustup target add x86_64-pc-windows-msvc
#   spikes/worker-process/tools/check-windows.sh            # extra arguments go to cargo, e.g. --all-targets
#   CARGO_CMD=clippy spikes/worker-process/tools/check-windows.sh -- -D warnings
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# The files spout-out/build.rs insists on, empty.
for f in SPOUTSDK/SpoutDirectX/SpoutDX/SpoutDX.cpp SPOUTSDK/SpoutGL/{SpoutCopy,SpoutDirectX,SpoutFrameCount,SpoutSenderNames,SpoutSharedMemory,SpoutUtils}.cpp; do
  mkdir -p "$tmp/spout/$(dirname "$f")"
  : > "$tmp/spout/$f"
done

# A "C++ compiler" that only writes the object file named by -o. (cc-rs also runs it with -E to detect what it is, which goes
# to the real compiler, and with -print-file-name to find the C++ runtime, which gets a placeholder directory.)
cat > "$tmp/fake-cxx" <<'SH'
#!/bin/sh
out=
for arg in "$@"; do
  case "$arg" in
    -E) exec "${CC:-cc}" "$@" ;;
    -print-file-name=*) echo "$(dirname "$0")/libstdc++.a"; exit 0 ;;
  esac
done
while [ $# -gt 0 ]; do
  if [ "$1" = "-o" ]; then out=$2; fi
  shift
done
exec "${CC:-cc}" -x c -c /dev/null -o "$out"
SH
# An "archiver" that only creates the library named by the MSVC-style -out: argument.
cat > "$tmp/fake-lib" <<'SH'
#!/bin/sh
for arg in "$@"; do
  case "$arg" in -out:*) : > "${arg#-out:}" ;; esac
done
SH
chmod +x "$tmp/fake-cxx" "$tmp/fake-lib"

cd "$here"
target=${TARGET:-x86_64-pc-windows-msvc}
env_target=${target//-/_}
env SPOUT2_DIR="$tmp/spout" "AR_$env_target=$tmp/fake-lib" "CXX_$env_target=$tmp/fake-cxx" "CC_$env_target=$tmp/fake-cxx" \
  cargo "${CARGO_CMD:-check}" --target "$target" --locked "$@"
