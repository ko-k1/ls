#!/usr/bin/env python3
"""Create benchmark fixtures for ls.

Names are realistic (mixed case, punctuation, spaces, non-ASCII, dotfiles)
and every directory is populated in shuffled order. Creating files in sorted
order would make readdir return them pre-sorted on some filesystems (tmpfs
returns them reversed), which lets the sort heuristics skip work real
directories never get to skip.

usage: mkfixtures.py [ROOT]   (default: $TMPDIR/ls-bench-fixtures)
"""
import os
import random
import sys
import tempfile

ROOT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(tempfile.gettempdir(), "ls-bench-fixtures")
rng = random.Random(42)

WORDS = ["report", "final", "draft", "notes", "build", "output", "config", "data",
         "image", "backup", "test", "main", "utils", "index", "README", "Makefile",
         "photo", "invoice", "summary", "src", "lib", "assets", "Cargo", "package",
         "資料", "写真", "メモ", "Übersicht", "café"]
EXTS = ["txt", "md", "rs", "jpg", "png", "pdf", "log", "json", "tar.gz", "py", "c", "h", ""]


def mixed_name(i):
    r = rng.random()
    if r < 0.25:
        prefix = rng.choice(["IMG", "img", "DSC", "Screenshot"])
        date = f"{rng.randint(2015, 2026)}{rng.randint(1, 12):02d}{rng.randint(1, 28):02d}"
        return f"{prefix}_{date}_{i:06d}.{rng.choice(['jpg', 'JPG', 'png', 'heic'])}"
    if r < 0.45:
        sep = rng.choice(["-", "_", " ", "."])
        words = sep.join(rng.choice(WORDS) for _ in range(rng.randint(1, 3)))
        ext = rng.choice(EXTS)
        return f"{words}{i}" + (f".{ext}" if ext else "")
    if r < 0.60:
        letters = "abcdefghijklmnopqrstuvwxyz0123456789"
        return "".join(rng.choice(letters) for _ in range(rng.randint(4, 14))) + str(i)
    if r < 0.70:
        return rng.choice(WORDS).capitalize() + str(i) + rng.choice(["", ".md", ".TXT", " (2).pdf"])
    if r < 0.80:
        return "." + rng.choice(WORDS).lower() + str(i)
    if r < 0.90:
        return f"file{i:06d}.{rng.choice(EXTS) or 'dat'}"
    return f"{rng.choice(WORDS)}_{rng.choice(WORDS)}_v{i}.{rng.choice(EXTS) or 'bin'}"


JA_WORDS = ["写真", "資料", "議事録", "メモ", "報告書", "請求書", "見積", "設計", "会議",
            "旅行", "家族", "仕事", "プロジェクト", "バックアップ", "ダウンロード", "スクリーンショット",
            "東京", "大阪", "最終版", "下書き", "ノート", "レシピ", "音楽", "動画"]


def japanese_name(i):
    """Names as found in a Japanese user's Documents or Pictures folder."""
    r = rng.random()
    words = "_".join(rng.choice(JA_WORDS) for _ in range(rng.randint(1, 3)))
    if r < 0.4:
        return f"{words}_{rng.randint(2015, 2026)}{rng.randint(1, 12):02d}{rng.randint(1, 28):02d}_{i}.{rng.choice(['jpg', 'pdf', 'docx', 'xlsx', 'png'])}"
    if r < 0.7:
        return f"{words}（{rng.randint(1, 9)}）{i}.{rng.choice(['pdf', 'txt', 'md'])}"
    return f"{words}{i}"


def half_japanese_name(i):
    return japanese_name(i) if rng.random() < 0.5 else mixed_name(i)


def lower_name(i):
    letters = "abcdefghijklmnopqrstuvwxyz"
    return "".join(rng.choice(letters) for _ in range(rng.randint(5, 12))) + str(i)


def populate(path, count, namer, kinds=True):
    """Fill `path` with `count` entries: ~5% dirs, ~5% symlinks (10% of them
    broken), ~5% executables, the rest regular files with spread-out mtimes."""
    os.makedirs(path, exist_ok=True)
    names = [namer(i) for i in range(count)]
    rng.shuffle(names)
    for name in names:
        full = os.path.join(path, name)
        r = rng.random() if kinds else 1.0
        if r < 0.05:
            os.mkdir(full)
        elif r < 0.10:
            target = rng.choice(names) if rng.random() < 0.9 else "does-not-exist"
            os.symlink(target, full)
        else:
            fd = os.open(full, os.O_CREAT | os.O_WRONLY, 0o755 if r < 0.15 else 0o644)
            if rng.random() < 0.3:
                os.write(fd, b"x" * rng.randint(1, 50000))
            os.close(fd)
            # Spread mtimes over ~3 years so -lt and the six-month rule are exercised.
            mtime = 1_700_000_000 + rng.randint(-50_000_000, 50_000_000)
            os.utime(full, (mtime, mtime), follow_symlinks=False)


def tree(path, fanout, depth, files):
    os.makedirs(path, exist_ok=True)
    populate(path, files, mixed_name, kinds=False)
    if depth > 0:
        for index in range(fanout):
            tree(os.path.join(path, f"dir{index}_{rng.choice(WORDS)}"), fanout, depth - 1, files)


FIXTURES = [
    ("empty", lambda p: os.makedirs(p)),
    ("small", lambda p: populate(p, 40, mixed_name)),
    ("m1k", lambda p: populate(p, 1_000, mixed_name)),
    ("m10k", lambda p: populate(p, 10_000, mixed_name)),
    ("m100k", lambda p: populate(p, 100_000, mixed_name)),
    ("l100k", lambda p: populate(p, 100_000, lower_name, kinds=False)),
    ("j1k", lambda p: populate(p, 1_000, japanese_name, kinds=False)),
    ("j10k", lambda p: populate(p, 10_000, japanese_name, kinds=False)),
    ("j100k", lambda p: populate(p, 100_000, japanese_name, kinds=False)),
    ("h100k", lambda p: populate(p, 100_000, half_japanese_name, kinds=False)),
    ("tree", lambda p: tree(p, 8, 3, 160)),  # 585 dirs x 160 files ~= 94k entries
]

os.makedirs(ROOT, exist_ok=True)
for name, build in FIXTURES:
    path = os.path.join(ROOT, name)
    if os.path.exists(path):
        print(f"{path}: exists, skipped")
        continue
    build(path)
    print(f"{path}: created")
