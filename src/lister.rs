use std::cmp::{Ordering, Reverse};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self};
use std::io::{self, IsTerminal, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use std::fmt::Write as _;

use rayon::prelude::*;

use crate::args::{ColorWhen, IgnoreMode, IndicatorStyle, Options, SortKey};
use crate::color::{color_code, paint};
use crate::entry::{entry_kind, kind_of, needs_metadata, Entry, EntryMeta, Kind};
use crate::ids::{group_map, user_map};
use crate::layout::{layout_columns, DisplayItem};
use crate::permissions::push_permissions;
use crate::quoting::{quote_for_message, quote_name};
use crate::sizes::{format_blocks, push_format_blocks, push_human_size};
use crate::sort::collate_names;
use crate::time::{now_seconds, push_format_time, terminal_width};

fn decimal_width(value: u64) -> usize {
    value
        .checked_ilog10()
        .map_or(1, |digits| digits as usize + 1)
}

/// Append a decimal integer without going through the formatting machinery.
fn push_u64(out: &mut String, value: u64) {
    let mut buffer = [0u8; 20];
    let mut start = buffer.len();
    let mut value = value;

    loop {
        start -= 1;
        buffer[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }

    // Every byte written above is an ASCII digit.
    out.push_str(unsafe { std::str::from_utf8_unchecked(&buffer[start..]) });
}

fn push_padded_u64(out: &mut String, value: u64, width: usize) {
    for _ in decimal_width(value)..width {
        out.push(' ');
    }
    push_u64(out, value);
}

fn push_right_aligned(out: &mut String, text: &str, width: usize) {
    for _ in text.chars().count()..width {
        out.push(' ');
    }
    out.push_str(text);
}

fn push_left_aligned(out: &mut String, text: &str, width: usize) {
    out.push_str(text);
    for _ in text.chars().count()..width {
        out.push(' ');
    }
}

fn push_colored_name(out: &mut String, entry: &Entry, tty: bool) {
    if let Some(code) = color_code(entry_kind(entry)) {
        out.push_str("\x1b[");
        out.push_str(code);
        out.push('m');
        push_display_name(out, entry.name.as_os_str(), tty);
        out.push_str("\x1b[0m");
    } else {
        push_display_name(out, entry.name.as_os_str(), tty);
    }
}

fn sort_names(entries: &mut [Entry]) {
    let pairs = entries.len().saturating_sub(1);
    if pairs == 0 {
        return;
    }

    // Sample adjacent pairs first. Readdir commonly returns names in nearly
    // sorted or reverse-sorted order, so avoid a full sort when a linear scan
    // can confirm the order instead.
    const SAMPLE: usize = 256;
    let probe = pairs.min(SAMPLE);
    let mut inversions = 0;
    let mut equal = 0;
    for pair in entries.windows(2).take(probe) {
        match collate_names(pair[0].name.as_os_str(), pair[1].name.as_os_str()) {
            Ordering::Greater => inversions += 1,
            Ordering::Equal => equal += 1,
            Ordering::Less => {}
        }
    }

    if probe == pairs {
        if inversions == 0 {
            // Already in collation order.
            return;
        }
        if equal == 0 && inversions == pairs {
            entries.reverse();
            return;
        }
    }

    // The sample suggests a monotonic run. Verifying the whole slice is still
    // cheaper than sorting it, and on large directories it can be done in
    // parallel.
    if equal == 0 && inversions * 4 < probe {
        if is_sorted(entries, false) {
            return;
        }
    } else if equal == 0 && inversions * 4 > probe * 3 && is_sorted(entries, true) {
        entries.reverse();
        return;
    }

    if entries.len() >= PARALLEL_SORT_THRESHOLD {
        entries.par_sort_by(|a, b| collate_names(a.name.as_os_str(), b.name.as_os_str()));
    } else {
        entries.sort_by(|a, b| collate_names(a.name.as_os_str(), b.name.as_os_str()));
    }
}

const PARALLEL_SORT_THRESHOLD: usize = 8 * 1024;
const PARALLEL_STAT_THRESHOLD: usize = 4 * 1024;

/// Turn a raw directory entry into an `Entry`, loading metadata when requested.
/// Returning the path with the error lets the caller report it from the main
/// thread while metadata loading happens in parallel.
fn build_entry(
    name: OsString,
    dir_entry: fs::DirEntry,
    opts: &Options,
    color: bool,
) -> Result<Entry, (PathBuf, io::Error)> {
    let metadata = if needs_metadata(opts) {
        match dir_entry.metadata() {
            Ok(meta) => Some(meta),
            Err(err) => return Err((dir_entry.path(), err)),
        }
    } else {
        None
    };

    let file_type = match &metadata {
        Some(metadata) => metadata.file_type(),
        None => match dir_entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => match dir_entry.metadata() {
                Ok(metadata) => metadata.file_type(),
                Err(err) => return Err((dir_entry.path(), err)),
            },
        },
    };

    // Building the full path costs an allocation per entry. It is only needed
    // for recursive descent into directories and for resolving symlinks in
    // long/color output.
    let needs_path =
        (opts.recursive && file_type.is_dir()) || ((opts.long || color) && file_type.is_symlink());
    let entry_path = if needs_path {
        dir_entry.path()
    } else {
        PathBuf::new()
    };
    let target = if color && file_type.is_symlink() {
        fs::metadata(&entry_path)
            .ok()
            .map(|meta| kind_of(meta.file_type(), Some(meta.mode())))
    } else {
        None
    };

    Ok(Entry {
        name,
        path: entry_path,
        file_type,
        meta: metadata.map(|meta| EntryMeta::from_metadata(&meta)),
        target,
    })
}

/// Check whether the entries are in ascending (or strictly descending) name
/// order, using the locale-aware comparator.
fn is_sorted(entries: &[Entry], descending: bool) -> bool {
    let pairs = entries.len().saturating_sub(1);

    if entries.len() >= PARALLEL_SORT_THRESHOLD {
        (0..pairs).into_par_iter().all(|index| {
            let ordering = collate_names(
                entries[index].name.as_os_str(),
                entries[index + 1].name.as_os_str(),
            );
            if descending {
                ordering == Ordering::Greater
            } else {
                ordering != Ordering::Greater
            }
        })
    } else {
        entries.windows(2).all(|pair| {
            let ordering = collate_names(pair[0].name.as_os_str(), pair[1].name.as_os_str());
            if descending {
                ordering == Ordering::Greater
            } else {
                ordering != Ordering::Greater
            }
        })
    }
}

/// Append `name` to `out`, applying the same quoting rules as `quote_name`
/// without first materialising a separate display string.
fn push_display_name(out: &mut String, name: &OsStr, tty: bool) {
    if tty {
        let lossy = name.to_string_lossy();
        out.push_str(&quote_name(&lossy, true));
        return;
    }

    if let Ok(name) = std::str::from_utf8(name.as_bytes()) {
        push_piped_name(out, name);
    } else {
        let lossy = name.to_string_lossy();
        push_piped_name(out, &lossy);
    }
}

fn push_piped_name(out: &mut String, name: &str) {
    // Overwhelmingly common: nothing needs replacing, so avoid even setting
    // up the split iterator.
    if !name.as_bytes().contains(&b'\n') {
        out.push_str(name);
        return;
    }

    let mut first = true;
    for part in name.split('\n') {
        if !first {
            out.push('?');
        }
        out.push_str(part);
        first = false;
    }
}

/// Sort by a descending primary key, then by name within equal-key runs.
fn sort_by_primary_then_names<K: Ord + Send>(
    entries: &mut [Entry],
    key: impl Fn(&Entry) -> K + Sync,
) {
    // `sort_by_key` is stable, so equal primary keys stay grouped in readdir order.
    if entries.len() >= PARALLEL_SORT_THRESHOLD {
        entries.par_sort_by_key(|entry| Reverse(key(entry)));
    } else {
        entries.sort_by_key(|entry| Reverse(key(entry)));
    }

    let mut start = 0;
    while start < entries.len() {
        let primary = key(&entries[start]);
        let mut end = start + 1;
        while end < entries.len() && key(&entries[end]) == primary {
            end += 1;
        }
        if end - start > 1 {
            sort_names(&mut entries[start..end]);
        }
        start = end;
    }
}

pub struct Lister {
    pub opts: Options,
    pub color: bool,
    pub tty: bool,
    pub out: io::BufWriter<io::Stdout>,
    pub had_error: bool,
    pub serious_error: bool,
    pub output_error: Option<io::Error>,
}

impl Lister {
    pub fn new(opts: Options) -> Self {
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
            out: io::BufWriter::with_capacity(64 * 1024, io::stdout()),
            had_error: false,
            serious_error: false,
            output_error: None,
        }
    }

    fn write_line(&mut self, line: &str) {
        if self.output_error.is_some() {
            return;
        }
        if let Err(err) = self.out.write_all(line.as_bytes()) {
            self.output_error = Some(err);
        } else if let Err(err) = self.out.write_all(b"\n") {
            self.output_error = Some(err);
        }
    }

    pub fn run(&mut self, mut operands: Vec<PathBuf>) -> i32 {
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
                        let file_type = meta.file_type();
                        let target = if self.color && file_type.is_symlink() {
                            fs::metadata(operand)
                                .ok()
                                .map(|target| kind_of(target.file_type(), Some(target.mode())))
                        } else {
                            None
                        };
                        files.push(Entry {
                            name: operand.as_os_str().to_owned(),
                            path: operand.clone(),
                            file_type,
                            meta: Some(EntryMeta::from_metadata(&meta)),
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
            let lossy_path = path.to_string_lossy();
            self.write_line(&format!("{}:", quote_name(&lossy_path, self.tty)));
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
                    let file_type = meta.file_type();
                    let target = if self.color && file_type.is_symlink() {
                        fs::metadata(&dot_path)
                            .ok()
                            .map(|target| kind_of(target.file_type(), Some(target.mode())))
                    } else {
                        None
                    };
                    entries.push(Entry {
                        name: OsString::from(dot),
                        path: dot_path,
                        file_type,
                        meta: Some(EntryMeta::from_metadata(&meta)),
                        target,
                    });
                }
            }
        }

        let mut raw_entries = Vec::new();
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
            raw_entries.push((name, dir_entry));
        }

        let loaded: Vec<Result<Entry, (PathBuf, io::Error)>> =
            if raw_entries.len() >= PARALLEL_STAT_THRESHOLD && needs_metadata(&self.opts) {
                let opts = &self.opts;
                let color = self.color;
                raw_entries
                    .into_par_iter()
                    .map(|(name, dir_entry)| build_entry(name, dir_entry, opts, color))
                    .collect()
            } else {
                let opts = &self.opts;
                let color = self.color;
                raw_entries
                    .into_iter()
                    .map(|(name, dir_entry)| build_entry(name, dir_entry, opts, color))
                    .collect()
            };

        for item in loaded {
            match item {
                Ok(entry) => entries.push(entry),
                Err((entry_path, err)) => {
                    self.had_error = true;
                    eprintln!(
                        "ls: cannot access {}: {}",
                        quote_for_message(&entry_path.to_string_lossy()),
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
                    entry.file_type.is_dir() && name != OsStr::new(".") && name != OsStr::new("..")
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
            SortKey::Name => sort_names(entries),
            SortKey::Time => {
                sort_by_primary_then_names(entries, |entry| {
                    (entry.metadata().mtime, entry.metadata().mtime_nsec)
                });
            }
            SortKey::Size => {
                sort_by_primary_then_names(entries, |entry| entry.metadata().size);
            }
        }

        if self.opts.reverse {
            entries.reverse();
        }

        if self.opts.dirs_first && self.opts.sort != SortKey::None {
            entries.sort_by_key(|entry| !entry.file_type.is_dir());
        }
    }

    fn print_entries(&mut self, entries: &[Entry], from_directory: bool) {
        if from_directory && (self.opts.long || self.opts.blocks) {
            let total: u64 = entries.iter().map(|entry| entry.metadata().blocks).sum();
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
        // `file_type` comes from stat'd metadata when it was loaded, otherwise
        // from dirent.d_type (cheap but unreliable on some filesystems).
        let ft = entry.file_type;
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
        if ft.is_file() && entry.meta.as_ref().is_some_and(|m| m.mode & 0o111 != 0) {
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
        paint(entry.target.unwrap_or(Kind::Broken), text)
    }

    fn short_item(&self, entry: &Entry) -> DisplayItem {
        let lossy_name = entry.name.to_string_lossy();
        let name = quote_name(&lossy_name, self.tty);
        let suffix = self.suffix(entry);
        let mut prefix = String::new();

        if self.opts.inode {
            prefix.push_str(&entry.metadata().ino.to_string());
            prefix.push(' ');
        }
        if self.opts.blocks {
            prefix.push_str(&format_blocks(entry.metadata().blocks, self.opts.human));
            prefix.push(' ');
        }

        let width = prefix.chars().count() + name.chars().count() + suffix.chars().count();
        let mut text = prefix;
        if self.color {
            text.push_str(&self.paint_entry(entry, name.as_ref()));
        } else {
            text.push_str(name.as_ref());
        }
        text.push_str(suffix);

        DisplayItem { text, width }
    }

    fn stream_short(&mut self, entries: &[Entry]) {
        let mut line = String::with_capacity(512);
        let mut scratch = String::new();

        for entry in entries {
            if self.output_error.is_some() {
                break;
            }

            line.clear();

            if self.opts.inode {
                let _ = write!(line, "{} ", entry.metadata().ino);
            }
            if self.opts.blocks {
                scratch.clear();
                push_format_blocks(entry.metadata().blocks, self.opts.human, &mut scratch);
                line.push_str(&scratch);
                line.push(' ');
            }

            if self.color {
                push_colored_name(&mut line, entry, self.tty);
            } else {
                push_display_name(&mut line, entry.name.as_os_str(), self.tty);
            }

            line.push_str(self.suffix(entry));
            line.push('\n');

            if let Err(err) = self.out.write_all(line.as_bytes()) {
                self.output_error = Some(err);
                break;
            }
        }
    }

    fn print_short(&mut self, entries: &[Entry]) {
        if self.opts.one_per_line || !self.tty {
            self.stream_short(entries);
            return;
        }

        let items: Vec<DisplayItem> = entries.iter().map(|entry| self.short_item(entry)).collect();
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
        let users = user_map();
        let groups = group_map();
        let now = now_seconds();

        // Pass 1: measure the display width of each padded column. All field
        // formatting is written into a single reused buffer so this pass does
        // not allocate per entry.
        let mut width_inode = 0;
        let mut width_blocks = 0;
        let mut width_links = 0;
        let mut width_owner = 0;
        let mut width_group = 0;
        let mut width_size = 0;
        let mut scratch = String::new();
        let mut owner = String::new();
        let mut group = String::new();
        let mut last_uid = None;
        let mut last_gid = None;

        for entry in entries {
            let meta = entry.metadata();

            let uid = meta.uid;
            let gid = meta.gid;
            if last_uid != Some(uid) {
                last_uid = Some(uid);
                owner.clear();
                match users.get(&uid) {
                    Some(name) => owner.push_str(name),
                    None => push_u64(&mut owner, uid.into()),
                }
            }
            if last_gid != Some(gid) {
                last_gid = Some(gid);
                group.clear();
                match groups.get(&gid) {
                    Some(name) => group.push_str(name),
                    None => push_u64(&mut group, gid.into()),
                }
            }
            width_owner = width_owner.max(owner.chars().count());
            width_group = width_group.max(group.chars().count());

            if self.opts.inode {
                width_inode = width_inode.max(decimal_width(meta.ino));
            }

            if self.opts.blocks {
                if self.opts.human {
                    push_format_blocks(meta.blocks, true, &mut scratch);
                    width_blocks = width_blocks.max(scratch.len());
                } else {
                    width_blocks = width_blocks.max(decimal_width(meta.blocks / 2));
                }
            }

            width_links = width_links.max(decimal_width(meta.nlink));

            if self.opts.human {
                push_human_size(meta.size, &mut scratch);
                width_size = width_size.max(scratch.len());
            } else {
                width_size = width_size.max(decimal_width(meta.size));
            }
        }

        // Pass 2: render each row directly into a reused output buffer,
        // avoiding one allocated String per field plus a `Row` struct per
        // entry. Permissions are always 10 characters; date and name are the
        // final (unpadded) fields, matching the previous layout.
        let line_capacity = width_inode
            + width_blocks
            + 10
            + width_links
            + width_owner
            + width_group
            + width_size
            + 12
            + 64;
        let mut line = String::with_capacity(line_capacity);
        last_uid = None;
        last_gid = None;
        let mut last_time = None;
        let mut time_text = String::new();

        for entry in entries {
            if self.output_error.is_some() {
                break;
            }

            let meta = entry.metadata();
            line.clear();

            if self.opts.inode {
                push_padded_u64(&mut line, meta.ino, width_inode);
                line.push(' ');
            }
            if self.opts.blocks {
                push_format_blocks(meta.blocks, self.opts.human, &mut scratch);
                push_right_aligned(&mut line, &scratch, width_blocks);
                line.push(' ');
            }

            push_permissions(entry.file_type, meta.mode, &mut scratch);
            line.push_str(&scratch);

            let uid = meta.uid;
            let gid = meta.gid;
            if last_uid != Some(uid) {
                last_uid = Some(uid);
                owner.clear();
                match users.get(&uid) {
                    Some(name) => owner.push_str(name),
                    None => push_u64(&mut owner, uid.into()),
                }
            }
            if last_gid != Some(gid) {
                last_gid = Some(gid);
                group.clear();
                match groups.get(&gid) {
                    Some(name) => group.push_str(name),
                    None => push_u64(&mut group, gid.into()),
                }
            }
            line.push(' ');
            push_padded_u64(&mut line, meta.nlink, width_links);
            line.push(' ');
            push_left_aligned(&mut line, &owner, width_owner);
            line.push(' ');
            push_left_aligned(&mut line, &group, width_group);
            line.push(' ');

            if self.opts.human {
                scratch.clear();
                push_human_size(meta.size, &mut scratch);
                push_right_aligned(&mut line, &scratch, width_size);
            } else {
                push_padded_u64(&mut line, meta.size, width_size);
            }
            line.push(' ');

            let mtime = meta.mtime;
            if last_time != Some(mtime) {
                last_time = Some(mtime);
                push_format_time(mtime, now, &mut time_text);
            }
            line.push_str(&time_text);
            line.push(' ');

            if self.color {
                push_colored_name(&mut line, entry, self.tty);
            } else {
                push_display_name(&mut line, entry.name.as_os_str(), self.tty);
            }
            line.push_str(self.suffix(entry));

            if entry.file_type.is_symlink() {
                if let Ok(target) = fs::read_link(&entry.path) {
                    let lossy_target = target.to_string_lossy();
                    let target = quote_name(&lossy_target, self.tty);
                    line.push_str(" -> ");
                    if self.color {
                        line.push_str(&self.paint_target(entry, target.as_ref()));
                    } else {
                        line.push_str(target.as_ref());
                    }
                }
            }

            self.write_line(&line);
        }
    }
}
