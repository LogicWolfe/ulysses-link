use std::path::Path;

use tracing::{info, warn};
use walkdir::WalkDir;

use crate::config::{Config, RepoConfig};
use crate::linker::{self, LinkOutcome};
use crate::matcher;

#[derive(Debug, Default)]
pub struct ScanResult {
    pub created: u32,
    pub already_existed: u32,
    pub skipped: u32,
    pub pruned: u32,
    pub errors: u32,
}

impl ScanResult {
    fn merge(&mut self, other: &ScanResult) {
        self.created += other.created;
        self.already_existed += other.already_existed;
        self.skipped += other.skipped;
        self.pruned += other.pruned;
        self.errors += other.errors;
    }
}

/// Scan all repos and reconcile the symlink mirror.
pub fn full_scan(config: &Config) -> ScanResult {
    let mut result = ScanResult::default();

    for repo_config in &config.repos {
        let repo_result = scan_repo(repo_config, &config.output_dir);
        result.merge(&repo_result);
    }

    result
}

/// Scan a single repo and reconcile its symlink mirror.
pub fn scan_repo(repo_config: &RepoConfig, output_dir: &Path) -> ScanResult {
    let mut result = ScanResult::default();
    let repo_path = &repo_config.path;

    if !repo_path.is_dir() {
        warn!(
            "Repo path does not exist, skipping: {}",
            repo_path.display()
        );
        return result;
    }

    let walker = WalkDir::new(repo_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            // Allow the root directory itself
            if entry.path() == repo_path {
                return true;
            }

            let rel_path = entry.path().strip_prefix(repo_path).unwrap_or(entry.path());
            let rel_str = rel_path.to_string_lossy();

            if entry.file_type().is_dir() {
                matcher::should_descend(&rel_str, &repo_config.exclude)
            } else {
                true // filter files in the body of the loop
            }
        });

    for entry in walker.filter_map(|e| e.ok()) {
        if entry.file_type().is_dir() {
            continue;
        }

        // Skip symlinks in the source repo
        if entry.path_is_symlink() {
            continue;
        }

        let rel_path = match entry.path().strip_prefix(repo_path) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };

        if !matcher::should_mirror(&rel_path, &repo_config.exclude, &repo_config.include) {
            continue;
        }

        match linker::ensure_symlink(repo_path, &repo_config.name, &rel_path, output_dir) {
            Ok(LinkOutcome::Created) => result.created += 1,
            Ok(LinkOutcome::AlreadyCorrect) => result.already_existed += 1,
            Ok(LinkOutcome::Skipped) => result.skipped += 1,
            Err(e) => {
                tracing::error!("Failed to create symlink for {}: {}", rel_path, e);
                result.errors += 1;
            }
        }
    }

    // Prune stale symlinks
    match linker::prune_stale(&repo_config.name, output_dir) {
        Ok(pruned) => result.pruned = pruned,
        Err(e) => {
            tracing::error!(
                "Failed to prune stale symlinks for {}: {}",
                repo_config.name,
                e
            );
            result.errors += 1;
        }
    }

    info!(
        "Scan complete for {}: {} created, {} existed, {} skipped, {} pruned, {} errors",
        repo_config.name,
        result.created,
        result.already_existed,
        result.skipped,
        result.pruned,
        result.errors,
    );

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use std::fs;
    use tempfile::TempDir;

    fn make_config(repo_path: &Path, output_dir: &Path) -> Config {
        let toml = format!(
            "version = 1\noutput_dir = \"{}\"\n\n[[repos]]\npath = \"{}\"",
            output_dir.display(),
            repo_path.display()
        );
        let config_file = repo_path.parent().unwrap().join("test-config.toml");
        fs::write(&config_file, toml).unwrap();
        config::load_config(Some(&config_file)).unwrap()
    }

    #[test]
    fn test_full_scan_creates_symlinks() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("my-repo");
        let output = tmp.path().join("output");
        fs::create_dir(&repo).unwrap();

        fs::write(repo.join("README.md"), "hello").unwrap();
        fs::create_dir(repo.join("docs")).unwrap();
        fs::write(repo.join("docs").join("guide.md"), "guide").unwrap();
        fs::write(repo.join("main.rs"), "fn main() {}").unwrap();

        let config = make_config(&repo, &output);
        let result = full_scan(&config);

        assert_eq!(result.created, 2);
        assert_eq!(result.errors, 0);
        assert!(output.join("my-repo").join("README.md").is_symlink());
        assert!(output
            .join("my-repo")
            .join("docs")
            .join("guide.md")
            .is_symlink());
        assert!(!output.join("my-repo").join("main.rs").exists());
    }

    #[test]
    fn test_scan_excludes_node_modules() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("my-repo");
        let output = tmp.path().join("output");
        fs::create_dir(&repo).unwrap();

        fs::create_dir(repo.join("node_modules")).unwrap();
        fs::create_dir(repo.join("node_modules").join("pkg")).unwrap();
        fs::write(
            repo.join("node_modules").join("pkg").join("README.md"),
            "npm",
        )
        .unwrap();
        fs::write(repo.join("README.md"), "root").unwrap();

        let config = make_config(&repo, &output);
        let result = full_scan(&config);

        assert_eq!(result.created, 1);
        assert!(output.join("my-repo").join("README.md").is_symlink());
        assert!(!output.join("my-repo").join("node_modules").exists());
    }

    #[test]
    fn test_scan_idempotent() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("my-repo");
        let output = tmp.path().join("output");
        fs::create_dir(&repo).unwrap();
        fs::write(repo.join("README.md"), "hello").unwrap();

        let config = make_config(&repo, &output);

        let result1 = full_scan(&config);
        assert_eq!(result1.created, 1);

        let result2 = full_scan(&config);
        assert_eq!(result2.created, 0);
        assert_eq!(result2.already_existed, 1);
    }

    #[test]
    fn test_scan_prunes_stale() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("my-repo");
        let output = tmp.path().join("output");
        fs::create_dir(&repo).unwrap();
        fs::write(repo.join("README.md"), "hello").unwrap();

        let config = make_config(&repo, &output);
        full_scan(&config);

        // Delete source file
        fs::remove_file(repo.join("README.md")).unwrap();

        let result = full_scan(&config);
        assert_eq!(result.pruned, 1);
        assert!(!output.join("my-repo").join("README.md").exists());
    }

    #[test]
    fn test_scan_missing_repo() {
        let tmp = TempDir::new().unwrap();
        let output = tmp.path().join("output");
        fs::create_dir(&output).unwrap();

        // Config with a repo that doesn't exist won't have any repos after validation
        // So we test scan_repo directly with a path that was deleted after config load
        let repo = tmp.path().join("deleted-repo");

        let repo_config = RepoConfig {
            path: repo,
            name: "deleted-repo".into(),
            exclude: {
                let b = ignore::gitignore::GitignoreBuilder::new("/");
                b.build().unwrap()
            },
            include: globset::GlobSetBuilder::new().build().unwrap(),
            include_patterns: vec![],
        };

        let result = scan_repo(&repo_config, &output);
        assert_eq!(result.created, 0);
        assert_eq!(result.errors, 0);
    }
}
