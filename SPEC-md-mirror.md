# md-mirror — Markdown Symlink Sync Service

## Project Summary

**md-mirror** is a lightweight, dependency-minimal background service that monitors one or more local code repositories for Markdown files, applies configurable include/exclude filtering (using `.gitignore`-style patterns), and maintains a mirror directory of symlinks. The mirror directory is designed to be pointed at by Ulysses (or any similar editor) so it sees *only* the Markdown files — no `node_modules`, no `.git`, no build artifacts — and never locks up.

---

## 1. Goals & Non-Goals

### Goals

- Monitor N repos for `.md`/`.mdx` (configurable) file creation, deletion, movement, and rename.
- Apply `.gitignore`-style exclude patterns plus explicit include globs per repo.
- Maintain a directory tree of symlinks mirroring only the matched files, preserving relative folder structure to avoid name collisions.
- Run as a background service on macOS (launchd), Linux (systemd), or Windows (Task Scheduler / NSSM).
- Ship as a **single standalone binary** — no system Python required. Built via PyInstaller.
- Provide a `run` (foreground), `install`, `uninstall`, and `status` CLI.
- Config hot-reload: re-read the YAML config on `SIGHUP` (Unix) or on config file change (all platforms).

### Non-Goals

- Real-time collaborative editing features.
- Any GUI — this is a CLI/service only.
- Bi-directional sync (Ulysses edits flow through the symlinks automatically; no reverse watching needed).

---

## 2. Architecture Overview

```
┌────────────────────────┐
│   Config (YAML file)   │
└───────────┬────────────┘
            │ parsed at startup + on SIGHUP / config change
            ▼
┌────────────────────────┐
│      Core Engine       │
│  ┌──────────────────┐  │
│  │  Initial Scanner  │  │  ← walks each repo, builds full symlink tree
│  └──────────────────┘  │
│  ┌──────────────────┐  │
│  │  FS Watcher       │  │  ← watchdog Observer per repo root
│  │  (debounced)      │  │
│  └──────────────────┘  │
│  ┌──────────────────┐  │
│  │  Symlink Manager  │  │  ← creates/removes symlinks, prunes stale
│  └──────────────────┘  │
│  ┌──────────────────┐  │
│  │  Path Matcher     │  │  ← gitignore-style include/exclude filtering
│  └──────────────────┘  │
└───────────┬────────────┘
            │ symlinks
            ▼
┌────────────────────────┐
│  Mirror Output Dir     │  ← point Ulysses external folder here
│  ~/md-mirror/          │
│    ├── my-repo/        │
│    │   ├── README.md → /code/my-repo/README.md
│    │   └── docs/       │
│    │       └── API.md → /code/my-repo/docs/API.md
│    └── other-repo/     │
│        └── ...         │
└────────────────────────┘
```

---

## 3. Configuration Format

Config file location search order:
1. Path passed via `--config` CLI flag
2. `./md-mirror.yaml`
3. `~/.config/md-mirror/config.yaml`
4. (macOS) `~/Library/Application Support/md-mirror/config.yaml`

### Schema

```yaml
# md-mirror configuration
version: 1

# Where the symlink mirror tree is rooted.
# Tilde and env vars are expanded.
output_dir: ~/md-mirror

# Global exclude patterns applied to ALL repos (gitignore syntax).
# These are checked BEFORE per-repo excludes.
global_exclude:
  - node_modules/
  - .git/
  - __pycache__/
  - .venv/
  - "*.pyc"
  - .DS_Store

# Global include patterns. If empty, defaults to ["*.md", "*.mdx"].
global_include:
  - "*.md"
  - "*.mdx"

# Debounce window in seconds for filesystem events.
# After a burst of events (e.g. git pull), wait this long before syncing.
debounce_seconds: 0.5

# Logging level: DEBUG, INFO, WARNING, ERROR
log_level: INFO

# Per-repo definitions
repos:
  - path: ~/code/my-project
    # Optional: override the name used in the mirror tree.
    # Defaults to the directory basename.
    name: my-project

    # Per-repo additional excludes (merged with global_exclude).
    exclude:
      - vendor/
      - dist/
      - "docs/generated/"

    # Per-repo additional includes (merged with global_include).
    # Useful if one repo has .rst files you also want.
    include:
      - "*.rst"

  - path: ~/code/another-repo
    # Minimal config — just the path. Uses all global defaults.

  - path: ~/code/monorepo
    name: monorepo
    exclude:
      - packages/legacy/
      - "**/fixtures/**"
```

