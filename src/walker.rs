//! Cross-platform parallel directory walker.
//!
//! Architecture: per-thread LIFO `Worker<PathBuf>` deques plus a global
//! `Injector<PathBuf>`. Idle threads steal directories from other workers'
//! deques and from the injector. Completion is signalled when the active-dir
//! counter drops to zero.
//!
//! Hot-path is zero-shared-state: each thread has its own visitor and its own
//! `PathBuf` scratch buffer. The only cross-thread synchronisation is the
//! work-stealing deque and one `AtomicUsize` counter for in-flight dirs.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

pub struct Entry<'a> {
    pub parent: &'a Path,
    pub name: &'a OsStr,
    pub kind: EntryKind,
    pub size: Option<u64>,
}

/// Backend trait implemented by each platform-specific bulk directory reader.
pub trait DirReader: Send + Sync {
    fn read_dir(
        &self,
        dir: &Path,
        each_entry: &mut dyn FnMut(&OsStr, EntryKind, Option<u64>),
    ) -> io::Result<()>;
}

pub struct WalkConfig {
    pub threads: usize,
    pub follow_symlinks: bool,
}

impl Default for WalkConfig {
    fn default() -> Self {
        Self {
            threads: optimal_thread_count(),
            follow_symlinks: false,
        }
    }
}

fn optimal_thread_count() -> usize {
    #[cfg(target_os = "macos")]
    {
        if let Some(p_cores) = macos_performance_cores() {
            return p_cores;
        }
    }
    thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
}

