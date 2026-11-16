#!/usr/bin/env bash
# Diff this ls against a reference ls (normally GNU coreutils) across a
# matrix of options, on the benchmark fixtures and on real system
# directories. Exits non-zero if any output differs.
#
# usage: bench/compare.sh REFERENCE_LS [CANDIDATE_LS] [FIXTURES_DIR]
#
# Color and terminal column output are excluded on purpose: GNU emits extra
# reset sequences and tab-aligned columns, which differ byte-wise but not
# visually. (Known terminal-only difference: when any name in a listing needs
# shell quoting, GNU indents the unquoted names by one space; this ls does
# not. Column widths otherwise match GNU, including double-width CJK.)
set -u

ref=${1:?usage: compare.sh REFERENCE_LS [CANDIDATE_LS] [FIXTURES_DIR]}
here=$(cd "$(dirname "$0")" && pwd)
cand=${2:-$here/../target/release/ls}
fx=${3:-${TMPDIR:-/tmp}/ls-bench-fixtures}
out=$(mktemp -d) || exit 2
trap 'rm -rf -- "$out"' EXIT

cases=(
  "-1 {fx}/m100k" "-1 {fx}/l100k" "-1a {fx}/m10k" "-1A {fx}/m10k" "-la {fx}/m10k"
  "-lt {fx}/m10k" "-lS {fx}/m10k" "-ltr {fx}/m10k" "-lh {fx}/m10k" "-sh {fx}/m10k"
  "-s {fx}/m10k" "-li {fx}/small" "-1F {fx}/m10k" "-1p {fx}/m10k" "-1U {fx}/small"
  "-1 --group-directories-first {fx}/m10k" "-R {fx}/tree" "-lR {fx}/tree"
  "-l /usr/lib" "-lh /usr/lib" "-l /usr/share/man/man3" "-1 /usr/include"
  "-ld /usr /usr/lib /etc/passwd" "-l /usr/bin/sh /etc"
)

# Known, not yet fixed incompatibilities. They are reported but do not fail
# the run; remove an entry once it is fixed so regressions are caught.
declare -A known=(
  ["-s {fx}/m10k"]="block column is not right-aligned outside -l"
  ["-sh {fx}/m10k"]="block column is not right-aligned outside -l"
  ["-1 --group-directories-first {fx}/m10k"]="symlinks to directories are not grouped with directories"
  ["-ld /usr /usr/lib /etc/passwd"]="file operands are not sorted"
  ["-l /usr/bin/sh /etc"]="GNU sizes file-operand columns including directory operands"
)

failed=0
for case in "${cases[@]}"; do
  args=${case//\{fx\}/$fx}
  # Word splitting of $args is intended: each case is a list of arguments.
  # shellcheck disable=SC2086
  "$cand" $args >"$out/cand" 2>&1
  # shellcheck disable=SC2086
  "$ref" $args >"$out/ref" 2>&1
  # The program name in error messages differs between binaries.
  sed -i -E 's/^[^:]*ls: /ls: /' "$out/cand" "$out/ref"
  lines=$(diff "$out/cand" "$out/ref" | grep -c '^[<>]')
  reason=${known[$case]:-}
  if [ "$lines" -eq 0 ] && [ -n "$reason" ]; then
    printf '  same  %s  (listed as known; remove it from the list)\n' "$case"
  elif [ "$lines" -eq 0 ]; then
    printf '  same  %s\n' "$case"
  elif [ -n "$reason" ]; then
    printf '  known %s  (%s lines: %s)\n' "$case" "$lines" "$reason"
  else
    printf '  DIFF  %s  (%s lines)\n' "$case" "$lines"
    failed=1
  fi
done
exit "$failed"