### Config Validation Rules

The implementing agent must validate on load:

- `version` must equal `1`.
- `output_dir` must be writable (create if missing).
- Each repo `path` must exist and be a directory. Log a warning and skip repos that don't exist (don't crash — the repo might appear later).
- `name` must be unique across repos. If not set, derive from `os.path.basename(path)`. If basenames collide, append a numeric suffix and warn.
- `debounce_seconds` must be >= 0.0 and <= 30.0.
- `global_include` must not be empty after merging defaults.

---

## 4. Dependencies

### Runtime Dependencies (Python packages)

| Package | Version | Purpose | Pure Python? |
|---------|---------|---------|-------------|
| `watchdog` | `>=4.0,<7.0` | Filesystem event monitoring. Uses FSEvents (macOS), inotify (Linux), ReadDirectoryChangesW (Windows) natively. | Has C extensions for native backends but ships pure-Python polling fallback. PyInstaller handles it well. |
| `pathspec` | `>=0.12,<2.0` | `.gitignore`-style pattern matching via `GitIgnoreSpec`. | Yes — pure Python. |
| `PyYAML` | `>=6.0,<7.0` | Config file parsing. | Has optional C extension (libyaml) but works without it. |

**That's it.** Three runtime dependencies, all well-established and PyInstaller-compatible.

### Build Dependencies

| Package | Purpose |
|---------|---------|
| `PyInstaller` `>=6.0` | Compile into standalone single-file binary. |
| `pytest` | Testing. |
| `pytest-mock` | Mocking filesystem events in tests. |

### System Dependencies

**None.** The whole point is grab-and-go. PyInstaller bundles the Python interpreter.

---

## 5. Module Structure

```
md-mirror/
├── pyproject.toml              # Project metadata, dependencies
├── README.md
├── md-mirror.yaml.example      # Example config
├── src/
│   └── md_mirror/
│       ├── __init__.py         # Package root, version string
│       ├── __main__.py         # Entry point: `python -m md_mirror`
│       ├── cli.py              # argparse CLI (run, install, uninstall, status, scan)
│       ├── config.py           # YAML loading, validation, schema defaults
│       ├── engine.py           # Core orchestrator: init scan + watcher lifecycle
│       ├── scanner.py          # Initial full-tree scan and symlink reconciliation
│       ├── watcher.py          # watchdog Observer setup, event handler, debouncing
│       ├── matcher.py          # pathspec-based include/exclude filtering
│       ├── linker.py           # Symlink creation, removal, stale pruning
│       ├── service.py          # OS service install/uninstall (launchd, systemd, Windows)
│       └── logging_config.py   # Structured logging setup
├── tests/
│   ├── conftest.py
│   ├── test_config.py
│   ├── test_matcher.py
│   ├── test_scanner.py
│   ├── test_linker.py
│   ├── test_watcher.py
│   └── test_engine.py
├── build.py                    # PyInstaller build script
└── service_templates/
    ├── launchd.plist.template
    ├── systemd.service.template
    └── README-windows.md
```

---

## 6. Detailed Module Specifications

### 6.1 `cli.py` — Command-Line Interface

Uses `argparse` (stdlib). No external CLI frameworks.

```
md-mirror run [--config PATH] [--foreground]
    Start the service in the foreground. This is the main entry point.

md-mirror scan [--config PATH]
    Run a one-shot scan: build/reconcile the full symlink tree, then exit.
    Useful for testing config or initial setup.

md-mirror install [--config PATH]
    Install the OS background service (launchd agent / systemd user unit).
    Writes the appropriate service file and enables it.

md-mirror uninstall
    Stop and remove the OS background service.

md-mirror status
    Check if the service is running and print summary (repos watched, files linked).

md-mirror version
    Print version and exit.
```

### 6.2 `config.py` — Configuration

**Responsibilities:**
- Load YAML from the resolved config path.
- Apply defaults for missing fields.
- Validate per the rules in Section 3.
- Expand `~` and environment variables in all path fields.
- Resolve all paths to absolute.
- Return a frozen dataclass (`Config`) containing a list of `RepoConfig` dataclasses.

```python
@dataclass(frozen=True)
class RepoConfig:
    path: Path              # Absolute resolved path to repo root
    name: str               # Mirror subdirectory name
    exclude_spec: PathSpec  # Compiled pathspec from global + per-repo excludes
    include_patterns: list[str]  # Raw glob list for include matching

@dataclass(frozen=True)
class Config:
    output_dir: Path
    repos: tuple[RepoConfig, ...]
    debounce_seconds: float
    log_level: str
```

