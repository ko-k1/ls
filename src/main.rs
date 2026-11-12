use std::cmp::Ordering;
use std::collections::HashMap;
use std::env;
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, Metadata};
use std::io::{self, IsTerminal, Write};
use std::os::raw::{c_char, c_int, c_long, c_ulong};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorWhen {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Time,
    Size,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IgnoreMode {
    Default,
    All,
    AlmostAll,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IndicatorStyle {
    None,
    Classify,
    Slash,
}

struct Options {
    ignore: IgnoreMode,
    long: bool,
    human: bool,
    reverse: bool,
    sort: SortKey,
    recursive: bool,
    directory: bool,
    indicator: IndicatorStyle,
    one_per_line: bool,
    inode: bool,
    blocks: bool,
    dirs_first: bool,
    color: ColorWhen,
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

fn main() {
    init_locale();

    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let (opts, operands) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("ls: {}", err);
            eprintln!("Try 'ls --help' for more information.");
            std::process::exit(2);
        }
    };

    let mut lister = Lister::new(opts);
    let mut code = lister.run(operands);

    let output_error = match lister.output_error.take() {
        Some(err) => Some(err),
        None => lister.out.flush().err(),
    };
    if let Some(err) = output_error {
        eprintln!("ls: write error: {}", err);
        code = 2;
    }

    std::process::exit(code);
}

fn parse_args(args: Vec<OsString>) -> Result<(Options, Vec<PathBuf>), String> {
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

fn apply_long_option(option: &str, opts: &mut Options) -> Result<(), String> {
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
                    std::process::exit(0);
                }
                "version" => {
                    println!("ls (rust) {}", VERSION);
                    std::process::exit(0);
                }
                _ => return Err(format!("unrecognized option '--{}'", name)),
            }
        }
    }

    Ok(())
}

fn print_help() {
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

struct Entry {
    name: OsString,
    path: PathBuf,
    meta: Metadata,
    target: Option<Metadata>,
}

#[derive(Clone, Copy)]
enum Kind {
    Regular,
    Dir,
    Link,
    Broken,
    Executable,
    Fifo,
    Socket,
    Device,
}

fn kind_of(meta: &Metadata) -> Kind {
    let ft = meta.file_type();
    if ft.is_dir() {
        Kind::Dir
    } else if ft.is_symlink() {
        Kind::Link
    } else if ft.is_fifo() {
        Kind::Fifo
    } else if ft.is_socket() {
        Kind::Socket
    } else if ft.is_block_device() || ft.is_char_device() {
        Kind::Device
    } else if ft.is_file() && meta.mode() & 0o111 != 0 {
        Kind::Executable
    } else {
        Kind::Regular
    }
}

fn entry_kind(entry: &Entry) -> Kind {
    if entry.meta.file_type().is_symlink() {
        if entry.target.is_some() {
            Kind::Link
        } else {
            Kind::Broken
        }
    } else {
        kind_of(&entry.meta)
    }
}

fn color_code(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::Regular => None,
        Kind::Dir => Some("1;34"),
        Kind::Link => Some("36"),
        Kind::Broken => Some("1;31"),
        Kind::Executable => Some("1;32"),
        Kind::Fifo => Some("33"),
        Kind::Socket => Some("1;35"),
        Kind::Device => Some("1;33"),
    }
}

fn paint(kind: Kind, text: &str) -> String {
    match color_code(kind) {
        Some(code) => format!("\x1b[{}m{}\x1b[0m", code, text),
        None => text.to_string(),
    }
}

struct Lister {
    opts: Options,
    color: bool,
    tty: bool,
    out: io::BufWriter<io::Stdout>,
    had_error: bool,
    serious_error: bool,
    output_error: Option<io::Error>,
}

impl Lister {
    fn new(opts: Options) -> Self {
        let tty = io::stdout().is_terminal();
        let color = match opts.color {
            ColorWhen::Always => true,
            ColorWhen::Never => false,
            ColorWhen::Auto => tty && env::var_os("NO_COLOR").is_none(),
        };

        Self {
            opts,
            color,
            tty,
            out: io::BufWriter::new(io::stdout()),
            had_error: false,
            serious_error: false,
            output_error: None,
        }
    }

