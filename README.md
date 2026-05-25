# kosu

Blazing-fast recursive disk crawler and junk finder for macOS.

Built in Rust. Uses platform-specific bulk syscalls (`getattrlistbulk`, `readdir`) + a custom work-stealing thread pool on `crossbeam-deque` — no rayon, no overhead.

> **Note:** Currently implemented and tested on macOS only (Apple Silicon). Linux and Windows backends are stubbed out and compile but have not been tested. See [TODO](#todo).

## Performance

825k entries across `/Projects`, warm cache, M4 Max:

| Tool | Time | vs `find` |
|---|---|---|
| `find` | 12.4s | 1x |
| `fd` (ignore crate) | 1.95s | 6.4x |
| **kosu (lean, no sizes)** | **1.22s** | **10x** |
| **kosu (with sizes)** | **1.70s** | **7.3x** |

### Why it's fast

- **readdir(3)** for lean mode — simpler kernel path on APFS than `getattrlistbulk` when only names + `d_type` are needed
- **getattrlistbulk(2)** for size mode — one syscall per ~800 entries instead of N `lstat` calls
- **Per-thread Cell counters** — zero shared-atomic contention in the hot path; counters merge once on thread exit
- **Thread-local reusable buffers** — sub-dir Vec and PathBuf scratch allocated once per thread, not per directory
- **Apple Silicon P-core detection** — auto-selects optimal thread count via `hw.perflevel0.physicalcpu` sysctl, avoids E-core scheduling overhead
- **Batched atomic ops** — one `fetch_add` per directory instead of per child entry

## Install

```bash
git clone --recurse-submodules https://github.com/gfhfyjbr/kosu.git
cd kosu
cargo build --release
# Binary: target/release/kosu
```

## Usage

### Fast walker

```bash
# Lean mode (readdir, no file sizes) — fastest
kosu /path/to/scan

# With file sizes (getattrlistbulk)
kosu /path/to/scan -s

# Print every path to stdout
kosu /path/to/scan --list

# Custom thread count
kosu /path/to/scan -t 8
```

### Junk finder

```bash
# Interactive TUI
kosu clean /path/to/scan

# Headless (piped output)
kosu clean /path/to/scan | less
```

TUI keybindings:

| Key | Action |
|---|---|
| `j` / `k` | Navigate up/down |
| `g` / `G` | Jump to top/bottom |
| `Space` | Toggle selection |
| `a` | Select/deselect all |
| `d` / `Enter` | Delete selected (non-blocking, via `rm -rf`) |
| `q` / `Esc` | Quit |

### Supported ecosystems (60+)

| Ecosystem | Detected directories | Marker file |
|---|---|---|
| Node.js | `node_modules` | — |
| Turborepo | `.turbo` | — |
| Python | `__pycache__`, `.tox`, `.eggs`, `.venv`, `.mypy_cache`, `.pytest_cache`, `.ruff_cache` | — |
| Python (venv) | `venv` | `pyvenv.cfg` |
| Pixi | `.pixi` | — |
| Jupyter | `.ipynb_checkpoints` | — |
| Rust (Cargo) | `target` | `Cargo.toml` |
| CMake | `build`, `cmake-build-debug`, `cmake-build-release` | `CMakeLists.txt` |
| Composer (PHP) | `vendor` | `composer.json` |
| Elixir | `_build`, `deps` | `mix.exs` |
| Godot 4.x | `.godot`, `.mono` | — |
| Gradle | `.gradle`, `build` | `build.gradle` / `build.gradle.kts` |
| Maven | `target` | `pom.xml` |
| Dart (pub) | `.dart_tool`, `build` | `pubspec.yaml` |
| SBT (Scala) | `target`, `.bsp` | `build.sbt` |
| Stack (Haskell) | `.stack-work` | — |
| Cabal (Haskell) | `dist-newstyle` | — |
| Swift (SPM) | `.build` | `Package.swift` |
| Xcode | `DerivedData` | — |
| Unity | `Library`, `Temp` | `ProjectSettings` |
| Unreal Engine | `Intermediate`, `Saved`, `DerivedDataCache` | — |
| Zig | `zig-cache`, `zig-out`, `.zig-cache` | — |
| .NET (C#/F#) | `bin`, `obj` | `*.csproj` / `*.fsproj` |
| Terraform | `.terraform` | — |
| React Native | `Pods` | `Podfile` |
| Docker | `com.docker.docker` | — |
| Homebrew | `Homebrew` (in Caches) | — |
| Safari | `com.apple.Safari` | — |
| Chrome | `com.google.Chrome`, `Google` | — |
| Firefox | `Firefox`, `org.mozilla.firefox` | — |
| Edge | `com.microsoft.Edge` | — |
| Brave | `com.brave.Browser` | — |
| Arc | `company.thebrowser.Browser` | — |
| Opera | `com.operasoftware.Opera` | — |
| VS Code | `Code` (CachedData), `.vscode` | `CachedData` |
| Cursor | `Cursor` (CachedData) | `CachedData` |
| Xcode | `DerivedData`, `Archives`, `iOS DeviceSupport` | — |
| npm | `.npm` | — |
| Yarn | `.yarn` | — |
| pnpm | `.pnpm-store` | — |
| Bun | `.bun` | — |
| Deno | `.deno` | — |
| Cargo | `.cargo` | — |
| Rustup | `.rustup` | — |
| CocoaPods | `.cocoapods` | — |
| Conda | `.conda` | — |
| Ollama | `.ollama` | — |
| HuggingFace | `huggingface` | — |
| Playwright | `ms-playwright` | — |
| Go | `go` (GOPATH) | `bin` |
| Next.js | `.next` | — |
| Nuxt.js | `.nuxt` | — |
| Angular | `.angular` | — |
| Parcel | `.parcel-cache` | — |
| SvelteKit | `.svelte-kit` | — |
| macOS | `CrashReporter`, `DiagnosticReports` | — |

Ambiguous directory names (`target`, `build`, `vendor`, `bin`, `obj`, `deps`, `Code`, `go`) require a marker file in the parent directory to avoid false positives.

## Architecture

```
src/
├── main.rs              CLI entry, per-thread counters, mode dispatch
├── walker.rs            Work-stealing parallel walker (crossbeam-deque)
├── junk.rs              Junk pattern definitions (25+ ecosystems)
├── scan.rs              Junk scan orchestration
├── tui_app.rs           Interactive TUI (kou-ui)
└── platform/
    ├── mod.rs           cfg-dispatched reader factory
    ├── macos.rs         getattrlistbulk + readdir backends
    ├── linux.rs         getdents64 + statx (stub)
    └── windows.rs       NtQueryDirectoryFile (stub)
```

### Backends

| Platform | Lean (no sizes) | With sizes | Status |
|---|---|---|---|
| macOS | `readdir(3)` + `d_type` | `getattrlistbulk(2)` | Implemented, tested |
| Linux | `getdents64` + `statx` fallback | — | Stubbed, compiles |
| Windows | `NtQueryDirectoryFile` | — | Stubbed, compiles |

### TUI

Built on [kou-ui](https://github.com/gfhfyjbr/kou-ui) — a React-like terminal UI framework in Rust with Fiber reconciler, hooks, signals, and typed styles. Included as a git submodule.

## TODO

- [ ] Test and fix Linux backend (`getdents64` + `statx`)
- [ ] Test and fix Windows backend (`NtQueryDirectoryFile`)
- [ ] After kou-ui implements native + web renderers, add native/web UI for kosu
- [ ] `io_uring` batching for Linux
- [ ] Size caching / incremental re-scan
- [ ] Filter/search within junk list
- [ ] Dry-run mode (`--dry-run`)
- [ ] Config file for custom junk patterns

## License

MIT
