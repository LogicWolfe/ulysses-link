use std::fs;
use std::path::Path;

use tempfile::TempDir;

/// Helper to write a config file and load it.
fn create_test_config(repo_paths: &[&Path], output_dir: &Path, config_dir: &Path) -> String {
    let repos_yaml: String = repo_paths
        .iter()
        .map(|p| format!("  - path: {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n");

    let config_content = format!(
        "version: 1\noutput_dir: {}\nrepos:\n{}",
        output_dir.display(),
        repos_yaml
    );

    let config_path = config_dir.join("doc-link.yaml");
    fs::write(&config_path, &config_content).unwrap();
    config_path.to_string_lossy().to_string()
}

#[test]
fn test_end_to_end_scan() {
    let tmp = TempDir::new().unwrap();

    // Create two repos with various files
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

    // Create config
    let config_path_str = create_test_config(
        &[repo1.as_path(), repo2.as_path()],
        &output,
        tmp.path(),
    );

    // Load and scan
    let config_path = std::path::PathBuf::from(&config_path_str);
    let config = doc_link::config::load_config(Some(&config_path)).unwrap();
    let result = doc_link::scanner::full_scan(&config);

    // Verify results
    assert!(result.errors == 0, "Expected no errors, got {}", result.errors);
    assert!(result.created > 0, "Expected some files created");

    // Verify correct symlinks exist
    assert!(output.join("repo1").join("README.md").is_symlink());
    assert!(output.join("repo1").join("docs").join("guide.md").is_symlink());
    assert!(output.join("repo1").join("docs").join("api.txt").is_symlink());
    assert!(output.join("repo1").join("LICENSE").is_symlink());
    assert!(output.join("repo2").join("README.md").is_symlink());
    assert!(output.join("repo2").join("CHANGELOG").is_symlink());

    // Verify symlinks point to correct targets
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
    let result2 = doc_link::scanner::full_scan(&config);
    assert_eq!(result2.created, 0, "Second scan should create nothing");
    assert_eq!(result2.already_existed, result.created, "All should already exist");

    // Delete a source file and re-scan — should prune
    fs::remove_file(repo1.join("docs").join("guide.md")).unwrap();
    let result3 = doc_link::scanner::full_scan(&config);
    assert_eq!(result3.pruned, 1, "Should prune one stale symlink");
    assert!(!output.join("repo1").join("docs").join("guide.md").exists());
}

#[test]
fn test_repo_name_collision_in_mirror() {
    let tmp = TempDir::new().unwrap();

    // Two repos with the same basename
    let repo1 = tmp.path().join("a").join("project");
    let repo2 = tmp.path().join("b").join("project");
    let output = tmp.path().join("mirror");

    fs::create_dir_all(&repo1).unwrap();
    fs::create_dir_all(&repo2).unwrap();
    fs::write(repo1.join("README.md"), "repo 1").unwrap();
    fs::write(repo2.join("README.md"), "repo 2").unwrap();

    let config_path_str = create_test_config(
        &[repo1.as_path(), repo2.as_path()],
        &output,
        tmp.path(),
    );

    let config_path = std::path::PathBuf::from(&config_path_str);
    let config = doc_link::config::load_config(Some(&config_path)).unwrap();

    assert_eq!(config.repos.len(), 2);
    assert_eq!(config.repos[0].name, "project");
    assert_eq!(config.repos[1].name, "project-2");

    let result = doc_link::scanner::full_scan(&config);
    assert_eq!(result.created, 2);

    // Both should have their own mirror directory
    assert!(output.join("project").join("README.md").is_symlink());
    assert!(output.join("project-2").join("README.md").is_symlink());

    assert_eq!(
        fs::read_to_string(output.join("project").join("README.md")).unwrap(),
        "repo 1"
    );
    assert_eq!(
        fs::read_to_string(output.join("project-2").join("README.md")).unwrap(),
        "repo 2"
    );
}
