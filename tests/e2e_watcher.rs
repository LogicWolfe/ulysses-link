use std::fs;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(10);

fn binary_path() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_ulysses-link").into()
}

fn wait_for<F: Fn() -> bool>(condition: F, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Let watcher event cycles settle so earlier operations don't interfere
/// with the next assertion (especially on macOS where FSEvents coalesces).
fn settle() {
    std::thread::sleep(Duration::from_secs(1));
}

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_service(config_path: &Path) -> ChildGuard {
    let child = Command::new(binary_path())
        .args(["run", "--config", &config_path.to_string_lossy()])
        .spawn()
        .expect("failed to spawn ulysses-link");
    ChildGuard(Some(child))
}

fn write_config(dir: &Path, repo_path: &Path, output_dir: &Path) -> std::path::PathBuf {
    let config_path = dir.join("config.toml");
    let content = format!(
        "version = 1\noutput_dir = \"{}\"\nauto_upgrade = false\n\n[[repos]]\npath = \"{}\"",
        output_dir.display(),
        repo_path.display()
    );
    fs::write(&config_path, content).unwrap();
    config_path
}

fn file_content(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

// ---------------------------------------------------------------------------
// Source → Mirror
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn test_watcher_full_sync_cycle() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let docs = repo.join("docs");
    let output = tmp.path().join("output");

    fs::create_dir_all(&docs).unwrap();
    fs::write(repo.join("README.md"), "# Hello").unwrap();
    fs::write(docs.join("guide.md"), "# Guide").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    let mirror_readme = mirror.join("README.md");
    let mirror_guide = mirror.join("docs").join("guide.md");

    // Initial scan
    assert!(
        wait_for(|| mirror_readme.exists() && mirror_guide.exists(), TIMEOUT),
        "mirror files should appear after initial scan"
    );
    assert_eq!(file_content(&mirror_readme).unwrap(), "# Hello");
    assert_eq!(file_content(&mirror_guide).unwrap(), "# Guide");

    // Source → mirror edit
    fs::write(repo.join("README.md"), "# Updated").unwrap();
    assert!(
        wait_for(
            || file_content(&mirror_readme).as_deref() == Some("# Updated"),
            TIMEOUT
        ),
        "mirror should reflect source edit"
    );

    // Mirror → source (atomic write)
    let tmp_file = mirror.join(".guide.md.tmp");
    fs::write(&tmp_file, "# Guide edited in Ulysses").unwrap();
    fs::rename(&tmp_file, &mirror_guide).unwrap();
    assert!(
        wait_for(
            || file_content(&docs.join("guide.md")).as_deref() == Some("# Guide edited in Ulysses"),
            TIMEOUT,
        ),
        "source should reflect mirror edit"
    );

    settle();

    // Source delete
    fs::remove_file(docs.join("guide.md")).unwrap();
    assert!(
        wait_for(|| !mirror_guide.exists(), TIMEOUT),
        "mirror file should be removed after source delete"
    );
}

#[test]
#[ignore]
fn test_watcher_no_sync_for_excluded_files() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Hello").unwrap();
    fs::write(repo.join("main.rs"), "fn main() {}").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    let mirror_readme = mirror.join("README.md");
    let mirror_main = mirror.join("main.rs");

    assert!(
        wait_for(|| mirror_readme.exists(), TIMEOUT),
        "README.md should be mirrored"
    );
    assert!(!mirror_main.exists(), "main.rs should not be mirrored");

    // Edit excluded file — should still not appear
    fs::write(repo.join("main.rs"), "fn main() { println!(\"hello\"); }").unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !mirror_main.exists(),
        "main.rs should still not be mirrored after edit"
    );
}

#[test]
#[ignore]
fn test_new_file_created_in_source() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Hello").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    assert!(
        wait_for(|| mirror.join("README.md").exists(), TIMEOUT),
        "initial scan"
    );

    // Create a new markdown file after initial scan
    fs::write(repo.join("CHANGELOG.md"), "# v1.0\n- Initial release").unwrap();
    assert!(
        wait_for(
            || file_content(&mirror.join("CHANGELOG.md")).as_deref()
                == Some("# v1.0\n- Initial release"),
            TIMEOUT,
        ),
        "new file should appear in mirror"
    );
}