    fn write_line(&mut self, line: &str) {
        if self.output_error.is_some() {
            return;
        }
        if let Err(err) = writeln!(self.out, "{}", line) {
            self.output_error = Some(err);
        }
    }

    fn run(&mut self, mut operands: Vec<PathBuf>) -> i32 {
        if operands.is_empty() {
            operands.push(PathBuf::from("."));
        }

        let show_headers = self.opts.recursive || operands.len() > 1;
        let follow_operand_symlink = !self.opts.directory
            && !self.opts.long
            && self.opts.indicator != IndicatorStyle::Classify;
        let mut files = Vec::new();
        let mut dirs = Vec::new();

        for operand in &operands {
            match fs::symlink_metadata(operand) {
                Ok(meta) => {
                    let mut is_dir = meta.is_dir();
                    if !is_dir && follow_operand_symlink && meta.file_type().is_symlink() {
                        is_dir = fs::metadata(operand)
                            .map(|target| target.is_dir())
                            .unwrap_or(false);
                    }

                    if is_dir && !self.opts.directory {
                        dirs.push(operand.clone());
                    } else {
                        let target = if meta.file_type().is_symlink() {
                            fs::metadata(operand).ok()
                        } else {
                            None
                        };
                        files.push(Entry {
                            name: operand.as_os_str().to_owned(),
                            path: operand.clone(),
                            meta,
                            target,
                        });
                    }
                }
                Err(err) => {
                    self.serious_error = true;
                    eprintln!(
                        "ls: cannot access {}: {}",
                        quote_for_message(&operand.to_string_lossy()),
                        err
                    );
                }
            }
        }

        let mut first_block = true;
        if !files.is_empty() {
            self.print_entries(&files, false);
            first_block = false;
        }

        for dir in &dirs {
            if self.output_error.is_some() {
                break;
            }
            self.list_dir_tree(dir, show_headers, &mut first_block, true);
        }

        if self.serious_error {
            2
        } else if self.had_error {
            1
        } else {
            0
        }
    }

    fn list_dir_tree(
        &mut self,
        path: &Path,
        show_header: bool,
        first_block: &mut bool,
        serious: bool,
    ) {
        if show_header {
            if !*first_block {
                self.write_line("");
            }
            self.write_line(&format!(
                "{}:",
                quote_name(&path.to_string_lossy(), self.tty)
            ));
            *first_block = false;
        }

        if self.output_error.is_some() {
            return;
        }

        let subdirs = self.list_one_dir(path, serious);

        if self.opts.recursive {
            for subdir in subdirs {
                if self.output_error.is_some() {
                    break;
                }
                self.list_dir_tree(&subdir, true, first_block, false);
            }
        }
    }

    fn list_one_dir(&mut self, path: &Path, serious: bool) -> Vec<PathBuf> {
        let read_dir = match fs::read_dir(path) {
            Ok(read_dir) => read_dir,
            Err(err) => {
                if serious {
                    self.serious_error = true;
                } else {
                    self.had_error = true;
                }
                eprintln!(
                    "ls: cannot open directory {}: {}",
                    quote_for_message(&path.to_string_lossy()),
                    err
                );
                return Vec::new();
            }
        };

        let mut entries = Vec::new();

        if self.opts.ignore == IgnoreMode::All {
            for dot in [".", ".."] {
                let dot_path = path.join(dot);
                if let Ok(meta) = fs::symlink_metadata(&dot_path) {
                    let target = if meta.file_type().is_symlink() {
                        fs::metadata(&dot_path).ok()
                    } else {
                        None
                    };
                    entries.push(Entry {
                        name: OsString::from(dot),
                        path: dot_path,
                        meta,
                        target,
                    });
                }
            }
        }

        for item in read_dir {
            let dir_entry = match item {
                Ok(dir_entry) => dir_entry,
                Err(err) => {
                    if serious {
                        self.serious_error = true;
                    } else {
                        self.had_error = true;
                    }
                    eprintln!(
                        "ls: reading directory {}: {}",
                        quote_for_message(&path.to_string_lossy()),
                        err
                    );
                    continue;
                }
            };

            let name = dir_entry.file_name();
            if self.opts.ignore == IgnoreMode::Default && name.as_bytes().first() == Some(&b'.') {
                continue;
            }

            match dir_entry.metadata() {
                Ok(meta) => {
                    let entry_path = dir_entry.path();
                    let target = if meta.file_type().is_symlink() {
                        fs::metadata(&entry_path).ok()
                    } else {
                        None
                    };
                    entries.push(Entry {
                        name,
                        path: entry_path,
                        meta,
                        target,
                    });
                }
                Err(err) => {
                    self.had_error = true;
                    eprintln!(
                        "ls: cannot access {}: {}",
                        quote_for_message(&dir_entry.path().to_string_lossy()),
                        err
                    );
                }
            }
        }

        self.sort_entries(&mut entries);
        self.print_entries(&entries, true);

        if self.opts.recursive {
            entries
                .into_iter()
                .filter(|entry| {
                    let name = entry.name.as_os_str();
                    entry.meta.file_type().is_dir()
                        && name != OsStr::new(".")
                        && name != OsStr::new("..")
                })
                .map(|entry| entry.path)
                .collect()
        } else {
            Vec::new()
        }
    }

