# Benchmarks and compatibility checks

Tools for measuring this `ls` against a reference implementation (normally
GNU coreutils `ls`) and checking that the output stays identical.

## 1. Get a reference `ls`

If `/usr/bin/ls` has been replaced by this binary, extract the original
from the package cache. On Arch Linux:

```sh
mkdir -p /tmp/gnu-ls && cd /tmp/gnu-ls
tar -xf /var/cache/pacman/pkg/coreutils-<version>.pkg.tar.* usr/bin/ls
mv usr/bin/ls ./gnu-ls
```

## 2. Create fixtures

```sh
bench/mkfixtures.py            # default: $TMPDIR/ls-bench-fixtures (~4 s, ~400k inodes)
```

Fixtures go on tmpfs by default, which measures CPU work rather than disk.
Every directory is populated in shuffled order with realistic names
(mixed case, punctuation, spaces, non-ASCII, dotfiles, ~5% dirs, ~5%
symlinks with some broken, ~5% executables). Do not create benchmark
directories in sorted order: readdir then returns them pre-sorted, and the
sort heuristics skip work that real directories never get to skip.

## 3. Check output compatibility

```sh
cargo build --release
bench/compare.sh /tmp/gnu-ls/gnu-ls                # exits 1 on any new difference
```

Known incompatibilities, if any, are listed in `compare.sh` and reported as
`known` without failing the run (the list is currently empty). Terminal
output (columns, quoting and its alignment, wide characters) is checked
under a pseudo-terminal by a second script:

```sh
bench/compare-tty.sh /tmp/gnu-ls/gnu-ls
```

## 4. Benchmark two builds

```sh
cp target/release/ls /tmp/ls-before
# ... make changes, cargo build --release ...
bench/bench.py /tmp/ls-before target/release/ls
bench/bench.py --filter m100k --runs 30 /tmp/ls-before target/release/ls
```

- **Small cases** report user-mode instructions from `perf stat`. They are
  deterministic; wall time for a small directory is dominated by exec
  (~0.6–0.9 ms on an aarch64 laptop) and varies by ±150 µs between runs.
- **Large cases** report median wall time, with runs of the two binaries
  interleaved. Parallel cases are sensitive to other load on the machine:
  check `uptime` first and treat results taken at a high load average as
  approximate. Compare user+sys CPU time when only the amount of work
  matters.
- **Terminal cases** (`tty`) run under a 200x50 pty, which turns on color
  and column output.