**Config reload:** On `SIGHUP` (Unix) or when the config file itself changes (detected by the watcher), re-parse the config. Diff against current state:
- New repos → start watching.
- Removed repos → stop watching + clean up symlinks.
- Changed patterns → trigger a full re-scan for that repo.

### 6.3 `matcher.py` — Path Matching

Uses `pathspec.GitIgnoreSpec` for `.gitignore`-compatible pattern matching.

**Logic for deciding if a file should be mirrored:**

```python
def should_mirror(file_rel_path: str, repo_config: RepoConfig) -> bool:
    """
    Returns True if the file should have a symlink in the mirror.

    Algorithm:
    1. Check if file_rel_path matches any EXCLUDE pattern → False
    2. Check if file_rel_path matches any INCLUDE pattern → True
    3. Otherwise → False

    Exclude is checked FIRST so that node_modules/*.md is still excluded.
    """
```

**Key implementation notes for the agent:**
- `pathspec.GitIgnoreSpec.from_lines()` compiles patterns once at config load time. Keep the compiled spec on `RepoConfig`, don't recompile per event.
- Patterns operate on **relative paths from the repo root**, using forward slashes regardless of OS.
- Directory-matching patterns (ending in `/`) should also be used to **short-circuit the initial directory walk** — if `node_modules/` is excluded, `os.walk` should skip descending into it entirely.

### 6.4 `scanner.py` — Initial / Full Scan

**Responsibilities:**
- Walk each repo's file tree using `os.walk()`.
- For each directory, check if it should be excluded **before descending** (prune `dirs` in-place during `os.walk` — this is the critical optimization that prevents touching `node_modules`).
- For each file that passes `matcher.should_mirror()`, ensure a corresponding symlink exists in the mirror.
- After walking, prune any **stale symlinks** — symlinks in the mirror that no longer point to a valid source file, or whose source file no longer matches the current config.

```python
def full_scan(config: Config) -> ScanResult:
    """
    Returns:
        ScanResult with counts of: created, already_existed, pruned, errors
    """
```

**Directory walk with pruning (critical for performance):**

```python
for dirpath, dirs, files in os.walk(repo_path):
    rel_dir = os.path.relpath(dirpath, repo_path)

    # CRITICAL: prune excluded directories IN-PLACE to prevent descent
    # This is what keeps node_modules from being traversed
    dirs[:] = [
        d for d in dirs
        if not exclude_spec.match_file(os.path.join(rel_dir, d) + '/')
    ]

    for f in files:
        rel_path = os.path.join(rel_dir, f)
        if should_mirror(rel_path, repo_config):
            linker.ensure_symlink(repo_config, rel_path)
```

### 6.5 `watcher.py` — Filesystem Event Handling

Uses `watchdog.observers.Observer` with a custom event handler.

**Event handler class:**

```python
class MirrorEventHandler(FileSystemEventHandler):
    """
    Handles filesystem events and dispatches to the linker.

    Events we care about:
    - FileCreatedEvent   → check matcher, create symlink if passes
    - FileDeletedEvent   → remove symlink if it exists
    - FileMovedEvent     → remove old symlink, check new path, create if passes
    - FileModifiedEvent  → ignore (symlinks follow the target automatically)
    - DirCreatedEvent    → ignore (we care about files)
    - DirDeletedEvent    → prune any symlinks under that directory
    - DirMovedEvent      → re-scan for symlinks that need updating
    """
```

**Debouncing implementation:**

Use a `threading.Timer`-based debouncer per repo. When an event arrives:
1. Record it in a pending set.
2. Cancel any existing timer for that repo.
3. Start a new timer for `debounce_seconds`.
4. When the timer fires, process all pending events at once.

This prevents thrashing during `git pull`, `git checkout`, or bulk file operations.

```python
class DebouncedHandler:
    def __init__(self, repo_config, linker, debounce_seconds):
        self._pending: dict[str, str] = {}  # path → event_type
        self._timer: threading.Timer | None = None
        self._lock = threading.Lock()

    def on_event(self, event):
        with self._lock:
            rel_path = os.path.relpath(event.src_path, self.repo_config.path)
            self._pending[rel_path] = event.event_type
            if self._timer:
                self._timer.cancel()
            self._timer = threading.Timer(
                self.debounce_seconds, self._flush
            )
            self._timer.start()

    def _flush(self):
        with self._lock:
            batch = dict(self._pending)
            self._pending.clear()
        self._process_batch(batch)
```

