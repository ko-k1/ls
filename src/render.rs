use std::borrow::Cow;
use std::os::unix::ffi::OsStrExt;

use rayon::prelude::*;

use crate::args::{IndicatorStyle, Options};
use crate::color::{color_code, push_paint};
use crate::entry::{entry_kind, Entry, Kind};
use crate::format::{
    decimal_width, push_left_aligned_n, push_padded_u64, push_right_aligned_n, push_u64,
    push_u64_bytes,
};
use crate::ids::{group_map, user_map};
use crate::layout::{layout_columns, DisplayItem};
use crate::permissions::push_permissions;
use crate::quoting::quote_name;
use crate::sizes::{format_blocks, push_format_blocks, push_human_size};
use crate::time::{now_seconds, push_format_time, terminal_width};

/// Long listings with at least this many entries measure columns in parallel.
const PARALLEL_MEASURE: usize = 4 * 1024;
const MEASURE_CHUNK: usize = 2 * 1024;

/// Renders already-loaded entries into an in-memory buffer. Traversal and
/// rendering are separate so directory loading does not need to know about
/// column layout or colors, and so directories can be rendered on worker
/// threads while the main thread writes earlier output in order.
pub struct Renderer<'a> {
    opts: &'a Options,
    color: bool,
    tty: bool,
    out: &'a mut Vec<u8>,
}

impl<'a> Renderer<'a> {
    pub fn new(opts: &'a Options, color: bool, tty: bool, out: &'a mut Vec<u8>) -> Self {
        Self {
            opts,
            color,
            tty,
            out,
        }
    }

    fn write_line(&mut self, line: &str) {
        self.out.extend_from_slice(line.as_bytes());
        self.out.push(b'\n');
    }

