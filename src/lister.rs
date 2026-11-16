use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::args::{ColorWhen, IgnoreMode, IndicatorStyle, Options};
use crate::dir::{DirReader, RawEntry};
use crate::entry::{build_entry, kind_of, may_stat, Entry, EntryMeta, EntryType};
use crate::quoting::{quote_for_message, quote_name};
use crate::render::Renderer;
use crate::sort::sort_entries;
use crate::walk::walk;

const PARALLEL_STAT_THRESHOLD: usize = 4 * 1024;

/// Read-only settings for listing directories, shared with worker threads.
pub struct Settings {
    pub opts: Options,
    pub color: bool,
    pub tty: bool,
}

/// Everything listing one directory produces. Directories may be listed on
/// worker threads; the main thread emits listings in order.
pub struct DirListing {
    /// Messages for stderr, without trailing newlines.
    pub messages: Vec<String>,
    pub output: Vec<u8>,
    pub had_error: bool,
    pub serious_error: bool,
    /// Subdirectories to descend into, in display order (`-R` only).
    pub subdirs: Vec<PathBuf>,
}

impl DirListing {
    fn fail(&mut self, serious: bool, message: String) {
        if serious {
            self.serious_error = true;
        } else {
            self.had_error = true;
        }
        self.messages.push(message);
    }
}

pub struct Lister {
    pub settings: Settings,
    pub output: Output,
}

/// Standard output and the error state of the run. Only the main thread
/// writes, so output order never depends on thread timing.
pub struct Output {
    pub out: io::BufWriter<io::Stdout>,
    pub had_error: bool,
    pub serious_error: bool,
    pub output_error: Option<io::Error>,
    /// No block has been printed yet, so a header needs no blank line.
    first_block: bool,
}

impl Output {
    fn write(&mut self, bytes: &[u8]) {
        if self.output_error.is_none() {
            if let Err(err) = self.out.write_all(bytes) {
                self.output_error = Some(err);
            }
        }
    }

    /// Write one directory's listing, preceded by its header if requested.
    /// Returns `false` once output has failed and listing should stop.
    fn emit(&mut self, header: Option<&Path>, tty: bool, listing: DirListing) -> bool {
        if let Some(path) = header {
            if !self.first_block {
                self.write(b"\n");
            }
            let lossy_path = path.to_string_lossy();
            self.write(format!("{}:\n", quote_name(&lossy_path, tty)).as_bytes());
            self.first_block = false;
        }

        if !listing.messages.is_empty() {
            // Like GNU ls, flush earlier output first so messages appear in
            // place when stdout and stderr share a terminal.
            if self.output_error.is_none() {
                if let Err(err) = self.out.flush() {
                    self.output_error = Some(err);
                }
            }
            for message in &listing.messages {
                eprintln!("{message}");
            }
        }

        self.write(&listing.output);
        self.had_error |= listing.had_error;
        self.serious_error |= listing.serious_error;
        self.output_error.is_none()
    }
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
            settings: Settings { opts, color, tty },
            output: Output {
                out: io::BufWriter::with_capacity(64 * 1024, io::stdout()),
                had_error: false,
                serious_error: false,
                output_error: None,
                first_block: true,
            },
        }
    }

    pub fn run(&mut self, mut operands: Vec<PathBuf>) -> i32 {
        if operands.is_empty() {
            operands.push(PathBuf::from("."));
        }

        let show_headers = self.settings.opts.recursive || operands.len() > 1;
        let follow_operand_symlink = !self.settings.opts.directory
            && !self.settings.opts.long
            && self.settings.opts.indicator != IndicatorStyle::Classify;
        // Every operand becomes an entry so that files and directories are
        // sorted together by the active sort key, as GNU ls does.
        let opts = &self.settings.opts;
        let mut entries = Vec::new();
        for operand in &operands {
            let meta = match fs::symlink_metadata(operand) {
                Ok(meta) => meta,
                Err(err) => {
                    self.output.serious_error = true;
                    eprintln!(
                        "ls: cannot access {}: {}",
                        quote_for_message(&operand.to_string_lossy()),
                        err
                    );
                    continue;
                }
            };
            // A symlink to a directory is listed as that directory (using
            // the directory's metadata) unless -d, -l or -F says otherwise.
            let meta = match follow_operand_symlink && meta.file_type().is_symlink() {
                true => fs::metadata(operand)
                    .ok()
                    .filter(|target| target.is_dir())
                    .unwrap_or(meta),
                false => meta,
            };
            let file_type = EntryType::from_mode(meta.mode());
            let target_kind = if (self.settings.color || opts.dirs_first) && file_type.is_symlink()
            {
                fs::metadata(operand)
                    .ok()
                    .map(|target| kind_of(EntryType::from_mode(target.mode()), Some(target.mode())))
            } else {
                None
            };
            let link_target = if opts.long && file_type.is_symlink() {
                fs::read_link(operand).ok().map(PathBuf::into_os_string)
            } else {
                None
            };
            entries.push(Entry {
                name: operand.as_os_str(),
                link_target,
                file_type,
                meta: Some(EntryMeta::from_metadata(&meta)),
                target_kind,
            });
        }

        let mut sorted: Vec<&Entry> = entries.iter().collect();
        if sorted.len() > 1 {
            sort_entries(&mut sorted, opts);
        }
        let listed = |entry: &&Entry| entry.file_type.is_dir() && !opts.directory;
        let files: Vec<&Entry> = sorted
            .iter()
            .copied()
            .filter(|entry| !listed(entry))
            .collect();
        let dirs: Vec<PathBuf> = sorted
            .iter()
            .filter(|entry| listed(entry))
            .map(|entry| PathBuf::from(entry.name))
            .collect();

        if !files.is_empty() {
            let mut rendered = Vec::new();
            let settings = &self.settings;
            Renderer::new(&settings.opts, settings.color, settings.tty, &mut rendered)
                .print_operands(&files, &sorted);
            self.output.write(&rendered);
            self.output.first_block = false;
        }

        let Self { settings, output } = self;
        if dirs.len() == 1 && !settings.opts.recursive {
            let listing = list_dir(settings, &dirs[0], true);
            let header = show_headers.then_some(dirs[0].as_path());
            output.emit(header, settings.tty, listing);
        } else if !dirs.is_empty() {
            // Several directories, or -R: list ahead on worker threads.
            walk(settings, dirs, |path, listing| {
                output.emit(show_headers.then_some(path), settings.tty, listing)
            });
        }

        if output.serious_error {
            2
        } else if output.had_error {
            1
        } else {
            0
        }
    }
}

