use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// Helper to write a TOML config file and return its path.
fn create_test_config(repo_paths: &[&Path], output_dir: &Path, config_dir: &Path) -> String {
    let repos_toml: String = repo_paths
        .iter()
        .map(|p| format!("[[repos]]\npath = \"{}\"", p.display()))
        .collect::<Vec<_>>()
        .join("\n\n");

    let config_content = format!(
        "version = 1\noutput_dir = \"{}\"\n\n{}",
        output_dir.display(),
        repos_toml
    );

    let config_path = config_dir.join("ulysses-link.toml");
    fs::write(&config_path, &config_content).unwrap();
    config_path.to_string_lossy().to_string()
}

fn load_manifests(
    config: &ulysses_link::config::Config,
) -> HashMap<PathBuf, ulysses_link::manifest::Manifest> {
    let mut manifests = HashMap::new();
    for od in config.active_output_dirs() {
        manifests
            .entry(od.clone())
            .or_insert_with(|| ulysses_link::manifest::Manifest::load(&od).unwrap());
    }
    manifests
}

#[test]
fn test_end_to_end_scan() {
    let tmp = TempDir::new().unwrap();

    let repo1 = tmp.path().join("repo1");
    let repo2 = tmp.path().join("repo2");
    let output = tmp.path().join("mirror");

    fs::create_dir_all(&repo1).unwrap();
    fs::create_dir_all(&repo2).unwrap();

    // Repo 1: mix of markdown and non-markdown
    fs::write(repo1.join("README.md"), "# Repo 1").unwrap();
    fs::create_dir(repo1.join("docs")).unwrap();
    fs::write(repo1.join("docs").join("guide.md"), "# Guide").unwrap();
    fs::write(repo1.join("docs").join("api.txt"), "API docs").unwrap();
    fs::write(repo1.join("main.rs"), "fn main() {}").unwrap();
    fs::write(repo1.join("Cargo.toml"), "[package]").unwrap();
    fs::write(repo1.join("LICENSE"), "MIT").unwrap();

    // Repo 1: excluded directories
    fs::create_dir_all(repo1.join("node_modules").join("pkg")).unwrap();
    fs::write(
        repo1.join("node_modules").join("pkg").join("README.md"),
        "npm package",
    )
    .unwrap();
    fs::create_dir(repo1.join(".git")).unwrap();
    fs::write(repo1.join(".git").join("HEAD"), "ref: refs/heads/main").unwrap();
    fs::create_dir(repo1.join("target")).unwrap();
    fs::write(repo1.join("target").join("doc.md"), "build output").unwrap();

    // Repo 2: simple
    fs::write(repo2.join("README.md"), "# Repo 2").unwrap();
    fs::write(repo2.join("CHANGELOG"), "v1.0").unwrap();
    fs::create_dir(repo2.join(".venv")).unwrap();
    fs::write(repo2.join(".venv").join("readme.md"), "venv").unwrap();

    let config_path_str =
        create_test_config(&[repo1.as_path(), repo2.as_path()], &output, tmp.path());

    let config_path = std::path::PathBuf::from(&config_path_str);
    let config = ulysses_link::config::load_config(Some(&config_path)).unwrap();
    let mut manifests = load_manifests(&config);
    let result = ulysses_link::scanner::full_scan(&config, &mut manifests);

    assert!(
        result.errors == 0,
        "Expected no errors, got {}",
        result.errors
    );
    assert!(result.created > 0, "Expected some files created");

    // Verify correct copies exist (regular files, not symlinks)
    let readme = output.join("repo1").join("README.md");
    assert!(readme.exists());
    assert!(!readme.is_symlink(), "Should be a copy, not a symlink");

    assert!(output.join("repo1").join("docs").join("guide.md").exists());
    assert!(output.join("repo1").join("docs").join("api.txt").exists());
    assert!(output.join("repo1").join("LICENSE").exists());
    assert!(output.join("repo2").join("README.md").exists());
    assert!(output.join("repo2").join("CHANGELOG").exists());

    // Verify content matches
    assert_eq!(
        fs::read_to_string(output.join("repo1").join("README.md")).unwrap(),
        "# Repo 1"
    );
    assert_eq!(
        fs::read_to_string(output.join("repo2").join("CHANGELOG")).unwrap(),
        "v1.0"
    );

    // Verify excluded files are NOT in mirror
    assert!(!output.join("repo1").join("main.rs").exists());
    assert!(!output.join("repo1").join("Cargo.toml").exists());
    assert!(!output.join("repo1").join("node_modules").exists());
    assert!(!output.join("repo1").join(".git").exists());
    assert!(!output.join("repo1").join("target").exists());
    assert!(!output.join("repo2").join(".venv").exists());

    // Run scan again — should be idempotent
    let result2 = ulysses_link::scanner::full_scan(&config, &mut manifests);
    assert_eq!(result2.created, 0, "Second scan should create nothing");
    assert_eq!(
        result2.already_existed, result.created,
        "All should already exist"
    );

    // Delete a source file and re-scan — should prune
    fs::remove_file(repo1.join("docs").join("guide.md")).unwrap();
    let result3 = ulysses_link::scanner::full_scan(&config, &mut manifests);
    assert_eq!(result3.pruned, 1, "Should prune one stale entry");
    assert!(!output.join("repo1").join("docs").join("guide.md").exists());
}

