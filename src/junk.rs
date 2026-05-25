//! Junk directory pattern definitions for 25+ ecosystems.
//!
//! Each pattern matches a **directory name** seen during the walk. Ambiguous
//! names (e.g. `target/`, `build/`, `vendor/`) require a **marker file** in
//! the same parent directory to avoid false positives.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct JunkPattern {
    /// Directory name to match (case-sensitive, no slashes).
    pub dir_name: &'static str,
    /// Human-readable ecosystem label shown in the TUI.
    pub ecosystem: &'static str,
    /// If `Some`, the parent directory must contain a file with this name for
    /// the match to count. Prevents flagging e.g. every `build/` as junk.
    pub marker: Option<&'static str>,
}

/// All known junk patterns. Order does not matter — the scanner linearly
/// probes this list for every directory entry whose `EntryKind == Dir`.
pub static PATTERNS: &[JunkPattern] = &[
    // ── Node / JavaScript ────────────────────────────────────────────
    JunkPattern { dir_name: "node_modules",       ecosystem: "Node",            marker: None },
    JunkPattern { dir_name: ".turbo",             ecosystem: "Turborepo",       marker: None },

    // ── Python ───────────────────────────────────────────────────────
    JunkPattern { dir_name: "__pycache__",         ecosystem: "Python",          marker: None },
    JunkPattern { dir_name: ".tox",                ecosystem: "Python (tox)",    marker: None },
    JunkPattern { dir_name: ".eggs",               ecosystem: "Python (eggs)",   marker: None },
    JunkPattern { dir_name: ".venv",               ecosystem: "Python (venv)",   marker: None },
    JunkPattern { dir_name: "venv",                ecosystem: "Python (venv)",   marker: Some("pyvenv.cfg") },
    JunkPattern { dir_name: ".mypy_cache",         ecosystem: "Python (mypy)",   marker: None },
    JunkPattern { dir_name: ".pytest_cache",       ecosystem: "Python (pytest)", marker: None },
    JunkPattern { dir_name: ".ruff_cache",         ecosystem: "Python (ruff)",   marker: None },

    // ── Pixi (Python) ────────────────────────────────────────────────
    JunkPattern { dir_name: ".pixi",               ecosystem: "Pixi",            marker: None },

    // ── Jupyter ──────────────────────────────────────────────────────
    JunkPattern { dir_name: ".ipynb_checkpoints",  ecosystem: "Jupyter",         marker: None },

    // ── Rust / Cargo ─────────────────────────────────────────────────
    JunkPattern { dir_name: "target",              ecosystem: "Rust (Cargo)",    marker: Some("Cargo.toml") },

    // ── CMake (C / C++) ──────────────────────────────────────────────
    JunkPattern { dir_name: "build",               ecosystem: "CMake",           marker: Some("CMakeLists.txt") },
    JunkPattern { dir_name: "cmake-build-debug",   ecosystem: "CMake",           marker: None },
    JunkPattern { dir_name: "cmake-build-release", ecosystem: "CMake",           marker: None },

    // ── Composer (PHP) ───────────────────────────────────────────────
    JunkPattern { dir_name: "vendor",              ecosystem: "Composer (PHP)",  marker: Some("composer.json") },

    // ── Elixir ───────────────────────────────────────────────────────
    JunkPattern { dir_name: "_build",              ecosystem: "Elixir",          marker: Some("mix.exs") },
    JunkPattern { dir_name: "deps",                ecosystem: "Elixir",          marker: Some("mix.exs") },

    // ── Godot 4.x ───────────────────────────────────────────────────
    JunkPattern { dir_name: ".godot",              ecosystem: "Godot",           marker: None },
    JunkPattern { dir_name: ".mono",               ecosystem: "Godot (C#)",      marker: None },

    // ── Gradle (Java) ───────────────────────────────────────────────
    JunkPattern { dir_name: ".gradle",             ecosystem: "Gradle",          marker: None },
    JunkPattern { dir_name: "build",               ecosystem: "Gradle",          marker: Some("build.gradle") },
    JunkPattern { dir_name: "build",               ecosystem: "Gradle (kts)",    marker: Some("build.gradle.kts") },

    // ── Maven (Java) ────────────────────────────────────────────────
    JunkPattern { dir_name: "target",              ecosystem: "Maven",           marker: Some("pom.xml") },

    // ── Pub (Dart) ──────────────────────────────────────────────────
    JunkPattern { dir_name: ".dart_tool",          ecosystem: "Dart (pub)",      marker: None },
    JunkPattern { dir_name: "build",               ecosystem: "Dart (pub)",      marker: Some("pubspec.yaml") },

    // ── SBT (Scala) ─────────────────────────────────────────────────
    JunkPattern { dir_name: "target",              ecosystem: "SBT (Scala)",     marker: Some("build.sbt") },
    JunkPattern { dir_name: ".bsp",                ecosystem: "SBT (Scala)",     marker: Some("build.sbt") },

    // ── Stack (Haskell) ─────────────────────────────────────────────
    JunkPattern { dir_name: ".stack-work",         ecosystem: "Stack (Haskell)", marker: None },

    // ── Cabal (Haskell) ─────────────────────────────────────────────
    JunkPattern { dir_name: "dist-newstyle",       ecosystem: "Cabal (Haskell)", marker: None },

    // ── Swift ────────────────────────────────────────────────────────
    JunkPattern { dir_name: ".build",              ecosystem: "Swift (SPM)",     marker: Some("Package.swift") },
    JunkPattern { dir_name: "DerivedData",         ecosystem: "Xcode",           marker: None },

    // ── Unity (C#) ──────────────────────────────────────────────────
    JunkPattern { dir_name: "Library",             ecosystem: "Unity",           marker: Some("ProjectSettings") },
    JunkPattern { dir_name: "Temp",                ecosystem: "Unity",           marker: Some("ProjectSettings") },

    // ── Unreal Engine (C++) ─────────────────────────────────────────
    JunkPattern { dir_name: "Intermediate",        ecosystem: "Unreal Engine",   marker: None },
    JunkPattern { dir_name: "Saved",               ecosystem: "Unreal Engine",   marker: None },
    JunkPattern { dir_name: "DerivedDataCache",    ecosystem: "Unreal Engine",   marker: None },

    // ── Zig ──────────────────────────────────────────────────────────
    JunkPattern { dir_name: "zig-cache",           ecosystem: "Zig",             marker: None },
    JunkPattern { dir_name: "zig-out",             ecosystem: "Zig",             marker: None },
    JunkPattern { dir_name: ".zig-cache",          ecosystem: "Zig",             marker: None },

    // ── .NET (C# / F#) ──────────────────────────────────────────────
    JunkPattern { dir_name: "bin",                 ecosystem: ".NET",            marker: Some("*.csproj") },
    JunkPattern { dir_name: "obj",                 ecosystem: ".NET",            marker: Some("*.csproj") },
    JunkPattern { dir_name: "bin",                 ecosystem: ".NET (F#)",       marker: Some("*.fsproj") },
    JunkPattern { dir_name: "obj",                 ecosystem: ".NET (F#)",       marker: Some("*.fsproj") },

    // ── Terraform ───────────────────────────────────────────────────
    JunkPattern { dir_name: ".terraform",          ecosystem: "Terraform",       marker: None },

    // ── React Native ────────────────────────────────────────────────
    JunkPattern { dir_name: "Pods",                ecosystem: "React Native (iOS)", marker: Some("Podfile") },

    // ── Docker ───────────────────────────────────────────────────────
    JunkPattern { dir_name: "com.docker.docker",   ecosystem: "Docker Desktop",  marker: None },

    // ── Homebrew ─────────────────────────────────────────────────────
    JunkPattern { dir_name: "Homebrew",            ecosystem: "Homebrew (cache)", marker: None },

    // ── Browser caches ───────────────────────────────────────────────
    JunkPattern { dir_name: "com.apple.Safari",    ecosystem: "Safari (cache)",   marker: None },
    JunkPattern { dir_name: "com.apple.WebKit.Networking", ecosystem: "WebKit (cache)", marker: None },
    JunkPattern { dir_name: "com.apple.WebKit.WebContent", ecosystem: "WebKit (cache)", marker: None },
    JunkPattern { dir_name: "com.google.Chrome",   ecosystem: "Chrome (cache)",   marker: None },
    JunkPattern { dir_name: "Google",              ecosystem: "Chrome (cache)",   marker: None },
    JunkPattern { dir_name: "Firefox",             ecosystem: "Firefox (cache)",  marker: None },
    JunkPattern { dir_name: "org.mozilla.firefox",  ecosystem: "Firefox (cache)", marker: None },
    JunkPattern { dir_name: "com.microsoft.Edge",  ecosystem: "Edge (cache)",     marker: None },
    JunkPattern { dir_name: "com.brave.Browser",   ecosystem: "Brave (cache)",    marker: None },
    JunkPattern { dir_name: "company.thebrowser.Browser", ecosystem: "Arc (cache)", marker: None },
    JunkPattern { dir_name: "com.operasoftware.Opera", ecosystem: "Opera (cache)", marker: None },

    // ── IDE / Editor caches ──────────────────────────────────────────
    JunkPattern { dir_name: "Code",                ecosystem: "VS Code (cache)",  marker: Some("CachedData") },
    JunkPattern { dir_name: "Cursor",              ecosystem: "Cursor (cache)",   marker: Some("CachedData") },
    JunkPattern { dir_name: ".vscode",             ecosystem: "VS Code",          marker: None },

    // ── Xcode ────────────────────────────────────────────────────────
    JunkPattern { dir_name: "DerivedData",         ecosystem: "Xcode",            marker: None },
    JunkPattern { dir_name: "Archives",            ecosystem: "Xcode (archives)", marker: None },
    JunkPattern { dir_name: "iOS DeviceSupport",   ecosystem: "Xcode (device)",   marker: None },
    JunkPattern { dir_name: "watchOS DeviceSupport", ecosystem: "Xcode (device)", marker: None },

    // ── Package manager global caches ────────────────────────────────
    JunkPattern { dir_name: ".npm",                ecosystem: "npm (global cache)", marker: None },
    JunkPattern { dir_name: ".yarn",               ecosystem: "Yarn (global cache)", marker: None },
    JunkPattern { dir_name: ".pnpm-store",         ecosystem: "pnpm (store)",     marker: None },
    JunkPattern { dir_name: ".bun",                ecosystem: "Bun (cache)",      marker: None },
    JunkPattern { dir_name: ".deno",               ecosystem: "Deno (cache)",     marker: None },
    JunkPattern { dir_name: ".cargo",              ecosystem: "Cargo (global cache)", marker: None },
    JunkPattern { dir_name: ".rustup",             ecosystem: "Rustup (toolchains)", marker: None },
    JunkPattern { dir_name: ".pub-cache",          ecosystem: "Dart (pub cache)", marker: None },
    JunkPattern { dir_name: ".cache",              ecosystem: "XDG cache",        marker: None },
    JunkPattern { dir_name: ".cocoapods",          ecosystem: "CocoaPods (cache)", marker: None },
    JunkPattern { dir_name: ".gradle",             ecosystem: "Gradle (global cache)", marker: None },
    JunkPattern { dir_name: ".m2",                 ecosystem: "Maven (local repo)", marker: None },
    JunkPattern { dir_name: ".ivy2",               ecosystem: "Ivy/SBT (cache)", marker: None },
    JunkPattern { dir_name: ".sbt",                ecosystem: "SBT (global)",    marker: None },
    JunkPattern { dir_name: ".conda",              ecosystem: "Conda (cache)",   marker: None },
    JunkPattern { dir_name: ".local",              ecosystem: "pip/pipx (cache)", marker: Some("share") },

    // ── AI / ML caches ───────────────────────────────────────────────
    JunkPattern { dir_name: ".ollama",             ecosystem: "Ollama (models)", marker: None },
    JunkPattern { dir_name: "huggingface",         ecosystem: "HuggingFace (cache)", marker: None },

    // ── Playwright / Puppeteer ───────────────────────────────────────
    JunkPattern { dir_name: "ms-playwright",       ecosystem: "Playwright (browsers)", marker: None },
    JunkPattern { dir_name: ".cache",              ecosystem: "Puppeteer (browsers)", marker: Some("puppeteer") },

    // ── Go ───────────────────────────────────────────────────────────
    JunkPattern { dir_name: "go",                  ecosystem: "Go (GOPATH)",     marker: Some("bin") },

    // ── Misc build artifacts ─────────────────────────────────────────
    JunkPattern { dir_name: ".next",               ecosystem: "Next.js (cache)", marker: None },
    JunkPattern { dir_name: ".nuxt",               ecosystem: "Nuxt.js (cache)", marker: None },
    JunkPattern { dir_name: ".angular",            ecosystem: "Angular (cache)", marker: None },
    JunkPattern { dir_name: ".parcel-cache",       ecosystem: "Parcel (cache)",  marker: None },
    JunkPattern { dir_name: "dist",                ecosystem: "Build output",    marker: Some("package.json") },
    JunkPattern { dir_name: ".svelte-kit",         ecosystem: "SvelteKit (cache)", marker: None },
    JunkPattern { dir_name: "coverage",            ecosystem: "Test coverage",   marker: Some("package.json") },
    JunkPattern { dir_name: ".pytest_cache",       ecosystem: "pytest (cache)",  marker: None },
    JunkPattern { dir_name: "htmlcov",             ecosystem: "Python coverage", marker: None },

    // ── macOS system caches / logs ───────────────────────────────────
    JunkPattern { dir_name: "CrashReporter",       ecosystem: "macOS (crashes)", marker: None },
    JunkPattern { dir_name: "DiagnosticReports",   ecosystem: "macOS (diagnostics)", marker: None },
];

