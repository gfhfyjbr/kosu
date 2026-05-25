//! TUI application for the junk finder + tree viewer, built on `kou-ui`.

use std::path::PathBuf;
use std::sync::OnceLock;

use kou_ui::prelude::*;
use kou_ui::renderer::HostId;
use kou_ui::router::{Router, link_styled, routes, use_navigate, use_route};

use crate::junk::JunkHit;
use crate::scan;
use crate::scan::TreeEntry;
use crate::walker::EntryKind;

fn rgb(r: u8, g: u8, b: u8) -> Color { Color::rgb(r, g, b) }
fn chars(n: f32) -> Dimension { Dimension::Length(n) }
type Ev = kou_ui::events::DomEvent<HostId>;

static HITS: OnceLock<Vec<JunkHit>> = OnceLock::new();
static TREE: OnceLock<Vec<TreeEntry>> = OnceLock::new();
/// Same entries as TREE but sorted by size descending (biggest first).
static TREE_BY_SIZE: OnceLock<Vec<usize>> = OnceLock::new();
/// Pre-computed: for each dir data_idx → direct children sorted by size desc.
static TREE_CHILDREN: OnceLock<std::collections::HashMap<usize, Vec<usize>>> = OnceLock::new();
static SCAN_ELAPSED: OnceLock<std::time::Duration> = OnceLock::new();

pub struct AppState { pub root: PathBuf, pub threads: Option<usize> }

pub fn run(state: AppState) -> std::io::Result<()> {
    let scan_start = std::time::Instant::now();
    let tc = state.threads.unwrap_or(8);

    // Single walk produces both junk hits and tree entries.
    eprintln!("[kosu] Scanning {}...", state.root.display());
    let t0 = std::time::Instant::now();
    let mut result = scan::scan_all(state.root.clone(), state.threads)?;
    eprintln!("[STAGE] scan_all: {:.3}s  ({} hits, {} tree)",
        t0.elapsed().as_secs_f64(), result.hits.len(), result.tree.len());

    let t1 = std::time::Instant::now();
    scan::compute_sizes(&mut result.hits, tc);
    eprintln!("[STAGE] compute_sizes: {:.3}s", t1.elapsed().as_secs_f64());

    let t2 = std::time::Instant::now();
    result.hits.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    eprintln!("[STAGE] sort hits: {:.3}s", t2.elapsed().as_secs_f64());

    let t3 = std::time::Instant::now();
    scan::apply_junk_sizes_to_tree(&result.hits, &mut result.tree);
    eprintln!("[STAGE] apply_junk_sizes_to_tree: {:.3}s", t3.elapsed().as_secs_f64());

    let t4 = std::time::Instant::now();
    scan::accumulate_tree(&mut result.tree);
    eprintln!("[STAGE] accumulate_tree: {:.3}s", t4.elapsed().as_secs_f64());

    if unsafe { libc::isatty(libc::STDOUT_FILENO) } == 0 {
        return headless_output(&result.hits);
    }

    let t5 = std::time::Instant::now();
    let mut by_size = (0..result.tree.len()).collect::<Vec<_>>();
    by_size.sort_unstable_by(|&a, &b| result.tree[b].size.cmp(&result.tree[a].size));
    eprintln!("[STAGE] by_size sort: {:.3}s", t5.elapsed().as_secs_f64());

    let t6 = std::time::Instant::now();
    let children_map = build_children_index(&result.tree);
    eprintln!("[STAGE] build_children_index: {:.3}s", t6.elapsed().as_secs_f64());

    HITS.set(result.hits).map_err(|_| std::io::Error::other("HITS set"))?;
    TREE.set(result.tree).map_err(|_| std::io::Error::other("TREE set"))?;
    TREE_BY_SIZE.set(by_size).map_err(|_| std::io::Error::other("TREE_BY_SIZE set"))?;
    TREE_CHILDREN.set(children_map).map_err(|_| std::io::Error::other("TREE_CHILDREN set"))?;
    SCAN_ELAPSED.set(scan_start.elapsed()).map_err(|_| std::io::Error::other("SCAN_ELAPSED set"))?;

    eprintln!("[kosu] Launching TUI...");
    kou_ui::tui::launch(app).map_err(|e| std::io::Error::other(format!("{}", e)))
}