    fn sort_entries(&self, entries: &mut [Entry]) {
        match self.opts.sort {
            SortKey::None => {}
            SortKey::Name => {
                entries.sort_by(|a, b| collate_names(a.name.as_os_str(), b.name.as_os_str()));
            }
            SortKey::Time => {
                entries.sort_by(|a, b| {
                    b.meta
                        .mtime()
                        .cmp(&a.meta.mtime())
                        .then_with(|| b.meta.mtime_nsec().cmp(&a.meta.mtime_nsec()))
                        .then_with(|| collate_names(a.name.as_os_str(), b.name.as_os_str()))
                });
            }
            SortKey::Size => {
                entries.sort_by(|a, b| {
                    b.meta
                        .size()
                        .cmp(&a.meta.size())
                        .then_with(|| collate_names(a.name.as_os_str(), b.name.as_os_str()))
                });
            }
        }

        if self.opts.reverse {
            entries.reverse();
        }

        if self.opts.dirs_first && self.opts.sort != SortKey::None {
            entries.sort_by_key(|entry| !entry.meta.file_type().is_dir());
        }
    }

    fn print_entries(&mut self, entries: &[Entry], from_directory: bool) {
        if from_directory && (self.opts.long || self.opts.blocks) {
            let total: u64 = entries.iter().map(|entry| entry.meta.blocks()).sum();
            self.write_line(&format!("total {}", format_blocks(total, self.opts.human)));
        }

        if entries.is_empty() || self.output_error.is_some() {
            return;
        }

        if self.opts.long {
            self.print_long(entries);
        } else {
            self.print_short(entries);
        }
    }