/// A junk directory discovered during the scan.
#[derive(Debug, Clone)]
pub struct JunkHit {
    pub path: std::path::PathBuf,
    pub ecosystem: &'static str,
    pub size_bytes: u64,
}

/// Check if `dir_name` matches any junk pattern. For patterns with a marker,
/// `parent_entries` is probed (single `access()` syscall per marker).
///
/// Returns the ecosystem name on match, or `None`.
pub fn match_junk(dir_name: &OsStr, parent: &Path) -> Option<&'static str> {
    let name_bytes = dir_name.as_bytes();
    for pattern in PATTERNS {
        if name_bytes != pattern.dir_name.as_bytes() {
            continue;
        }
        match pattern.marker {
            None => return Some(pattern.ecosystem),
            Some(marker) => {
                if marker_exists(parent, marker) {
                    return Some(pattern.ecosystem);
                }
            }
        }
    }
    None
}

/// Check whether `marker` exists in `parent`. Supports:
/// - literal filenames (`Cargo.toml`)
/// - glob prefix (`*.csproj`) — scans dir for matching extension.
fn marker_exists(parent: &Path, marker: &str) -> bool {
    if let Some(ext) = marker.strip_prefix("*.") {
        // Glob: check if ANY file in parent has this extension.
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                if let Some(e) = entry.path().extension() {
                    if e == ext {
                        return true;
                    }
                }
            }
        }
        return false;
    }
    // Literal: might be a file OR a directory (e.g. "ProjectSettings").
    parent.join(marker).exists()
}

/// Calculate **actual disk usage** of a directory tree (blocking).
///
/// Uses `st_blocks * 512` instead of `st_size` so that sparse files
/// (e.g. Docker.raw) report their real on-disk footprint, not the
/// virtual file size.
pub fn dir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
    dir_size_inner(path, &mut total);
    total
}

fn dir_size_inner(path: &Path, total: &mut u64) {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if metadata.is_file() {
            *total += disk_usage(&metadata);
        } else if metadata.is_dir() {
            dir_size_inner(&entry.path(), total);
        }
    }
}

/// Actual bytes on disk: `st_blocks * 512`.
/// Falls back to `st_size` on platforms without block info.
#[cfg(unix)]
fn disk_usage(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks() * 512
}

#[cfg(not(unix))]
fn disk_usage(metadata: &std::fs::Metadata) -> u64 {
    metadata.len()
}
