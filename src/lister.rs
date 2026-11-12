use std::cmp::{Ordering, Reverse};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self};
use std::io::{self, IsTerminal, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use std::fmt::Write as _;

use crate::args::{ColorWhen, IgnoreMode, IndicatorStyle, Options, SortKey};
use crate::color::paint;
use crate::entry::{entry_kind, kind_of, needs_metadata, Entry, Kind};
use crate::ids::{group_map, user_map};
use crate::layout::{layout_columns, DisplayItem};
use crate::permissions::push_permissions;
use crate::quoting::{quote_for_message, quote_name};
use crate::sizes::{format_blocks, push_format_blocks, push_human_size};
use crate::sort::{collate_key, collate_names};
use crate::time::{now_seconds, push_format_time, terminal_width};

fn decimal_width(value: u64) -> usize {
    value
        .checked_ilog10()
        .map_or(1, |digits| digits as usize + 1)
}

fn sort_names(entries: &mut [Entry]) {
    // Precomputed collation keys are a large win on arbitrary readdir order,
    // but TimSort handles ascending and descending runs in linear time. Sample
    // a few hundred adjacent pairs and use keys only when the order looks
    // genuinely mixed.
    const SAMPLE: usize = 256;
    let pairs = entries.len().saturating_sub(1).min(SAMPLE);
    let inversions = entries
        .windows(2)
        .take(SAMPLE)
        .filter(|pair| {
            collate_names(pair[0].name.as_os_str(), pair[1].name.as_os_str()) == Ordering::Greater
        })
        .count();

    if pairs == 0 {
        return;
    }

    if inversions * 4 > pairs * 3 || inversions * 4 < pairs {
        // Ascending, descending, or nearly monotonic.
        entries.sort_by(|a, b| collate_names(a.name.as_os_str(), b.name.as_os_str()));
    } else {
        entries.sort_by_cached_key(|entry| collate_key(entry.name.as_os_str()));
    }
}

/// Sort by a descending primary key, then by name within equal-key runs.
fn sort_by_primary_then_names<K: Ord>(entries: &mut [Entry], key: impl Fn(&Entry) -> K) {
    // `sort_by_key` is stable, so equal primary keys stay grouped in readdir order.
    entries.sort_by_key(|entry| Reverse(key(entry)));

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
                            fs::metadata(operand).ok()
                        } else {
                            None
                        };
                        files.push(Entry {
                            name: operand.as_os_str().to_owned(),
                            path: operand.clone(),
                            file_type,
                            meta: Some(meta),
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
                        fs::metadata(&dot_path).ok()
                    } else {
                        None
                    };
                    entries.push(Entry {
                        name: OsString::from(dot),
                        path: dot_path,
                        file_type,
                        meta: Some(meta),
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

            let meta = if needs_metadata(&self.opts) {
                match dir_entry.metadata() {
                    Ok(meta) => Some(meta),
                    Err(err) => {
                        self.had_error = true;
                        eprintln!(
                            "ls: cannot access {}: {}",
                            quote_for_message(&dir_entry.path().to_string_lossy()),
                            err
                        );
                        continue;
                    }
                }
            } else {
                None
            };
            let file_type = match &meta {
                Some(metadata) => metadata.file_type(),
                None => match dir_entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => match dir_entry.metadata() {
                        Ok(metadata) => metadata.file_type(),
                        Err(err) => {
                            self.had_error = true;
                            eprintln!(
                                "ls: cannot access {}: {}",
                                quote_for_message(&dir_entry.path().to_string_lossy()),
                                err
                            );
                            continue;
                        }
                    },
                },
            };
            let entry_path = dir_entry.path();
            let target = if self.color && file_type.is_symlink() {
                fs::metadata(&entry_path).ok()
            } else {
                None
            };
            entries.push(Entry {
                name,
                path: entry_path,
                file_type,
                meta,
                target,
            });
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
                    (entry.metadata().mtime(), entry.metadata().mtime_nsec())
                });
            }
            SortKey::Size => {
                sort_by_primary_then_names(entries, |entry| entry.metadata().size());
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
            let total: u64 = entries.iter().map(|entry| entry.metadata().blocks()).sum();
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
        // Prefer the authoritative file_type from stat'd metadata when available;
        // fall back to dirent.d_type (cheap but unreliable on some filesystems)
        // when metadata was not loaded.
        let ft = entry
            .meta
            .as_ref()
            .map_or(entry.file_type, |m| m.file_type());
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
        if ft.is_file() && entry.meta.as_ref().is_some_and(|m| m.mode() & 0o111 != 0) {
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
            Some(meta) => kind_of(meta.file_type(), Some(meta.mode())),
            None => Kind::Broken,
        };
        paint(kind, text)
    }

    fn short_item(&self, entry: &Entry) -> DisplayItem {
        let lossy_name = entry.name.to_string_lossy();
        let name = quote_name(&lossy_name, self.tty);
        let suffix = self.suffix(entry);
        let mut prefix = String::new();

        if self.opts.inode {
            prefix.push_str(&entry.metadata().ino().to_string());
            prefix.push(' ');
        }
        if self.opts.blocks {
            prefix.push_str(&format_blocks(entry.metadata().blocks(), self.opts.human));
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
        let mut owner_fallback = String::new();
        let mut group_fallback = String::new();

        for entry in entries {
            let meta = entry.metadata();

            let uid = meta.uid();
            let gid = meta.gid();
            let owner: &str = match users.get(&uid) {
                Some(name) => name.as_str(),
                None => {
                    owner_fallback.clear();
                    let _ = write!(owner_fallback, "{}", uid);
                    owner_fallback.as_str()
                }
            };
            let group: &str = match groups.get(&gid) {
                Some(name) => name.as_str(),
                None => {
                    group_fallback.clear();
                    let _ = write!(group_fallback, "{}", gid);
                    group_fallback.as_str()
                }
            };
            width_owner = width_owner.max(owner.chars().count());
            width_group = width_group.max(group.chars().count());

            if self.opts.inode {
                width_inode = width_inode.max(decimal_width(meta.ino()));
            }

            if self.opts.blocks {
                if self.opts.human {
                    push_format_blocks(meta.blocks(), true, &mut scratch);
                    width_blocks = width_blocks.max(scratch.len());
                } else {
                    width_blocks = width_blocks.max(decimal_width(meta.blocks() / 2));
                }
            }

            width_links = width_links.max(decimal_width(meta.nlink()));

            if self.opts.human {
                push_human_size(meta.size(), &mut scratch);
                width_size = width_size.max(scratch.len());
            } else {
                width_size = width_size.max(decimal_width(meta.size()));
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

        for entry in entries {
            if self.output_error.is_some() {
                break;
            }

            let meta = entry.metadata();
            line.clear();

            if self.opts.inode {
                let _ = write!(line, "{:>width$} ", meta.ino(), width = width_inode);
            }
            if self.opts.blocks {
                push_format_blocks(meta.blocks(), self.opts.human, &mut scratch);
                let _ = write!(line, "{:>width$} ", scratch, width = width_blocks);
            }

            push_permissions(meta, &mut scratch);
            line.push_str(&scratch);

            let uid = meta.uid();
            let gid = meta.gid();
            let owner: &str = match users.get(&uid) {
                Some(name) => name.as_str(),
                None => {
                    owner_fallback.clear();
                    let _ = write!(owner_fallback, "{}", uid);
                    owner_fallback.as_str()
                }
            };
            let group: &str = match groups.get(&gid) {
                Some(name) => name.as_str(),
                None => {
                    group_fallback.clear();
                    let _ = write!(group_fallback, "{}", gid);
                    group_fallback.as_str()
                }
            };
            let _ = write!(
                line,
                " {:>width_links$} {:<width_owner$} {:<width_group$} ",
                meta.nlink(),
                owner,
                group,
                width_links = width_links,
                width_owner = width_owner,
                width_group = width_group,
            );

            scratch.clear();
            if self.opts.human {
                push_human_size(meta.size(), &mut scratch);
            } else {
                let _ = write!(scratch, "{}", meta.size());
            }
            let _ = write!(line, "{:>width$} ", scratch, width = width_size);

            push_format_time(meta.mtime(), now, &mut scratch);
            line.push_str(&scratch);
            line.push(' ');

            let lossy_name = entry.name.to_string_lossy();
            let quoted_name = quote_name(&lossy_name, self.tty);
            if self.color {
                line.push_str(&self.paint_entry(entry, quoted_name.as_ref()));
            } else {
                line.push_str(quoted_name.as_ref());
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