#[test]
#[ignore]
fn test_new_directory_with_files() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Hello").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    assert!(
        wait_for(|| mirror.join("README.md").exists(), TIMEOUT),
        "initial scan"
    );

    // Create a new subdirectory with multiple files
    let new_dir = repo.join("docs").join("api");
    fs::create_dir_all(&new_dir).unwrap();
    fs::write(new_dir.join("overview.md"), "# API Overview").unwrap();
    fs::write(new_dir.join("auth.md"), "# Authentication").unwrap();
    // Non-matching file should be ignored
    fs::write(new_dir.join("schema.json"), "{}").unwrap();

    let mirror_overview = mirror.join("docs").join("api").join("overview.md");
    let mirror_auth = mirror.join("docs").join("api").join("auth.md");
    let mirror_schema = mirror.join("docs").join("api").join("schema.json");

    assert!(
        wait_for(|| mirror_overview.exists() && mirror_auth.exists(), TIMEOUT),
        "new directory files should appear in mirror"
    );
    assert_eq!(file_content(&mirror_overview).unwrap(), "# API Overview");
    assert_eq!(file_content(&mirror_auth).unwrap(), "# Authentication");
    assert!(
        !mirror_schema.exists(),
        "non-matching file should not be mirrored"
    );
}

#[test]
#[ignore]
fn test_source_directory_deletion() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let docs = repo.join("docs");
    let output = tmp.path().join("output");

    fs::create_dir_all(&docs).unwrap();
    fs::write(repo.join("README.md"), "# Hello").unwrap();
    fs::write(docs.join("a.md"), "# A").unwrap();
    fs::write(docs.join("b.md"), "# B").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    let mirror_a = mirror.join("docs").join("a.md");
    let mirror_b = mirror.join("docs").join("b.md");

    assert!(
        wait_for(|| mirror_a.exists() && mirror_b.exists(), TIMEOUT),
        "initial scan should create mirror files"
    );

    settle();

    // Remove the entire docs directory
    fs::remove_dir_all(&docs).unwrap();
    assert!(
        wait_for(|| !mirror_a.exists() && !mirror_b.exists(), TIMEOUT),
        "all mirror files in directory should be removed"
    );
}

#[test]
#[ignore]
fn test_non_matching_file_created_after_scan() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Hello").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    assert!(
        wait_for(|| mirror.join("README.md").exists(), TIMEOUT),
        "initial scan"
    );

    // Create various non-matching files
    fs::write(repo.join("main.rs"), "fn main() {}").unwrap();
    fs::write(repo.join("Cargo.toml"), "[package]").unwrap();
    fs::write(repo.join("data.json"), "{}").unwrap();

    std::thread::sleep(Duration::from_secs(3));

    assert!(
        !mirror.join("main.rs").exists(),
        ".rs should not be mirrored"
    );
    assert!(
        !mirror.join("Cargo.toml").exists(),
        ".toml should not be mirrored"
    );
    assert!(
        !mirror.join("data.json").exists(),
        ".json should not be mirrored"
    );
}

#[test]
#[ignore]
fn test_file_with_spaces_in_name() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Hello").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    assert!(
        wait_for(|| mirror.join("README.md").exists(), TIMEOUT),
        "initial scan"
    );

    // Create file with spaces
    fs::write(repo.join("my notes.md"), "# My Notes").unwrap();
    let mirror_notes = mirror.join("my notes.md");
    assert!(
        wait_for(
            || file_content(&mirror_notes).as_deref() == Some("# My Notes"),
            TIMEOUT
        ),
        "file with spaces should be mirrored"
    );

    // Edit it
    fs::write(repo.join("my notes.md"), "# My Notes (updated)").unwrap();
    assert!(
        wait_for(
            || file_content(&mirror_notes).as_deref() == Some("# My Notes (updated)"),
            TIMEOUT,
        ),
        "edit to file with spaces should propagate"
    );
}

#[test]
#[ignore]
fn test_deeply_nested_directory() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("output");

    let deep = repo.join("a").join("b").join("c").join("d");
    fs::create_dir_all(&deep).unwrap();
    fs::write(repo.join("README.md"), "# Root").unwrap();
    fs::write(deep.join("deep.md"), "# Deep").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    let mirror_deep = mirror
        .join("a")
        .join("b")
        .join("c")
        .join("d")
        .join("deep.md");

    assert!(
        wait_for(
            || mirror.join("README.md").exists() && mirror_deep.exists(),
            TIMEOUT
        ),
        "deeply nested file should be mirrored"
    );
    assert_eq!(file_content(&mirror_deep).unwrap(), "# Deep");

    // Edit the deeply nested file
    fs::write(deep.join("deep.md"), "# Deep (edited)").unwrap();
    assert!(
        wait_for(
            || file_content(&mirror_deep).as_deref() == Some("# Deep (edited)"),
            TIMEOUT
        ),
        "edit to deeply nested file should propagate"
    );
}

