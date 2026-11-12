use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process;

use crate::VERSION;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ColorWhen {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Time,
    Size,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IgnoreMode {
    Default,
    All,
    AlmostAll,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IndicatorStyle {
    None,
    Classify,
    Slash,
}

pub struct Options {
    pub ignore: IgnoreMode,
    pub long: bool,
    pub human: bool,
    pub reverse: bool,
    pub sort: SortKey,
    pub recursive: bool,
    pub directory: bool,
    pub indicator: IndicatorStyle,
    pub one_per_line: bool,
    pub inode: bool,
    pub blocks: bool,
    pub dirs_first: bool,
    pub color: ColorWhen,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            ignore: IgnoreMode::Default,
            long: false,
            human: false,
            reverse: false,
            sort: SortKey::Name,
            recursive: false,
            directory: false,
            indicator: IndicatorStyle::None,
            one_per_line: false,
            inode: false,
            blocks: false,
            dirs_first: false,
            color: ColorWhen::Auto,
        }
    }
}

pub fn parse_args(args: Vec<OsString>) -> Result<(Options, Vec<PathBuf>), String> {
    let mut opts = Options::default();
    let mut operands = Vec::new();
    let mut only_paths = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        let bytes = arg.as_bytes();

        if only_paths || bytes.is_empty() || bytes == b"-" || bytes[0] != b'-' {
            operands.push(PathBuf::from(arg));
            continue;
        }

        if bytes == b"--" {
            only_paths = true;
            continue;
        }

        if let Some(long) = bytes.strip_prefix(b"--") {
            let long = String::from_utf8_lossy(long).into_owned();
            if long == "sort" {
                let value = args
                    .next()
                    .ok_or_else(|| "option '--sort' requires an argument".to_string())?;
                opts.sort = parse_sort_word(&value.to_string_lossy())?;
                continue;
            }
            apply_long_option(&long, &mut opts)?;
            continue;
        }

        for &byte in &bytes[1..] {
            match byte {
                b'a' => opts.ignore = IgnoreMode::All,
                b'A' => opts.ignore = IgnoreMode::AlmostAll,
                b'l' => opts.long = true,
                b'h' => opts.human = true,
                b'r' => opts.reverse = true,
                b't' => opts.sort = SortKey::Time,
                b'S' => opts.sort = SortKey::Size,
                b'R' => opts.recursive = true,
                b'd' => opts.directory = true,
                b'F' => opts.indicator = IndicatorStyle::Classify,
                b'1' => opts.one_per_line = true,
                b'i' => opts.inode = true,
                b's' => opts.blocks = true,
                b'p' => opts.indicator = IndicatorStyle::Slash,
                b'U' => opts.sort = SortKey::None,
                other => return Err(format!("invalid option -- '{}'", other as char)),
            }
        }
    }

    Ok((opts, operands))
}

fn parse_sort_word(word: &str) -> Result<SortKey, String> {
    match word {
        "name" => Ok(SortKey::Name),
        "time" => Ok(SortKey::Time),
        "size" => Ok(SortKey::Size),
        "none" => Ok(SortKey::None),
        other => Err(format!("invalid argument '{}' for '--sort'", other)),
    }
}

pub fn apply_long_option(option: &str, opts: &mut Options) -> Result<(), String> {
    let (name, value) = match option.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (option, None),
    };

    match name {
        "color" => {
            let when = value.unwrap_or("always");
            opts.color = match when {
                "always" | "yes" | "force" => ColorWhen::Always,
                "never" | "no" | "none" => ColorWhen::Never,
                "auto" | "tty" | "if-tty" => ColorWhen::Auto,
                other => return Err(format!("invalid argument '{}' for '--color'", other)),
            };
        }
        "sort" => {
            let word = value.ok_or_else(|| "option '--sort' requires an argument".to_string())?;
            opts.sort = parse_sort_word(word)?;
        }
        _ => {
            if let Some(unexpected) = value {
                return Err(format!(
                    "option '--{}' doesn't allow an argument (got '{}')",
                    name, unexpected
                ));
            }
            match name {
                "all" => opts.ignore = IgnoreMode::All,
                "almost-all" => opts.ignore = IgnoreMode::AlmostAll,
                "long" => opts.long = true,
                "human-readable" => opts.human = true,
                "reverse" => opts.reverse = true,
                "recursive" => opts.recursive = true,
                "directory" => opts.directory = true,
                "classify" => opts.indicator = IndicatorStyle::Classify,
                "inode" => opts.inode = true,
                "size" => opts.blocks = true,
                "group-directories-first" => opts.dirs_first = true,
                "help" => {
                    print_help();
                    process::exit(0);
                }
                "version" => {
                    println!("ls (rust) {}", VERSION);
                    process::exit(0);
                }
                _ => return Err(format!("unrecognized option '--{}'", name)),
            }
        }
    }

    Ok(())
}

pub fn print_help() {
    print!(
        "\
Usage: ls [OPTION]... [FILE]...

List information about the FILEs (the current directory by default).
Sort entries alphabetically if none of -t, -S nor -U is given.

  -a, --all                 do not ignore entries starting with .
  -A, --almost-all          do not list implied . and ..
  -d, --directory           list directories themselves, not their contents
  -F, --classify            append indicator (one of */=@|) to entries
  -h, --human-readable      with -l and -s, print sizes like 1K 234M 2G
  -i, --inode               print the index number of each file
  -l, --long                use a long listing format
  -p                        append / indicator to directories
  -r, --reverse             reverse order while sorting
  -R, --recursive           list subdirectories recursively
  -s, --size                print the allocated size of each file, in blocks
  -t                        sort by time, newest first
  -S                        sort by file size, largest first
  -U                        do not sort; list entries in directory order
  -1                        list one file per line
      --color[=WHEN]        colorize the output; WHEN is always, auto, never
      --group-directories-first
                            group directories before files
      --sort=WORD           sort by WORD: name, time, size, none
      --help                display this help and exit
      --version             output version information and exit

Exit status:
 0  if OK,
 1  if minor problems (e.g., cannot access subdirectory),
 2  if serious trouble (e.g., invalid command line argument).
"
    );
}
