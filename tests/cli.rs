use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rust-ls-test-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            tag
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run(dir: &TestDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ls"))
        .args(args)
        .current_dir(dir.path())
        .output()
        .expect("failed to run ls")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn lists_sorted_and_hides_dotfiles() {
    let dir = TestDir::new("basic");
    fs::write(dir.path().join("b.txt"), "").unwrap();
    fs::write(dir.path().join("a.txt"), "").unwrap();
    fs::write(dir.path().join(".hidden"), "").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();

    let output = run(&dir, &["-1"]);
    assert!(output.status.success());
    assert_eq!(stdout(&output), "a.txt\nb.txt\nsub\n");
}

#[test]
fn all_includes_dot_entries() {
    let dir = TestDir::new("all");
    fs::write(dir.path().join("a.txt"), "").unwrap();
    fs::write(dir.path().join(".hidden"), "").unwrap();

    let output = run(&dir, &["-a", "-1"]);
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();

    assert_eq!(lines[0], ".");
    assert_eq!(lines[1], "..");
    assert!(lines.contains(&".hidden"));
    assert!(lines.contains(&"a.txt"));
}

#[test]
fn almost_all_skips_dot_entries() {
    let dir = TestDir::new("almost-all");
    fs::write(dir.path().join("a.txt"), "").unwrap();
    fs::write(dir.path().join(".hidden"), "").unwrap();

    let output = run(&dir, &["-A", "-1"]);
    let text = stdout(&output);

    assert!(text.contains(".hidden"));
    assert!(!text.lines().any(|line| line == "." || line == ".."));
}

#[test]
fn long_format_has_permissions_size_and_total() {
    let dir = TestDir::new("long");
    fs::write(dir.path().join("a.txt"), "hello").unwrap();

    let output = run(&dir, &["-l"]);
    let text = stdout(&output);
    let mut lines = text.lines();

    assert!(lines.next().unwrap().starts_with("total "));

    let line = lines.next().unwrap();
    let fields: Vec<&str> = line.split_whitespace().collect();

    assert_eq!(fields[0], "-rw-r--r--");
    assert_eq!(fields[1], "1");
    assert_eq!(fields[4], "5");
    assert!(line.ends_with(" a.txt"));
}

#[test]
fn long_format_shows_symlink_target() {
    let dir = TestDir::new("symlink");
    fs::write(dir.path().join("target.txt"), "data").unwrap();
    symlink("target.txt", dir.path().join("link")).unwrap();

    let output = run(&dir, &["-l", "link"]);
    let text = stdout(&output);

    assert!(text.contains("link -> target.txt"));
    assert!(text.lines().next().unwrap().starts_with('l'));
}

#[test]
fn directory_flag_lists_directory_itself() {
    let dir = TestDir::new("directory");
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("sub/inside.txt"), "").unwrap();

    let output = run(&dir, &["-d", "sub"]);

    assert_eq!(stdout(&output), "sub\n");
}