#[test]
#[ignore]
fn test_rapid_successive_edits() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "v0").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    let mirror_readme = mirror.join("README.md");
    assert!(wait_for(|| mirror_readme.exists(), TIMEOUT), "initial scan");

    // Fire off many rapid edits — debounce should coalesce them
    for i in 1..=10 {
        fs::write(repo.join("README.md"), format!("v{i}")).unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }

    // The final state should eventually be the last write
    assert!(
        wait_for(
            || file_content(&mirror_readme).as_deref() == Some("v10"),
            TIMEOUT
        ),
        "mirror should reflect final edit after rapid succession"
    );
}

#[test]
#[ignore]
fn test_source_file_rename() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("old-name.md"), "# Content").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    let mirror_old = mirror.join("old-name.md");
    let mirror_new = mirror.join("new-name.md");

    assert!(
        wait_for(|| mirror_old.exists(), TIMEOUT),
        "original file should be mirrored"
    );

    settle();

    // Rename the source file
    fs::rename(repo.join("old-name.md"), repo.join("new-name.md")).unwrap();

    assert!(
        wait_for(|| mirror_new.exists() && !mirror_old.exists(), TIMEOUT),
        "rename should remove old mirror and create new one"
    );
    assert_eq!(file_content(&mirror_new).unwrap(), "# Content");
}

// ---------------------------------------------------------------------------
// Mirror → Source
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn test_mirror_in_place_edit() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("notes.md"), "# Original").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    let mirror_notes = mirror.join("notes.md");

    assert!(wait_for(|| mirror_notes.exists(), TIMEOUT), "initial scan");
    settle();

    // Direct in-place write to the mirror file (no atomic rename)
    fs::write(&mirror_notes, "# Edited directly").unwrap();
    assert!(
        wait_for(
            || file_content(&repo.join("notes.md")).as_deref() == Some("# Edited directly"),
            TIMEOUT,
        ),
        "in-place mirror edit should propagate to source"
    );
}

#[test]
#[ignore]
fn test_mirror_file_deletion_propagates_to_source() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Keep").unwrap();
    fs::write(repo.join("delete-me.md"), "# Delete me").unwrap();

    let config_path = write_config(tmp.path(), &repo, &output);
    let _guard = spawn_service(&config_path);

    let mirror = output.join("repo");
    let mirror_delete = mirror.join("delete-me.md");

    assert!(
        wait_for(
            || mirror.join("README.md").exists() && mirror_delete.exists(),
            TIMEOUT
        ),
        "initial scan"
    );
    settle();

    // Delete the mirror file — should propagate to source
    fs::remove_file(&mirror_delete).unwrap();
    assert!(
        wait_for(|| !repo.join("delete-me.md").exists(), TIMEOUT),
        "source file should be deleted when mirror is deleted"
    );
    // The other file should be unaffected
    assert!(
        repo.join("README.md").exists(),
        "unrelated source file should survive"
    );
}

// ---------------------------------------------------------------------------
// Config reload
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn test_config_reload_adds_repo() {
    let tmp = TempDir::new().unwrap();
    let repo1 = tmp.path().join("repo1");
    let repo2 = tmp.path().join("repo2");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo1).unwrap();
    fs::create_dir_all(&repo2).unwrap();
    fs::write(repo1.join("README.md"), "# Repo 1").unwrap();
    fs::write(repo2.join("README.md"), "# Repo 2").unwrap();

    // Start with only repo1
    let config_path = write_config(tmp.path(), &repo1, &output);
    let _guard = spawn_service(&config_path);

    let mirror1 = output.join("repo1").join("README.md");
    let mirror2 = output.join("repo2").join("README.md");

    assert!(
        wait_for(|| mirror1.exists(), TIMEOUT),
        "repo1 should be mirrored"
    );
    assert!(!mirror2.exists(), "repo2 should not yet be mirrored");

    // Let config watcher fully initialize before modifying config
    settle();

    // Modify config to add repo2
    let new_content = format!(
        "version = 1\noutput_dir = \"{}\"\nauto_upgrade = false\n\n[[repos]]\npath = \"{}\"\n\n[[repos]]\npath = \"{}\"",
        output.display(),
        repo1.display(),
        repo2.display()
    );
    fs::write(&config_path, new_content).unwrap();

    assert!(
        wait_for(|| mirror2.exists(), TIMEOUT),
        "repo2 should appear after config reload"
    );
    assert_eq!(file_content(&mirror2).unwrap(), "# Repo 2");

    // Verify repo2 watcher is active: edit a file in repo2
    fs::write(repo2.join("README.md"), "# Repo 2 updated").unwrap();
    assert!(
        wait_for(
            || file_content(&mirror2).as_deref() == Some("# Repo 2 updated"),
            TIMEOUT
        ),
        "edits in newly added repo should propagate"
    );
}

