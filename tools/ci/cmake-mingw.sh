#!/usr/bin/env bash
# cmake wrapper for the x86_64-pc-windows-gnu cross-build (set as CMAKE_x86_64_pc_windows_gnu in .cargo/config.toml).
#
# ggml's CMake drops the `lib` prefix on MinGW (`ggml.a`), but rustc looks for `libggml.a` or `ggml.lib` on windows-gnu,
# so whisper-rs-sys fails with "could not find native static library `ggml`". After a `--build` this adds the
# `lib`-prefixed name next to each unprefixed static library. Every other cmake call passes straight through.
set -uo pipefail
cmake "$@" || exit $?
if [ "${1:-}" = "--build" ] && [ -d "${2:-}" ]; then
  find "$2" -name '*.a' ! -name 'lib*' -print0 | while IFS= read -r -d '' lib; do
    cp -f "$lib" "$(dirname "$lib")/lib$(basename "$lib")"
  done
fi
