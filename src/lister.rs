use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::args::{ColorWhen, IgnoreMode, IndicatorStyle, Options};
use crate::dir::DirReader;
use crate::entry::{
    build_entry, kind_of, needs_metadata, Entry, EntryMeta, EntryType,
};
use crate::quoting::{quote_for_message, quote_name};
use crate::render::Renderer;
use crate::sort::sort_entries;

const PARALLEL_STAT_THRESHOLD: usize = 4 * 1024;

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
                        let file_type = EntryType::from_mode(meta.mode());
                        let target = if self.color && file_type.is_symlink() {
                            fs::metadata(operand).ok().map(|target| {
                                kind_of(EntryType::from_mode(target.mode()), Some(target.mode()))
                            })
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
            self.render(&files, false);
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

    fn render(&mut self, entries: &[Entry], from_directory: bool) {
        let mut renderer = Renderer::new(
            &self.opts,
            self.color,
            self.tty,
            &mut self.out,
            &mut self.output_error,
        );
        renderer.print_entries(entries, from_directory);
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

    fn write_line(&mut self, line: &str) {
        if self.output_error.is_some() {
            return;
        }
        use std::io::Write as _;
        if let Err(err) = self.out.write_all(line.as_bytes()) {
            self.output_error = Some(err);
        } else if let Err(err) = self.out.write_all(b"\n") {
            self.output_error = Some(err);
        }
    }

    fn list_one_dir(&mut self, path: &Path, serious: bool) -> Vec<PathBuf> {
        let mut reader = match DirReader::open(path) {
            Ok(reader) => reader,
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

        let dir_fd = reader.fd();
        let mut entries = Vec::new();

        if self.opts.ignore == IgnoreMode::All {
            for dot in [".", ".."] {
                let dot_path = path.join(dot);
                if let Ok(meta) = fs::symlink_metadata(&dot_path) {
                    let file_type = EntryType::from_mode(meta.mode());
                    let target = if self.color && file_type.is_symlink() {
                        fs::metadata(&dot_path).ok().map(|target| {
                            kind_of(EntryType::from_mode(target.mode()), Some(target.mode()))
                        })
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
        while let Some(item) = reader.next_entry() {
            let raw = match item {
                Ok(raw) => raw,
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

            if self.opts.ignore == IgnoreMode::Default
                && raw.name.as_bytes().first() == Some(&b'.')
            {
                continue;
            }
            raw_entries.push(raw);
        }

        let loaded: Vec<Result<Entry, (PathBuf, io::Error)>> =
            if raw_entries.len() >= PARALLEL_STAT_THRESHOLD
                && needs_metadata(&self.opts, self.color)
            {
                let opts = &self.opts;
                let color = self.color;
                raw_entries
                    .into_par_iter()
                    .map(|raw| build_entry(path, dir_fd, raw, opts, color))
                    .collect()
            } else {
                let opts = &self.opts;
                let color = self.color;
                raw_entries
                    .into_iter()
                    .map(|raw| build_entry(path, dir_fd, raw, opts, color))
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

        sort_entries(&mut entries, &self.opts);
        self.render(&entries, true);

        if self.opts.recursive {
            entries
                .into_iter()
                .filter(|entry| {
                    let name = entry.name.as_os_str();
                    entry.file_type.is_dir() && name != "." && name != ".."
                })
                .map(|entry| entry.path)
                .collect()
        } else {
            Vec::new()
        }
    }
}
