use std::fs;
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
