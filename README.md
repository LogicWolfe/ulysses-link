# ulysses-link

A background service that extracts documentation files from code repositories and mirrors them into a directory structure that [Ulysses](https://ulysses.app) can import as an external folder.

Files are copied (using APFS copy-on-write, so zero extra disk space) and kept in bidirectional sync — edits made in Ulysses flow back to the source repos automatically.

## Getting started

Install the binary:

```sh
cargo install ulysses-link
```

Sync your first repo, specifying where the mirror tree should be rooted:

```sh
ulysses-link sync ~/code/my-project ~/ulysses-link
```

This creates a config file, scans the repo for documentation files, and copies them under `~/ulysses-link/my-project/`.

Add more repos the same way:

```sh
ulysses-link sync ~/code/another-project ~/ulysses-link
```

Then open Ulysses, go to **Library > Add External Folder**, and point it at `~/ulysses-link`.

## Keep it synced

The `sync` command does a one-time scan. To watch for changes continuously, install the background service:

```sh
ulysses-link install
```

This installs a **launchd user agent** on macOS or a **systemd user unit** on Linux that starts on login and watches configured repos for changes. Edits made in Ulysses are detected and synced back to source repos.

After installing the service, running `ulysses-link sync <path> <output>` will add the repo and notify the running service to pick it up.

## Managing repos

```sh
ulysses-link sync <path> <output>  # add a repo and sync it
ulysses-link sync                  # re-sync all configured repos
ulysses-link remove <path>         # remove a repo (prompts for confirmation)
```

## Configuration

ulysses-link manages its config file automatically when you use `sync` and `remove`. To edit it directly:

```sh
ulysses-link config
```

This opens `~/.config/ulysses-link/config.toml` in your `$EDITOR`.

## Bidirectional sync

ulysses-link uses a manifest file (`.ulysses-link` in the output directory) to track every file it owns. This enables:

- **Source → mirror:** Changes in your repos are copied to the mirror tree
- **Mirror → source:** Edits made in Ulysses are copied back to the source repo
- **Three-way merge:** When both sides change non-overlapping sections, changes are merged cleanly
- **Conflict resolution:** When both sides change the same lines, the newest version wins and the older version is saved as a `.conflict_<timestamp>` file

Files not tracked in the manifest (like Ulysses metadata files) are never modified or deleted.

## Service management

```sh
ulysses-link install               # install background service
ulysses-link uninstall             # remove background service (prompts)
ulysses-link status                # check if the service is running
```

## CLI reference

```
ulysses-link sync [path] [output]  Sync a repo (or all repos if no path given)
ulysses-link remove <path>         Remove a repo from config
ulysses-link config                Open config in your editor
ulysses-link install               Install as background service
ulysses-link uninstall             Remove background service
ulysses-link status                Check service status
ulysses-link version               Print version
```

## Config file format

The config file is located at `~/.config/ulysses-link/config.toml`. It is created automatically on the first `sync` and updated by `sync` and `remove`. Tilde (`~`) and environment variables are expanded in all paths.

### Minimal example

```toml
version = 1
output_dir = "~/ulysses-link"

[[repos]]
path = "~/code/my-project"
```

### Multiple repos with overrides

```toml
version = 1
output_dir = "~/ulysses-link"

[[repos]]
path = "~/code/my-project"
name = "my-project"                # optional, defaults to directory basename
exclude = ["docs/generated/"]      # merged with global excludes
include = ["*.tex"]                # also link LaTeX files for this repo

[[repos]]
path = "~/code/another-project"
```

### Global options

| Field | Default | Description |
|---|---|---|
| `version` | — | Required. Must be `1`. |
| `output_dir` | — | Required. Root of the mirror tree. |
| `debounce_seconds` | `0.5` | Seconds to wait after a burst of filesystem events before syncing. Range: 0.0–30.0. |
| `log_level` | `"INFO"` | One of `TRACE`, `DEBUG`, `INFO`, `WARNING`, `ERROR`. |
| `rescan_interval` | `"auto"` | How often to do a full rescan. `"auto"` scales with scan speed, `"never"` disables, or a number of seconds. |
| `global_exclude` | *(see below)* | Exclude patterns applied to all repos. `.gitignore` syntax. |
| `global_include` | *(see below)* | Include patterns applied to all repos. Glob syntax. |

### Per-repo options (`[[repos]]`)

| Field | Default | Description |
|---|---|---|
| `path` | — | Required. Path to the repository. |
| `name` | directory basename | Name used for the mirror subdirectory. |
| `exclude` | `[]` | Additional exclude patterns, merged with `global_exclude`. |
| `include` | `[]` | Additional include patterns, merged with `global_include`. |

### Default patterns

**Includes:** `*.md`, `*.mdx`, `*.markdown`, `*.txt`, `*.rst`, `*.adoc`, `*.org`, `README`, `LICENSE`, `LICENCE`, `CHANGELOG`, `CONTRIBUTING`, `AUTHORS`, `COPYING`, `TODO`

**Excludes:** `.git/`, `.svn/`, `.hg/`, `node_modules/`, `bower_components/`, `vendor/`, `.pnpm-store/`, `.venv/`, `venv/`, `dist/`, `build/`, `out/`, `target/`, `_build/`, `.next/`, `.nuxt/`, `.svelte-kit/`, `.docusaurus/`, `__pycache__/`, `*.pyc`, `*.pyo`, `.mypy_cache/`, `.pytest_cache/`, `.ruff_cache/`, `.tox/`, `*.egg-info/`, `.idea/`, `.vscode/`, `*.swp`, `*.swo`, `*~`, `.DS_Store`, `Thumbs.db`, `coverage/`, `htmlcov/`, `.nyc_output/`, `.cache/`, `.gradle/`, `.terraform/`

Exclude patterns are checked before includes, so a file like `node_modules/pkg/README.md` stays excluded. Setting `global_exclude` or `global_include` in the config replaces the defaults entirely.

### Manifest file

The manifest (`.ulysses-link`) is stored in the output directory and tracks every file ulysses-link owns. A base version cache (`.ulysses-link.d/`) stores the last-synced content of each file for three-way merging. Both are managed automatically.

## Development

```sh
cargo test                         # run all tests
cargo clippy -- -D warnings        # lint
cargo fmt                          # format
```

### Pre-commit hooks

This project uses [Lefthook](https://github.com/evilmartians/lefthook) to run `cargo fmt` and `cargo clippy --fix` before each commit. Fixed files are automatically staged.

After cloning, install the hooks:

```sh
lefthook install
```
