use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::{debug, error, warn};
use walkdir::WalkDir;

#[derive(Debug, PartialEq)]
pub enum LinkOutcome {
    Created,
    AlreadyCorrect,
    Skipped,
}

/// Create a symlink if it doesn't exist or fix it if it points to the wrong target.
/// Returns `Created` for new/fixed symlinks, `AlreadyCorrect` if already pointing
/// to the right target, or `Skipped` if a real file/directory occupies the target path.
pub fn ensure_symlink(
    repo_path: &Path,
    repo_name: &str,
    rel_path: &str,
    output_dir: &Path,
) -> Result<LinkOutcome> {
    let source = repo_path.join(rel_path);
    let target = output_dir.join(repo_name).join(rel_path);

    if target.is_symlink() {
        match (target.canonicalize(), source.canonicalize()) {
            (Ok(t), Ok(s)) if t == s => return Ok(LinkOutcome::AlreadyCorrect),
            _ => {
                // Points somewhere wrong or broken, fix it
                fs::remove_file(&target).with_context(|| {
                    format!("Failed to remove wrong symlink {}", target.display())
                })?;
            }
        }
    } else if target.exists() {
        warn!(
            "Skipping {}: real file exists at {}",
            rel_path,
            target.display()
        );
        return Ok(LinkOutcome::Skipped);
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent dirs for {}", target.display()))?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&source, &target).with_context(|| {
            format!(
                "Failed to create symlink {} -> {}",
                target.display(),
                source.display()
            )
        })?;
    }

    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(&source, &target).with_context(|| {
            format!(
                "Failed to create symlink {} -> {}",
                target.display(),
                source.display()
            )
        })?;
    }

    debug!(
        "Created symlink: {} -> {}",
        target.display(),
        source.display()
    );
    Ok(LinkOutcome::Created)
}

/// Remove a symlink. Only removes if it IS a symlink (safety check).
/// Returns true if a symlink was removed.
pub fn remove_symlink(repo_name: &str, rel_path: &str, output_dir: &Path) -> Result<bool> {
    let target = output_dir.join(repo_name).join(rel_path);

    if !target.is_symlink() {
        return Ok(false);
    }

    fs::remove_file(&target)
        .with_context(|| format!("Failed to remove symlink {}", target.display()))?;
    debug!("Removed symlink: {}", target.display());

    let stop_at = output_dir.join(repo_name);
    if let Some(parent) = target.parent() {
        prune_empty_parents(parent, &stop_at);
    }

    Ok(true)
}

/// Walk mirror tree and remove any symlinks whose target no longer exists.
/// Returns the count of pruned symlinks.
pub fn prune_stale(repo_name: &str, output_dir: &Path) -> Result<u32> {
    let mirror_root = output_dir.join(repo_name);
    if !mirror_root.exists() {
        return Ok(0);
    }

    let mut pruned = 0u32;

    for entry in WalkDir::new(&mirror_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_symlink() {
            // Check if target exists by trying to read the symlink target
            let target_exists = fs::read_link(path)
                .ok()
                .map(|link_target| {
                    // Resolve relative to parent dir
                    let absolute = if link_target.is_absolute() {
                        link_target
                    } else {
                        path.parent().unwrap_or(path).join(&link_target)
                    };
                    absolute.exists()
                })
                .unwrap_or(false);

            if !target_exists {
                if let Err(e) = fs::remove_file(path) {
                    error!("Failed to prune stale symlink {}: {}", path.display(), e);
                } else {
                    debug!("Pruned stale symlink: {}", path.display());
                    pruned += 1;
                }
            }
        }
    }

    prune_empty_dirs(&mirror_root);
    Ok(pruned)
}

