# ulysses-link

Extract the meaningful documentation from your code repositories and link it all into one place for [Ulysses](https://ulysses.app) external folder importing. No `node_modules`, no `.git`, no build artifacts — just your docs.

Single native binary. No runtime dependencies. Install with `cargo install ulysses-link`.

## How it works

ulysses-link watches your repos for file changes and maintains a parallel directory tree of symlinks to just the documentation files:

```
~/ulysses-link/
  ├── my-project/
  │   ├── README.md → ~/code/my-project/README.md
  │   ├── LICENSE → ~/code/my-project/LICENSE
  │   └── docs/
  │       └── guide.rst → ~/code/my-project/docs/guide.rst
  └── other-repo/
      └── ...
```

Point Ulysses at `~/ulysses-link` as an external folder and you get a clean, unified view of all your project documentation. Symlinks mean edits in Ulysses flow through to the real files instantly — no syncing, no copies.

## What gets linked

Out of the box, ulysses-link includes these file types:

| Pattern | What it catches |
|---------|-----------------|
| `*.md`, `*.mdx`, `*.markdown` | Markdown in all common extensions |
| `*.txt` | Plain text (notes, requirements.txt, etc.) |
| `*.rst` | reStructuredText (Sphinx docs) |
| `*.adoc` | AsciiDoc |
| `*.org` | Org-mode |
| `README`, `LICENSE`, `LICENCE` | Common extensionless doc files |
| `CHANGELOG`, `CONTRIBUTING` | Common extensionless doc files |
| `AUTHORS`, `COPYING`, `TODO` | Common extensionless doc files |

And automatically skips these directories:

| Category | Excluded |
|----------|----------|
| Version control | `.git/`, `.svn/`, `.hg/` |
| Dependencies | `node_modules/`, `bower_components/`, `vendor/`, `.pnpm-store/` |
| Virtual envs | `.venv/`, `venv/` |
| Build output | `dist/`, `build/`, `out/`, `target/`, `_build/` |
| Framework caches | `.next/`, `.nuxt/`, `.svelte-kit/`, `.docusaurus/` |
| Python caches | `__pycache__/`, `.mypy_cache/`, `.pytest_cache/`, `.ruff_cache/`, `.tox/`, `*.egg-info/` |
| IDE / editor | `.idea/`, `.vscode/`, `*.swp`, `*~` |
| Test coverage | `coverage/`, `htmlcov/`, `.nyc_output/` |
| Misc | `.cache/`, `.gradle/`, `.terraform/`, `.DS_Store`, `Thumbs.db` |

All patterns are configurable per-repo. Excludes are checked first, so `node_modules/README.md` stays excluded.

## Install

```sh
cargo install ulysses-link
```

Or build from source:

```sh
git clone https://github.com/LogicWolfe/ulysses-link.git && cd ulysses-link/ulysses-link
cargo install --path .
```

## Configure

On first run, ulysses-link generates a default config at `~/.config/ulysses-link/config.toml`. Or copy the example:

```sh
cp ulysses-link.toml.example ~/.config/ulysses-link/config.toml
```

Then add your repos and you're ready to go.

Config search order:

1. `--config PATH` (explicit CLI flag)
2. `./ulysses-link.toml` (current directory)
3. `~/.config/ulysses-link/config.toml`
4. `~/Library/Application Support/ulysses-link/config.toml` (macOS only)

### Minimal config

All you need is a version, output directory, and at least one repo:

```toml
version = 1
output_dir = "~/ulysses-link"

[[repos]]
path = "~/code/my-project"
```

This uses all the default include/exclude patterns listed above.

### Per-repo overrides

Add extra excludes or includes for specific repos:

```toml
version = 1
output_dir = "~/ulysses-link"

[[repos]]
path = "~/code/my-project"
name = "my-project"                # optional, defaults to directory basename
exclude = ["docs/generated/"]      # merged with global_exclude
include = ["*.tex"]                # also mirror LaTeX files for this repo

[[repos]]
path = "~/code/another-repo"      # minimal — just the path, all defaults
```

### All config options

```toml
version = 1                        # required, must be 1
output_dir = "~/ulysses-link"      # where the symlink tree lives
debounce_seconds = 0.5             # batch rapid events (0.0–30.0)
log_level = "INFO"                 # TRACE, DEBUG, INFO, WARNING, ERROR
global_exclude = ["..."]           # override the default exclude list
global_include = ["..."]           # override the default include list

[[repos]]                          # one section per repo
path = "~/code/repo"               # required
name = "repo"                      # optional, derived from path basename
exclude = ["..."]                  # merged with global_exclude
include = ["..."]                  # merged with global_include
```

Exclude patterns use `.gitignore` syntax. Include patterns use glob syntax.

## Use with Ulysses

1. Run `ulysses-link scan` to build the symlink tree
2. In Ulysses, go to Library > Add External Folder
3. Point it at `~/ulysses-link` (or your configured `output_dir`)
4. All your documentation from all repos appears in one place
5. Run `ulysses-link install` to keep it synced automatically in the background

## Run

### One-shot scan

Build the full symlink tree once and exit. Good for testing your config:

```sh
ulysses-link scan
ulysses-link scan --config ~/my-config.toml
```

### Foreground service

Watch repos for changes continuously:

```sh
ulysses-link run
ulysses-link run --config ~/my-config.toml
```

Stop with `Ctrl-C`. Send `SIGHUP` to reload config without restarting.

### Background service

Install as an OS service that starts on login:

```sh
ulysses-link install --config ~/.config/ulysses-link/config.toml
```

This installs a **launchd user agent** on macOS or a **systemd user unit** on Linux. On Windows, it prints manual setup instructions for Task Scheduler or NSSM.

Manage the service:

```sh
ulysses-link status      # check if running
ulysses-link uninstall   # stop and remove
```

## CLI reference

```
ulysses-link run [--config PATH]       Start watching repos (foreground)
ulysses-link scan [--config PATH]      One-shot scan, then exit
ulysses-link install [--config PATH]   Install as OS background service
ulysses-link uninstall                 Remove OS background service
ulysses-link status                    Check service status
ulysses-link version                   Print version
```

## Development

Run all tests:

```sh
cargo test
```

Run a specific test:

```sh
cargo test test_node_modules_excluded
```

Check and lint:

```sh
cargo check
cargo clippy
cargo fmt
```

### Project structure

```
ulysses-link/
├── src/
│   ├── main.rs          # clap CLI entry point
│   ├── lib.rs           # library root
│   ├── config.rs        # TOML loading, validation, path expansion
│   ├── matcher.rs       # include/exclude filtering (ignore + globset)
│   ├── linker.rs        # symlink creation/removal/pruning
│   ├── scanner.rs       # full tree scan + reconciliation
│   ├── watcher.rs       # notify filesystem events + debouncing
│   ├── engine.rs        # core orchestrator (scan + watch lifecycle)
│   └── service.rs       # OS service install/uninstall/status
└── tests/
    └── integration.rs   # end-to-end tests
```

### Key crates

| Crate | Purpose |
|-------|---------|
| `notify` | Filesystem event watching (FSEvents/inotify/ReadDirectoryChanges) |
| `ignore` | Gitignore-style exclude pattern matching |
| `globset` | Glob include pattern matching |
| `serde` + `toml` | TOML config deserialization |
| `clap` | CLI argument parsing |
| `tracing` | Structured logging |
