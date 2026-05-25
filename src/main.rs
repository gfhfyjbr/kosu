//! kosu — blazing-fast recursive directory walker & junk finder.
//!
//! Modes:
//!
//! - `fast` (default): platform bulk syscalls + work-stealing pool.
//!     - macOS:  `getattrlistbulk(2)` (with `--with-size`) or `readdir(3)`.
//!     - Linux:  `getdents64(2)` (+ `statx(2)` fallback for `DT_UNKNOWN`).
//!     - Win:    `NtQueryDirectoryFile` / `FILE_DIRECTORY_INFORMATION`.
//! - `clean`: interactive TUI junk finder (node_modules, target, __pycache__...).
//! - `--list`: also print every path to stdout (slower; for verification).

mod junk;
mod platform;
mod scan;
mod tui_app;
mod walker;

use std::cell::Cell;
use std::env;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug)]
struct Args {
    root: PathBuf,
    mode: Mode,
    threads: Option<usize>,
    follow_symlinks: bool,
    list: bool,
    with_size: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Fast,
    Clean,
}

/// Per-thread counters merged into shared atomics when the thread finishes.
struct ThreadCounters<'a> {
    entries: Cell<u64>,
    dirs: Cell<u64>,
    files: Cell<u64>,
    symlinks: Cell<u64>,
    total_size: Cell<u64>,
    // Shared atomics — written to exactly once per thread (at merge time).
    shared_entries: &'a AtomicU64,
    shared_dirs: &'a AtomicU64,
    shared_files: &'a AtomicU64,
    shared_symlinks: &'a AtomicU64,
    shared_total_size: &'a AtomicU64,
}

impl Drop for ThreadCounters<'_> {
    fn drop(&mut self) {
        self.shared_entries
            .fetch_add(self.entries.get(), Ordering::Relaxed);
        self.shared_dirs
            .fetch_add(self.dirs.get(), Ordering::Relaxed);
        self.shared_files
            .fetch_add(self.files.get(), Ordering::Relaxed);
        self.shared_symlinks
            .fetch_add(self.symlinks.get(), Ordering::Relaxed);
        self.shared_total_size
            .fetch_add(self.total_size.get(), Ordering::Relaxed);
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kosu: {}", error);
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let args = parse_args()?;

    let entries = AtomicU64::new(0);
    let dirs = AtomicU64::new(0);
    let files = AtomicU64::new(0);
    let symlinks = AtomicU64::new(0);
    let total_size = AtomicU64::new(0);

    let writer = Mutex::new(BufWriter::with_capacity(64 * 1024, io::stdout()));
    let list = args.list;

    let start = Instant::now();

    match args.mode {
        Mode::Clean => {
            return tui_app::run(tui_app::AppState {
                root: args.root,
                threads: args.threads,
            });
        }
        Mode::Fast => {
            // Per-thread visitor factory: each thread gets its own counters
            // and a PathBuf scratch buffer. ZERO cross-thread atomic ops
            // in the hot path — counters merge once when the thread exits.
            let writer_ref = &writer;
            let make_visitor = || {
                let mut path_buf = PathBuf::with_capacity(256);
                let counters = ThreadCounters {
                    entries: Cell::new(0),
                    dirs: Cell::new(0),
                    files: Cell::new(0),
                    symlinks: Cell::new(0),
                    total_size: Cell::new(0),
                    shared_entries: &entries,
                    shared_dirs: &dirs,
                    shared_files: &files,
                    shared_symlinks: &symlinks,
                    shared_total_size: &total_size,
                };
                move |entry: &walker::Entry| -> bool {
                    counters.entries.set(counters.entries.get() + 1);
                    match entry.kind {
                        walker::EntryKind::Dir => counters.dirs.set(counters.dirs.get() + 1),
                        walker::EntryKind::File => counters.files.set(counters.files.get() + 1),
                        walker::EntryKind::Symlink => {
                            counters.symlinks.set(counters.symlinks.get() + 1)
                        }
                        walker::EntryKind::Other => (),
                    }
                    if let Some(size) = entry.size {
                        counters
                            .total_size
                            .set(counters.total_size.get() + size);
                    }
                    if list {
                        path_buf.clear();
                        path_buf.push(entry.parent);
                        path_buf.push(entry.name);
                        if let Ok(mut guard) = writer_ref.lock() {
                            if writeln!(guard, "{}", path_buf.display()).is_err() {}
                        }
                    }
                    false // never skip descent in stat mode
                }
            };

            let mut config = walker::WalkConfig::default();
            if let Some(threads) = args.threads {
                config.threads = threads.max(1);
            }
            config.follow_symlinks = args.follow_symlinks;

            let reader = platform::build_reader(platform::ReaderOptions {
                with_size: args.with_size,
            });
            walker::walk(args.root.clone(), &config, reader.as_ref(), make_visitor)?;
        }

    }

    let elapsed = start.elapsed();
    let total = entries.load(Ordering::Relaxed);

    if let Ok(mut guard) = writer.lock() {
        guard.flush()?;
    }

    let rate = if elapsed.as_secs_f64() > 0.0 {
        total as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };
    eprintln!(
        "\n[kosu] mode={:?} root={} threads={} elapsed={:.3}s",
        args.mode,
        args.root.display(),
        args.threads
            .map(|t| t.to_string())
            .unwrap_or_else(|| "auto".to_owned()),
        elapsed.as_secs_f64()
    );
    eprintln!(
        "[kosu] entries={} dirs={} files={} symlinks={} bytes={} rate={:.0}/s",
        total,
        dirs.load(Ordering::Relaxed),
        files.load(Ordering::Relaxed),
        symlinks.load(Ordering::Relaxed),
        total_size.load(Ordering::Relaxed),
        rate
    );

    Ok(())
}