#[test]
fn test_repo_name_collision_in_mirror() {
    let tmp = TempDir::new().unwrap();

    let repo1 = tmp.path().join("a").join("project");
    let repo2 = tmp.path().join("b").join("project");
    let output = tmp.path().join("mirror");

    fs::create_dir_all(&repo1).unwrap();
    fs::create_dir_all(&repo2).unwrap();
    fs::write(repo1.join("README.md"), "repo 1").unwrap();
    fs::write(repo2.join("README.md"), "repo 2").unwrap();

    let config_path_str =
        create_test_config(&[repo1.as_path(), repo2.as_path()], &output, tmp.path());

    let config_path = std::path::PathBuf::from(&config_path_str);
    let config = ulysses_link::config::load_config(Some(&config_path)).unwrap();

    assert_eq!(config.repos.len(), 2);
    assert_eq!(config.repos[0].name, "project");
    assert_eq!(config.repos[1].name, "project-2");

    let mut manifests = load_manifests(&config);
    let result = ulysses_link::scanner::full_scan(&config, &mut manifests);
    assert_eq!(result.created, 2);

    // Both should have their own mirror directory
    assert!(output.join("project").join("README.md").exists());
    assert!(output.join("project-2").join("README.md").exists());

    assert_eq!(
        fs::read_to_string(output.join("project").join("README.md")).unwrap(),
        "repo 1"
    );
    assert_eq!(
        fs::read_to_string(output.join("project-2").join("README.md")).unwrap(),
        "repo 2"
    );
}

#[test]
fn test_source_edit_propagates_to_mirror() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("mirror");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "original").unwrap();

    let config_path_str = create_test_config(&[repo.as_path()], &output, tmp.path());
    let config_path = std::path::PathBuf::from(&config_path_str);
    let config = ulysses_link::config::load_config(Some(&config_path)).unwrap();
    let mut manifests = load_manifests(&config);

    ulysses_link::scanner::full_scan(&config, &mut manifests);
    assert_eq!(
        fs::read_to_string(output.join("repo").join("README.md")).unwrap(),
        "original"
    );

    // Edit source
    fs::write(repo.join("README.md"), "updated").unwrap();
    ulysses_link::scanner::full_scan(&config, &mut manifests);

    assert_eq!(
        fs::read_to_string(output.join("repo").join("README.md")).unwrap(),
        "updated"
    );
}

