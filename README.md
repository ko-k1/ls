# ls (Rust)

A fast `ls` implementation for Unix, written in Rust.

Large directories are listed with parallel metadata loading and sorting (via
Rayon), with a fast path for the common lowercase-ASCII/digit filename alphabet.

## Features

- Default multi-column output on a terminal, one file per line when piped
- Long format with Unix permissions (including setuid/setgid/sticky bits),
  hard-link count, owner, group, size, and modification time
- Human-readable sizes (`-h`), inode and block columns
- Sorting by name, modification time (`-t`), size (`-S`), or directory order
  (`-U`), with `-r` to reverse
- Recursive listing (`-R`), hidden entries (`-a` / `-A`), directory-only mode
  (`-d`), and type indicators (`-F` / `-p`)
- ANSI colors for directories, symlinks, executables, sockets, FIFOs, devices,
  and broken links, with `--color=always|auto|never` and `NO_COLOR` support
- Multiple operands, `--` end-of-options, and GNU-style exit statuses

## Build

```sh
cargo build --release
./target/release/ls --help
```

Install it on your `PATH`:

```sh
cargo install --path .
```

## Usage

```text
Usage: ls [OPTION]... [FILE]...
```

Examples:

```sh
ls                     # current directory
ls -lah                # long format, hidden files, human-readable sizes
ls -lt                 # newest first
ls -R src              # recurse into src
ls -lF --color=never . # long listing with type suffixes, no color
```

Run `ls --help` for the full option list.

## Notes

- This is a Unix-only program; it uses Unix metadata (`st_uid`, `st_gid`,
  `st_blocks`, file type bits, etc.).
- Timestamps are displayed in UTC.
- Color is enabled automatically only when stdout is a terminal. Setting
  `NO_COLOR` disables it, and `--color=always` forces it.
- Column layout uses the terminal width from `ioctl(TIOCGWINSZ)`, or
  `COLUMNS` when set.

## Tests

```sh
cargo test
```

The suite includes unit tests for size/date formatting and integration tests
that exercise the built binary.
