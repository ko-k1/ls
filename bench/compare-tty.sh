#!/usr/bin/env bash
# Diff terminal output (columns, long format, quoting and its alignment,
# double-width characters) against a reference ls (normally GNU coreutils),
# run under a pseudo-terminal via script(1). GNU aligns columns with tabs,
# so its output is passed through expand(1). Exits non-zero on differences.
#
# usage: bench/compare-tty.sh REFERENCE_LS [CANDIDATE_LS]
set -u

ref=${1:?usage: compare-tty.sh REFERENCE_LS [CANDIDATE_LS]}
here=$(cd "$(dirname "$0")" && pwd)
cand=${2:-$here/../target/release/ls}
# The comparisons run inside the test directories, so resolve both binaries.
ref=$(realpath -e "$(command -v "$ref" || echo "$ref")") || exit 2
cand=$(realpath -e "$(command -v "$cand" || echo "$cand")") || exit 2
work=$(mktemp -d) || exit 2
trap 'rm -rf -- "$work"' EXIT

# Names that need shell quoting, double-width and combining characters, a
# directory, an executable and symlinks.
mkdir -p "$work/mixed/sub dir" "$work/plain"
cd "$work/mixed" || exit 2
touch plain other "with space" "it's" '$dollar' 写真フォルダ 資料_2024年度 メモ café \
  "e$(printf '\xcc\x81')t" 한국어 run.sh
chmod +x run.sh
ln -s "with space" link
ln -s missing broken
touch "$work/plain/"{a,bb,ccc,dddd,eeeee}.txt

run() { # COLUMNS DIR BINARY ARGS...
  local columns=$1 dir=$2 binary=$3
  shift 3
  (cd "$dir" && COLUMNS=$columns script -qc "$(printf '%q ' "$binary" "$@")" /dev/null) |
    sed 's/\r$//'
}

failed=0
for dir in "$work/mixed" "$work/plain"; do
  for columns in 30 60 120; do
    for args in "" "-F" "-is" "-l" "-li" "-1"; do
      # shellcheck disable=SC2086
      if diff <(run "$columns" "$dir" "$cand" --color=never $args) \
        <(run "$columns" "$dir" "$ref" --color=never $args | expand) >/dev/null; then
        printf '  same  %s cols=%s %s\n' "$(basename "$dir")" "$columns" "$args"
      else
        printf '  DIFF  %s cols=%s %s\n' "$(basename "$dir")" "$columns" "$args"
        failed=1
      fi
    done
  done
done
exit "$failed"