/// Remove an entire repo's mirror directory, only removing symlinks and empty dirs.
pub fn remove_repo_mirror(repo_name: &str, output_dir: &Path) -> Result<()> {
    let mirror_root = output_dir.join(repo_name);
    if !mirror_root.exists() {
        return Ok(());
    }

    // Remove all symlinks (walk bottom-up)
    for entry in WalkDir::new(&mirror_root)
        .contents_first(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_symlink() {
            let _ = fs::remove_file(path);
        }
    }

    // Remove empty directories bottom-up
    prune_empty_dirs(&mirror_root);

    // Remove the root itself if empty
    if mirror_root.exists() && is_dir_empty(&mirror_root) {
        let _ = fs::remove_dir(&mirror_root);
    }

    Ok(())
}

/// Remove all symlinks under a mirror directory (for dir_deleted events).
/// Returns the count of removed symlinks.
pub fn remove_dir_symlinks(repo_name: &str, dir_rel_path: &str, output_dir: &Path) -> Result<u32> {
    let mirror_dir = output_dir.join(repo_name).join(dir_rel_path);
    if !mirror_dir.exists() {
        return Ok(0);
    }

    let mut removed = 0u32;
    for entry in WalkDir::new(&mirror_dir)
        .contents_first(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_symlink() {
            let _ = fs::remove_file(path);
            removed += 1;
        }
    }

    prune_empty_dirs(&mirror_dir);
    if mirror_dir.exists() && is_dir_empty(&mirror_dir) {
        let _ = fs::remove_dir(&mirror_dir);
    }

    // Prune parents up to repo root
    let stop_at = output_dir.join(repo_name);
    if let Some(parent) = mirror_dir.parent() {
        prune_empty_parents(parent, &stop_at);
    }

    Ok(removed)
}