/// Build parent→children index in O(N). Tree must be sorted by path.
/// Each dir maps to its direct children, sorted by size descending.
fn build_children_index(tree: &[TreeEntry]) -> std::collections::HashMap<usize, Vec<usize>> {
    use std::collections::HashMap;

    // Stack tracks ancestor dirs. Since tree is sorted by path, children
    // of a dir are contiguous — we push dirs and pop when path diverges.
    let mut parent_stack: Vec<(usize, usize)> = Vec::new(); // (data_idx, depth)
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();

    for (i, entry) in tree.iter().enumerate() {
        // Pop stack until top is an ancestor of current entry.
        while let Some(&(_, d)) = parent_stack.last() {
            if entry.depth > d {
                break;
            }
            parent_stack.pop();
        }
        // If stack is non-empty, the top is our nearest ancestor.
        // Only record as direct child if depth == parent_depth + 1.
        if let Some(&(parent_idx, parent_depth)) = parent_stack.last() {
            if entry.depth == parent_depth + 1 {
                children.entry(parent_idx).or_default().push(i);
            }
        }
        if matches!(entry.kind, EntryKind::Dir) {
            parent_stack.push((i, entry.depth));
        }
    }

    // Sort each children list by size descending.
    for list in children.values_mut() {
        list.sort_unstable_by(|&a, &b| tree[b].size.cmp(&tree[a].size));
    }
    children
}

fn headless_output(hits: &[JunkHit]) -> std::io::Result<()> {
    use std::io::Write;
    let total: u64 = hits.iter().map(|h| h.size_bytes).sum();
    let mut o = std::io::BufWriter::new(std::io::stdout());
    writeln!(o, "\n  kosu junk finder — {} items — {}", hits.len(), fmt_size(total))?;
    writeln!(o, "  {}", "─".repeat(70))?;
    for h in hits { writeln!(o, "  {:>10}  {:<22} {}", fmt_size(h.size_bytes), h.ecosystem, h.path.display())?; }
    writeln!(o, "  {}\n  Total: {}\n", "─".repeat(70), fmt_size(total))?;
    o.flush()
}

fn app() -> Element {
    Router::new(routes!["/" => junk_page, "/tree" => tree_page])
        .initial_path("/").build()
}

// ── Shared shell ─────────────────────────────────────────────────────────

fn shell(content: Element, page_key: impl Fn(&Ev) + 'static, page_scroll: impl Fn(i32) + 'static) -> Element {
    let path = use_route();
    let navigate = use_navigate();

    let on_key = {
        let nav = navigate.clone();
        let p = path.clone();
        move |event: &Ev| {
            let kou_ui::events::DomEvent::Keyboard(ref kb) = *event else { return };
            match kb.key {
                Key::Char('q') => { restore_terminal(); std::process::exit(0); }
                Key::Char('1') => nav("/"),
                Key::Char('2') => nav("/tree"),
                _ => page_key(event),
            }
        }
    };

    let on_wheel = {
        move |event: &Ev| {
            let kou_ui::events::DomEvent::Scroll(ref se) = *event else { return };
            let delta = if se.delta.1 > 0.0 { 3 } else if se.delta.1 < 0.0 { -3 } else { 0 };
            if delta != 0 { page_scroll(delta); }
        }
    };

    let tab = |active: bool| {
        let bg = if active { rgb(60, 60, 100) } else { rgb(30, 30, 50) };
        Style::new().padding_x(2.0).padding_y(1.0).background(bg)
    };
    let is_junk = path == "/";

    div()
        .style(Style::flex_col()
            .width(Dimension::Percent(100.0)).height(Dimension::Percent(100.0))
            .background(rgb(10, 10, 15)).color(rgb(220, 220, 230)))
        .on_key_down(on_key)
        .on_scroll(on_wheel)
        .child(
            div().style(Style::flex_row().background(rgb(20, 20, 35)))
                .child(link_styled("/", text(" 1 Junk "), tab(is_junk)))
                .child(link_styled("/tree", text(" 2 Tree "), tab(!is_junk)))
                .child(span().style(Style::new().flex_grow(1.0).padding_x(2.0).padding_y(1.0)
                    .color(rgb(80, 80, 100)).background(rgb(20, 20, 35)))
                    .child(text(format!("scanned in {:.1}s    1/2:tabs  q:quit",
                        SCAN_ELAPSED.get().map(|d| d.as_secs_f64()).unwrap_or(0.0)))).build())
                .build()
        )
        .child(content)
        .build()
}