#[test]
#[ignore]
fn test_config_reload_removes_repo() {
    let tmp = TempDir::new().unwrap();
    let repo1 = tmp.path().join("repo1");
    let repo2 = tmp.path().join("repo2");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo1).unwrap();
    fs::create_dir_all(&repo2).unwrap();
    fs::write(repo1.join("README.md"), "# Repo 1").unwrap();
    fs::write(repo2.join("README.md"), "# Repo 2").unwrap();

    // Start with both repos
    let config_path = tmp.path().join("config.toml");
    let content = format!(
        "version = 1\noutput_dir = \"{}\"\nauto_upgrade = false\n\n[[repos]]\npath = \"{}\"\n\n[[repos]]\npath = \"{}\"",
        output.display(),
        repo1.display(),
        repo2.display()
    );
    fs::write(&config_path, &content).unwrap();
    let _guard = spawn_service(&config_path);

    let mirror1 = output.join("repo1").join("README.md");
    let mirror2 = output.join("repo2").join("README.md");

    assert!(
        wait_for(|| mirror1.exists() && mirror2.exists(), TIMEOUT),
        "both repos should be mirrored"
    );

    // Let config watcher fully initialize before modifying config
    settle();

    // Remove repo2 from config
    let new_content = format!(
        "version = 1\noutput_dir = \"{}\"\nauto_upgrade = false\n\n[[repos]]\npath = \"{}\"",
        output.display(),
        repo1.display()
    );
    fs::write(&config_path, new_content).unwrap();

    assert!(
        wait_for(|| !mirror2.exists(), TIMEOUT),
        "repo2 mirror should be pruned after removal from config"
    );
    // repo1 should be unaffected
    assert!(mirror1.exists(), "repo1 should still be mirrored");
}

// ---------------------------------------------------------------------------
// Multi-repo
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn test_two_repos_isolation() {
    let tmp = TempDir::new().unwrap();
    let repo1 = tmp.path().join("repo1");
    let repo2 = tmp.path().join("repo2");
    let output = tmp.path().join("output");

    fs::create_dir_all(&repo1).unwrap();
    fs::create_dir_all(&repo2).unwrap();
    fs::write(repo1.join("README.md"), "# Repo 1").unwrap();
    fs::write(repo2.join("README.md"), "# Repo 2").unwrap();

    let config_path = tmp.path().join("config.toml");
    let content = format!(
        "version = 1\noutput_dir = \"{}\"\nauto_upgrade = false\n\n[[repos]]\npath = \"{}\"\n\n[[repos]]\npath = \"{}\"",
        output.display(),
        repo1.display(),
        repo2.display()
    );
    fs::write(&config_path, &content).unwrap();
    let _guard = spawn_service(&config_path);

    let mirror1 = output.join("repo1");
    let mirror2 = output.join("repo2");

    assert!(
        wait_for(
            || mirror1.join("README.md").exists() && mirror2.join("README.md").exists(),
            TIMEOUT,
        ),
        "both repos should be mirrored"
    );

    // Edit only repo1
    fs::write(repo1.join("README.md"), "# Repo 1 edited").unwrap();
    assert!(
        wait_for(
            || file_content(&mirror1.join("README.md")).as_deref() == Some("# Repo 1 edited"),
            TIMEOUT,
        ),
        "repo1 mirror should update"
    );
    // repo2 should be unchanged
    assert_eq!(
        file_content(&mirror2.join("README.md")).unwrap(),
        "# Repo 2",
        "repo2 mirror should be unaffected"
    );

    // Edit only repo2
    fs::write(repo2.join("README.md"), "# Repo 2 edited").unwrap();
    assert!(
        wait_for(
            || file_content(&mirror2.join("README.md")).as_deref() == Some("# Repo 2 edited"),
            TIMEOUT,
        ),
        "repo2 mirror should update"
    );
    // repo1 should still have its edit
    assert_eq!(
        file_content(&mirror1.join("README.md")).unwrap(),
        "# Repo 1 edited",
        "repo1 mirror should be unaffected by repo2 edit"
    );

    // Create new file in repo1, verify it doesn't appear in repo2's mirror
    fs::write(repo1.join("NOTES.md"), "# Notes").unwrap();
    assert!(
        wait_for(|| mirror1.join("NOTES.md").exists(), TIMEOUT),
        "new file should appear in repo1 mirror"
    );
    assert!(
        !mirror2.join("NOTES.md").exists(),
        "repo1's new file should not appear in repo2 mirror"
    );
}