/// List one directory: read, stat, sort and render it into a [`DirListing`].
/// Failures to open or read it are serious for command-line operands.
pub fn list_dir(settings: &Settings, path: &Path, serious: bool) -> DirListing {
    let opts = &settings.opts;
    let color = settings.color;
    let mut listing = DirListing {
        messages: Vec::new(),
        output: Vec::new(),
        had_error: false,
        serious_error: false,
        subdirs: Vec::new(),
    };

    let reader = match DirReader::open(path) {
        Ok(reader) => reader,
        Err(err) => {
            let path = quote_for_message(&path.to_string_lossy());
            listing.fail(serious, format!("ls: cannot open directory {path}: {err}"));
            return listing;
        }
    };

    let dir_fd = reader.fd();
    let mut dots = Vec::new();

    if opts.ignore == IgnoreMode::All {
        for dot in [".", ".."] {
            let dot_path = path.join(dot);
            if let Ok(meta) = fs::symlink_metadata(&dot_path) {
                let file_type = EntryType::from_mode(meta.mode());
                let target_kind = if color && file_type.is_symlink() {
                    fs::metadata(&dot_path).ok().map(|target| {
                        kind_of(EntryType::from_mode(target.mode()), Some(target.mode()))
                    })
                } else {
                    None
                };
                dots.push(Entry {
                    name: OsStr::new(dot),
                    link_target: None,
                    file_type,
                    meta: Some(EntryMeta::from_metadata(&meta)),
                    target_kind,
                });
            }
        }
    }

    // All names live in one buffer that the entries borrow from.
    let mut names = Vec::new();
    let mut raw_entries = Vec::new();
    let skip_hidden = opts.ignore == IgnoreMode::Default;
    if let Err(err) = reader.read_all(skip_hidden, &mut names, &mut raw_entries) {
        let path = quote_for_message(&path.to_string_lossy());
        listing.fail(serious, format!("ls: reading directory {path}: {err}"));
    }

    let build =
        |raw: &RawEntry| build_entry(path, dir_fd, raw.name(&names), raw.file_type, opts, color);
    let loaded: Vec<Result<Entry, (PathBuf, io::Error)>> =
        if raw_entries.len() >= PARALLEL_STAT_THRESHOLD && may_stat(opts, color) {
            raw_entries.par_iter().map(build).collect()
        } else {
            raw_entries.iter().map(build).collect()
        };

    for (entry_path, err) in loaded.iter().filter_map(|item| item.as_ref().err()) {
        let path = quote_for_message(&entry_path.to_string_lossy());
        listing.fail(false, format!("ls: cannot access {path}: {err}"));
    }

    // Sort references straight into `loaded`; entries are never copied.
    let mut sorted: Vec<&Entry> = dots
        .iter()
        .chain(loaded.iter().filter_map(|item| item.as_ref().ok()))
        .collect();
    sort_entries(&mut sorted, opts);
    Renderer::new(opts, color, settings.tty, &mut listing.output).print_entries(&sorted);

    if opts.recursive {
        listing.subdirs = sorted
            .into_iter()
            .filter(|entry| entry.file_type.is_dir() && entry.name != "." && entry.name != "..")
            .map(|entry| path.join(entry.name))
            .collect();
    }
    listing
}