/// Helper: move cursor & viewport by `delta` lines.
fn scroll_by(
    delta: i32,
    cursor: usize,
    viewport: usize,
    page: usize,
    max_idx: usize,
    set_cursor: &Setter<usize>,
    set_viewport: &Setter<usize>,
) {
    let nc = if delta > 0 {
        (cursor + delta as usize).min(max_idx)
    } else {
        cursor.saturating_sub((-delta) as usize)
    };
    let mut nv = viewport;
    if nc >= nv + page { nv = nc.saturating_sub(page - 1); }
    if nc < nv { nv = nc; }
    set_cursor.set(nc);
    set_viewport.set(nv);
}

// ═══════════════════════════════════════════════════════════════════════════
//  TAB 1: JUNK FINDER
// ═══════════════════════════════════════════════════════════════════════════

fn junk_page() -> Element {
    component(junk_content)
}

fn junk_content() -> Element {
    let hits = HITS.get().expect("HITS");
    let hits_len = hits.len();

    let (selected, set_selected) = use_state({ let n = hits_len; move || vec![true; n] });
    let (cursor, set_cursor) = use_state(|| 0usize);
    let (viewport, set_viewport) = use_state(|| 0usize);
    let (message, set_message) = use_state(|| String::new());
    let (delete_children, set_delete_children) = use_state(|| Vec::<DeleteJob>::new());
    let (delete_total, set_delete_total) = use_state(|| 0usize);

    // Poll deletions.
    if !delete_children.is_empty() {
        let mut jobs = (*delete_children).clone();
        let mut still_running = Vec::new();
        let mut ok = 0usize;
        for job in jobs.drain(..) {
            match job.try_wait() { Ok(Some(_)) | Err(_) => ok += 1, Ok(None) => still_running.push(job) }
        }
        let total = *delete_total;
        let done = total - still_running.len();
        if still_running.is_empty() { set_message.set(format!("Done: {} deleted.", done)); }
        else { set_message.set(format!("Deleting {}/{}...", done, total)); }
        set_delete_children.set(still_running);
    }

    let term_h = crossterm::terminal::size().map(|(_, h)| h as usize).unwrap_or(40);
    let page = term_h.saturating_sub(5);
    let last = hits_len.saturating_sub(1);

    let total_sel_size: u64 = hits.iter().enumerate()
        .filter(|(i, _)| *i < selected.len() && selected[*i]).map(|(_, h)| h.size_bytes).sum();
    let total_size: u64 = hits.iter().map(|h| h.size_bytes).sum();
    let sel_count = selected.iter().filter(|s| **s).count();

    // Key handler passed to shell.
    let page_key = {
        let s_cur = set_cursor.clone(); let s_vp = set_viewport.clone();
        let s_sel = set_selected.clone(); let s_msg = set_message.clone();
        let s_dc = set_delete_children.clone(); let s_dt = set_delete_total.clone();
        let sel_ref = selected.clone(); let cur = *cursor; let vp = *viewport;
        let hits_ref = HITS.get().expect("HITS");
        move |event: &Ev| {
            let kou_ui::events::DomEvent::Keyboard(ref kb) = *event else { return };
            match kb.key {
                Key::Char('j') | Key::ArrowDown => scroll_by(1, cur, vp, page, last, &s_cur, &s_vp),
                Key::Char('k') | Key::ArrowUp => scroll_by(-1, cur, vp, page, last, &s_cur, &s_vp),
                Key::Char('G') | Key::End => { s_cur.set(last); s_vp.set(last.saturating_sub(page - 1)); }
                Key::Char('g') | Key::Home => { s_cur.set(0); s_vp.set(0); }
                Key::PageDown => scroll_by(page as i32, cur, vp, page, last, &s_cur, &s_vp),
                Key::PageUp => scroll_by(-(page as i32), cur, vp, page, last, &s_cur, &s_vp),
                Key::Space | Key::Char(' ') => {
                    if cur < hits_len {
                        let mut ns = (*sel_ref).clone();
                        if cur < ns.len() { ns[cur] = !ns[cur]; }
                        s_sel.set(ns);
                    }
                }
                Key::Char('a') => {
                    let all = sel_ref.iter().all(|s| *s);
                    s_sel.set(sel_ref.iter().map(|_| !all).collect());
                }
                Key::Char('d') | Key::Enter => {
                    let paths: Vec<_> = hits_ref.iter().enumerate()
                        .filter(|(i, _)| *i < sel_ref.len() && sel_ref[*i])
                        .map(|(_, h)| h.path.clone()).collect();
                    if paths.is_empty() { s_msg.set("Nothing selected.".to_owned()); }
                    else {
                        let n = paths.len();
                        let mut jobs = Vec::with_capacity(n);
                        for p in paths {
                            if let Ok(c) = std::process::Command::new("rm")
                                .args(["-rf"]).arg(&p)
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null()).spawn()
                            { jobs.push(DeleteJob::new(c)); }
                        }
                        s_msg.set(format!("Deleting 0/{}...", n));
                        s_dt.set(n); s_dc.set(jobs);
                    }
                }
                _ => (),
            }
        }
    };

    // Scroll handler passed to shell.
    let page_scroll = {
        let s_cur = set_cursor.clone(); let s_vp = set_viewport.clone();
        let cur = *cursor; let vp = *viewport;
        move |delta: i32| { scroll_by(delta, cur, vp, page, last, &s_cur, &s_vp); }
    };

    // Header.
    let header = div()
        .style(Style::flex_row().padding_x(1.0).gap(3.0).background(rgb(25, 25, 40)))
        .child(text(format!("{} items    {}", hits_len, fmt_size(total_size))))
        .child(text(format!("Selected: {} ({})", sel_count, fmt_size(total_sel_size))))
        .build();

    // Rows.
    let mut rows = Vec::with_capacity(page);
    for (index, hit) in hits.iter().enumerate().skip(*viewport).take(page) {
        let is_sel = index < selected.len() && selected[index];
        let is_cur = index == *cursor;
        let bg = if is_cur { rgb(50, 50, 80) } else if index % 2 == 0 { rgb(18, 18, 28) } else { rgb(14, 14, 22) };

        let row_click = {
            let s_cur = set_cursor.clone();
            let s_sel = set_selected.clone();
            let sel_snap = selected.clone();
            move |_: &Ev| {
                s_cur.set(index);
                let mut ns = (*sel_snap).clone();
                if index < ns.len() { ns[index] = !ns[index]; }
                s_sel.set(ns);
            }
        };

        rows.push(div().style(Style::flex_row().background(bg)).on_click(row_click)
            .child(span().style(Style::new().width(chars(5.0)))
                .child(text(if is_sel { " [x] " } else { " [ ] " }.to_owned())).build())
            .child(span().style(Style::new().width(chars(24.0)).color(rgb(100, 170, 255)))
                .child(text(pad_r(hit.ecosystem, 24))).build())
            .child(span().style(Style::new().width(chars(10.0)).color(rgb(255, 200, 80)))
                .child(text(pad_l(&fmt_size(hit.size_bytes), 10))).build())
            .child(span().style(Style::new().flex_grow(1.0).color(rgb(160, 160, 180)))
                .child(text(format!(" {}", hit.path.display()))).build())
            .build());
    }

    let mut body = div().style(Style::flex_col().flex_grow(1.0));
    for r in rows { body = body.child(r); }
    let body = body.build();

    let pos = format!("{}/{}", *cursor + 1, hits_len);
    let mut foot = format!(" j/k ↑↓ PgUp/Dn g/G space a d  {}", pos);
    if !message.is_empty() { foot = format!("{}  │ {}", foot, &*message); }
    let footer = div()
        .style(Style::flex_row().padding_x(1.0).background(rgb(25, 25, 40)).color(rgb(120, 120, 150)))
        .child(text(foot)).build();

    shell(fragment(vec![header, body, footer]), page_key, page_scroll)
}

