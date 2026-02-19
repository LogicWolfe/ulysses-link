# ulysses-link

A background service that extracts documentation files from code repositories and links them into a directory structure that [Ulysses](https://ulysses.app) can import as an external folder.

Files are linked via symlinks, so edits in Ulysses write directly to the original files.

## Getting started

Install the binary:

```sh
cargo install ulysses-link
```

Sync your first repo, specifying where the symlink tree should be rooted:

```sh
ulysses-link sync ~/code/my-project ~/ulysses-link
```

This creates a config file, scans the repo for documentation files, and creates symlinks under `~/ulysses-link/my-project/`.

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

This installs a **launchd user agent** on macOS or a **systemd user unit** on Linux that starts on login and watches configured repos for changes.

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

This opens `~/.config/ulysses-link/config.toml` in your `$EDITOR`. The config looks like this:

```toml
version = 1
output_dir = "~/ulysses-link"

[[repos]]
path = "~/code/my-project"

[[repos]]
path = "~/code/another-project"
```

### Per-repo overrides

Add extra excludes or includes for specific repos:

```toml
[[repos]]
path = "~/code/my-project"
name = "my-project"                # optional, defaults to directory basename
exclude = ["docs/generated/"]      # merged with global excludes
include = ["*.tex"]                # also link LaTeX files for this repo
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

## What gets linked

By default, ulysses-link includes: `*.md`, `*.mdx`, `*.markdown`, `*.txt`, `*.rst`, `*.adoc`, `*.org`, `README`, `LICENSE`, `CHANGELOG`, `CONTRIBUTING`, `AUTHORS`, `COPYING`, and `TODO`.

It skips: `.git/`, `node_modules/`, `vendor/`, `.venv/`, `dist/`, `build/`, `target/`, `__pycache__/`, `.idea/`, `.vscode/`, `coverage/`, `.DS_Store`, and other common non-documentation directories. All patterns are configurable.

## Non-destructive sync

Sync will never overwrite real files in the output directory. If a regular file or directory exists where a symlink would be placed, it is skipped with a warning. Existing symlinks pointing to the wrong target are replaced.

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