#[test]
fn test_mirror_edit_propagates_to_source() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("mirror");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "original").unwrap();

    let config_path_str = create_test_config(&[repo.as_path()], &output, tmp.path());
    let config_path = std::path::PathBuf::from(&config_path_str);
    let config = ulysses_link::config::load_config(Some(&config_path)).unwrap();
    let mut manifests = load_manifests(&config);

    ulysses_link::scanner::full_scan(&config, &mut manifests);

    // Edit mirror
    fs::write(output.join("repo").join("README.md"), "edited in ulysses").unwrap();
    ulysses_link::scanner::full_scan(&config, &mut manifests);

    assert_eq!(
        fs::read_to_string(repo.join("README.md")).unwrap(),
        "edited in ulysses"
    );
}

#[test]
fn test_non_overlapping_edits_merge() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("mirror");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "line1\nline2\nline3\n").unwrap();

    let config_path_str = create_test_config(&[repo.as_path()], &output, tmp.path());
    let config_path = std::path::PathBuf::from(&config_path_str);
    let config = ulysses_link::config::load_config(Some(&config_path)).unwrap();
    let mut manifests = load_manifests(&config);

    ulysses_link::scanner::full_scan(&config, &mut manifests);

    // Source edits line1, mirror edits line3
    fs::write(repo.join("README.md"), "LINE1\nline2\nline3\n").unwrap();
    fs::write(
        output.join("repo").join("README.md"),
        "line1\nline2\nLINE3\n",
    )
    .unwrap();

    let result = ulysses_link::scanner::full_scan(&config, &mut manifests);
    assert_eq!(result.merged, 1);

    let content = fs::read_to_string(repo.join("README.md")).unwrap();
    assert!(content.contains("LINE1"), "Source should have merged LINE1");
    assert!(content.contains("LINE3"), "Source should have merged LINE3");
    assert_eq!(
        fs::read_to_string(output.join("repo").join("README.md")).unwrap(),
        content,
        "Mirror should match source after merge"
    );
}

#[test]
fn test_conflicting_edits_create_conflict_file() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("mirror");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "original\n").unwrap();

    let config_path_str = create_test_config(&[repo.as_path()], &output, tmp.path());
    let config_path = std::path::PathBuf::from(&config_path_str);
    let config = ulysses_link::config::load_config(Some(&config_path)).unwrap();
    let mut manifests = load_manifests(&config);

    ulysses_link::scanner::full_scan(&config, &mut manifests);

    // Both edit same line
    fs::write(repo.join("README.md"), "source version\n").unwrap();
    fs::write(output.join("repo").join("README.md"), "mirror version\n").unwrap();

    let result = ulysses_link::scanner::full_scan(&config, &mut manifests);
    assert_eq!(result.conflicts, 1);

    // Check that a conflict file exists somewhere
    let has_conflict_in_repo = fs::read_dir(&repo)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains(".conflict_"));
    let has_conflict_in_mirror = fs::read_dir(output.join("repo"))
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains(".conflict_"));
    assert!(
        has_conflict_in_repo || has_conflict_in_mirror,
        "Expected a conflict file"
    );
}

#[test]
fn test_delete_from_source_propagates() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("mirror");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "hello").unwrap();

    let config_path_str = create_test_config(&[repo.as_path()], &output, tmp.path());
    let config_path = std::path::PathBuf::from(&config_path_str);
    let config = ulysses_link::config::load_config(Some(&config_path)).unwrap();
    let mut manifests = load_manifests(&config);

    ulysses_link::scanner::full_scan(&config, &mut manifests);
    assert!(output.join("repo").join("README.md").exists());

    // Delete source
    fs::remove_file(repo.join("README.md")).unwrap();
    let result = ulysses_link::scanner::full_scan(&config, &mut manifests);
    assert_eq!(result.pruned, 1);
    assert!(!output.join("repo").join("README.md").exists());
}