// ═══════════════════════════════════════════════════════════════════════════
//  TAB 2: FILE TREE
// ═══════════════════════════════════════════════════════════════════════════

fn tree_page() -> Element {
    component(tree_content)
}

/// A visible row in the size-sorted tree view.
struct VisibleRow {
    data_idx: usize,
    indent: usize,
    parent_data_idx: Option<usize>,
}

/// Build visible row list for BY SIZE mode using pre-computed children index.
/// Only traverses expanded subtrees — O(visible) not O(total).
fn build_visible_list(
    _tree: &[TreeEntry],
    by_size: &[usize],
    expanded: &std::collections::HashSet<usize>,
) -> Vec<VisibleRow> {
    let children_map = TREE_CHILDREN.get().expect("TREE_CHILDREN");
    let mut result = Vec::with_capacity(1024);
    for &data_idx in by_size {
        push_row(&mut result, children_map, data_idx, 0, None, expanded);
        if result.len() > 100_000 { break; }
    }
    result
}

fn push_row(
    result: &mut Vec<VisibleRow>,
    children_map: &std::collections::HashMap<usize, Vec<usize>>,
    data_idx: usize,
    indent: usize,
    parent: Option<usize>,
    expanded: &std::collections::HashSet<usize>,
) {
    result.push(VisibleRow { data_idx, indent, parent_data_idx: parent });
    if !expanded.contains(&data_idx) {
        return;
    }
    // Children are pre-sorted by size desc in the index.
    if let Some(kids) = children_map.get(&data_idx) {
        for &child_idx in kids {
            push_row(result, children_map, child_idx, indent + 1, Some(data_idx), expanded);
        }
    }
}