**Important watchdog notes for the implementer:**
- `watchdog` v6.x is the current stable release. Requires Python 3.9+.
- On macOS, watchdog uses FSEvents by default — this is efficient and does **not** require opening file descriptors for every file (unlike kqueue).
- On Linux, watchdog uses inotify. Be aware of the `fs.inotify.max_user_watches` sysctl limit (default 8192 on some distros). For deeply nested repos, you may need to increase it. Log a warning if an `OSError` about inotify limits is caught.
- watchdog's `PatternMatchingEventHandler` exists but is **not sufficient** for our needs because it only filters on filename globs, not directory-based gitignore patterns. Use the base `FileSystemEventHandler` and do our own filtering via `pathspec`.
- Vim creates swap files and uses rename-to-replace. This generates `MovedEvent` rather than `ModifiedEvent`. The handler must handle `MovedEvent` where the destination matches our include pattern.

### 6.6 `linker.py` — Symlink Management

**Responsibilities:**
- Create parent directories in the mirror as needed.
- Create symlinks (`os.symlink()`).
- Remove symlinks.
- Prune stale symlinks (walk the mirror tree, check each symlink target).
- Handle race conditions (file deleted between event and processing).
- **Never delete real files** — only remove symlinks. Include a safety check: `os.path.islink()` before any `os.remove()`.

```python
def ensure_symlink(repo_config: RepoConfig, rel_path: str, output_dir: Path):
    """Create symlink if it doesn't exist. Idempotent."""
    source = repo_config.path / rel_path
    target = output_dir / repo_config.name / rel_path

    if target.is_symlink():
        if target.resolve() == source.resolve():
            return  # Already correct
        target.unlink()  # Points somewhere wrong, fix it

    target.parent.mkdir(parents=True, exist_ok=True)
    os.symlink(source, target)

def remove_symlink(repo_config: RepoConfig, rel_path: str, output_dir: Path):
    """Remove symlink. Only removes if it IS a symlink (safety)."""
    target = output_dir / repo_config.name / rel_path
    if target.is_symlink():
        target.unlink()
        # Clean up empty parent directories up to the repo mirror root
        _prune_empty_parents(target.parent, output_dir / repo_config.name)

def prune_stale(repo_config: RepoConfig, output_dir: Path):
    """Walk mirror tree and remove any symlinks whose target no longer exists."""
    mirror_root = output_dir / repo_config.name
    if not mirror_root.exists():
        return
    for dirpath, dirs, files in os.walk(mirror_root):
        for f in files:
            link = Path(dirpath) / f
            if link.is_symlink() and not link.resolve().exists():
                link.unlink()
    # Prune empty directories bottom-up
    _prune_empty_dirs(mirror_root)
```

**Windows note:** `os.symlink()` on Windows requires either admin privileges or Developer Mode enabled. The `service.py` module should detect this and provide a clear error message if symlinks fail, suggesting the user enable Developer Mode.

### 6.7 `service.py` — OS Service Management

Detects the current OS and manages service installation accordingly.

#### macOS — launchd User Agent

Install location: `~/Library/LaunchAgents/com.md-mirror.agent.plist`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.md-mirror.agent</string>
    <key>ProgramArguments</key>
    <array>
        <string>{BINARY_PATH}</string>
        <string>run</string>
        <string>--config</string>
        <string>{CONFIG_PATH}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{LOG_DIR}/md-mirror.stdout.log</string>
    <key>StandardErrorPath</key>
    <string>{LOG_DIR}/md-mirror.stderr.log</string>
    <key>ProcessType</key>
    <string>Background</string>
    <key>Nice</key>
    <integer>5</integer>
</dict>
</plist>
```

Commands:
- Install: write plist, then `launchctl load ~/Library/LaunchAgents/com.md-mirror.agent.plist`
- Uninstall: `launchctl unload ...`, then delete plist
- Status: `launchctl list | grep com.md-mirror`

#### Linux — systemd User Unit

Install location: `~/.config/systemd/user/md-mirror.service`

```ini
[Unit]
Description=md-mirror — Markdown symlink sync service
After=default.target

[Service]
Type=simple
ExecStart={BINARY_PATH} run --config {CONFIG_PATH}
Restart=on-failure
RestartSec=5
Environment=PYTHONUNBUFFERED=1