#[test]
fn classify_appends_indicators() {
    let dir = TestDir::new("classify");
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("plain.txt"), "").unwrap();
    fs::write(dir.path().join("run.sh"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(dir.path().join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("plain.txt", dir.path().join("link")).unwrap();

    let output = run(&dir, &["-F", "-1"]);
    let text = stdout(&output);

    assert!(text.lines().any(|line| line == "sub/"));
    assert!(text.lines().any(|line| line == "run.sh*"));
    assert!(text.lines().any(|line| line == "link@"));
    assert!(text.lines().any(|line| line == "plain.txt"));
}

#[test]
fn recursive_lists_subdirectories() {
    let dir = TestDir::new("recursive");
    fs::write(dir.path().join("a.txt"), "").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("sub/b.txt"), "").unwrap();

    let output = run(&dir, &["-R", "-1"]);
    let text = stdout(&output);

    assert!(text.contains(".:\n"));
    assert!(text.contains("./sub:\n"));
    assert_eq!(text.matches("b.txt").count(), 1);
}

#[test]
fn sorts_by_size_and_reverses() {
    let dir = TestDir::new("size");
    fs::write(dir.path().join("small"), "x").unwrap();
    fs::write(dir.path().join("big"), vec![b'x'; 4096]).unwrap();
    fs::write(dir.path().join("medium"), vec![b'x'; 1024]).unwrap();

    let output = run(&dir, &["-S", "-1"]);
    assert_eq!(stdout(&output), "big\nmedium\nsmall\n");

    let output = run(&dir, &["-S", "-r", "-1"]);
    assert_eq!(stdout(&output), "small\nmedium\nbig\n");
}

#[test]
fn equal_size_ties_sort_by_name() {
    let dir = TestDir::new("size-ties");
    for name in ["c", "a", "b"] {
        fs::write(dir.path().join(name), "x").unwrap();
    }

    let output = run(&dir, &["-S", "-1"]);
    assert_eq!(stdout(&output), "a\nb\nc\n");
}

#[test]
fn human_readable_sizes() {
    let dir = TestDir::new("human");
    fs::write(dir.path().join("file"), vec![b'x'; 2048]).unwrap();

    let output = run(&dir, &["-l", "-h"]);
    let text = stdout(&output);

    assert!(text
        .lines()
        .any(|line| line.split_whitespace().nth(4) == Some("2.0K")));
}

#[test]
fn inode_column_contains_a_number() {
    let dir = TestDir::new("inode");
    fs::write(dir.path().join("file"), "").unwrap();

    let output = run(&dir, &["-i", "-1"]);
    let text = stdout(&output);
    let first = text.split_whitespace().next().unwrap();

    assert!(first.parse::<u64>().is_ok());
    assert!(text.ends_with(" file\n"));
}

#[test]
fn multiple_operands_get_headers() {
    let dir = TestDir::new("multi");
    fs::create_dir(dir.path().join("one")).unwrap();
    fs::create_dir(dir.path().join("two")).unwrap();
    fs::write(dir.path().join("one/a"), "").unwrap();
    fs::write(dir.path().join("two/b"), "").unwrap();

    let output = run(&dir, &["-1", "one", "two"]);
    let text = stdout(&output);

    assert!(text.starts_with("one:\na\n\ntwo:\nb\n"));
}

#[test]
fn missing_file_returns_error_status() {
    let dir = TestDir::new("missing");

    let output = run(&dir, &["does-not-exist"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot access"));
}

#[test]
fn command_line_symlink_to_directory_lists_contents() {
    let dir = TestDir::new("deref-directory");
    fs::create_dir(dir.path().join("target")).unwrap();
    fs::write(dir.path().join("target/inside.txt"), "").unwrap();
    symlink("target", dir.path().join("link")).unwrap();

    let output = run(&dir, &["-1", "link"]);
    assert_eq!(stdout(&output), "inside.txt\n");

    let output = run(&dir, &["-p", "-1", "link"]);
    assert_eq!(stdout(&output), "inside.txt\n");

    let output = run(&dir, &["-d", "link"]);
    assert_eq!(stdout(&output), "link\n");

    let output = run(&dir, &["-F", "-1", "link"]);
    assert_eq!(stdout(&output), "link@\n");

    let output = run(&dir, &["-l", "link"]);
    assert!(stdout(&output).starts_with('l'));
}

#[test]
fn last_hidden_flag_wins() {
    let dir = TestDir::new("hidden-precedence");
    fs::write(dir.path().join(".hidden"), "").unwrap();

    let output = run(&dir, &["-aA", "-1"]);
    let text = stdout(&output);
    assert!(text.lines().any(|line| line == ".hidden"));
    assert!(!text.lines().any(|line| line == "." || line == ".."));

    let output = run(&dir, &["-Aa", "-1"]);
    let text = stdout(&output);
    assert!(text.lines().any(|line| line == "."));
    assert!(text.lines().any(|line| line == ".."));
}

#[test]
fn last_indicator_flag_wins() {
    let dir = TestDir::new("indicator-precedence");
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("run.sh"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(dir.path().join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();

    let output = run(&dir, &["-Fp", "-1"]);
    let text = stdout(&output);
    assert!(text.lines().any(|line| line == "run.sh"));
    assert!(text.lines().any(|line| line == "sub/"));

    let output = run(&dir, &["-pF", "-1"]);
    let text = stdout(&output);
    assert!(text.lines().any(|line| line == "run.sh*"));
    assert!(text.lines().any(|line| line == "sub/"));
}

#[test]
fn sort_option_accepts_separate_argument() {
    let dir = TestDir::new("sort-argument");
    fs::write(dir.path().join("small"), "x").unwrap();
    fs::write(dir.path().join("big"), vec![b'x'; 1024]).unwrap();

    let output = run(&dir, &["--sort", "size", "-1"]);
    assert_eq!(stdout(&output), "big\nsmall\n");

    let output = run(&dir, &["--sort"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires an argument"));
}

#[test]
fn color_marks_executables() {
    let dir = TestDir::new("color-exec");
    fs::write(dir.path().join("run.sh"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(dir.path().join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();

    let output = run(&dir, &["--color=always", "-1"]);
    let text = stdout(&output);

    assert!(text.contains("\x1b[1;32mrun.sh\x1b[0m"));
}

#[test]
#[cfg(target_os = "linux")]
fn piped_output_preserves_non_utf8_names() {
    let dir = TestDir::new("non-utf8");
    let name = OsString::from_vec(b"f\xffo".to_vec());
    fs::write(dir.path().join(&name), "").unwrap();

    let output = run(&dir, &["-1"]);

    assert_eq!(output.stdout, b"f\xffo\n");
}

#[test]
fn newline_names_do_not_split_piped_output() {
    let dir = TestDir::new("newline-name");
    fs::write(dir.path().join("line\nbreak"), "").unwrap();

    let output = run(&dir, &["-1"]);

    assert_eq!(stdout(&output), "line?break\n");
}

#[test]
#[cfg(target_os = "linux")]
fn write_errors_return_failure() {
    let dir = TestDir::new("write-error");
    fs::write(dir.path().join("file"), "").unwrap();
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_ls"))
        .arg("-1")
        .current_dir(dir.path())
        .stdout(Stdio::from(full))
        .stderr(Stdio::null())
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(2));
}

#[test]
fn invalid_option_returns_status_2() {
    let dir = TestDir::new("invalid");

    let output = run(&dir, &["--definitely-not-an-option"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized option"));
}

#[test]
fn long_format_reads_symlink_targets_inside_listed_directories() {
    let dir = TestDir::new("symlink-dir");
    fs::create_dir_all(dir.path().join("sub/deeper")).unwrap();
    fs::write(dir.path().join("sub/target.txt"), "data").unwrap();
    symlink("target.txt", dir.path().join("sub/link")).unwrap();
    symlink("../target.txt", dir.path().join("sub/deeper/uplink")).unwrap();

    // Targets are read relative to the listed directory, not the cwd.
    let output = run(&dir, &["-l", "sub"]);
    assert!(stdout(&output).contains(" link -> target.txt\n"));

    // Recursion reaches nested directories through their parent's path.
    let output = run(&dir, &["-lR", "sub"]);
    let text = stdout(&output);
    assert!(text.contains("sub/deeper:\n"));
    assert!(text.contains(" uplink -> ../target.txt\n"));
}

#[test]
fn color_marks_each_kind_without_long_format() {
    let dir = TestDir::new("color-kinds");
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("plain.txt"), "").unwrap();
    fs::write(dir.path().join("run.sh"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(dir.path().join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("plain.txt", dir.path().join("link")).unwrap();
    symlink("missing", dir.path().join("broken")).unwrap();

    let output = run(&dir, &["--color=always", "-1"]);
    let text = stdout(&output);

    assert!(text.contains("\x1b[1;34msub\x1b[0m\n"));
    assert!(text.contains("\x1b[1;32mrun.sh\x1b[0m\n"));
    assert!(text.contains("\x1b[36mlink\x1b[0m\n"));
    assert!(text.contains("\x1b[1;31mbroken\x1b[0m\n"));
    assert!(text.lines().any(|line| line == "plain.txt"));
}

#[test]
fn group_directories_first_keeps_order_within_groups() {
    let dir = TestDir::new("dirs-first");
    for name in ["a.txt", "c.txt", "e.txt"] {
        fs::write(dir.path().join(name), "").unwrap();
    }
    for name in ["b", "d"] {
        fs::create_dir(dir.path().join(name)).unwrap();
    }

    let output = run(&dir, &["-1", "--group-directories-first"]);
    assert_eq!(stdout(&output), "b\nd\na.txt\nc.txt\ne.txt\n");

    let output = run(&dir, &["-1r", "--group-directories-first"]);
    assert_eq!(stdout(&output), "d\nb\ne.txt\nc.txt\na.txt\n");
}

/// Small directories are sorted with plain `strcoll` and large ones with
/// byte-order shortcuts. Both must agree on every pair of names.
#[test]
fn small_and_large_directories_sort_consistently() {
    let prefixes = [
        "file", "File", "FILE", "_file", "file.", "file-", "a", "A", "b", "B", "z", "Z", "0", "9",
        "$x", "café", "資料",
    ];
    let suffixes = [
        "", "1", "10", "2", ".txt", ".TXT", "_x", "-y", " z", "a", "B",
    ];
    let mut names: std::collections::BTreeSet<String> = prefixes
        .iter()
        .flat_map(|prefix| {
            suffixes
                .iter()
                .map(move |suffix| format!("{prefix}{suffix}"))
        })
        .collect();
    names.extend((0..120).map(|index| format!("n{index}")));
    let names: Vec<String> = names.into_iter().collect();
    let subset: Vec<&String> = names.iter().step_by(10).collect();
    assert!(names.len() >= 300 && subset.len() < 64);

    let large = TestDir::new("collate-large");
    let small = TestDir::new("collate-small");
    for name in &names {
        fs::write(large.path().join(name), "").unwrap();
    }
    for name in &subset {
        fs::write(small.path().join(name), "").unwrap();
    }

    let list = |dir: &TestDir| {
        let output = Command::new(env!("CARGO_BIN_EXE_ls"))
            .arg("-1")
            .current_dir(dir.path())
            .env("LANG", "en_US.UTF-8")
            .env_remove("LC_ALL")
            .env_remove("LC_COLLATE")
            .output()
            .expect("failed to run ls");
        assert!(output.status.success());
        stdout(&output)
    };

    let large_order = list(&large);
    let small_order = list(&small);
    let expected: Vec<&str> = large_order
        .lines()
        .filter(|line| subset.iter().any(|name| name.as_str() == *line))
        .collect();
    assert_eq!(small_order.lines().collect::<Vec<_>>(), expected);
}

/// Names that stress collation: families that tie at level 1 and differ
/// only in case or punctuation, `$` (weighted at level 1, unlike other
/// punctuation), names made only of ignored characters, and non-ASCII.
fn collation_corpus() -> Vec<String> {
    const PIECES: &[&str] = &[
        "a", "b", "m", "z", "A", "B", "M", "Z", "0", "1", "5", "9", "$", " ", "-", "_", ".", ",",
        "+", "(", ")", "[", "]", "'", "!", "@", "#", "%", "&", "~", "=", "é", "ü", "資", "メ",
    ];
    let mut names = std::collections::BTreeSet::new();
    for base in ["abc", "file1", "readme", "img2023", "x"] {
        names.insert(base.to_string());
        names.insert(base.to_uppercase());
        names.insert(base[..1].to_uppercase() + &base[1..]);
        for separator in ["_", "-", ".", " ", "$"] {
            for position in 0..=base.len() {
                names.insert(format!(
                    "{}{separator}{}",
                    &base[..position],
                    &base[position..]
                ));
            }
        }
    }

    // Deterministic xorshift so failures reproduce.
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    while names.len() < 3000 {
        let len = 1 + next() % 10;
        let name: String = (0..len)
            .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
            .collect();
        if name != "." && name != ".." {
            names.insert(name);
        }
    }
    names.into_iter().collect()
}

/// The listing must be in `strcoll` order, checked with this process's own
/// libc. Debug builds of ls additionally assert every comparison that its
/// sort keys decide against `strcoll`.
#[test]
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn name_order_matches_strcoll() {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int};

    extern "C" {
        fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
        fn strcoll(a: *const c_char, b: *const c_char) -> c_int;
    }
    const LC_COLLATE: c_int = 3;

    let locale = CString::new("en_US.UTF-8").unwrap();
    // Only this test uses locale-dependent libc functions in this process.
    if unsafe { setlocale(LC_COLLATE, locale.as_ptr()) }.is_null() {
        eprintln!("en_US.UTF-8 is not installed; skipping");
        return;
    }
    let collate = |a: &str, b: &str| {
        let (a, b) = (CString::new(a).unwrap(), CString::new(b).unwrap());
        unsafe { strcoll(a.as_ptr(), b.as_ptr()) }
    };

    let names = collation_corpus();
    let dir = TestDir::new("strcoll-order");
    for name in &names {
        fs::write(dir.path().join(name), "").unwrap();
    }

    let output = Command::new(env!("CARGO_BIN_EXE_ls"))
        .arg("-1A")
        .current_dir(dir.path())
        .env("LANG", "en_US.UTF-8")
        .env_remove("LC_ALL")
        .env_remove("LC_COLLATE")
        .output()
        .expect("failed to run ls");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let listed: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .collect();
    let mut expected_set: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut listed_set = listed.clone();
    expected_set.sort_unstable();
    listed_set.sort_unstable();
    assert_eq!(
        listed_set, expected_set,
        "listing is not a permutation of the created names"
    );
    for pair in listed.windows(2) {
        assert!(
            collate(pair[0], pair[1]) <= 0,
            "{:?} listed before {:?}",
            pair[0],
            pair[1]
        );
    }
}

/// Build a tree `fanout` wide and `depth` deep with `files` files per
/// directory, and return the `ls -R -1` output a serial depth-first
/// traversal must produce. Names are lowercase ASCII with fixed-width
/// numbers, so byte order and collation order agree.
fn build_tree(root: &Path, fanout: usize, depth: usize, files: usize) -> String {
    fn visit(dir: &Path, shown: &str, fanout: usize, depth: usize, files: usize, out: &mut String) {
        let mut names: Vec<String> = (0..files).map(|index| format!("f{index:02}")).collect();
        let subdirs: Vec<String> = if depth == 0 {
            Vec::new()
        } else {
            (0..fanout).map(|index| format!("d{index:02}")).collect()
        };
        for name in &names {
            fs::write(dir.join(name), "").unwrap();
        }
        for name in &subdirs {
            fs::create_dir(dir.join(name)).unwrap();
        }
        names.extend(subdirs.iter().cloned());
        names.sort();

        out.push_str(&format!("{shown}:\n"));
        for name in &names {
            out.push_str(name);
            out.push('\n');
        }
        for name in &subdirs {
            out.push('\n');
            visit(
                &dir.join(name),
                &format!("{shown}/{name}"),
                fanout,
                depth - 1,
                files,
                out,
            );
        }
    }

    let mut expected = String::new();
    visit(root, ".", fanout, depth, files, &mut expected);
    expected
}

#[test]
fn recursive_output_is_deterministic_depth_first() {
    let dir = TestDir::new("walk-order");
    let expected = build_tree(dir.path(), 4, 4, 6); // 341 directories

    for _ in 0..5 {
        let output = Command::new(env!("CARGO_BIN_EXE_ls"))
            .args(["-R", "-1"])
            .current_dir(dir.path())
            .env("LC_ALL", "C")
            .output()
            .expect("failed to run ls");
        assert!(output.status.success());
        assert_eq!(stdout(&output), expected);
    }
}

#[test]
fn unreadable_subdirectory_is_reported_in_place() {
    let dir = TestDir::new("walk-error");
    build_tree(dir.path(), 3, 2, 2);
    let locked = dir.path().join("d01");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        // Running with CAP_DAC_OVERRIDE (e.g. as root): nothing to test.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_ls"))
        .args(["-R", "-1"])
        .current_dir(dir.path())
        .env("LC_ALL", "C")
        .output()
        .expect("failed to run ls");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("cannot open directory './d01'").count(),
        1,
        "{stderr}"
    );
    let text = stdout(&output);
    // The locked directory keeps its place: its header follows d00's
    // subtree, and d02's subtree is still listed after it.
    let d00 = text.find("./d00/d02:\n").expect("d00 subtree listed");
    let d01 = text.find("./d01:\n").expect("d01 header printed");
    let d02 = text.find("./d02/d00:\n").expect("d02 subtree listed");
    assert!(d00 < d01 && d01 < d02);
    assert!(!text.contains("./d01/"));
}

#[test]
fn recursive_listing_stops_when_output_closes() {
    let dir = TestDir::new("walk-epipe");
    // 259 directories and ~6k long-format lines: ~300 KB, far more than a
    // pipe buffer holds, so ls is still writing when the reader goes away.
    build_tree(dir.path(), 6, 3, 20);

    let mut child = Command::new(env!("CARGO_BIN_EXE_ls"))
        .args(["-lR"])
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to run ls");
    let mut first = [0u8; 16];
    std::io::Read::read_exact(child.stdout.as_mut().unwrap(), &mut first).unwrap();
    drop(child.stdout.take());

    // ls must notice the closed pipe and exit, not hang waiting on workers.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(2));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ls did not exit after EPIPE"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Large long listings render in parallel chunks; columns must still be
/// sized over the whole listing, not per chunk.
#[test]
fn large_long_listing_aligns_columns_across_chunks() {
    let dir = TestDir::new("long-chunks");
    let mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    for index in 0..5000u64 {
        let file = fs::File::create(dir.path().join(format!("f{index:05}"))).unwrap();
        // One huge file at the end makes the size column widest there.
        file.set_len(if index == 4999 {
            10_000_000_000
        } else {
            index % 7
        })
        .unwrap();
        file.set_modified(mtime).unwrap();
    }

    let output = Command::new(env!("CARGO_BIN_EXE_ls"))
        .args(["-l"])
        .current_dir(dir.path())
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .expect("failed to run ls");
    assert!(output.status.success());
    let text = stdout(&output);
    let rows: Vec<&str> = text.lines().skip(1).collect();
    assert_eq!(rows.len(), 5000);
    assert!(
        rows.iter().all(|row| row.len() == rows[0].len()),
        "misaligned rows"
    );
    for (index, row) in rows.iter().enumerate() {
        assert!(row.ends_with(&format!(" f{index:05}")), "{row}");
    }
}

#[test]
fn operands_are_sorted_like_gnu() {
    let dir = TestDir::new("operand-order");
    for name in ["c.txt", "a.txt", "b.txt"] {
        fs::write(dir.path().join(name), name.repeat(10)).unwrap();
    }
    fs::write(dir.path().join("big"), vec![b'x'; 4000]).unwrap();
    for name in ["zdir", "adir"] {
        fs::create_dir(dir.path().join(name)).unwrap();
        fs::write(dir.path().join(name).join("inner"), "").unwrap();
    }

    let output = run(&dir, &["-1", "c.txt", "zdir", "a.txt", "adir", "b.txt"]);
    assert_eq!(
        stdout(&output),
        "a.txt\nb.txt\nc.txt\n\nadir:\ninner\n\nzdir:\ninner\n"
    );

    let output = run(&dir, &["-1S", "a.txt", "big", "b.txt"]);
    assert_eq!(stdout(&output), "big\na.txt\nb.txt\n");
}

#[test]
fn block_column_is_aligned_outside_long_format() {
    let dir = TestDir::new("blocks-aligned");
    fs::write(dir.path().join("empty"), "").unwrap();
    fs::write(dir.path().join("large"), vec![b'x'; 200_000]).unwrap();

    let output = run(&dir, &["-1s"]);
    let text = stdout(&output);
    let rows: Vec<&str> = text.lines().skip(1).collect();
    let starts: Vec<usize> = rows
        .iter()
        .map(|row| row.len() - row.trim_start().split_once(' ').unwrap().1.len())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(starts[0], starts[1], "names not aligned: {rows:?}");
    assert!(
        rows[0].starts_with(' '),
        "small count not right-aligned: {rows:?}"
    );
}

#[test]
fn group_directories_first_includes_symlinks_to_directories() {
    let dir = TestDir::new("dirs-first-links");
    fs::create_dir(dir.path().join("real")).unwrap();
    fs::write(dir.path().join("file"), "").unwrap();
    symlink("real", dir.path().join("alias")).unwrap();
    symlink("missing", dir.path().join("broken")).unwrap();

    for args in [
        ["-1", "--group-directories-first"],
        ["-l", "--group-directories-first"],
    ] {
        let output = run(&dir, &args);
        let names: Vec<String> = stdout(&output)
            .lines()
            .filter(|line| !line.starts_with("total"))
            .map(|line| {
                let line = line.split(" -> ").next().unwrap();
                line.rsplit(' ').next().unwrap().to_string()
            })
            .collect();
        assert_eq!(names, ["alias", "real", "broken", "file"], "{args:?}");
    }
}

#[test]
fn file_modified_just_now_shows_time_of_day() {
    let dir = TestDir::new("fresh");
    fs::write(dir.path().join("new"), "").unwrap();

    let output = run(&dir, &["-l", "new"]);
    let text = stdout(&output);
    let fields: Vec<&str> = text.split_whitespace().collect();
    let time = fields[fields.len() - 2];
    assert!(time.contains(':'), "expected a time of day, got {text:?}");
}
