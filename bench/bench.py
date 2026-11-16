#!/usr/bin/env python3
"""Benchmark two ls binaries against each other.

Small cases are measured in user-mode instructions (`perf stat`), which are
deterministic; their wall time is dominated by exec and jitters by
+-150us on a laptop. Larger cases are measured in wall time, with runs of the
two binaries interleaved so frequency scaling and cache state affect both
equally. Terminal cases run under a 200x50 pty.

usage: bench.py [--runs N] [--filter TEXT] [--fixtures DIR] BIN_A BIN_B
"""
import argparse
import fcntl
import os
import pty
import select
import shutil
import statistics
import struct
import subprocess
import tempfile
import termios
import time

HOME = os.path.expanduser("~")

# (label, directory, args, tty). "{fx}" is replaced by the fixtures root.
INSTRUCTION_CASES = [
    ("empty dir", "{fx}/empty", []),
    ("small(40) -1", "{fx}/small", ["-1"]),
    ("small(40) -1U", "{fx}/small", ["-1U"]),
    ("small(40) -l", "{fx}/small", ["-l"]),
    ("small(40) -la --color", "{fx}/small", ["-la", "--color=always"]),
    ("m1k -1", "{fx}/m1k", ["-1"]),
    ("m1k -l", "{fx}/m1k", ["-l"]),
    ("small(40) -R", "{fx}/small", ["-R"]),
    ("j1k -1 (Japanese names)", "{fx}/j1k", ["-1"]),
]

TIMED_CASES = [
    ("small(40) tty", "{fx}/small", [], True),
    ("m1k tty", "{fx}/m1k", [], True),
    ("/usr/include -l", "/usr/include", ["-l"], False),
    ("/usr/lib -l", "/usr/lib", ["-l"], False),
    ("/usr/lib tty -l", "/usr/lib", ["-l"], True),
    ("/usr/lib tty", "/usr/lib", [], True),
    ("m10k -1", "{fx}/m10k", ["-1"], False),
    ("m10k -l", "{fx}/m10k", ["-l"], False),
    ("m10k tty", "{fx}/m10k", [], True),
    ("man3 -l", "/usr/share/man/man3", ["-l"], False),
    ("m100k -1", "{fx}/m100k", ["-1"], False),
    ("m100k -1U", "{fx}/m100k", ["-1U"], False),
    ("l100k -1", "{fx}/l100k", ["-1"], False),
    ("j10k -1 (Japanese)", "{fx}/j10k", ["-1"], False),
    ("j100k -1 (Japanese)", "{fx}/j100k", ["-1"], False),
    ("j100k tty (Japanese)", "{fx}/j100k", [], True),
    ("h100k -1 (58% Japanese)", "{fx}/h100k", ["-1"], False),
    ("m100k -l", "{fx}/m100k", ["-l"], False),
    ("m100k -lt", "{fx}/m100k", ["-lt"], False),
    ("m100k -la --color", "{fx}/m100k", ["-la", "--color=always"], False),
    ("m100k -1 --color", "{fx}/m100k", ["-1", "--color=always"], False),
    ("m100k tty", "{fx}/m100k", [], True),
    ("3 dirs -1", "{fx}", ["-1", "small", "m1k", "m10k"], False),
    ("/usr/include -R", "/usr/include", ["-R"], False),
    ("tree -R", "{fx}/tree", ["-R"], False),
    ("tree -lR", "{fx}/tree", ["-lR"], False),
    ("cargo-src -lR", f"{HOME}/.cargo/registry/src", ["-lR"], False),
    ("/usr/share -R", "/usr/share", ["-R"], False),
]


def run_once(binary, cwd, args, tty):
    """Run `binary` once; return wall milliseconds."""
    if tty:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 200, 0, 0))
        out = slave
    else:
        out = os.open(os.devnull, os.O_WRONLY)
    start = time.perf_counter_ns()
    pid = os.fork()
    if pid == 0:
        try:
            os.chdir(cwd)
            os.dup2(out, 1)
            os.dup2(os.open(os.devnull, os.O_WRONLY), 2)
            os.execv(binary, [binary] + args)
        finally:
            os._exit(127)
    os.close(out)
    if tty:
        # Drain the pty until the child exits and closes its end.
        while True:
            ready, _, _ = select.select([master], [], [], 0.05)
            try:
                if ready and not os.read(master, 1 << 20):
                    break
            except OSError:
                break
        os.close(master)
    os.waitpid(pid, 0)
    return (time.perf_counter_ns() - start) / 1e6


def instructions(binary, cwd, args, runs):
    result = subprocess.run(
        ["perf", "stat", "-x,", "-e", "instructions:u", "-r", str(runs), binary] + args,
        cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, check=True)
    for line in result.stderr.splitlines():
        fields = line.split(",")
        if len(fields) > 2 and fields[2].startswith("instructions"):
            return int(fields[0])
    raise RuntimeError(result.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--runs", type=int, default=15)
    parser.add_argument("--filter", default="")
    parser.add_argument("--fixtures",
                        default=os.path.join(tempfile.gettempdir(), "ls-bench-fixtures"))
    parser.add_argument("a", help="baseline binary")
    parser.add_argument("b", help="candidate binary")
    opts = parser.parse_args()
    a, b = os.path.abspath(opts.a), os.path.abspath(opts.b)

    def resolve(cases):
        for case in cases:
            label, cwd = case[0], case[1].replace("{fx}", opts.fixtures)
            if opts.filter in label and os.path.isdir(cwd):
                yield (label, cwd) + tuple(case[2:])

    if shutil.which("perf"):
        print(f"{'instructions:u':28} {'A':>12} {'B':>12} {'B/A':>7}")
        for label, cwd, args in resolve(INSTRUCTION_CASES):
            ia = instructions(a, cwd, args, max(opts.runs, 10))
            ib = instructions(b, cwd, args, max(opts.runs, 10))
            print(f"{label:28} {ia:12,} {ib:12,} {ib / ia:7.2f}", flush=True)
        print()
    else:
        print("perf not found; skipping instruction counts\n")

    print(f"{'wall ms (median)':28} {'A':>12} {'B':>12} {'speedup':>8}")
    for label, cwd, args, tty in resolve(TIMED_CASES):
        times = {a: [], b: []}
        for binary in (a, b):
            run_once(binary, cwd, args, tty)  # warm caches
        for _ in range(opts.runs):
            for binary in (a, b):
                times[binary].append(run_once(binary, cwd, args, tty))
        ma, mb = statistics.median(times[a]), statistics.median(times[b])
        print(f"{label:28} {ma:12.2f} {mb:12.2f} {ma / mb:7.2f}x", flush=True)


if __name__ == "__main__":
    main()
