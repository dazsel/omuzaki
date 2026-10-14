#!/usr/bin/env bash
# Third-party notices for a release build: cargo-about for every crate in the build, plus what it can't see
# by itself: the font licences egui bundles, and this repository's own NOTICES.md files.
#
# Usage: tools/release/notices.sh [--out <file>]
#   default output: target/notices/THIRD-PARTY-NOTICES.txt
# Exit 0 and the output path on success. Exit 2 with a one-line reason on any failure. Nothing is written to the output
# path until every step has succeeded, so a failed run never leaves a partial file behind.
#
# Needs cargo-about 0.9.2 (the version this script was written and checked against):
#   cargo install cargo-about --locked --version 0.9.2 --features cli   (the binary is behind the `cli` feature)
# The output is deterministic: sorting is C-locale and the file has no timestamps, so two runs give identical bytes.
set -uo pipefail
# A closed stdout pipe would otherwise kill bash with SIGPIPE, before any `|| die` could report it. Ignored, the write
# fails with EPIPE instead, and the final answer's check exits 2 with a reason.
trap '' PIPE

die() { echo "notices.sh: $*" >&2; exit 2; }

script_dir=$(dirname "${BASH_SOURCE[0]}") || die "cannot locate the script folder"
ROOT=$(cd "$script_dir/../.." && pwd) || die "cannot locate the repository root"
CARGO_ABOUT_VERSION=0.9.2

# Appends a format string and its arguments to the temp file. Every write is checked on its own, so a failed write
# fails the run and can't be hidden by a later command that succeeds.
# Every caller passes a literal format string, so the variable format that SC2059 warns about injects nothing here.
# shellcheck disable=SC2059
emit() { printf -- "$@" >> "$tmp" || die "cannot write $tmp"; }

# A titled block of the notices file. The title is a plain line, easy to find with grep.
section() { emit '\n==== %s ====\n\n' "$1"; }

# A file's full text under a label (its file name, or its path in the repository). An unreadable file is fatal: a notice
# that silently drops out of the bundle is worse than no bundle, and the caller's temp file is then never published.
block() {
  [[ -r $2 ]] || die "cannot read $2"
  emit '--- %s ---\n' "$1"
  cat -- "$2" >> "$tmp" || die "cannot write $tmp (from $2)"
  emit '\n'
}

out=""
while (( $# > 0 )); do
  case $1 in
    --out)
      (( $# >= 2 )) || die "--out needs a file name"
      out=$2
      shift 2
      ;;
    *) die "unknown argument: $1 (usage: notices.sh [--out <file>])" ;;
  esac
done

# A relative --out is relative to where the caller stands, not to the repository root.
[[ -n $out ]] || out="$ROOT/target/notices/THIRD-PARTY-NOTICES.txt"
[[ $out == /* ]] || out="$PWD/$out"
# A directory would receive the temp file under a hidden name and report success, so it is refused up front.
[[ ! -d $out ]] || die "--out is a directory, not a file: $out"

# Step 1: the tool itself.
cargo about --version >/dev/null 2>&1 \
  || die "cargo-about is not available; install it: cargo install cargo-about --locked --version $CARGO_ABOUT_VERSION --features cli"

cd "$ROOT" || die "cannot enter $ROOT"
out_dir=$(dirname "$out") || die "cannot find the folder of $out"
mkdir -p "$out_dir" || die "cannot create $out_dir"
tmp=$(mktemp "$out_dir/.notices.XXXXXX") || die "cannot create a temporary file in $out_dir"
trap '[[ -n ${tmp:-} ]] && rm -f "$tmp"' EXIT

# Step 2: cargo-about's text for every crate in the locked build. It fails on any licence not in about.toml's `accepted`.
cargo about generate --locked about.hbs > "$tmp" \
  || die "cargo about generate failed (the reason is above); a licence may be missing from about.toml's accepted list"

# Step 3: the fonts egui bundles. Their licences ship with the binary, but no crate metadata names them. The folder comes
# from the locked graph, so the version is the one that is built.
meta=$(cargo metadata --locked --format-version 1) || die "cargo metadata failed (is Cargo.lock current?)"
locate_fonts='
import json, os, sys
meta = json.load(sys.stdin)
found = [p for p in meta["packages"] if p["name"] == "epaint_default_fonts"]
if len(found) != 1:
    sys.exit(f"expected one epaint_default_fonts package in the locked graph, found {len(found)}")
print(os.path.join(os.path.dirname(found[0]["manifest_path"]), "fonts"))
'
fonts_dir=$(printf '%s' "$meta" | python3 -I -c "$locate_fonts" 2>&1) || die "$fonts_dir"
font_files=(OFL.txt UFL.txt Hack-Regular.txt emoji-icon-font-mit-license.txt)
for f in "${font_files[@]}"; do
  [[ -f "$fonts_dir/$f" ]] || die "epaint_default_fonts is missing $fonts_dir/$f"
done

# Step 4: this repository's own notices, each with the OFL licence files in its folder. Tracked files only, so a local
# scratch file never ends up in a release. Matching is done in bash rather than in a grep pipeline: a pipeline's failure
# can't then be lost, and "nothing matched" is just an empty array. `git ls-files` lists paths in byte order already.
tracked=$(git ls-files) || die "git ls-files failed"
notices=()
ofl_files=()
while IFS= read -r path; do
  case $path in
    crates/NOTICES.md | crates/*/NOTICES.md)
      [[ $path == */tests/* ]] || notices+=("$path")
      ;;
  esac
  case $path in
    */OFL*.txt) ofl_files+=("$path") ;;
  esac
done <<< "$tracked"
(( ${#notices[@]} > 0 )) || die "no NOTICES.md is tracked under crates/ outside tests/"

section "Fonts bundled by egui"
for f in "${font_files[@]}"; do
  block "$f" "$fonts_dir/$f"
done

section "Notices from this repository"
for notice in "${notices[@]}"; do
  block "$notice" "$notice"
  dir=${notice%/*}
  for ofl in "${ofl_files[@]}"; do
    # Only the OFL files in the notice's own folder (not a subfolder, not a sibling).
    if [[ ${ofl%/*} == "$dir" && ${ofl##*/} == OFL*.txt ]]; then block "$ofl" "$ofl"; fi
  done
done

chmod 644 "$tmp" || die "cannot set permissions on $tmp"
mv -f "$tmp" "$out" || die "cannot write $out"
tmp=""
# The path is the script's answer to its caller, so a failed write of it (closed or full stdout) is a failure too.
printf '%s\n' "$out" || die "cannot write the output path to stdout"
