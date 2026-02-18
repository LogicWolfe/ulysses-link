# doc-link

A lightweight background service that monitors local code repositories for documentation files and maintains a mirror directory of symlinks. Point Ulysses (or any editor) at the mirror to see only your docs — no `node_modules`, no `.git`, no build artifacts.

Single native binary. No runtime dependencies. Install with `cargo install doc-link`.

## How it works

doc-link watches your repos for file changes and maintains a parallel directory tree of symlinks to just the documentation files:

```
~/doc-link/
  ├── my-project/
  │   ├── README.md → ~/code/my-project/README.md
  │   ├── LICENSE → ~/code/my-project/LICENSE
  │   └── docs/
  │       └── guide.rst → ~/code/my-project/docs/guide.rst
  └── other-repo/
      └── ...
```

Symlinks mean edits in Ulysses flow through to the real files instantly — no syncing, no copies.

## What gets mirrored

Out of the box, doc-link includes these file types:

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
cargo install doc-link
```

Or build from source:

```sh
git clone <repo-url> && cd ulysses-code-docs/doc-link
cargo install --path .
```

## Configure

On first run, doc-link generates a default config at `~/.config/doc-link/config.yaml`. Or copy the example:

```sh
cp doc-link.yaml.example ~/.config/doc-link/config.yaml
```

Then add your repos and you're ready to go.

Config search order:

1. `--config PATH` (explicit CLI flag)
2. `./doc-link.yaml` (current directory)
3. `~/.config/doc-link/config.yaml`
4. `~/Library/Application Support/doc-link/config.yaml` (macOS only)

### Minimal config

All you need is a version, output directory, and at least one repo:

```yaml
version: 1
output_dir: ~/doc-link
repos:
  - path: ~/code/my-project
```

This uses all the default include/exclude patterns listed above.

### Per-repo overrides

Add extra excludes or includes for specific repos:

```yaml
version: 1
output_dir: ~/doc-link
repos:
  - path: ~/code/my-project
    name: my-project          # optional, defaults to directory basename
    exclude:
      - docs/generated/       # skip generated docs
    include:
      - "*.tex"               # also mirror LaTeX files for this repo

  - path: ~/code/another-repo # minimal — just the path, all defaults
```

### All config options

```yaml
version: 1                    # required, must be 1
output_dir: ~/doc-link        # where the symlink tree lives
debounce_seconds: 0.5         # batch rapid events (0.0–30.0)
log_level: INFO               # TRACE, DEBUG, INFO, WARNING, ERROR
global_exclude: [...]         # override the default exclude list
global_include: [...]         # override the default include list
repos:                        # list of repos to watch
  - path: ~/code/repo         # required
    name: repo                # optional, derived from path basename
    exclude: [...]            # merged with global_exclude
    include: [...]            # merged with global_include
```

Exclude patterns use `.gitignore` syntax. Include patterns use glob syntax.

## Run

### One-shot scan

Build the full symlink tree once and exit. Good for testing your config:

```sh
doc-link scan
doc-link scan --config ~/my-config.yaml
```

### Foreground service

Watch repos for changes continuously:

```sh
doc-link run
doc-link run --config ~/my-config.yaml
```

Stop with `Ctrl-C`. Send `SIGHUP` to reload config without restarting.

### Background service

Install as an OS service that starts on login:

```sh
doc-link install --config ~/.config/doc-link/config.yaml
```

This installs a **launchd user agent** on macOS or a **systemd user unit** on Linux. On Windows, it prints manual setup instructions for Task Scheduler or NSSM.

Manage the service:

```sh
doc-link status      # check if running
doc-link uninstall   # stop and remove
```

## CLI reference

```
doc-link run [--config PATH]       Start watching repos (foreground)
doc-link scan [--config PATH]      One-shot scan, then exit
doc-link install [--config PATH]   Install as OS background service
doc-link uninstall                 Remove OS background service
doc-link status                    Check service status
doc-link version                   Print version
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
doc-link/
├── src/
│   ├── main.rs          # clap CLI entry point
│   ├── lib.rs           # library root
│   ├── config.rs        # YAML loading, validation, path expansion
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
| `serde` + `serde_yaml` | YAML config deserialization |
| `clap` | CLI argument parsing |
| `tracing` | Structured logging |
