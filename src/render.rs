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
use crate::layout::{display_width, layout_columns};
use crate::permissions::push_permissions;
use crate::quoting::{is_plain_ascii, needs_terminal_quoting, quote_name};
use crate::sizes::{format_blocks, push_format_blocks, push_human_size};
use crate::time::{
    is_recent, local_time, push_format_time_at, terminal_width, DayOffsets, Timestamp,
};

/// Listings with at least this many entries are rendered in parallel chunks.
const PARALLEL_RENDER: usize = 4 * 1024;
const RENDER_CHUNK: usize = 2 * 1024;

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

    /// Render a directory's entries, preceded by its block total.
    pub fn print_entries(&mut self, entries: &[&Entry<'_>]) {
        if self.opts.long || self.opts.blocks {
            let total: u64 = entries.iter().map(|entry| entry.metadata().blocks).sum();
            self.write_line(&format!("total {}", format_blocks(total, self.opts.human)));
        }
        self.print_batch(entries, entries);
    }

    /// Render the command-line file operands. Like GNU ls, column widths
    /// and quote alignment account for every operand, including the
    /// directories listed separately afterwards.
    pub fn print_operands(&mut self, files: &[&Entry<'_>], all: &[&Entry<'_>]) {
        self.print_batch(files, all);
    }

    /// Render `entries`, sizing padded columns over `measured` (a superset).
    fn print_batch(&mut self, entries: &[&Entry<'_>], measured: &[&Entry<'_>]) {
        if entries.is_empty() {
            return;
        }
        let quote_pad = self.quote_pad(measured);
        if self.opts.long {
            self.print_long(entries, measured, quote_pad);
        } else {
            let batch = Batch {
                inode_width: if self.opts.inode {
                    measured
                        .iter()
                        .map(|entry| decimal_width(entry.metadata().ino))
                        .max()
                        .unwrap_or(0)
                } else {
                    0
                },
                blocks_width: if self.opts.blocks {
                    let mut scratch = String::new();
                    measured
                        .iter()
                        .map(|entry| {
                            push_format_blocks(
                                entry.metadata().blocks,
                                self.opts.human,
                                &mut scratch,
                            );
                            scratch.len()
                        })
                        .max()
                        .unwrap_or(0)
                } else {
                    0
                },
                quote_pad,
            };
            self.print_short(entries, &batch);
        }
    }

    /// Whether unquoted names get a leading space so they line up with
    /// quoted ones. GNU ls does this in terminal column and long output
    /// (not `-1`) when any name in the batch needs quoting.
    fn quote_pad(&self, measured: &[&Entry<'_>]) -> bool {
        let quoted = |entry: &&Entry<'_>| needs_terminal_quoting(entry.name.as_bytes());
        self.tty
            && (self.opts.long || !self.opts.one_per_line)
            && if measured.len() < PARALLEL_RENDER {
                measured.iter().any(quoted)
            } else {
                measured.par_iter().any(quoted)
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

    fn push_name(&self, out: &mut String, entry: &Entry<'_>, quote_pad: bool) {
        let kind = entry_kind(entry);
        let name = Self::quoted_name(entry, self.tty);
        if quote_pad && matches!(name, Cow::Borrowed(_)) {
            out.push(' ');
        }
        if self.color {
            push_paint(out, kind, &name);
        } else {
            out.push_str(&name);
        }
    }

    /// Append one entry as it appears in column output (inode and block
    /// prefixes, colored and quoted name, indicator) to `text`, returning
    /// its display width.
    fn push_short_item(
        &self,
        text: &mut Vec<u8>,
        entry: &Entry<'_>,
        scratch: &mut String,
        batch: &Batch,
    ) -> usize {
        let start = text.len();
        self.push_number_prefixes(text, entry, scratch, batch);
        // Prefixes and indicators are ASCII: one column per byte.
        let mut width = text.len() - start;
        let plain = is_plain_ascii(entry.name.as_bytes());
        if batch.quote_pad && !needs_terminal_quoting(entry.name.as_bytes()) {
            text.push(b' ');
            width += 1;
        }

        let code = self.color.then(|| color_code(entry_kind(entry))).flatten();
        if let Some(code) = code {
            text.extend_from_slice(b"\x1b[");
            text.extend_from_slice(code.as_bytes());
            text.push(b'm');
        }
        let name = entry.name.as_bytes();
        if plain {
            text.extend_from_slice(name);
            width += name.len();
        } else {
            let quoted = Self::quoted_name(entry, self.tty);
            text.extend_from_slice(quoted.as_bytes());
            width += display_width(&quoted);
        }
        if code.is_some() {
            text.extend_from_slice(b"\x1b[0m");
        }

        let suffix = self.suffix(entry);
        text.extend_from_slice(suffix.as_bytes());
        width + suffix.len()
    }

    /// Append the inode and block-count prefixes, right-aligned to the
    /// batch's widths as GNU ls does outside the long format too.
    fn push_number_prefixes(
        &self,
        out: &mut Vec<u8>,
        entry: &Entry<'_>,
        scratch: &mut String,
        batch: &Batch,
    ) {
        if self.opts.inode {
            let ino = entry.metadata().ino;
            out.resize(
                out.len() + batch.inode_width.saturating_sub(decimal_width(ino)),
                b' ',
            );
            push_u64_bytes(out, ino);
            out.push(b' ');
        }
        if self.opts.blocks {
            push_format_blocks(entry.metadata().blocks, self.opts.human, scratch);
            out.resize(
                out.len() + batch.blocks_width.saturating_sub(scratch.len()),
                b' ',
            );
            out.extend_from_slice(scratch.as_bytes());
            out.push(b' ');
        }
    }

    fn stream_short(&mut self, entries: &[&Entry<'_>], batch: &Batch) {
        if self.tty {
            self.stream_short_text(entries, batch);
        } else {
            self.stream_short_bytes(entries, batch);
        }
    }

    fn stream_short_text(&mut self, entries: &[&Entry<'_>], batch: &Batch) {
        let mut prefix = Vec::with_capacity(64);
        let mut line = String::with_capacity(512);
        let mut scratch = String::new();

        for entry in entries {
            prefix.clear();
            self.push_number_prefixes(&mut prefix, entry, &mut scratch, batch);
            line.clear();
            self.push_name(&mut line, entry, false);
            line.push_str(self.suffix(entry));
            line.push('\n');

            self.out.extend_from_slice(&prefix);
            self.out.extend_from_slice(line.as_bytes());
        }
    }

    /// Piped output does not need UTF-8 conversion: write filename bytes
    /// through and only protect record boundaries.
    fn stream_short_bytes(&mut self, entries: &[&Entry<'_>], batch: &Batch) {
        let mut line = Vec::with_capacity(512);
        let mut scratch = String::new();

        for entry in entries {
            line.clear();
            self.push_number_prefixes(&mut line, entry, &mut scratch, batch);

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

    fn print_short(&mut self, entries: &[&Entry<'_>], batch: &Batch) {
        if self.opts.one_per_line || !self.tty {
            self.stream_short(entries, batch);
            return;
        }

        // Render every item once into a shared buffer, remembering where
        // each one ends and how wide it displays. Large listings render
        // chunks in parallel and concatenate them.
        let render = |chunk: &[&Entry<'_>]| {
            let mut text = Vec::with_capacity(chunk.len() * 24);
            let mut ends = Vec::with_capacity(chunk.len());
            let mut widths = Vec::with_capacity(chunk.len());
            let mut scratch = String::new();
            for entry in chunk {
                widths.push(self.push_short_item(&mut text, entry, &mut scratch, batch));
                ends.push(text.len());
            }
            (text, ends, widths)
        };
        let (text, ends, widths) = if entries.len() < PARALLEL_RENDER {
            render(entries)
        } else {
            let parts: Vec<_> = entries.par_chunks(RENDER_CHUNK).map(render).collect();
            let mut text = Vec::with_capacity(parts.iter().map(|part| part.0.len()).sum());
            let mut ends = Vec::with_capacity(entries.len());
            let mut widths = Vec::with_capacity(entries.len());
            for (part_text, part_ends, part_widths) in parts {
                let base = text.len();
                text.extend_from_slice(&part_text);
                ends.extend(part_ends.iter().map(|end| base + end));
                widths.extend(part_widths);
            }
            (text, ends, widths)
        };

        const SPACES: [u8; 256] = [b' '; 256];
        let layout = layout_columns(&widths, terminal_width());
        let count = entries.len();
        for row in 0..layout.rows {
            for column in 0..layout.columns {
                let index = column * layout.rows + row;
                if index >= count {
                    break;
                }
                let start = if index == 0 { 0 } else { ends[index - 1] };
                self.out.extend_from_slice(&text[start..ends[index]]);

                if index + layout.rows < count {
                    let mut padding = layout.widths[column] - widths[index] + 2;
                    while padding > 0 {
                        let chunk = padding.min(SPACES.len());
                        self.out.extend_from_slice(&SPACES[..chunk]);
                        padding -= chunk;
                    }
                }
            }
            self.out.push(b'\n');
        }
    }

    fn print_long(&mut self, entries: &[&Entry<'_>], measured: &[&Entry<'_>], quote_pad: bool) {
        // Column widths depend on every entry. Large listings measure chunks
        // in parallel and merge them; nothing in this pass formats dates.
        let widths = if measured.len() < PARALLEL_RENDER {
            self.measure_long(measured)
        } else {
            measured
                .par_chunks(RENDER_CHUNK)
                .map(|chunk| self.measure_long(chunk))
                .reduce(LongWidths::default, LongWidths::max)
        };

        let now = Timestamp::now();
        if entries.len() < PARALLEL_RENDER {
            let mut out = std::mem::take(self.out);
            self.render_long(entries, &widths, now, None, quote_pad, &mut out);
            *self.out = out;
            return;
        }

        // Every row depends only on its entry, the widths and a shared,
        // read-only table of day offsets (built here, on one thread), so
        // render chunks in parallel and append them in order.
        let days = DayOffsets::for_times(entries.iter().map(|entry| entry.metadata().mtime));
        let parts: Vec<Vec<u8>> = entries
            .par_chunks(RENDER_CHUNK)
            .map(|chunk| {
                let mut out = Vec::with_capacity(chunk.len() * 80);
                self.render_long(chunk, &widths, now, Some(&days), quote_pad, &mut out);
                out
            })
            .collect();
        for part in parts {
            self.out.extend_from_slice(&part);
        }
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
        now: Timestamp,
        days: Option<&DayOffsets>,
        quote_pad: bool,
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
            let when = Timestamp {
                seconds: mtime,
                nanoseconds: meta.mtime_nsec,
            };
            // Like GNU ls, re-read the clock before calling a time in the
            // future, in case the file was modified after `now` was taken.
            let recent = is_recent(when, now) || (when >= now && is_recent(when, Timestamp::now()));
            if last_time != Some((mtime, recent)) {
                last_time = Some((mtime, recent));
                let datetime = match days {
                    Some(days) => days.local_time(mtime),
                    None => local_time(mtime),
                };
                push_format_time_at(datetime, recent, &mut time_text);
            }
            line.push_str(&time_text);
            line.push(' ');

            self.push_name(&mut line, entry, quote_pad);
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

/// Per-batch layout facts shared by every short-format row.
struct Batch {
    inode_width: usize,
    blocks_width: usize,
    quote_pad: bool,
}