    fn suffix(&self, entry: &Entry) -> &'static str {
        let ft = entry.meta.file_type();
        if ft.is_symlink() {
            return if self.opts.indicator == IndicatorStyle::Classify {
                "@"
            } else {
                ""
            };
        }
        if ft.is_dir() {
            return if self.opts.indicator != IndicatorStyle::None {
                "/"
            } else {
                ""
            };
        }
        if self.opts.indicator != IndicatorStyle::Classify {
            return "";
        }
        if ft.is_file() && entry.meta.mode() & 0o111 != 0 {
            "*"
        } else if ft.is_fifo() {
            "|"
        } else if ft.is_socket() {
            "="
        } else {
            ""
        }
    }

    fn paint_entry(&self, entry: &Entry, text: &str) -> String {
        if !self.color {
            return text.to_string();
        }
        paint(entry_kind(entry), text)
    }

    fn paint_target(&self, entry: &Entry, text: &str) -> String {
        if !self.color {
            return text.to_string();
        }
        let kind = match &entry.target {
            Some(meta) => kind_of(meta),
            None => Kind::Broken,
        };
        paint(kind, text)
    }

    fn short_item(&self, entry: &Entry) -> DisplayItem {
        let name = quote_name(&entry.name.to_string_lossy(), self.tty);
        let suffix = self.suffix(entry);
        let mut prefix = String::new();

        if self.opts.inode {
            prefix.push_str(&entry.meta.ino().to_string());
            prefix.push(' ');
        }
        if self.opts.blocks {
            prefix.push_str(&format_blocks(entry.meta.blocks(), self.opts.human));
            prefix.push(' ');
        }

        let width = prefix.chars().count() + name.chars().count() + suffix.chars().count();
        let mut text = prefix;
        text.push_str(&self.paint_entry(entry, &name));
        text.push_str(suffix);

        DisplayItem { text, width }
    }

    fn print_short(&mut self, entries: &[Entry]) {
        let items: Vec<DisplayItem> = entries.iter().map(|entry| self.short_item(entry)).collect();

        if self.opts.one_per_line || !self.tty {
            for item in &items {
                if self.output_error.is_some() {
                    break;
                }
                self.write_line(&item.text);
            }
            return;
        }

        let width = terminal_width();
        let layout = layout_columns(&items, width);

        for row in &layout.rows {
            if self.output_error.is_some() {
                break;
            }

            let mut line = String::new();

            for (position, &(index, column)) in row.iter().enumerate() {
                let item = &items[index];
                line.push_str(&item.text);

                if position + 1 < row.len() {
                    let padding = layout.widths[column].saturating_sub(item.width) + 2;
                    for _ in 0..padding {
                        line.push(' ');
                    }
                }
            }

            self.write_line(&line);
        }
    }

    fn print_long(&mut self, entries: &[Entry]) {
        struct Row {
            inode: String,
            blocks: String,
            permissions: String,
            links: String,
            owner: String,
            group: String,
            size: String,
            date: String,
            name: String,
        }

        let users = user_map();
        let groups = group_map();
        let mut rows = Vec::with_capacity(entries.len());

        for entry in entries {
            let meta = &entry.meta;
            let uid = meta.uid();
            let gid = meta.gid();

            let owner = users.get(&uid).cloned().unwrap_or_else(|| uid.to_string());
            let group = groups.get(&gid).cloned().unwrap_or_else(|| gid.to_string());

            let size = if self.opts.human {
                human_size(meta.size())
            } else {
                meta.size().to_string()
            };

            let blocks = format_blocks(meta.blocks(), self.opts.human);

            let quoted_name = quote_name(&entry.name.to_string_lossy(), self.tty);
            let mut name = self.paint_entry(entry, &quoted_name);
            name.push_str(self.suffix(entry));

            if meta.file_type().is_symlink() {
                if let Ok(target) = fs::read_link(&entry.path) {
                    let target = quote_name(&target.to_string_lossy(), self.tty);
                    name.push_str(" -> ");
                    name.push_str(&self.paint_target(entry, &target));
                }
            }

            rows.push(Row {
                inode: meta.ino().to_string(),
                blocks,
                permissions: permissions(meta),
                links: meta.nlink().to_string(),
                owner,
                group,
                size,
                date: format_time(meta.mtime()),
                name,
            });
        }

        let width_inode = max_width(rows.iter().map(|row| row.inode.as_str()));
        let width_blocks = max_width(rows.iter().map(|row| row.blocks.as_str()));
        let width_links = max_width(rows.iter().map(|row| row.links.as_str()));
        let width_owner = max_width(rows.iter().map(|row| row.owner.as_str()));
        let width_group = max_width(rows.iter().map(|row| row.group.as_str()));
        let width_size = max_width(rows.iter().map(|row| row.size.as_str()));

        for row in rows {
            if self.output_error.is_some() {
                break;
            }

            let mut line = String::new();

            if self.opts.inode {
                line.push_str(&format!("{:>width$} ", row.inode, width = width_inode));
            }
            if self.opts.blocks {
                line.push_str(&format!("{:>width$} ", row.blocks, width = width_blocks));
            }

            line.push_str(&row.permissions);
            line.push_str(&format!(
                " {:>width$} {} {} ",
                row.links,
                pad_right(&row.owner, width_owner),
                pad_right(&row.group, width_group),
                width = width_links
            ));
            line.push_str(&format!("{:>width$} ", row.size, width = width_size));
            line.push_str(&row.date);
            line.push(' ');
            line.push_str(&row.name);

            self.write_line(&line);
        }
    }
}

struct DisplayItem {
    text: String,
    width: usize,
}

