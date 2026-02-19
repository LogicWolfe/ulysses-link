use std::path::Path;

use tracing::{info, warn};
use walkdir::WalkDir;

use crate::config::{Config, RepoConfig};
use crate::linker::{self, SyncOutcome};
use crate::manifest::Manifest;
use crate::matcher;

#[derive(Debug, Default)]
pub struct ScanResult {
    pub created: u32,
    pub already_existed: u32,
    pub skipped: u32,
    pub pruned: u32,
    pub merged: u32,
    pub conflicts: u32,
    pub errors: u32,
}

impl ScanResult {
    fn merge(&mut self, other: &ScanResult) {
        self.created += other.created;
        self.already_existed += other.already_existed;
        self.skipped += other.skipped;
        self.pruned += other.pruned;
        self.merged += other.merged;
        self.conflicts += other.conflicts;
        self.errors += other.errors;
    }
}

/// Scan all repos and reconcile the mirror tree.
pub fn full_scan(config: &Config, manifest: &mut Manifest) -> ScanResult {
    let mut result = ScanResult::default();

    for repo_config in &config.repos {
        let repo_result = scan_repo(repo_config, &config.output_dir, manifest);
        result.merge(&repo_result);
    }

    result
}

/// Scan a single repo and reconcile its mirror.
pub fn scan_repo(
    repo_config: &RepoConfig,
    output_dir: &Path,
    manifest: &mut Manifest,
) -> ScanResult {
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
            if entry.path() == repo_path {
                return true;
            }

            let rel_path = entry.path().strip_prefix(repo_path).unwrap_or(entry.path());
            let rel_str = rel_path.to_string_lossy();

            if entry.file_type().is_dir() {
                matcher::should_descend(&rel_str, &repo_config.exclude)
            } else {
                true
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

        let source = repo_path.join(&rel_path);
        let manifest_rel = format!("{}/{}", repo_config.name, rel_path);
        let mirror = output_dir.join(&manifest_rel);

        match linker::sync_file(&source, &mirror, manifest, &manifest_rel, output_dir) {
            Ok(SyncOutcome::Copied) => result.created += 1,
            Ok(SyncOutcome::AlreadyInSync | SyncOutcome::Claimed) => result.already_existed += 1,
            Ok(SyncOutcome::Skipped) => result.skipped += 1,
            Ok(SyncOutcome::Merged) => result.merged += 1,
            Ok(SyncOutcome::Conflict) => result.conflicts += 1,
            Err(e) => {
                tracing::error!("Failed to sync {}: {}", rel_path, e);
                result.errors += 1;
            }
        }
    }

    // Prune stale entries using manifest
    match linker::prune_stale(&repo_config.name, output_dir, manifest) {
        Ok(pruned) => result.pruned = pruned,
        Err(e) => {
            tracing::error!(
                "Failed to prune stale entries for {}: {}",
                repo_config.name,
                e
            );
            result.errors += 1;
        }
    }

    if let Err(e) = manifest.save(output_dir) {
        tracing::error!("Failed to save manifest: {}", e);
        result.errors += 1;
    }

    info!(
        "Scan complete for {}: {} created, {} existed, {} skipped, {} merged, {} conflicts, {} pruned, {} errors",
        repo_config.name,
        result.created,
        result.already_existed,
        result.skipped,
        result.merged,
        result.conflicts,
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
    fn test_full_scan_creates_copies() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("my-repo");
        let output = tmp.path().join("output");
        fs::create_dir(&repo).unwrap();

        fs::write(repo.join("README.md"), "hello").unwrap();
        fs::create_dir(repo.join("docs")).unwrap();
        fs::write(repo.join("docs").join("guide.md"), "guide").unwrap();
        fs::write(repo.join("main.rs"), "fn main() {}").unwrap();

        let config = make_config(&repo, &output);
        let mut manifest = Manifest::load(&output).unwrap();
        let result = full_scan(&config, &mut manifest);

        assert_eq!(result.created, 2);
        assert_eq!(result.errors, 0);

        // Should be regular files, not symlinks
        let readme = output.join("my-repo").join("README.md");
        assert!(readme.exists());
        assert!(!readme.is_symlink());
        assert_eq!(fs::read_to_string(&readme).unwrap(), "hello");

        let guide = output.join("my-repo").join("docs").join("guide.md");
        assert!(guide.exists());
        assert!(!guide.is_symlink());

        // Non-matching files should not be in mirror
        assert!(!output.join("my-repo").join("main.rs").exists());

        // Manifest should track the files
        assert!(manifest.get("my-repo/README.md").is_some());
        assert!(manifest.get("my-repo/docs/guide.md").is_some());
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
        let mut manifest = Manifest::load(&output).unwrap();
        let result = full_scan(&config, &mut manifest);

        assert_eq!(result.created, 1);
        assert!(output.join("my-repo").join("README.md").exists());
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
        let mut manifest = Manifest::load(&output).unwrap();

        let result1 = full_scan(&config, &mut manifest);
        assert_eq!(result1.created, 1);

        let result2 = full_scan(&config, &mut manifest);
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
        let mut manifest = Manifest::load(&output).unwrap();
        full_scan(&config, &mut manifest);

        // Delete source file
        fs::remove_file(repo.join("README.md")).unwrap();

        let result = full_scan(&config, &mut manifest);
        assert_eq!(result.pruned, 1);
        assert!(!output.join("my-repo").join("README.md").exists());
    }

    #[test]
    fn test_scan_missing_repo() {
        let tmp = TempDir::new().unwrap();
        let output = tmp.path().join("output");
        fs::create_dir(&output).unwrap();

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

        let mut manifest = Manifest::load(&output).unwrap();
        let result = scan_repo(&repo_config, &output, &mut manifest);
        assert_eq!(result.created, 0);
        assert_eq!(result.errors, 0);
    }
}