fn parse_args() -> io::Result<Args> {
    let mut root: Option<PathBuf> = None;
    let mut mode = Mode::Fast;
    let mut threads: Option<usize> = None;
    let mut follow_symlinks = false;
    let mut list = false;
    let mut with_size = false;

    let raw = env::args().skip(1).collect::<Vec<_>>();
    let mut iter = raw.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "clean" | "--clean" | "--junk" => mode = Mode::Clean,
            "--threads" | "-t" => {
                let value = iter.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--threads requires a value")
                })?;
                let parsed = value.parse::<usize>().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--threads value must be a number")
                })?;
                threads = Some(parsed);
            }
            "--follow-symlinks" | "-L" => follow_symlinks = true,
            "--list" | "-l" => list = true,
            "--with-size" | "-s" => with_size = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other if !other.starts_with('-') => {
                root = Some(PathBuf::from(other));
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown argument: {}", other),
                ));
            }
        }
    }

    let root = root.unwrap_or_else(|| PathBuf::from("."));
    Ok(Args {
        root,
        mode,
        threads,
        follow_symlinks,
        list,
        with_size,
    })
}

fn print_help() {
    eprintln!(
        "kosu — blazing-fast recursive directory walker\n\
         \n\
         USAGE:\n  \
           kosu [PATH] [OPTIONS]\n\
         \n\
         OPTIONS:\n  \
           clean, --clean, --junk   interactive TUI junk finder\n  \
           --threads, -t <N>        worker thread count (default: P-cores)\n  \
           --with-size, -s          request file sizes (uses getattrlistbulk)\n  \
           --follow-symlinks, -L    recurse into symlinks\n  \
           --list, -l               print each entry path to stdout\n  \
           -h, --help               show this help\n\
         \n\
         BACKENDS:\n  \
           macOS    readdir(3) / getattrlistbulk(2) + work-stealing pool\n  \
           Linux    getdents64(2) + statx(2) fallback + work-stealing\n  \
           Windows  NtQueryDirectoryFile / FILE_DIRECTORY_INFORMATION\n"
    );
}