struct Layout {
    rows: Vec<Vec<(usize, usize)>>,
    widths: Vec<usize>,
}

fn layout_columns(items: &[DisplayItem], max_width: usize) -> Layout {
    let count = items.len();
    if count == 0 {
        return Layout {
            rows: Vec::new(),
            widths: Vec::new(),
        };
    }

    let max_columns = max_width.div_ceil(3).max(1).min(count);

    for columns in (1..=max_columns).rev() {
        let rows = count.div_ceil(columns);
        if count.div_ceil(rows) != columns {
            continue;
        }

        let mut widths = vec![0usize; columns];
        for (index, item) in items.iter().enumerate() {
            let column = index / rows;
            widths[column] = widths[column].max(item.width);
        }

        let total: usize = widths.iter().sum::<usize>() + 2 * (columns - 1);
        if total <= max_width || columns == 1 {
            let mut result = vec![Vec::new(); rows];
            for (row, cells) in result.iter_mut().enumerate() {
                for column in 0..widths.len() {
                    let index = column * rows + row;
                    if index < count {
                        cells.push((index, column));
                    }
                }
            }
            return Layout {
                rows: result,
                widths,
            };
        }
    }

    Layout {
        rows: (0..count).map(|index| vec![(index, 0)]).collect(),
        widths: vec![items.iter().map(|item| item.width).max().unwrap_or(1)],
    }
}

fn max_width<'a>(values: impl Iterator<Item = &'a str>) -> usize {
    values.map(|value| value.chars().count()).max().unwrap_or(1)
}

fn pad_right(value: &str, width: usize) -> String {
    let length = value.chars().count();
    let mut result = String::with_capacity(value.len() + width.saturating_sub(length));
    result.push_str(value);
    for _ in length..width {
        result.push(' ');
    }
    result
}

fn permissions(meta: &Metadata) -> String {
    let ft = meta.file_type();
    let kind = if ft.is_dir() {
        'd'
    } else if ft.is_symlink() {
        'l'
    } else if ft.is_fifo() {
        'p'
    } else if ft.is_socket() {
        's'
    } else if ft.is_block_device() {
        'b'
    } else if ft.is_char_device() {
        'c'
    } else {
        '-'
    };

    let mode = meta.mode();
    let mut result = String::with_capacity(10);
    result.push(kind);

    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];

    for (index, (mask, character)) in bits.iter().enumerate() {
        let mut ch = if mode & mask != 0 { *character } else { '-' };

        match index {
            2 if mode & 0o4000 != 0 => {
                ch = if ch == 'x' { 's' } else { 'S' };
            }
            5 if mode & 0o2000 != 0 => {
                ch = if ch == 'x' { 's' } else { 'S' };
            }
            8 if mode & 0o1000 != 0 => {
                ch = if ch == 'x' { 't' } else { 'T' };
            }
            _ => {}
        }

        result.push(ch);
    }

    result
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "K", "M", "G", "T", "P", "E"];

    if bytes < 1024 {
        return bytes.to_string();
    }

    let mut value = bytes as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if value < 10.0 {
        format!("{:.1}{}", value, UNITS[unit])
    } else {
        format!("{:.0}{}", value, UNITS[unit])
    }
}

fn format_blocks(blocks: u64, human: bool) -> String {
    if human {
        human_size(blocks.saturating_mul(512))
    } else {
        (blocks / 2).to_string()
    }
}