#[cfg(target_os = "macos")]
fn macos_performance_cores() -> Option<usize> {
    let key = c"hw.perflevel0.physicalcpu";
    let mut value: u32 = 0;
    let mut len = size_of::<u32>();
    // SAFETY: passing a NUL-terminated key, an out-pointer to a u32 and
    // its matching size; the call does not retain any of these pointers.
    let result = unsafe {
        libc::sysctlbyname(
            key.as_ptr(),
            &mut value as *mut u32 as *mut std::ffi::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || value == 0 {
        return None;
    }
    Some(value as usize)
}

/// Walk a directory tree in parallel.
///
/// `make_visitor` is called once per worker thread, returning a per-thread
/// visitor closure. This eliminates ALL shared-atomic contention in the hot
/// path: each thread's visitor can mutate thread-local state (counters,
/// buffers) without any synchronisation.
///
/// The visitor returns `true` to **skip descent** into a directory (the
/// directory entry is still reported, but its children are not visited).
/// Returning `false` means normal recursive descent.
pub fn walk<MkV, V>(
    root: PathBuf,
    config: &WalkConfig,
    reader: &dyn DirReader,
    make_visitor: MkV,
) -> io::Result<()>
where
    MkV: Fn() -> V + Sync,
    V: FnMut(&Entry) -> bool,
{
    let threads = config.threads.max(1);

    let injector: Injector<PathBuf> = Injector::new();
    let workers = (0..threads)
        .map(|_| Worker::<PathBuf>::new_lifo())
        .collect::<Vec<_>>();
    let stealers = workers.iter().map(|w| w.stealer()).collect::<Vec<_>>();

    let active_dirs = AtomicUsize::new(1);
    let done = AtomicBool::new(false);

    injector.push(root);

    thread::scope(|scope| {
        let injector_ref = &injector;
        let stealers_ref = stealers.as_slice();
        let active_ref = &active_dirs;
        let done_ref = &done;
        let reader_ref = reader;
        let make_visitor_ref = &make_visitor;
        let follow = config.follow_symlinks;

        for (worker_idx, worker) in workers.into_iter().enumerate() {
            scope.spawn(move || {
                let mut visitor = make_visitor_ref();
                run_worker(
                    worker_idx,
                    worker,
                    injector_ref,
                    stealers_ref,
                    active_ref,
                    done_ref,
                    reader_ref,
                    &mut visitor,
                    follow,
                );
            });
        }
    });

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_worker<V>(
    worker_idx: usize,
    worker: Worker<PathBuf>,
    injector: &Injector<PathBuf>,
    stealers: &[Stealer<PathBuf>],
    active_dirs: &AtomicUsize,
    done: &AtomicBool,
    reader: &dyn DirReader,
    visit: &mut V,
    follow_symlinks: bool,
) where
    V: FnMut(&Entry) -> bool,
{
    let mut sub_dirs = Vec::<PathBuf>::with_capacity(32);
    let mut idle_iterations: u32 = 0;

    loop {
        match find_task(&worker, injector, stealers, worker_idx) {
            Some(dir) => {
                idle_iterations = 0;
                if let Err(error) = process_dir(
                    &dir,
                    &worker,
                    reader,
                    visit,
                    active_dirs,
                    done,
                    follow_symlinks,
                    &mut sub_dirs,
                ) {
                    eprintln!("[kosu] read_dir({}): {}", dir.display(), error);
                }
            }
            None => {
                if done.load(Ordering::Acquire) {
                    break;
                }
                idle_iterations = idle_iterations.saturating_add(1);
                if idle_iterations < 32 {
                    thread::yield_now();
                } else {
                    thread::sleep(Duration::from_micros(20));
                }
            }
        }
    }
}

fn find_task(
    worker: &Worker<PathBuf>,
    injector: &Injector<PathBuf>,
    stealers: &[Stealer<PathBuf>],
    own_idx: usize,
) -> Option<PathBuf> {
    if let Some(p) = worker.pop() {
        return Some(p);
    }
    loop {
        match injector.steal() {
            Steal::Success(p) => return Some(p),
            Steal::Retry => continue,
            Steal::Empty => (),
        }
        let mut any_retry = false;
        for offset in 1..stealers.len() {
            let idx = (own_idx + offset) % stealers.len();
            match stealers[idx].steal() {
                Steal::Success(p) => return Some(p),
                Steal::Retry => any_retry = true,
                Steal::Empty => (),
            }
        }
        if !any_retry {
            return None;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_dir<V>(
    dir: &Path,
    worker: &Worker<PathBuf>,
    reader: &dyn DirReader,
    visit: &mut V,
    active_dirs: &AtomicUsize,
    done: &AtomicBool,
    follow_symlinks: bool,
    sub_dirs: &mut Vec<PathBuf>,
) -> io::Result<()>
where
    V: FnMut(&Entry) -> bool,
{
    sub_dirs.clear();

    let result = reader.read_dir(dir, &mut |name, kind, size| {
        let entry = Entry {
            parent: dir,
            name,
            kind,
            size,
        };
        let skip = visit(&entry);
        let descend = !skip
            && (matches!(kind, EntryKind::Dir)
                || (follow_symlinks && matches!(kind, EntryKind::Symlink)));
        if descend {
            let mut child = dir.to_path_buf();
            child.push(name);
            sub_dirs.push(child);
        }
    });

    let added = sub_dirs.len();
    if added == 0 {
        if active_dirs.fetch_sub(1, Ordering::AcqRel) == 1 {
            done.store(true, Ordering::Release);
        }
    } else {
        if added > 1 {
            active_dirs.fetch_add(added - 1, Ordering::AcqRel);
        }
        for child in sub_dirs.drain(..) {
            worker.push(child);
        }
    }
    result
}

// ─────────────────────────────────────────────────────────────────────────
// walk_aggregated_sizes — sum file sizes per-root across N junk subtrees
// in ONE work-stealing pass. Replaces the std::fs::read_dir recursion in
// the old `junk::dir_size`, which was the dominant cost of `compute_sizes`.
//
// Each queued task carries its `root_idx` so a child dir stolen by another
// worker still aggregates into the right counter. Per-thread `Vec<u64>`
// accumulates locally and flushes to shared atomics exactly once on drop.
// ─────────────────────────────────────────────────────────────────────────

/// Walk every `roots[i]` subtree in parallel and return the sum of
/// `entry.size` (file sizes from the reader) for each root.
///
/// Uses the same work-stealing pool as [`walk`]: one large subtree can
/// be drained by all threads, small subtrees don't waste a thread.
pub fn walk_aggregated_sizes(
    roots: &[PathBuf],
    config: &WalkConfig,
    reader: &dyn DirReader,
) -> Vec<u64> {
    if roots.is_empty() {
        return Vec::new();
    }

    let n_roots = roots.len();
    let threads = config.threads.max(1);

    let injector: Injector<RootedDir> = Injector::new();
    let workers = (0..threads)
        .map(|_| Worker::<RootedDir>::new_lifo())
        .collect::<Vec<_>>();
    let stealers = workers.iter().map(|w| w.stealer()).collect::<Vec<_>>();

    let totals: Vec<AtomicU64> = (0..n_roots).map(|_| AtomicU64::new(0)).collect();
    let active_dirs = AtomicUsize::new(n_roots);
    let done = AtomicBool::new(false);

    for (i, root) in roots.iter().enumerate() {
        injector.push(RootedDir {
            root_idx: i as u32,
            path: root.clone(),
        });
    }

    thread::scope(|scope| {
        let injector_ref = &injector;
        let stealers_ref = stealers.as_slice();
        let active_ref = &active_dirs;
        let done_ref = &done;
        let totals_ref: &[AtomicU64] = &totals;
        let reader_ref = reader;
        let follow = config.follow_symlinks;

        for (worker_idx, worker) in workers.into_iter().enumerate() {
            scope.spawn(move || {
                let mut local = vec![0u64; n_roots];
                run_size_worker(
                    worker_idx,
                    worker,
                    injector_ref,
                    stealers_ref,
                    active_ref,
                    done_ref,
                    reader_ref,
                    &mut local,
                    follow,
                );
                // Flush per-thread local counters to shared atomics. Each
                // thread touches each slot at most once — contention is
                // bounded by `threads`, not by `entries`.
                for (i, &v) in local.iter().enumerate() {
                    if v > 0 {
                        totals_ref[i].fetch_add(v, Ordering::Relaxed);
                    }
                }
            });
        }
    });

    totals.into_iter().map(|a| a.into_inner()).collect()
}

struct RootedDir {
    root_idx: u32,
    path: PathBuf,
}

#[allow(clippy::too_many_arguments)]
fn run_size_worker(
    worker_idx: usize,
    worker: Worker<RootedDir>,
    injector: &Injector<RootedDir>,
    stealers: &[Stealer<RootedDir>],
    active_dirs: &AtomicUsize,
    done: &AtomicBool,
    reader: &dyn DirReader,
    local: &mut [u64],
    follow_symlinks: bool,
) {
    let mut sub_dirs = Vec::<PathBuf>::with_capacity(32);
    let mut idle_iterations: u32 = 0;

    loop {
        match find_size_task(&worker, injector, stealers, worker_idx) {
            Some(task) => {
                idle_iterations = 0;
                if let Err(error) = process_size_dir(
                    task,
                    &worker,
                    reader,
                    local,
                    active_dirs,
                    done,
                    follow_symlinks,
                    &mut sub_dirs,
                ) {
                    eprintln!("[kosu] read_dir(size): {}", error);
                }
            }
            None => {
                if done.load(Ordering::Acquire) {
                    break;
                }
                idle_iterations = idle_iterations.saturating_add(1);
                if idle_iterations < 32 {
                    thread::yield_now();
                } else {
                    thread::sleep(Duration::from_micros(20));
                }
            }
        }
    }
}

fn find_size_task(
    worker: &Worker<RootedDir>,
    injector: &Injector<RootedDir>,
    stealers: &[Stealer<RootedDir>],
    own_idx: usize,
) -> Option<RootedDir> {
    if let Some(t) = worker.pop() {
        return Some(t);
    }
    loop {
        match injector.steal() {
            Steal::Success(t) => return Some(t),
            Steal::Retry => continue,
            Steal::Empty => (),
        }
        let mut any_retry = false;
        for offset in 1..stealers.len() {
            let idx = (own_idx + offset) % stealers.len();
            match stealers[idx].steal() {
                Steal::Success(t) => return Some(t),
                Steal::Retry => any_retry = true,
                Steal::Empty => (),
            }
        }
        if !any_retry {
            return None;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_size_dir(
    task: RootedDir,
    worker: &Worker<RootedDir>,
    reader: &dyn DirReader,
    local: &mut [u64],
    active_dirs: &AtomicUsize,
    done: &AtomicBool,
    follow_symlinks: bool,
    sub_dirs: &mut Vec<PathBuf>,
) -> io::Result<()> {
    sub_dirs.clear();
    let RootedDir { root_idx, path: dir } = task;
    let slot = root_idx as usize;

    let result = reader.read_dir(&dir, &mut |name, kind, size| {
        if let Some(bytes) = size {
            local[slot] += bytes;
        }
        let descend = matches!(kind, EntryKind::Dir)
            || (follow_symlinks && matches!(kind, EntryKind::Symlink));
        if descend {
            let mut child = dir.clone();
            child.push(name);
            sub_dirs.push(child);
        }
    });

    let added = sub_dirs.len();
    if added == 0 {
        if active_dirs.fetch_sub(1, Ordering::AcqRel) == 1 {
            done.store(true, Ordering::Release);
        }
    } else {
        if added > 1 {
            active_dirs.fetch_add(added - 1, Ordering::AcqRel);
        }
        for child in sub_dirs.drain(..) {
            worker.push(RootedDir { root_idx, path: child });
        }
    }
    result
}