#[test]
fn test_non_owned_files_never_touched() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("mirror");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "hello").unwrap();

    // Create a non-owned file in the mirror directory before sync
    fs::create_dir_all(output.join("repo")).unwrap();
    fs::write(
        output.join("repo").join(".Ulysses-Group.plist"),
        "ulysses data",
    )
    .unwrap();
    fs::write(output.join("repo").join("manual.md"), "user file").unwrap();

    let config_path_str = create_test_config(&[repo.as_path()], &output, tmp.path());
    let config_path = std::path::PathBuf::from(&config_path_str);
    let config = ulysses_link::config::load_config(Some(&config_path)).unwrap();
    let mut manifests = load_manifests(&config);

    ulysses_link::scanner::full_scan(&config, &mut manifests);

    // Non-owned files should still exist
    assert!(output.join("repo").join(".Ulysses-Group.plist").exists());
    assert_eq!(
        fs::read_to_string(output.join("repo").join(".Ulysses-Group.plist")).unwrap(),
        "ulysses data"
    );
    // manual.md content doesn't match any source, so it should be skipped
    assert!(output.join("repo").join("manual.md").exists());
    assert_eq!(
        fs::read_to_string(output.join("repo").join("manual.md")).unwrap(),
        "user file"
    );
}

#[test]
fn test_manifest_persisted_across_scans() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let output = tmp.path().join("mirror");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "hello").unwrap();

    let config_path_str = create_test_config(&[repo.as_path()], &output, tmp.path());
    let config_path = std::path::PathBuf::from(&config_path_str);
    let config = ulysses_link::config::load_config(Some(&config_path)).unwrap();

    // First scan with fresh manifests
    let mut manifests = load_manifests(&config);
    ulysses_link::scanner::full_scan(&config, &mut manifests);

    // Load manifests from disk (simulating a new process)
    let mut manifests2 = load_manifests(&config);
    let manifest2 = manifests2.get(&config.repos[0].output_dir).unwrap();
    assert!(manifest2.get("repo/README.md").is_some());

    // Second scan should recognize files as already existing
    let result = ulysses_link::scanner::full_scan(&config, &mut manifests2);
    assert_eq!(result.created, 0);
    assert_eq!(result.already_existed, 1);
}

#[test]
fn test_per_repo_output_dir() {
    let tmp = TempDir::new().unwrap();

    let repo1 = tmp.path().join("repo1");
    let repo2 = tmp.path().join("repo2");
    let output1 = tmp.path().join("output1");
    let output2 = tmp.path().join("output2");

    fs::create_dir_all(&repo1).unwrap();
    fs::create_dir_all(&repo2).unwrap();

    fs::write(repo1.join("README.md"), "repo 1").unwrap();
    fs::write(repo2.join("README.md"), "repo 2").unwrap();

    let config_content = format!(
        "version = 1\noutput_dir = \"{}\"\n\n[[repos]]\npath = \"{}\"\n\n[[repos]]\npath = \"{}\"\noutput_dir = \"{}\"",
        output1.display(),
        repo1.display(),
        repo2.display(),
        output2.display(),
    );
    let config_path = tmp.path().join("ulysses-link.toml");
    fs::write(&config_path, &config_content).unwrap();

    let config = ulysses_link::config::load_config(Some(&config_path)).unwrap();
    assert_eq!(
        config.repos[0].output_dir,
        std::fs::canonicalize(&output1).unwrap()
    );
    assert_eq!(
        config.repos[1].output_dir,
        std::fs::canonicalize(&output2).unwrap()
    );

    let mut manifests = load_manifests(&config);
    let result = ulysses_link::scanner::full_scan(&config, &mut manifests);
    assert_eq!(result.created, 2);

    // Files should appear in their respective output dirs
    let canon_out1 = std::fs::canonicalize(&output1).unwrap();
    let canon_out2 = std::fs::canonicalize(&output2).unwrap();
    assert!(canon_out1.join("repo1").join("README.md").exists());
    assert!(canon_out2.join("repo2").join("README.md").exists());

    // Files should NOT appear in the wrong output dir
    assert!(!canon_out1.join("repo2").exists());
    assert!(!canon_out2.join("repo1").exists());

    assert_eq!(
        fs::read_to_string(canon_out1.join("repo1").join("README.md")).unwrap(),
        "repo 1"
    );
    assert_eq!(
        fs::read_to_string(canon_out2.join("repo2").join("README.md")).unwrap(),
        "repo 2"
    );
}