fn quote_name(name: &str, tty: bool) -> String {
    if !tty {
        return name.replace('\n', "?");
    }

    if name.is_empty() {
        return "''".to_string();
    }

    if !needs_shell_quoting(name) {
        return name.to_string();
    }

    if name.chars().any(char::is_control) {
        let mut result = String::from("$'");

        for ch in name.chars() {
            match ch {
                '\x07' => result.push_str("\\a"),
                '\x08' => result.push_str("\\b"),
                '\t' => result.push_str("\\t"),
                '\n' => result.push_str("\\n"),
                '\x0b' => result.push_str("\\v"),
                '\x0c' => result.push_str("\\f"),
                '\r' => result.push_str("\\r"),
                '\\' => result.push_str("\\\\"),
                '\'' => result.push_str("\\'"),
                ch if ch.is_ascii_control() => {
                    use std::fmt::Write as _;
                    let _ = write!(result, "\\{:03o}", ch as u32);
                }
                ch if ch.is_control() => {
                    use std::fmt::Write as _;
                    let _ = write!(result, "\\u{{{:x}}}", ch as u32);
                }
                ch => result.push(ch),
            }
        }

        result.push('\'');
        return result;
    }

    if !name.contains('\'') {
        return format!("'{}'", name);
    }

    if !name
        .chars()
        .any(|ch| matches!(ch, '"' | '$' | '`' | '\\' | '!'))
    {
        return format!("\"{}\"", name);
    }

    let mut result = String::new();
    for (index, part) in name.split('\'').enumerate() {
        if index > 0 {
            result.push_str("\\'");
        }
        if !part.is_empty() {
            result.push('\'');
            result.push_str(part);
            result.push('\'');
        }
    }

    if result.is_empty() {
        "''".to_string()
    } else {
        result
    }
}

fn needs_shell_quoting(name: &str) -> bool {
    name.chars().any(|ch| {
        ch.is_control()
            || ch.is_whitespace()
            || matches!(
                ch,
                '\'' | '"'
                    | '\\'
                    | '$'
                    | '`'
                    | '&'
                    | ';'
                    | '|'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '*'
                    | '?'
                    | '#'
                    | '~'
                    | '!'
            )
    })
}

fn quote_for_message(name: &str) -> String {
    let quoted = quote_name(name, true);
    if quoted.starts_with('\'') || quoted.starts_with('"') || quoted.starts_with("$'") {
        quoted
    } else {
        format!("'{}'", quoted)
    }
}

fn collate_names(a: &OsStr, b: &OsStr) -> Ordering {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    match (CString::new(a_bytes), CString::new(b_bytes)) {
        (Ok(a), Ok(b)) => unsafe { strcoll(a.as_ptr(), b.as_ptr()) }.cmp(&0),
        _ => a_bytes.cmp(b_bytes),
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Copy)]
struct DateTime {
    year: i64,
    month: u32,
    day: u32,
    hour: i64,
    minute: i64,
}

fn utc_datetime(seconds: i64) -> DateTime {
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);

    DateTime {
        year,
        month,
        day,
        hour: second_of_day / 3600,
        minute: (second_of_day % 3600) / 60,
    }
}

fn local_datetime(seconds: i64) -> Option<DateTime> {
    let time: TimeT = seconds as TimeT;
    let mut tm = std::mem::MaybeUninit::<Tm>::uninit();
    let result = unsafe { localtime_r(&time, tm.as_mut_ptr()) };

    if result.is_null() {
        return None;
    }

    let tm = unsafe { tm.assume_init() };

    Some(DateTime {
        year: tm.tm_year as i64 + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as i64,
        minute: tm.tm_min as i64,
    })
}

fn format_time(seconds: i64) -> String {
    let datetime = local_datetime(seconds).unwrap_or_else(|| utc_datetime(seconds));
    format_time_at(seconds, now_seconds(), datetime)
}

fn format_time_at(seconds: i64, now: i64, datetime: DateTime) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const HALF_YEAR: i64 = 15_778_476;

    let month_name = MONTHS[(datetime.month - 1) as usize];

    if seconds < now && now - seconds <= HALF_YEAR {
        format!(
            "{} {:>2} {:02}:{:02}",
            month_name, datetime.day, datetime.hour, datetime.minute
        )
    } else {
        format!("{} {:>2}  {}", month_name, datetime.day, datetime.year)
    }
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;

    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn user_map() -> &'static HashMap<u32, String> {
    static MAP: OnceLock<HashMap<u32, String>> = OnceLock::new();
    MAP.get_or_init(|| parse_id_map("/etc/passwd"))
}

fn group_map() -> &'static HashMap<u32, String> {
    static MAP: OnceLock<HashMap<u32, String>> = OnceLock::new();
    MAP.get_or_init(|| parse_id_map("/etc/group"))
}