[Install]
WantedBy=default.target
```

Commands:
- Install: write unit file, `systemctl --user daemon-reload`, `systemctl --user enable --now md-mirror`
- Uninstall: `systemctl --user disable --now md-mirror`, delete unit file, `systemctl --user daemon-reload`
- Status: `systemctl --user status md-mirror`

#### Windows

Print instructions for the user to set up a Scheduled Task or use NSSM, rather than trying to automate Windows service management (which requires admin rights and has many edge cases). Provide the exact command line to use.

### 6.8 `engine.py` — Core Orchestrator

Ties everything together:

```python
class MirrorEngine:
    def __init__(self, config: Config):
        self.config = config
        self.observers: dict[str, Observer] = {}

    def start(self):
        """
        1. Run full_scan() for each repo.
        2. Start a watchdog Observer for each repo.
        3. Register SIGHUP handler for config reload.
        4. Enter main loop (sleep + health check).
        """

    def stop(self):
        """
        1. Stop all observers.
        2. Join observer threads.
        3. Optionally prune all symlinks (controlled by CLI flag).
        """

    def reload_config(self):
        """
        1. Re-read config.
        2. Diff repos: compute added, removed, changed.
        3. For removed: stop observer + prune symlinks.
        4. For added: full_scan + start observer.
        5. For changed patterns: full_scan (which handles pruning).
        """
```

**Graceful shutdown:** Handle `SIGTERM` and `SIGINT` to call `stop()`. Critical for launchd, which sends `SIGTERM` on unload.

---

## 7. Build & Distribution

### PyInstaller Single Binary

```python
# build.py
import PyInstaller.__main__

PyInstaller.__main__.run([
    'src/md_mirror/__main__.py',
    '--onefile',
    '--name', 'md-mirror',
    '--strip',                    # Strip debug symbols (smaller binary)
    '--noupx',                    # UPX causes issues on macOS ARM
    '--hidden-import', 'watchdog.observers',
    '--hidden-import', 'watchdog.observers.fsevents',  # macOS
    '--hidden-import', 'watchdog.observers.inotify',   # Linux
    '--hidden-import', 'watchdog.observers.read_directory_changes',  # Windows
])
```

**Build matrix (CI):**

| OS | Architecture | Binary name |
|----|-------------|-------------|
| macOS | arm64 (Apple Silicon) | `md-mirror-macos-arm64` |
| macOS | x86_64 | `md-mirror-macos-x64` |
| Linux | x86_64 | `md-mirror-linux-x64` |
| Windows | x86_64 | `md-mirror-windows-x64.exe` |

PyInstaller does **not** support cross-compilation. Each binary must be built on its target OS. Use GitHub Actions with a matrix build.

### pyproject.toml

```toml
[project]
name = "md-mirror"
version = "0.1.0"
description = "Markdown symlink sync service for Ulysses and other editors"
requires-python = ">=3.10"
dependencies = [
    "watchdog>=4.0,<7.0",
    "pathspec>=0.12,<2.0",
    "PyYAML>=6.0,<7.0",
]

[project.optional-dependencies]
dev = [
    "pyinstaller>=6.0",
    "pytest>=7.0",
    "pytest-mock>=3.0",
]

[project.scripts]
md-mirror = "md_mirror.cli:main"