fn tree_content() -> Element {
    let tree = TREE.get().expect("TREE");
    let by_size = TREE_BY_SIZE.get().expect("TREE_BY_SIZE");
    let tree_len = tree.len();

    let (cursor, set_cursor) = use_state(|| 0usize);
    let (viewport, set_viewport) = use_state(|| 0usize);
    let (size_mode, set_size_mode) = use_state(|| true);
    let (selected, set_selected) = use_state({ let n = tree_len; move || vec![false; n] });
    let (message, set_message) = use_state(|| String::new());
    let (delete_children, set_delete_children) = use_state(|| Vec::<DeleteJob>::new());
    let (delete_total, set_delete_total) = use_state(|| 0usize);
    // Expanded directories in BY SIZE mode: set of data indices.
    let (expanded, set_expanded) = use_state(|| std::collections::HashSet::<usize>::new());

    // Poll deletions.
    if !delete_children.is_empty() {
        let mut jobs = (*delete_children).clone();
        let mut still_running = Vec::new();
        let mut ok = 0usize;
        for job in jobs.drain(..) {
            match job.try_wait() { Ok(Some(_)) | Err(_) => ok += 1, Ok(None) => still_running.push(job) }
        }
        let total = *delete_total;
        let done = total - still_running.len();
        if still_running.is_empty() { set_message.set(format!("Done: {} deleted.", done)); }
        else { set_message.set(format!("Deleting {}/{}...", done, total)); }
        set_delete_children.set(still_running);
    }

    let term_h = crossterm::terminal::size().map(|(_, h)| h as usize).unwrap_or(40);
    let page = term_h.saturating_sub(5);
    let last = tree_len.saturating_sub(1);
    let is_size = *size_mode;

    let sel_count = selected.iter().filter(|s| **s).count();
    let sel_size: u64 = selected.iter().enumerate()
        .filter(|(_, s)| **s)
        .map(|(i, _)| tree[i].size)
        .sum();

    let page_key = {
        let s_cur = set_cursor.clone(); let s_vp = set_viewport.clone();
        let s_mode = set_size_mode.clone(); let s_sel = set_selected.clone();
        let s_msg = set_message.clone(); let s_dc = set_delete_children.clone();
        let s_dt = set_delete_total.clone(); let sel_ref = selected.clone();
        let s_exp = set_expanded.clone(); let exp_ref = expanded.clone();
        let cur = *cursor; let vp = *viewport;
        move |event: &Ev| {
            let kou_ui::events::DomEvent::Keyboard(ref kb) = *event else { return };
            match kb.key {
                Key::Char('j') | Key::ArrowDown => scroll_by(1, cur, vp, page, last, &s_cur, &s_vp),
                Key::Char('k') | Key::ArrowUp => scroll_by(-1, cur, vp, page, last, &s_cur, &s_vp),
                Key::Char('G') | Key::End => { s_cur.set(last); s_vp.set(last.saturating_sub(page - 1)); }
                Key::Char('g') | Key::Home => { s_cur.set(0); s_vp.set(0); }
                Key::PageDown => scroll_by(page as i32, cur, vp, page, last, &s_cur, &s_vp),
                Key::PageUp => scroll_by(-(page as i32), cur, vp, page, last, &s_cur, &s_vp),
                Key::Char('s') => { s_mode.set(!is_size); s_cur.set(0); s_vp.set(0); }
                Key::Char('l') | Key::ArrowRight => {
                    // Expand directory in size mode.
                    if is_size {
                        let vis = build_visible_list(tree, by_size, &exp_ref);
                        if cur < vis.len() {
                            let di = vis[cur].data_idx;
                            if matches!(tree[di].kind, EntryKind::Dir) {
                                let mut ne = (*exp_ref).clone();
                                ne.insert(di);
                                s_exp.set(ne);
                            }
                        }
                    }
                }
                Key::Char('h') | Key::ArrowLeft => {
                    // Collapse directory in size mode.
                    if is_size {
                        let vis = build_visible_list(tree, by_size, &exp_ref);
                        if cur < vis.len() {
                            let di = vis[cur].data_idx;
                            let mut ne = (*exp_ref).clone();
                            if ne.contains(&di) {
                                ne.remove(&di);
                            } else if let Some(parent) = vis[cur].parent_data_idx {
                                ne.remove(&parent);
                            }
                            s_exp.set(ne);
                        }
                    }
                }
                Key::Space | Key::Char(' ') => {
                    let vis = if is_size { build_visible_list(tree, by_size, &exp_ref) } else { Vec::new() };
                    let data_idx = if is_size && cur < vis.len() { vis[cur].data_idx } else if !is_size { cur } else { 0 };
                    if data_idx < tree_len {
                        let mut ns = (*sel_ref).clone();
                        if data_idx < ns.len() { ns[data_idx] = !ns[data_idx]; }
                        s_sel.set(ns);
                    }
                }
                Key::Char('a') => {
                    let any_on = sel_ref.iter().any(|s| *s);
                    s_sel.set(sel_ref.iter().map(|_| !any_on).collect());
                }
                Key::Char('d') | Key::Enter => {
                    let tree_ref = TREE.get().unwrap();
                    let paths: Vec<_> = sel_ref.iter().enumerate()
                        .filter(|(_, s)| **s)
                        .map(|(i, _)| tree_ref[i].path.clone())
                        .collect();
                    if paths.is_empty() { s_msg.set("Nothing selected.".to_owned()); }
                    else {
                        let n = paths.len();
                        let mut jobs = Vec::with_capacity(n);
                        for p in paths {
                            if let Ok(c) = std::process::Command::new("rm")
                                .args(["-rf"]).arg(&p)
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null()).spawn()
                            { jobs.push(DeleteJob::new(c)); }
                        }
                        s_msg.set(format!("Deleting 0/{}...", n));
                        s_dt.set(n); s_dc.set(jobs);
                    }
                }
                _ => (),
            }
        }
    };

    let page_scroll = {
        let s_cur = set_cursor.clone(); let s_vp = set_viewport.clone();
        let cur = *cursor; let vp = *viewport;
        move |delta: i32| { scroll_by(delta, cur, vp, page, last, &s_cur, &s_vp); }
    };

    // Build visible rows for size mode (with expanded children).
    let vis = if is_size { build_visible_list(tree, by_size, &expanded) } else { Vec::new() };
    let vis_len = if is_size { vis.len() } else { tree_len };
    // Recalculate last for visible list length.
    let vis_last = vis_len.saturating_sub(1);

    let mode_label = if is_size { "BY SIZE" } else { "TREE" };
    let expand_hint = if is_size { "  l/→:expand h/←:collapse" } else { "" };
    let header = div()
        .style(Style::flex_row().padding_x(1.0).gap(3.0).background(rgb(25, 25, 40)))
        .child(text(format!("{} entries  [{}]{}", vis_len, mode_label, expand_hint)))
        .child(text(format!("Selected: {} ({})", sel_count, fmt_size(sel_size))))
        .build();

    let mut rows = Vec::with_capacity(page);
    let vp = *viewport;
    for view_idx in vp..(vp + page).min(vis_len) {
        let (data_idx, row_indent) = if is_size {
            (vis[view_idx].data_idx, vis[view_idx].indent)
        } else {
            (view_idx, 0)
        };
        let entry = &tree[data_idx];
        let is_cur = view_idx == *cursor;
        let is_sel = data_idx < selected.len() && selected[data_idx];
        let bg = if is_cur { rgb(50, 50, 80) } else if view_idx % 2 == 0 { rgb(18, 18, 28) } else { rgb(14, 14, 22) };

        let color = if entry.is_junk { rgb(255, 100, 100) }
            else { match entry.kind { EntryKind::Dir => rgb(100, 170, 255), EntryKind::Symlink => rgb(200, 150, 255), _ => rgb(180, 180, 190) } };
        let size_str = if entry.size > 0 { fmt_size(entry.size) } else { String::new() };
        let checkbox = if is_sel { "[x]" } else { "[ ]" };

        let is_dir = matches!(entry.kind, EntryKind::Dir);
        let is_expanded = is_size && is_dir && expanded.contains(&data_idx);
        let expand_marker = if is_size && is_dir {
            if is_expanded { "▼ " } else { "▶ " }
        } else { "  " };

        let click = {
            let s_cur = set_cursor.clone(); let s_sel = set_selected.clone();
            let sel_snap = selected.clone();
            move |_: &Ev| {
                s_cur.set(view_idx);
                let mut ns = (*sel_snap).clone();
                if data_idx < ns.len() { ns[data_idx] = !ns[data_idx]; }
                s_sel.set(ns);
            }
        };

        if is_size {
            let indent_str = "  ".repeat(row_indent);
            let icon = match entry.kind { EntryKind::Dir => "📁 ", EntryKind::Symlink => "🔗 ", _ => "   " };
            let name = entry.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            // Top-level (indent 0) shows full path, nested shows just name.
            let label = if row_indent == 0 {
                format!("{}", entry.path.display())
            } else {
                format!("{}{}{}{}", indent_str, expand_marker, icon, name)
            };
            rows.push(div().style(Style::flex_row().background(bg)).on_click(click)
                .child(span().style(Style::new().width(chars(5.0)))
                    .child(text(format!(" {} ", checkbox))).build())
                .child(span().style(Style::new().width(chars(10.0)).color(rgb(255, 200, 80)))
                    .child(text(pad_l(&size_str, 10))).build())
                .child(span().style(Style::new().flex_grow(1.0).color(color))
                    .child(text(format!(" {}{}{}", if row_indent == 0 { "" } else { "" }, if row_indent == 0 { expand_marker } else { "" }, label))).build())
                .build());
        } else {
            let indent = "  ".repeat(entry.depth);
            let icon = match entry.kind { EntryKind::Dir => "📁 ", EntryKind::Symlink => "🔗 ", _ => "   " };
            let name = entry.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            rows.push(div().style(Style::flex_row().background(bg)).on_click(click)
                .child(span().style(Style::new().width(chars(5.0)))
                    .child(text(format!(" {} ", checkbox))).build())
                .child(span().style(Style::new().width(chars(10.0)).color(rgb(255, 200, 80)))
                    .child(text(pad_l(&size_str, 10))).build())
                .child(span().style(Style::new().color(color))
                    .child(text(format!("{}{}{}", indent, icon, name))).build())
                .build());
        }
    }

    let mut body = div().style(Style::flex_col().flex_grow(1.0));
    for r in rows { body = body.child(r); }
    let body = body.build();

    let pos = format!("{}/{}", *cursor + 1, vis_len);
    let mut foot = format!(" j/k ↑↓ l/h:expand/collapse space a d s:sort  {}", pos);
    if !message.is_empty() { foot = format!("{}  │ {}", foot, &*message); }
    let footer = div()
        .style(Style::flex_row().padding_x(1.0).background(rgb(25, 25, 40)).color(rgb(120, 120, 150)))
        .child(text(foot)).build();

    shell(fragment(vec![header, body, footer]), page_key, page_scroll)
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn restore_terminal() {
    use std::io::Write;
    let _ = crossterm::execute!(std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show);
    let _ = crossterm::terminal::disable_raw_mode();
}

#[derive(Clone)]
struct DeleteJob { child: std::sync::Arc<std::sync::Mutex<std::process::Child>> }
impl DeleteJob {
    fn new(c: std::process::Child) -> Self { Self { child: std::sync::Arc::new(std::sync::Mutex::new(c)) } }
    fn try_wait(&self) -> Result<Option<std::process::ExitStatus>, std::io::Error> {
        self.child.lock().map_err(|_| std::io::Error::other("poisoned"))?.try_wait()
    }
}

fn pad_r(v: &str, w: usize) -> String { format!("{:<w$}", v, w = w) }
fn pad_l(v: &str, w: usize) -> String { format!("{:>w$}", v, w = w) }
fn fmt_size(b: u64) -> String {
    const KB: u64 = 1024; const MB: u64 = 1024*KB; const GB: u64 = 1024*MB;
    if b >= GB { format!("{:.1} GB", b as f64/GB as f64) }
    else if b >= MB { format!("{:.1} MB", b as f64/MB as f64) }
    else if b >= KB { format!("{:.0} KB", b as f64/KB as f64) }
    else { format!("{} B", b) }
}
