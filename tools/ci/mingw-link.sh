#!/usr/bin/env bash
# Linker for the x86_64-pc-windows-gnu cross-build (`linker` in .cargo/config.toml).
#
# Wraps the posix-thread MinGW gcc so the MinGW runtime is linked statically and a Windows exe needs no extra DLLs.
# `-static-libstdc++` and `-static` don't do it: whisper-rs-sys asks for `dylib=stdc++`, and rustc brackets that with
# `-Bdynamic`, which makes ld pick libstdc++.dll.a (an import of libstdc++-6.dll). Rewriting that one argument to
# an explicit static link is the reliable way. rustc moves long linker command lines into a GNU `@response` file (one
# argument per line, optionally double-quoted), so the same rewrite is applied inside those files, on a copy.
# MINGW_GCC overrides the compiler (tools/tests/mingw_link.sh uses a fake one).
set -uo pipefail

gcc=${MINGW_GCC:-x86_64-w64-mingw32-gcc-posix}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Rewrite a response file: the `-lstdc++` line (quoted or not) becomes the three static-link lines.
rewrite_response_file() {
  local in=$1 out=$2 line
  : >"$out"
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      -lstdc++ | '"-lstdc++"') printf '%s\n' '-Wl,-Bstatic' '-lstdc++' '-Wl,-Bdynamic' >>"$out" ;;
      *) printf '%s\n' "$line" >>"$out" ;;
    esac
  done <"$in"
}

args=()
n=0
for a in "$@"; do
  case "$a" in
    -lstdc++) args+=("-Wl,-Bstatic" "-lstdc++" "-Wl,-Bdynamic") ;;
    @?*)
      if [ -f "${a#@}" ]; then
        n=$((n + 1))
        rewrite_response_file "${a#@}" "$tmp/rsp.$n"
        args+=("@$tmp/rsp.$n")
      else
        args+=("$a")
      fi
      ;;
    *) args+=("$a") ;;
  esac
done
# Not `exec`: the EXIT trap must remove the rewritten copies after the link.
"$gcc" -static-libgcc -static-libstdc++ "${args[@]}"