/// Remove empty parent directories up to (but not including) stop_at.
fn prune_empty_parents(start: &Path, stop_at: &Path) {
    let mut current = start.to_path_buf();
    while current != *stop_at && current.starts_with(stop_at) {
        if !current.exists() || !is_dir_empty(&current) {
            break;
        }
        if fs::remove_dir(&current).is_err() {
            break;
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }
}

/// Remove empty directories bottom-up within root (not including root itself).
fn prune_empty_dirs(root: &Path) {
    // Collect dirs bottom-up
    let dirs: Vec<_> = WalkDir::new(root)
        .contents_first(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path() != root && e.file_type().is_dir())
        .map(|e| e.path().to_path_buf())
        .collect();

    for dir in dirs {
        if dir.exists() && is_dir_empty(&dir) {
            let _ = fs::remove_dir(&dir);
        }
    }
}

fn is_dir_empty(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, TempDir) {
        let repo = TempDir::new().unwrap();
        let output = TempDir::new().unwrap();
        (repo, output)
    }

    #[test]
    fn test_ensure_symlink_creates() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "hello").unwrap();

        let outcome = ensure_symlink(repo.path(), "my-repo", "doc.md", output.path()).unwrap();
        assert_eq!(outcome, LinkOutcome::Created);

        let link = output.path().join("my-repo").join("doc.md");
        assert!(link.is_symlink());
        assert_eq!(fs::read_to_string(&link).unwrap(), "hello");
    }

    #[test]
    fn test_ensure_symlink_idempotent() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "hello").unwrap();

        assert_eq!(
            ensure_symlink(repo.path(), "my-repo", "doc.md", output.path()).unwrap(),
            LinkOutcome::Created
        );
        assert_eq!(
            ensure_symlink(repo.path(), "my-repo", "doc.md", output.path()).unwrap(),
            LinkOutcome::AlreadyCorrect
        );
    }

    #[test]
    fn test_ensure_symlink_fixes_wrong_target() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "correct").unwrap();

        // Create a symlink pointing to wrong target
        let link = output.path().join("my-repo").join("doc.md");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        let wrong_target = repo.path().join("other.md");
        fs::write(&wrong_target, "wrong").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&wrong_target, &link).unwrap();

        let outcome = ensure_symlink(repo.path(), "my-repo", "doc.md", output.path()).unwrap();
        assert_eq!(outcome, LinkOutcome::Created);
        assert_eq!(fs::read_to_string(&link).unwrap(), "correct");
    }

    #[test]
    fn test_ensure_symlink_skips_real_file() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "source").unwrap();

        // Place a real file at the target path
        let target = output.path().join("my-repo").join("doc.md");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "real content").unwrap();

        let outcome = ensure_symlink(repo.path(), "my-repo", "doc.md", output.path()).unwrap();
        assert_eq!(outcome, LinkOutcome::Skipped);
        assert_eq!(fs::read_to_string(&target).unwrap(), "real content");
    }

    #[test]
    fn test_ensure_symlink_creates_parent_dirs() {
        let (repo, output) = setup();
        let source_dir = repo.path().join("deep").join("nested");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(source_dir.join("doc.md"), "hello").unwrap();

        ensure_symlink(repo.path(), "my-repo", "deep/nested/doc.md", output.path()).unwrap();

        let link = output
            .path()
            .join("my-repo")
            .join("deep")
            .join("nested")
            .join("doc.md");
        assert!(link.is_symlink());
    }

    #[test]
    fn test_remove_symlink() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "hello").unwrap();

        ensure_symlink(repo.path(), "my-repo", "doc.md", output.path()).unwrap();
        let removed = remove_symlink("my-repo", "doc.md", output.path()).unwrap();
        assert!(removed);

        let link = output.path().join("my-repo").join("doc.md");
        assert!(!link.exists());
    }

    #[test]
    fn test_remove_symlink_nonexistent() {
        let output = TempDir::new().unwrap();
        let removed = remove_symlink("my-repo", "nonexistent.md", output.path()).unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_remove_symlink_never_deletes_real_files() {
        let output = TempDir::new().unwrap();
        let real_file = output.path().join("my-repo").join("real.md");
        fs::create_dir_all(real_file.parent().unwrap()).unwrap();
        fs::write(&real_file, "real content").unwrap();

        let removed = remove_symlink("my-repo", "real.md", output.path()).unwrap();
        assert!(!removed);
        assert!(real_file.exists());
    }

    #[test]
    fn test_prune_stale() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "hello").unwrap();

        ensure_symlink(repo.path(), "my-repo", "doc.md", output.path()).unwrap();

        // Delete the source file to make the symlink stale
        fs::remove_file(&source).unwrap();

        let pruned = prune_stale("my-repo", output.path()).unwrap();
        assert_eq!(pruned, 1);

        let link = output.path().join("my-repo").join("doc.md");
        assert!(!link.exists());
    }

    #[test]
    fn test_remove_repo_mirror() {
        let (repo, output) = setup();
        fs::write(repo.path().join("a.md"), "a").unwrap();
        fs::create_dir(repo.path().join("sub")).unwrap();
        fs::write(repo.path().join("sub").join("b.md"), "b").unwrap();

        ensure_symlink(repo.path(), "my-repo", "a.md", output.path()).unwrap();
        ensure_symlink(repo.path(), "my-repo", "sub/b.md", output.path()).unwrap();

        remove_repo_mirror("my-repo", output.path()).unwrap();

        assert!(!output.path().join("my-repo").exists());
    }

    #[test]
    fn test_empty_dir_cleanup_on_remove() {
        let (repo, output) = setup();
        let source_dir = repo.path().join("deep").join("nested");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(source_dir.join("doc.md"), "hello").unwrap();

        ensure_symlink(repo.path(), "my-repo", "deep/nested/doc.md", output.path()).unwrap();
        remove_symlink("my-repo", "deep/nested/doc.md", output.path()).unwrap();

        // Empty parent directories should be cleaned up
        assert!(!output
            .path()
            .join("my-repo")
            .join("deep")
            .join("nested")
            .exists());
        assert!(!output.path().join("my-repo").join("deep").exists());
    }
}
