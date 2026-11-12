use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self};
use std::io::{self, IsTerminal, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::args::{ColorWhen, IgnoreMode, IndicatorStyle, Options, SortKey};
use crate::color::paint;
use crate::entry::{entry_kind, kind_of, needs_metadata, Entry, Kind};
use crate::ids::{group_map, user_map};
use crate::layout::{layout_columns, max_width, pad_right, DisplayItem};
use crate::permissions::permissions;
use crate::quoting::{quote_for_message, quote_name};
use crate::sizes::{format_blocks, human_size};
use crate::sort::collate_names;
use crate::time::{format_time, now_seconds, terminal_width};

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
            let file_type = match dir_entry.file_type() {
                Ok(ft) => ft,
                Err(_) => match &meta {
                    Some(m) => m.file_type(),
                    None => match dir_entry.metadata() {
                        Ok(m) => m.file_type(),
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
            SortKey::Name => {
                entries.sort_by(|a, b| collate_names(a.name.as_os_str(), b.name.as_os_str()));
            }
            SortKey::Time => {
                entries.sort_by(|a, b| {
                    b.metadata()
                        .mtime()
                        .cmp(&a.metadata().mtime())
                        .then_with(|| b.metadata().mtime_nsec().cmp(&a.metadata().mtime_nsec()))
                        .then_with(|| collate_names(a.name.as_os_str(), b.name.as_os_str()))
                });
            }
            SortKey::Size => {
                entries.sort_by(|a, b| {
                    b.metadata()
                        .size()
                        .cmp(&a.metadata().size())
                        .then_with(|| collate_names(a.name.as_os_str(), b.name.as_os_str()))
                });
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
        let ft = entry.meta.as_ref().map_or(entry.file_type, |m| m.file_type());
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
        let name = quote_name(&entry.name.to_string_lossy(), self.tty);
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
        let now = now_seconds();
        let mut rows = Vec::with_capacity(entries.len());

        for entry in entries {
            let meta = entry.metadata();
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

            if entry.file_type.is_symlink() {
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
                date: format_time(meta.mtime(), now),
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