[build-system]
requires = ["setuptools>=68.0"]
build-backend = "setuptools.backends._legacy:_Backend"
```

---

## 8. Testing Strategy

### Unit Tests

| Module | What to test |
|--------|-------------|
| `test_config.py` | YAML parsing, defaults, validation errors, path expansion, name collision handling |
| `test_matcher.py` | Include/exclude logic with `pathspec`. Test: `node_modules/README.md` is excluded, `docs/api.md` is included, `**` globs, negation patterns |
| `test_scanner.py` | Full scan creates correct symlinks, prunes stale, handles missing repos gracefully. Use `tmp_path` fixture. |
| `test_linker.py` | Symlink creation, idempotency, safety (never deletes real files), empty dir cleanup |
| `test_watcher.py` | Mock watchdog events → correct linker calls. Debounce batching. |
| `test_engine.py` | Start/stop lifecycle. Config reload diff logic. |

### Integration Test

One test that creates a real temp directory tree, starts the engine, creates/deletes/moves `.md` files, and asserts the mirror directory is correct after the debounce window.

### Manual Smoke Test Checklist

- [ ] Point at a real repo with `node_modules`. Verify no symlinks appear for anything inside `node_modules`.
- [ ] `git checkout` a branch with different `.md` files. Verify mirror updates.
- [ ] Rename a `.md` file. Verify old symlink removed, new one created.
- [ ] Delete a `.md` file. Verify symlink removed and empty parent dirs cleaned up.
- [ ] Add a new repo to config. Send `SIGHUP`. Verify new repo appears in mirror.
- [ ] Remove a repo from config. Send `SIGHUP`. Verify mirror subdirectory cleaned up.
- [ ] Run `md-mirror install`, reboot, verify service starts and mirror is correct.

---

## 9. Edge Cases & Gotchas

The implementing agent should handle these explicitly:

1. **Broken symlinks:** If a source file is deleted between the event and the symlink check, `os.symlink` will succeed (creating a broken link) and then the delete event should clean it up. But if events are lost, `prune_stale()` on the next full scan catches it.

2. **Rapidly changing files:** The debouncer collapses multiple events. A file created then immediately deleted should result in no symlink.

3. **Git operations:** `git checkout`, `git rebase`, `git stash pop` can generate hundreds of events. The debouncer is essential here. Consider logging "burst of N events in repo X, debouncing..." at DEBUG level.

4. **Circular symlinks in source:** `os.walk(followlinks=False)` (the default) prevents following symlinks in the source repo. Keep this default.

5. **Case sensitivity:** macOS filesystems are typically case-insensitive. `pathspec` should be initialized with `case_sensitive=False` on macOS, `True` on Linux. Detect via `sys.platform`.

6. **Long paths on Windows:** Use `\\?\` prefix for paths exceeding 260 characters if targeting Windows.

7. **inotify limits on Linux:** If `OSError: [Errno 28] inotify watch limit reached` is caught, log a clear message with the fix:
   ```
   WARNING: inotify watch limit reached. Run:
   echo fs.inotify.max_user_watches=524288 | sudo tee -a /etc/sysctl.conf
   sudo sysctl -p
   ```

8. **Repo disappears:** If a watched directory is deleted (e.g., `rm -rf ~/code/my-repo`), watchdog will raise an error. Catch it, log a warning, stop the observer for that repo, and mark it for re-scan on config reload.

9. **Config file doesn't exist on first run:** Generate a default config at the primary config path with a single commented-out repo entry and helpful comments.

10. **Mirror dir inside a watched repo:** Detect and refuse if `output_dir` is inside any repo `path`. This would create an infinite loop.

---

## 10. Logging

Use Python's `logging` module. Format:

```
2025-02-18 14:30:22 INFO  [engine] Started watching 3 repos, 47 files mirrored
2025-02-18 14:30:25 DEBUG [watcher:my-repo] FileCreated: docs/new-guide.md
2025-02-18 14:30:25 DEBUG [linker] Created symlink: ~/md-mirror/my-repo/docs/new-guide.md
2025-02-18 14:31:00 INFO  [watcher:my-repo] Debounced batch: 12 events → 3 creates, 2 deletes
```

Log to stderr when running in foreground. When running as a service, logs flow through launchd/systemd/Windows Event Log automatically via the stdout/stderr redirection in the service config.

---

## 11. Implementation Order

For the implementing agent, build in this order to allow testing at each stage:

1. **`config.py`** + `test_config.py` — Get config loading working first.
2. **`matcher.py`** + `test_matcher.py` — Pattern matching in isolation.
3. **`linker.py`** + `test_linker.py` — Symlink ops in isolation.
4. **`scanner.py`** + `test_scanner.py` — Combine matcher + linker for initial scan.
5. **`watcher.py`** + `test_watcher.py` — Add live filesystem monitoring.
6. **`engine.py`** + `test_engine.py` — Wire it all together.
7. **`cli.py`** — Add the CLI frontend.
8. **`service.py`** — OS service management.
9. **`build.py`** — PyInstaller packaging.
10. **Integration test** — End-to-end.

---

## 12. Future Enhancements (Out of Scope for v1)

- **Respect `.gitignore` files in repos automatically** — read each repo's `.gitignore` and merge with config excludes. Would require walking up the tree for nested `.gitignore` files.
- **Menu bar indicator** (macOS) — show sync status, quick access to config.
- **File count dashboard** — `md-mirror status --detail` showing per-repo file counts.
- **Hardlinks option** — for editors that don't follow symlinks.
- **Watch for new repos** — monitor a parent directory (e.g., `~/code/`) and auto-add repos that appear.
