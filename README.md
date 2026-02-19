# ulysses-link

Your code repos are full of useful documentation — READMEs, guides, changelogs, design docs — buried under `node_modules`, build artifacts, and thousands of source files. ulysses-link extracts just the docs and links them into a single clean directory that [Ulysses](https://ulysses.app) can import as an external folder.

Edits in Ulysses flow through to the real files instantly via symlinks. No syncing, no copies.

## Getting started

Install the binary:

```sh
cargo install ulysses-link
```

Sync your first repo:

```sh
ulysses-link sync ~/code/my-project
```

On the first run, ulysses-link will ask to create a config file. Hit enter to accept the defaults. It will scan the repo, find all the documentation files, and create symlinks in `~/ulysses-link/my-project/`.

Add more repos the same way:

```sh
ulysses-link sync ~/code/another-project
```

Now open Ulysses, go to **Library > Add External Folder**, and point it at `~/ulysses-link`. All your docs from all your repos appear in one place.

## Keep it synced

The `sync` command does a one-time scan. To keep things synced as files change, install the background service:

```sh
ulysses-link install
```

This installs a **launchd user agent** on macOS or a **systemd user unit** on Linux that starts on login and watches your repos for changes. Any new or deleted docs are reflected immediately.

After installing the service, running `ulysses-link sync <path>` will add the repo and automatically notify the running service to pick it up.

## Managing repos

```sh
ulysses-link sync <path>       # add a repo and sync it
ulysses-link sync              # re-sync all configured repos
ulysses-link remove <path>     # remove a repo (prompts for confirmation)
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

It automatically skips: `.git/`, `node_modules/`, `vendor/`, `.venv/`, `dist/`, `build/`, `target/`, `__pycache__/`, `.idea/`, `.vscode/`, `coverage/`, `.DS_Store`, and many other common non-doc directories. All patterns are configurable.

## Service management

```sh
ulysses-link install               # install background service
ulysses-link uninstall             # remove background service (prompts)
ulysses-link status                # check if the service is running
```

## CLI reference

```
ulysses-link sync [path]           Sync a repo (or all repos if no path given)
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