    pub fn print_entries(&mut self, entries: &[&Entry<'_>], from_directory: bool) {
        if from_directory && (self.opts.long || self.opts.blocks) {
            let total: u64 = entries.iter().map(|entry| entry.metadata().blocks).sum();
            self.write_line(&format!("total {}", format_blocks(total, self.opts.human)));
        }

        if entries.is_empty() {
            return;
        }

        if self.opts.long {
            self.print_long(entries);
        } else {
            self.print_short(entries);
        }
    }

    fn suffix(&self, entry: &Entry<'_>) -> &'static str {
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

    /// Return the shell-quoted display name for `entry`, handling both valid
    /// and invalid UTF-8 filenames. Invalid UTF-8 is rare and always takes the
    /// owned path to avoid borrowing from a temporary lossy string.
    fn quoted_name<'n>(entry: &Entry<'n>, tty: bool) -> Cow<'n, str> {
        match entry.name.to_str() {
            Some(name) => quote_name(name, tty),
            None => {
                let lossy = entry.name.to_string_lossy();
                Cow::Owned(quote_name(&lossy, tty).into_owned())
            }
        }
    }

    fn push_name(&self, out: &mut String, entry: &Entry<'_>) {
        let kind = entry_kind(entry);
        let name = Self::quoted_name(entry, self.tty);
        if self.color {
            push_paint(out, kind, &name);
        } else {
            out.push_str(&name);
        }
    }

    fn short_item(&self, entry: &Entry<'_>) -> DisplayItem {
        let name = Self::quoted_name(entry, self.tty);
        let suffix = self.suffix(entry);
        let mut prefix = String::new();

        if self.opts.inode {
            push_u64(&mut prefix, entry.metadata().ino);
            prefix.push(' ');
        }
        if self.opts.blocks {
            prefix.push_str(&format_blocks(entry.metadata().blocks, self.opts.human));
            prefix.push(' ');
        }

        let width = prefix.chars().count() + name.chars().count() + suffix.chars().count();
        let mut text = String::with_capacity(width + 16);
        text.push_str(&prefix);
        if self.color {
            push_paint(&mut text, entry_kind(entry), &name);
        } else {
            text.push_str(&name);
        }
        text.push_str(suffix);

        DisplayItem { text, width }
    }

    fn stream_short(&mut self, entries: &[&Entry<'_>]) {
        if self.tty {
            self.stream_short_text(entries);
        } else {
            self.stream_short_bytes(entries);
        }
    }

    fn stream_short_text(&mut self, entries: &[&Entry<'_>]) {
        let mut line = String::with_capacity(512);
        let mut scratch = String::new();

        for entry in entries {
            line.clear();

            if self.opts.inode {
                push_u64(&mut line, entry.metadata().ino);
                line.push(' ');
            }
            if self.opts.blocks {
                push_format_blocks(entry.metadata().blocks, self.opts.human, &mut scratch);
                line.push_str(&scratch);
                line.push(' ');
            }

            self.push_name(&mut line, entry);
            line.push_str(self.suffix(entry));
            line.push('\n');

            self.out.extend_from_slice(line.as_bytes());
        }
    }

    /// Piped output does not need UTF-8 conversion: write filename bytes
    /// through and only protect record boundaries.
    fn stream_short_bytes(&mut self, entries: &[&Entry<'_>]) {
        let mut line = Vec::with_capacity(512);
        let mut scratch = String::new();

        for entry in entries {
            line.clear();

            if self.opts.inode {
                push_u64_bytes(&mut line, entry.metadata().ino);
                line.push(b' ');
            }
            if self.opts.blocks {
                push_format_blocks(entry.metadata().blocks, self.opts.human, &mut scratch);
                line.extend_from_slice(scratch.as_bytes());
                line.push(b' ');
            }

            self.push_name_bytes(&mut line, entry);
            line.extend_from_slice(self.suffix(entry).as_bytes());
            line.push(b'\n');

            self.out.extend_from_slice(&line);
        }
    }

    fn push_name_bytes(&self, out: &mut Vec<u8>, entry: &Entry<'_>) {
        let code = self.color.then(|| color_code(entry_kind(entry))).flatten();
        if let Some(code) = code {
            out.extend_from_slice(b"\x1b[");
            out.extend_from_slice(code.as_bytes());
            out.push(b'm');
        }

        let bytes = entry.name.as_bytes();
        if !bytes.contains(&b'\n') {
            out.extend_from_slice(bytes);
        } else {
            let mut first = true;
            for part in bytes.split(|&byte| byte == b'\n') {
                if !first {
                    out.push(b'?');
                }
                out.extend_from_slice(part);
                first = false;
            }
        }

        if code.is_some() {
            out.extend_from_slice(b"\x1b[0m");
        }
    }

    fn print_short(&mut self, entries: &[&Entry<'_>]) {
        if self.opts.one_per_line || !self.tty {
            self.stream_short(entries);
            return;
        }

        let items: Vec<DisplayItem> = entries.iter().map(|entry| self.short_item(entry)).collect();
        let width = terminal_width();
        let layout = layout_columns(&items, width);
        let mut line = String::with_capacity(width);

        for row in &layout.rows {
            line.clear();
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

    fn print_long(&mut self, entries: &[&Entry<'_>]) {
        // Column widths depend on every entry. Large listings measure chunks
        // in parallel and merge them; nothing in this pass formats dates.
        let widths = if entries.len() < PARALLEL_MEASURE {
            self.measure_long(entries)
        } else {
            entries
                .par_chunks(MEASURE_CHUNK)
                .map(|chunk| self.measure_long(chunk))
                .reduce(LongWidths::default, LongWidths::max)
        };

        // Rows are rendered on one thread: every distinct mtime goes through
        // `localtime_r`, which takes a process-wide lock in glibc, so
        // parallel rendering spent its time contending for that lock.
        let mut out = std::mem::take(self.out);
        self.render_long(entries, &widths, now_seconds(), &mut out);
        *self.out = out;
    }

    /// Pass 1 of the long format: the display width of each padded column.
    /// All field formatting reuses a few buffers, so this does not allocate
    /// per entry.
    fn measure_long(&self, entries: &[&Entry<'_>]) -> LongWidths {
        let mut width_inode = 0;
        let mut width_blocks = 0;
        let mut width_links = 0;
        let mut width_owner = 0;
        let mut width_group = 0;
        let mut width_size = 0;
        let mut scratch = String::new();
        let mut owner = String::new();
        let mut group = String::new();
        let mut owner_width = 0;
        let mut group_width = 0;
        let mut last_uid = None;
        let mut last_gid = None;
        let users = user_map();
        let groups = group_map();

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
                owner_width = owner.chars().count();
            }
            if last_gid != Some(gid) {
                last_gid = Some(gid);
                group.clear();
                match groups.get(&gid) {
                    Some(name) => group.push_str(name),
                    None => push_u64(&mut group, gid.into()),
                }
                group_width = group.chars().count();
            }
            width_owner = width_owner.max(owner_width);
            width_group = width_group.max(group_width);

            if self.opts.inode {
                width_inode = width_inode.max(decimal_width(meta.ino));
            }

            if self.opts.blocks {
                if self.opts.human {
                    push_format_blocks(meta.blocks, true, &mut scratch);
                    width_blocks = width_blocks.max(scratch.len());
                } else {
                    width_blocks = width_blocks.max(decimal_width(meta.blocks.div_ceil(2)));
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

        LongWidths {
            inode: width_inode,
            blocks: width_blocks,
            links: width_links,
            owner: width_owner,
            group: width_group,
            size: width_size,
        }
    }

    /// Pass 2 of the long format: render each row into a reused line buffer
    /// and append it to `out`. Permissions are always 10 characters; date and
    /// name are the final (unpadded) fields.
    fn render_long(
        &self,
        entries: &[&Entry<'_>],
        widths: &LongWidths,
        now: i64,
        out: &mut Vec<u8>,
    ) {
        let LongWidths {
            inode: width_inode,
            blocks: width_blocks,
            links: width_links,
            owner: width_owner,
            group: width_group,
            size: width_size,
        } = *widths;
        let users = user_map();
        let groups = group_map();
        let mut line = String::with_capacity(
            width_inode + width_blocks + width_links + width_owner + width_group + width_size + 86,
        );
        let mut scratch = String::new();
        let mut owner = String::new();
        let mut group = String::new();
        let mut owner_width = 0;
        let mut group_width = 0;
        let mut last_uid = None;
        let mut last_gid = None;
        let mut last_time = None;
        let mut time_text = String::new();

        for entry in entries {
            let meta = entry.metadata();
            line.clear();

            if self.opts.inode {
                push_padded_u64(&mut line, meta.ino, width_inode);
                line.push(' ');
            }
            if self.opts.blocks {
                push_format_blocks(meta.blocks, self.opts.human, &mut scratch);
                push_right_aligned_n(&mut line, &scratch, scratch.len(), width_blocks);
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
                owner_width = owner.chars().count();
            }
            if last_gid != Some(gid) {
                last_gid = Some(gid);
                group.clear();
                match groups.get(&gid) {
                    Some(name) => group.push_str(name),
                    None => push_u64(&mut group, gid.into()),
                }
                group_width = group.chars().count();
            }
            line.push(' ');
            push_padded_u64(&mut line, meta.nlink, width_links);
            line.push(' ');
            push_left_aligned_n(&mut line, &owner, owner_width, width_owner);
            line.push(' ');
            push_left_aligned_n(&mut line, &group, group_width, width_group);
            line.push(' ');

            if self.opts.human {
                scratch.clear();
                push_human_size(meta.size, &mut scratch);
                push_right_aligned_n(&mut line, &scratch, scratch.len(), width_size);
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

            self.push_name(&mut line, entry);
            line.push_str(self.suffix(entry));

            if entry.file_type.is_symlink() {
                if let Some(target) = &entry.link_target {
                    let lossy_target = target.to_string_lossy();
                    let target = quote_name(&lossy_target, self.tty);
                    line.push_str(" -> ");
                    if self.color {
                        push_paint(
                            &mut line,
                            entry.target_kind.unwrap_or(Kind::Broken),
                            &target,
                        );
                    } else {
                        line.push_str(&target);
                    }
                }
            }

            out.extend_from_slice(line.as_bytes());
            out.push(b'\n');
        }
    }
}

/// Column widths of a long listing.
#[derive(Clone, Copy, Default)]
struct LongWidths {
    inode: usize,
    blocks: usize,
    links: usize,
    owner: usize,
    group: usize,
    size: usize,
}

impl LongWidths {
    fn max(self, other: Self) -> Self {
        Self {
            inode: self.inode.max(other.inode),
            blocks: self.blocks.max(other.blocks),
            links: self.links.max(other.links),
            owner: self.owner.max(other.owner),
            group: self.group.max(other.group),
            size: self.size.max(other.size),
        }
    }
}
