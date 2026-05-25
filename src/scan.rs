//! Scan orchestration — single walk produces BOTH junk hits and tree entries.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::junk::{self, JunkHit};
use crate::platform;
use crate::walker::{self, EntryKind, WalkConfig};

const TREE_MAX_ENTRIES: usize = 4_000_000;

/// Everything produced by a single scan pass.
pub struct FullScanResult {
    pub hits: Vec<JunkHit>,
    pub tree: Vec<TreeEntry>,
}

/// A flat entry in the scanned directory tree.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub path: PathBuf,
    pub kind: EntryKind,
    pub depth: usize,
    pub size: u64,
    pub is_junk: bool,
}

/// Single walk that collects junk hits AND tree entries simultaneously.
/// Junk dir subtrees are skipped (not descended into).
pub fn scan_all(root: PathBuf, threads: Option<usize>) -> io::Result<FullScanResult> {
    let root_depth = root.components().count();
    let tree_count = std::sync::atomic::AtomicUsize::new(0);

    let mut config = WalkConfig::default();
    if let Some(tc) = threads {
        config.threads = tc.max(1);
    }
    // getattrlistbulk with sizes — one walk for everything.
    let reader = platform::build_reader(platform::ReaderOptions { with_size: true });

    // Per-thread local storage for both hits and tree entries.
    let all_locals: Arc<Mutex<Vec<(Vec<JunkHit>, Vec<TreeEntry>)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let all_ref = &all_locals;
    let tree_count_ref = &tree_count;

    walker::walk(root, &config, reader.as_ref(), || {
        let all = Arc::clone(all_ref);

        struct FlushOnDrop {
            local: Option<(Vec<JunkHit>, Vec<TreeEntry>)>,
            all: Arc<Mutex<Vec<(Vec<JunkHit>, Vec<TreeEntry>)>>>,
        }
        impl Drop for FlushOnDrop {
            fn drop(&mut self) {
                if let Some(data) = self.local.take() {
                    if let Ok(mut guard) = self.all.lock() {
                        guard.push(data);
                    }
                }
            }
        }

        let guard = std::cell::RefCell::new(FlushOnDrop {
            local: Some((Vec::with_capacity(256), Vec::with_capacity(8192))),
            all,
        });

        move |entry: &walker::Entry| -> bool {
            let is_junk = matches!(entry.kind, EntryKind::Dir)
                && junk::match_junk(entry.name, entry.parent).is_some();

            let mut g = guard.borrow_mut();
            let Some((ref mut hits, ref mut tree)) = g.local else { return is_junk };

            // Record junk hit.
            if is_junk {
                let mut full_path = entry.parent.to_path_buf();
                full_path.push(entry.name);
                hits.push(JunkHit {
                    path: full_path,
                    ecosystem: junk::match_junk(entry.name, entry.parent).unwrap_or(""),
                    size_bytes: 0,
                });
            }

            // Record tree entry (up to cap).
            if tree_count_ref.load(std::sync::atomic::Ordering::Relaxed) < TREE_MAX_ENTRIES {
                let mut full_path = entry.parent.to_path_buf();
                full_path.push(entry.name);
                let depth = full_path.components().count().saturating_sub(root_depth);
                tree.push(TreeEntry {
                    path: full_path,
                    kind: entry.kind,
                    depth,
                    size: entry.size.unwrap_or(0),
                    is_junk,
                });
                tree_count_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }

            is_junk // skip descent into junk dirs
        }
    })?;

    // Merge thread-local data.
    let locals = Arc::try_unwrap(all_locals)
        .unwrap_or_else(|arc| {
            Mutex::new(arc.lock().unwrap_or_else(|p| p.into_inner()).clone())
        })
        .into_inner()
        .unwrap_or_else(|p| p.into_inner());

    let mut all_hits = Vec::new();
    let mut all_tree = Vec::new();
    for (mut hits, mut tree) in locals {
        all_hits.append(&mut hits);
        all_tree.append(&mut tree);
    }

    // Dedup junk hits.
    all_hits.sort_by(|a, b| a.path.cmp(&b.path));
    all_hits.dedup_by(|a, b| a.path == b.path);

    // Sort tree by path for accumulation.
    if all_tree.len() > TREE_MAX_ENTRIES {
        all_tree.truncate(TREE_MAX_ENTRIES);
    }
    all_tree.sort_unstable_by(|a, b| a.path.cmp(&b.path));

    Ok(FullScanResult {
        hits: all_hits,
        tree: all_tree,
    })
}

/// Compute sizes for junk hits in parallel.
pub fn compute_sizes(hits: &mut [JunkHit], threads: usize) {
    let chunk_size = (hits.len() / threads.max(1)).max(1);
    std::thread::scope(|scope| {
        for chunk in hits.chunks_mut(chunk_size) {
            scope.spawn(|| {
                for hit in chunk.iter_mut() {
                    hit.size_bytes = junk::dir_size(&hit.path);
                }
            });
        }
    });
}

/// Copy already-computed junk sizes from hits into matching tree entries.
/// Both are sorted by path, so we merge in O(N+M).
pub fn apply_junk_sizes_to_tree(hits: &[JunkHit], tree: &mut [TreeEntry]) {
    use std::collections::HashMap;
    let size_map: HashMap<&std::path::Path, u64> = hits.iter()
        .map(|h| (h.path.as_path(), h.size_bytes))
        .collect();
    for entry in tree.iter_mut() {
        if entry.is_junk && entry.size == 0 {
            if let Some(&size) = size_map.get(entry.path.as_path()) {
                entry.size = size;
            }
        }
    }
}

/// Accumulate sizes bottom-up (no junk recomputation).
pub fn accumulate_tree(tree: &mut [TreeEntry]) {
    accumulate_dir_sizes(tree);
}

/// Propagate file sizes up to parent directories.
fn accumulate_dir_sizes(entries: &mut [TreeEntry]) {
    use std::collections::HashMap;

    let mut dir_index = HashMap::<&std::path::Path, usize>::with_capacity(entries.len() / 8);
    for (i, entry) in entries.iter().enumerate() {
        if matches!(entry.kind, EntryKind::Dir) {
            dir_index.insert(&entry.path, i);
        }
    }

    let mut additions = Vec::<(usize, u64)>::new();
    for entry in entries.iter() {
        if entry.size == 0 { continue; }
        let mut ancestor = entry.path.parent();
        while let Some(parent) = ancestor {
            if let Some(&idx) = dir_index.get(parent) {
                additions.push((idx, entry.size));
            }
            ancestor = parent.parent();
        }
    }
    for (idx, size) in additions {
        entries[idx].size += size;
    }
}