fn parse_id_map(path: &str) -> HashMap<u32, String> {
    let mut map = HashMap::new();

    if let Ok(contents) = fs::read_to_string(path) {
        for line in contents.lines() {
            let mut fields = line.split(':');
            let name = match fields.next() {
                Some(name) if !name.is_empty() => name,
                _ => continue,
            };
            let _password = fields.next();
            let id = match fields.next().and_then(|field| field.parse::<u32>().ok()) {
                Some(id) => id,
                None => continue,
            };
            map.entry(id).or_insert_with(|| name.to_string());
        }
    }

    map
}

type TimeT = c_long;

#[repr(C)]
#[allow(dead_code)]
struct Tm {
    tm_sec: c_int,
    tm_min: c_int,
    tm_hour: c_int,
    tm_mday: c_int,
    tm_mon: c_int,
    tm_year: c_int,
    tm_wday: c_int,
    tm_yday: c_int,
    tm_isdst: c_int,
    tm_gmtoff: c_long,
    tm_zone: *const c_char,
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
const LC_ALL: c_int = 0;
#[cfg(not(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
const LC_ALL: c_int = 6;

#[repr(C)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn localtime_r(timep: *const TimeT, result: *mut Tm) -> *mut Tm;
    fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
    fn strcoll(a: *const c_char, b: *const c_char) -> c_int;
}

fn init_locale() {
    unsafe {
        setlocale(LC_ALL, c"".as_ptr());
    }
}

#[cfg(target_os = "linux")]
const TIOCGWINSZ: c_ulong = 0x5413;
#[cfg(not(target_os = "linux"))]
const TIOCGWINSZ: c_ulong = 0x4008_7468;

fn terminal_width() -> usize {
    if let Some(columns) = env::var_os("COLUMNS")
        .and_then(|value| value.to_str().and_then(|value| value.parse::<usize>().ok()))
    {
        if columns > 0 {
            return columns;
        }
    }

    let mut size = WinSize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let result = unsafe { ioctl(1, TIOCGWINSZ, &mut size as *mut WinSize) };

    if result == 0 && size.ws_col > 0 {
        size.ws_col as usize
    } else {
        80
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(0), "0");
        assert_eq!(human_size(512), "512");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(10 * 1024), "10K");
        assert_eq!(human_size(1024 * 1024), "1.0M");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0G");
    }

    #[test]
    fn dates_are_converted_from_unix_time() {
        let now = 4_000_000_000;
        assert_eq!(format_time_at(0, now, utc_datetime(0)), "Jan  1  1970");
        assert_eq!(
            format_time_at(86_400, now, utc_datetime(86_400)),
            "Jan  2  1970"
        );
        assert_eq!(
            format_time_at(951_782_400, now, utc_datetime(951_782_400)),
            "Feb 29  2000"
        );
    }

    #[test]
    fn recent_times_are_one_sided() {
        let now = 1_700_000_100;
        assert_eq!(
            format_time_at(1_700_000_000, now, utc_datetime(1_700_000_000)),
            "Nov 14 22:13"
        );
        assert_eq!(format_time_at(now, now, utc_datetime(now)), "Nov 14  2023");
        assert_eq!(
            format_time_at(now + 60, now, utc_datetime(now + 60)),
            "Nov 14  2023"
        );
    }

    #[test]
    fn block_counts_match_gnu_units() {
        assert_eq!(format_blocks(0, false), "0");
        assert_eq!(format_blocks(1, false), "0");
        assert_eq!(format_blocks(8, false), "4");
        assert_eq!(format_blocks(8, true), "4.0K");
        assert_eq!(format_blocks(1, true), "512");
    }

    #[test]
    fn filenames_are_quoted_for_terminals() {
        assert_eq!(quote_name("plain.txt", true), "plain.txt");
        assert_eq!(quote_name("a b", true), "'a b'");
        assert_eq!(quote_name("quote'single", true), "\"quote'single\"");
        assert_eq!(quote_name("line\nbreak", true), "$'line\\nbreak'");
        assert_eq!(quote_name("\x1b[31mred", true), "$'\\033[31mred'");
        assert_eq!(quote_name("both'\"quotes", true), "'both'\\''\"quotes'");
    }

    #[test]
    fn piped_names_protect_record_boundaries() {
        assert_eq!(quote_name("line\nbreak", false), "line?break");
        assert_eq!(quote_name("a b", false), "a b");
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }
}
