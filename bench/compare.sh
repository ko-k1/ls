#!/usr/bin/env bash
# Diff this ls against a reference ls (normally GNU coreutils) across a
# matrix of options, on the benchmark fixtures and on real system
# directories. Exits non-zero if any output differs.
#
# usage: bench/compare.sh REFERENCE_LS [CANDIDATE_LS] [FIXTURES_DIR]
#
# Color and terminal column output are excluded on purpose: GNU emits extra
# reset sequences and tab-aligned columns, which differ byte-wise but not
# visually. Terminal output is checked separately by bench/compare-tty.sh.
set -u

ref=${1:?usage: compare.sh REFERENCE_LS [CANDIDATE_LS] [FIXTURES_DIR]}
here=$(cd "$(dirname "$0")" && pwd)
cand=${2:-$here/../target/release/ls}
fx=${3:-${TMPDIR:-/tmp}/ls-bench-fixtures}
out=$(mktemp -d) || exit 2
trap 'rm -rf -- "$out"' EXIT
# A file modified just now must show its time of day, not its year.
mkdir "$out/fresh" && touch "$out/fresh/new"

cases=(
  "-1 {fx}/m100k" "-1 {fx}/l100k" "-1a {fx}/m10k" "-1A {fx}/m10k" "-la {fx}/m10k"
  "-lt {fx}/m10k" "-lS {fx}/m10k" "-ltr {fx}/m10k" "-lh {fx}/m10k" "-sh {fx}/m10k"
  "-s {fx}/m10k" "-li {fx}/small" "-1F {fx}/m10k" "-1p {fx}/m10k" "-1U {fx}/small"
  "-1 --group-directories-first {fx}/m10k" "-R {fx}/tree" "-lR {fx}/tree"
  "-l /usr/lib" "-lh /usr/lib" "-l /usr/share/man/man3" "-1 /usr/include"
  "-ld /usr /usr/lib /etc/passwd" "-l /usr/bin/sh /etc" "-1si {fx}/small"
  "-1 /usr/share /etc/passwd /usr /etc/hosts" "-1t /usr/share /etc/passwd /usr"
  "-lS /etc/passwd /etc/hosts /usr/bin/sh" "-1 --group-directories-first /usr/lib"
  "-li /usr/include /etc/passwd" "-l {tmp}/fresh"
)

# Known, not yet fixed incompatibilities. They are reported but do not fail
# the run; remove an entry once it is fixed so regressions are caught.
declare -A known=()

failed=0
for case in "${cases[@]}"; do
  args=${case//\{fx\}/$fx}
  args=${args//\{tmp\}/$out}
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
