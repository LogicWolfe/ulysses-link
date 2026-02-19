use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, error, warn};
use walkdir::WalkDir;

use crate::manifest::{hash_bytes, hash_file, Manifest, ManifestEntry};

const BASE_CACHE_DIR: &str = ".ulysses-link.d";

#[derive(Debug, PartialEq)]
pub enum SyncOutcome {
    Copied,
    AlreadyInSync,
    Merged,
    Claimed,
    Skipped,
    Conflict,
}

/// Sync a single file between source and mirror using three-way algorithm.
///
/// The `rel_path` is relative to `output_dir` (e.g. "repo-name/docs/guide.md").
pub fn sync_file(
    source: &Path,
    mirror: &Path,
    manifest: &mut Manifest,
    rel_path: &str,
    output_dir: &Path,
) -> Result<SyncOutcome> {
    let source_exists = source.exists();
    let mirror_exists = mirror.exists() && !mirror.is_symlink();

    // New file: source exists, mirror doesn't
    if source_exists && !mirror_exists {
        if let Some(parent) = mirror.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create dirs for {}", mirror.display()))?;
        }
        fs::copy(source, mirror).with_context(|| {
            format!(
                "Failed to copy {} -> {}",
                source.display(),
                mirror.display()
            )
        })?;
        let hash = hash_file(source)?;
        write_base(output_dir, rel_path, &fs::read_to_string(source)?)?;
        manifest.insert(
            rel_path.to_string(),
            ManifestEntry {
                source: source.to_path_buf(),
                hash: hash.clone(),
            },
        );
        debug!(
            "Copied new file: {} -> {}",
            source.display(),
            mirror.display()
        );
        return Ok(SyncOutcome::Copied);
    }

    // Mirror exists but not in manifest — try to claim ownership
    if source_exists && mirror_exists && manifest.get(rel_path).is_none() {
        let source_hash = hash_file(source)?;
        let mirror_hash = hash_file(mirror)?;
        if source_hash == mirror_hash {
            write_base(output_dir, rel_path, &fs::read_to_string(source)?)?;
            manifest.insert(
                rel_path.to_string(),
                ManifestEntry {
                    source: source.to_path_buf(),
                    hash: source_hash,
                },
            );
            debug!("Claimed existing file: {}", rel_path);
            return Ok(SyncOutcome::Claimed);
        }
        debug!("Skipping non-owned file: {} (content mismatch)", rel_path);
        return Ok(SyncOutcome::Skipped);
    }

    // Both exist and file is in manifest — three-way sync
    if source_exists && mirror_exists {
        let entry = manifest.get(rel_path).unwrap();
        let manifest_hash = entry.hash.clone();

        let source_hash = hash_file(source)?;
        let mirror_hash = hash_file(mirror)?;

        if source_hash == mirror_hash {
            // In sync — update manifest hash if needed
            if manifest_hash != source_hash {
                manifest.insert(
                    rel_path.to_string(),
                    ManifestEntry {
                        source: source.to_path_buf(),
                        hash: source_hash.clone(),
                    },
                );
                let content = fs::read_to_string(source)?;
                write_base(output_dir, rel_path, &content)?;
            }
            return Ok(SyncOutcome::AlreadyInSync);
        }

        if source_hash == manifest_hash {
            // Source unchanged, mirror changed → copy mirror → source
            fs::copy(mirror, source).with_context(|| {
                format!(
                    "Failed to copy {} -> {}",
                    mirror.display(),
                    source.display()
                )
            })?;
            let content = fs::read_to_string(mirror)?;
            write_base(output_dir, rel_path, &content)?;
            manifest.insert(
                rel_path.to_string(),
                ManifestEntry {
                    source: source.to_path_buf(),
                    hash: mirror_hash,
                },
            );
            debug!("Synced mirror edit back to source: {}", rel_path);
            return Ok(SyncOutcome::Copied);
        }

        if mirror_hash == manifest_hash {
            // Mirror unchanged, source changed → copy source → mirror
            fs::copy(source, mirror).with_context(|| {
                format!(
                    "Failed to copy {} -> {}",
                    source.display(),
                    mirror.display()
                )
            })?;
            let content = fs::read_to_string(source)?;
            write_base(output_dir, rel_path, &content)?;
            manifest.insert(
                rel_path.to_string(),
                ManifestEntry {
                    source: source.to_path_buf(),
                    hash: source_hash,
                },
            );
            debug!("Synced source change to mirror: {}", rel_path);
            return Ok(SyncOutcome::Copied);
        }

        // Both changed — attempt three-way merge
        let base_content = read_base(output_dir, rel_path)?;
        if let Some(base) = base_content {
            let source_content = fs::read_to_string(source)?;
            let mirror_content = fs::read_to_string(mirror)?;

            let merge_result = diffy::merge(&base, &source_content, &mirror_content);
            match merge_result {
                Ok(merged) => {
                    fs::write(source, &merged).with_context(|| {
                        format!("Failed to write merged result to {}", source.display())
                    })?;
                    fs::write(mirror, &merged).with_context(|| {
                        format!("Failed to write merged result to {}", mirror.display())
                    })?;
                    let merged_hash = hash_bytes(merged.as_bytes());
                    write_base(output_dir, rel_path, &merged)?;
                    manifest.insert(
                        rel_path.to_string(),
                        ManifestEntry {
                            source: source.to_path_buf(),
                            hash: merged_hash,
                        },
                    );
                    debug!("Clean merge applied: {}", rel_path);
                    return Ok(SyncOutcome::Merged);
                }
                Err(_) => {
                    return resolve_conflict(source, mirror, manifest, rel_path, output_dir);
                }
            }
        }

        // No base available — resolve as conflict
        return resolve_conflict(source, mirror, manifest, rel_path, output_dir);
    }

    // Source doesn't exist, mirror does — not our concern during sync_file
    // (handled by propagate_delete / propagate_mirror_delete)
    Ok(SyncOutcome::Skipped)
}

/// Resolve a conflict by keeping the newest version and saving the older as .conflict_<timestamp>.
fn resolve_conflict(
    source: &Path,
    mirror: &Path,
    manifest: &mut Manifest,
    rel_path: &str,
    output_dir: &Path,
) -> Result<SyncOutcome> {
    let source_mtime = fs::metadata(source)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH);
    let mirror_mtime = fs::metadata(mirror)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH);

    if source_mtime >= mirror_mtime {
        // Keep source, save mirror as conflict (in mirror dir)
        let mirror_content = fs::read_to_string(mirror)?;
        save_conflict(mirror, &mirror_content)?;
        fs::copy(source, mirror)?;
        let hash = hash_file(source)?;
        let content = fs::read_to_string(source)?;
        write_base(output_dir, rel_path, &content)?;
        manifest.insert(
            rel_path.to_string(),
            ManifestEntry {
                source: source.to_path_buf(),
                hash,
            },
        );
    } else {
        // Keep mirror, save source as conflict (in source dir)
        let source_content = fs::read_to_string(source)?;
        save_conflict(source, &source_content)?;
        fs::copy(mirror, source)?;
        let hash = hash_file(mirror)?;
        let content = fs::read_to_string(mirror)?;
        write_base(output_dir, rel_path, &content)?;
        manifest.insert(
            rel_path.to_string(),
            ManifestEntry {
                source: source.to_path_buf(),
                hash,
            },
        );
    }

    warn!("Conflict resolved for {}: kept newest version", rel_path);
    Ok(SyncOutcome::Conflict)
}

/// Called when a source file is deleted: removes mirror + base cache + manifest entry.
pub fn propagate_delete(
    rel_path: &str,
    manifest: &mut Manifest,
    output_dir: &Path,
) -> Result<bool> {
    if manifest.get(rel_path).is_none() {
        return Ok(false);
    }

    let mirror = output_dir.join(rel_path);
    if mirror.exists() && !mirror.is_symlink() {
        fs::remove_file(&mirror)
            .with_context(|| format!("Failed to remove mirror {}", mirror.display()))?;
        debug!("Removed mirror file: {}", mirror.display());
    }

    remove_base(output_dir, rel_path)?;
    manifest.remove(rel_path);

    // Prune empty parent dirs up to the repo name dir
    if let Some(parent) = mirror.parent() {
        let repo_name = rel_path.split('/').next().unwrap_or("");
        let stop_at = output_dir.join(repo_name);
        prune_empty_parents(parent, &stop_at);
    }

    Ok(true)
}

/// Called when a mirror file is deleted: removes source + base cache + manifest entry.
pub fn propagate_mirror_delete(
    rel_path: &str,
    manifest: &mut Manifest,
    output_dir: &Path,
) -> Result<bool> {
    let entry = match manifest.get(rel_path) {
        Some(e) => e.clone(),
        None => return Ok(false),
    };

    if entry.source.exists() {
        fs::remove_file(&entry.source)
            .with_context(|| format!("Failed to remove source {}", entry.source.display()))?;
        debug!("Removed source file: {}", entry.source.display());
    }

    remove_base(output_dir, rel_path)?;
    manifest.remove(rel_path);
    Ok(true)
}

/// Remove all mirror files for a repo (only those in manifest), plus base cache entries.
pub fn remove_repo_mirror(
    repo_name: &str,
    output_dir: &Path,
    manifest: &mut Manifest,
) -> Result<()> {
    let entries: Vec<String> = manifest
        .entries_for_repo(repo_name)
        .iter()
        .map(|(k, _)| (*k).clone())
        .collect();

    for rel_path in &entries {
        let mirror = output_dir.join(rel_path);
        if mirror.exists() && !mirror.is_symlink() {
            let _ = fs::remove_file(&mirror);
        }
        let _ = remove_base(output_dir, rel_path);
        manifest.remove(rel_path);
    }

    // Clean up empty directories
    let mirror_root = output_dir.join(repo_name);
    if mirror_root.exists() {
        prune_empty_dirs(&mirror_root);
        if mirror_root.exists() && is_dir_empty(&mirror_root) {
            let _ = fs::remove_dir(&mirror_root);
        }
    }

    // Clean up base cache directory
    let base_root = base_cache_dir(output_dir).join(repo_name);
    if base_root.exists() {
        prune_empty_dirs(&base_root);
        if base_root.exists() && is_dir_empty(&base_root) {
            let _ = fs::remove_dir(&base_root);
        }
    }

    Ok(())
}

/// Iterate manifest entries for a repo, remove entries where source is gone.
/// Deletes corresponding mirror files + base cache entries.
pub fn prune_stale(repo_name: &str, output_dir: &Path, manifest: &mut Manifest) -> Result<u32> {
    let entries: Vec<(String, ManifestEntry)> = manifest
        .entries_for_repo(repo_name)
        .iter()
        .map(|(k, v)| ((*k).clone(), (*v).clone()))
        .collect();

    let mut pruned = 0u32;

    for (rel_path, entry) in &entries {
        if !entry.source.exists() {
            let mirror = output_dir.join(rel_path);
            if mirror.exists() && !mirror.is_symlink() {
                if let Err(e) = fs::remove_file(&mirror) {
                    error!("Failed to prune mirror {}: {}", mirror.display(), e);
                    continue;
                }
            }
            let _ = remove_base(output_dir, rel_path);
            manifest.remove(rel_path);
            debug!("Pruned stale entry: {}", rel_path);
            pruned += 1;
        }
    }

    if pruned > 0 {
        let mirror_root = output_dir.join(repo_name);
        if mirror_root.exists() {
            prune_empty_dirs(&mirror_root);
        }
    }

    Ok(pruned)
}

/// Remove manifest entries + mirror files + base cache entries under a directory prefix.
pub fn remove_dir_mirrors(
    repo_name: &str,
    dir_rel_path: &str,
    output_dir: &Path,
    manifest: &mut Manifest,
) -> Result<u32> {
    let prefix = format!("{repo_name}/{dir_rel_path}");
    let entries: Vec<String> = manifest
        .entries_for_repo(repo_name)
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .map(|(k, _)| (*k).clone())
        .collect();

    let mut removed = 0u32;
    for rel_path in &entries {
        let mirror = output_dir.join(rel_path);
        if mirror.exists() && !mirror.is_symlink() {
            let _ = fs::remove_file(&mirror);
            removed += 1;
        }
        let _ = remove_base(output_dir, rel_path);
        manifest.remove(rel_path);
    }

    let mirror_dir = output_dir.join(repo_name).join(dir_rel_path);
    if mirror_dir.exists() {
        prune_empty_dirs(&mirror_dir);
        if mirror_dir.exists() && is_dir_empty(&mirror_dir) {
            let _ = fs::remove_dir(&mirror_dir);
        }
    }

    let stop_at = output_dir.join(repo_name);
    if let Some(parent) = mirror_dir.parent() {
        prune_empty_parents(parent, &stop_at);
    }

    Ok(removed)
}

/// Save content as a conflict file: `path.conflict_YYYYMMDD_HHMMSS`.
pub fn save_conflict(path: &Path, content: &str) -> Result<PathBuf> {
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let conflict_name = format!("{file_name}.conflict_{timestamp}");
    let conflict_path = path.with_file_name(conflict_name);

    fs::write(&conflict_path, content)
        .with_context(|| format!("Failed to write conflict file {}", conflict_path.display()))?;
    debug!("Saved conflict file: {}", conflict_path.display());
    Ok(conflict_path)
}

// --- Base cache helpers ---

fn base_cache_dir(output_dir: &Path) -> PathBuf {
    output_dir.join(BASE_CACHE_DIR)
}

fn base_cache_path(output_dir: &Path, rel_path: &str) -> PathBuf {
    base_cache_dir(output_dir).join(rel_path)
}

pub fn write_base(output_dir: &Path, rel_path: &str, content: &str) -> Result<()> {
    let path = base_cache_path(output_dir, rel_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, content)?;
    Ok(())
}

pub fn read_base(output_dir: &Path, rel_path: &str) -> Result<Option<String>> {
    let path = base_cache_path(output_dir, rel_path);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(fs::read_to_string(&path)?))
}

pub fn remove_base(output_dir: &Path, rel_path: &str) -> Result<()> {
    let path = base_cache_path(output_dir, rel_path);
    if path.exists() {
        fs::remove_file(&path)?;
        // Prune empty parent dirs in base cache
        if let Some(parent) = path.parent() {
            let stop = base_cache_dir(output_dir);
            prune_empty_parents(parent, &stop);
        }
    }
    Ok(())
}

// --- Directory helpers ---

/// Remove empty parent directories up to (but not including) stop_at.
pub fn prune_empty_parents(start: &Path, stop_at: &Path) {
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
pub fn prune_empty_dirs(root: &Path) {
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

pub fn is_dir_empty(path: &Path) -> bool {
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
    fn test_sync_file_new_file() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "hello").unwrap();

        let mirror = output.path().join("my-repo").join("doc.md");
        let mut manifest = Manifest::load(output.path()).unwrap();

        let outcome = sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        assert_eq!(outcome, SyncOutcome::Copied);
        assert!(mirror.exists());
        assert!(!mirror.is_symlink());
        assert_eq!(fs::read_to_string(&mirror).unwrap(), "hello");
        assert!(manifest.get("my-repo/doc.md").is_some());
    }

    #[test]
    fn test_sync_file_already_in_sync() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "hello").unwrap();

        let mirror = output.path().join("my-repo").join("doc.md");
        let mut manifest = Manifest::load(output.path()).unwrap();

        // First sync
        sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        // Second sync — should be in sync
        let outcome = sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        assert_eq!(outcome, SyncOutcome::AlreadyInSync);
    }

    #[test]
    fn test_sync_file_source_changed() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "original").unwrap();

        let mirror = output.path().join("my-repo").join("doc.md");
        let mut manifest = Manifest::load(output.path()).unwrap();

        sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        // Change source
        fs::write(&source, "updated").unwrap();

        let outcome = sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        assert_eq!(outcome, SyncOutcome::Copied);
        assert_eq!(fs::read_to_string(&mirror).unwrap(), "updated");
    }

    #[test]
    fn test_sync_file_mirror_changed() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "original").unwrap();

        let mirror = output.path().join("my-repo").join("doc.md");
        let mut manifest = Manifest::load(output.path()).unwrap();

        sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        // Change mirror
        fs::write(&mirror, "edited in ulysses").unwrap();

        let outcome = sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        assert_eq!(outcome, SyncOutcome::Copied);
        assert_eq!(fs::read_to_string(&source).unwrap(), "edited in ulysses");
    }

    #[test]
    fn test_sync_file_both_changed_clean_merge() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "line1\nline2\nline3\n").unwrap();

        let mirror = output.path().join("my-repo").join("doc.md");
        let mut manifest = Manifest::load(output.path()).unwrap();

        sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        // Change source (line1)
        fs::write(&source, "LINE1\nline2\nline3\n").unwrap();
        // Change mirror (line3)
        fs::write(&mirror, "line1\nline2\nLINE3\n").unwrap();

        let outcome = sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        assert_eq!(outcome, SyncOutcome::Merged);
        let result = fs::read_to_string(&source).unwrap();
        assert!(result.contains("LINE1"));
        assert!(result.contains("LINE3"));
        assert_eq!(fs::read_to_string(&mirror).unwrap(), result);
    }

    #[test]
    fn test_sync_file_both_changed_conflict() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "original content\n").unwrap();

        let mirror = output.path().join("my-repo").join("doc.md");
        let mut manifest = Manifest::load(output.path()).unwrap();

        sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        // Both change the same line
        fs::write(&source, "source version\n").unwrap();
        fs::write(&mirror, "mirror version\n").unwrap();

        let outcome = sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        assert_eq!(outcome, SyncOutcome::Conflict);
        // One of them should have a conflict file
        let source_dir_entries: Vec<_> = fs::read_dir(repo.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".conflict_"))
            .collect();
        let mirror_dir_entries: Vec<_> = fs::read_dir(output.path().join("my-repo"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".conflict_"))
            .collect();
        // One of the dirs should have a conflict file
        assert!(
            !source_dir_entries.is_empty() || !mirror_dir_entries.is_empty(),
            "Expected a conflict file"
        );
    }

    #[test]
    fn test_sync_file_no_base_falls_back_to_conflict() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "source content").unwrap();

        let mirror = output.path().join("my-repo").join("doc.md");
        fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        fs::write(&mirror, "mirror content").unwrap();

        // Manually insert manifest entry with a hash that matches neither
        let mut manifest = Manifest::load(output.path()).unwrap();
        manifest.insert(
            "my-repo/doc.md".into(),
            ManifestEntry {
                source: source.clone(),
                hash: "stale_hash_value".into(),
            },
        );

        let outcome = sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        assert_eq!(outcome, SyncOutcome::Conflict);
    }

    #[test]
    fn test_sync_file_claim_ownership() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "same content").unwrap();

        // Mirror already exists with same content
        let mirror = output.path().join("my-repo").join("doc.md");
        fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        fs::write(&mirror, "same content").unwrap();

        let mut manifest = Manifest::load(output.path()).unwrap();

        let outcome = sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        assert_eq!(outcome, SyncOutcome::Claimed);
        assert!(manifest.get("my-repo/doc.md").is_some());
    }

    #[test]
    fn test_sync_file_skip_non_matching() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "source content").unwrap();

        // Mirror already exists with different content
        let mirror = output.path().join("my-repo").join("doc.md");
        fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        fs::write(&mirror, "different content").unwrap();

        let mut manifest = Manifest::load(output.path()).unwrap();

        let outcome = sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        assert_eq!(outcome, SyncOutcome::Skipped);
        assert!(manifest.get("my-repo/doc.md").is_none());
        // Mirror content should be unchanged
        assert_eq!(fs::read_to_string(&mirror).unwrap(), "different content");
    }

    #[test]
    fn test_propagate_delete() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "hello").unwrap();

        let mirror = output.path().join("my-repo").join("doc.md");
        let mut manifest = Manifest::load(output.path()).unwrap();

        sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();
        assert!(mirror.exists());

        // Delete source
        fs::remove_file(&source).unwrap();

        let deleted = propagate_delete("my-repo/doc.md", &mut manifest, output.path()).unwrap();
        assert!(deleted);
        assert!(!mirror.exists());
        assert!(manifest.get("my-repo/doc.md").is_none());
    }

    #[test]
    fn test_propagate_delete_not_in_manifest() {
        let output = TempDir::new().unwrap();
        let mut manifest = Manifest::load(output.path()).unwrap();

        let deleted =
            propagate_delete("my-repo/nonexistent.md", &mut manifest, output.path()).unwrap();
        assert!(!deleted);
    }

    #[test]
    fn test_propagate_mirror_delete() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "hello").unwrap();

        let mirror = output.path().join("my-repo").join("doc.md");
        let mut manifest = Manifest::load(output.path()).unwrap();

        sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        // Delete mirror
        fs::remove_file(&mirror).unwrap();

        let deleted =
            propagate_mirror_delete("my-repo/doc.md", &mut manifest, output.path()).unwrap();
        assert!(deleted);
        assert!(!source.exists());
        assert!(manifest.get("my-repo/doc.md").is_none());
    }

    #[test]
    fn test_prune_stale_via_manifest() {
        let (repo, output) = setup();
        let source = repo.path().join("doc.md");
        fs::write(&source, "hello").unwrap();

        let mirror = output.path().join("my-repo").join("doc.md");
        let mut manifest = Manifest::load(output.path()).unwrap();

        sync_file(
            &source,
            &mirror,
            &mut manifest,
            "my-repo/doc.md",
            output.path(),
        )
        .unwrap();

        // Delete source to make it stale
        fs::remove_file(&source).unwrap();

        let pruned = prune_stale("my-repo", output.path(), &mut manifest).unwrap();
        assert_eq!(pruned, 1);
        assert!(!mirror.exists());
        assert!(manifest.get("my-repo/doc.md").is_none());
    }

    #[test]
    fn test_remove_repo_mirror_with_manifest() {
        let (repo, output) = setup();
        fs::write(repo.path().join("a.md"), "a").unwrap();
        fs::create_dir(repo.path().join("sub")).unwrap();
        fs::write(repo.path().join("sub").join("b.md"), "b").unwrap();

        let mut manifest = Manifest::load(output.path()).unwrap();

        sync_file(
            &repo.path().join("a.md"),
            &output.path().join("my-repo").join("a.md"),
            &mut manifest,
            "my-repo/a.md",
            output.path(),
        )
        .unwrap();
        sync_file(
            &repo.path().join("sub").join("b.md"),
            &output.path().join("my-repo").join("sub").join("b.md"),
            &mut manifest,
            "my-repo/sub/b.md",
            output.path(),
        )
        .unwrap();

        remove_repo_mirror("my-repo", output.path(), &mut manifest).unwrap();

        assert!(!output.path().join("my-repo").exists());
        assert!(manifest.entries_for_repo("my-repo").is_empty());
    }

    #[test]
    fn test_base_cache_read_write_remove() {
        let output = TempDir::new().unwrap();

        write_base(output.path(), "repo/doc.md", "base content").unwrap();
        let content = read_base(output.path(), "repo/doc.md").unwrap();
        assert_eq!(content, Some("base content".into()));

        remove_base(output.path(), "repo/doc.md").unwrap();
        let content = read_base(output.path(), "repo/doc.md").unwrap();
        assert_eq!(content, None);
    }

    #[test]
    fn test_empty_dir_cleanup() {
        let output = TempDir::new().unwrap();
        let deep = output.path().join("my-repo").join("deep").join("nested");
        fs::create_dir_all(&deep).unwrap();
        let file = deep.join("doc.md");
        fs::write(&file, "hello").unwrap();

        fs::remove_file(&file).unwrap();

        let stop_at = output.path().join("my-repo");
        prune_empty_parents(&deep, &stop_at);

        assert!(!output
            .path()
            .join("my-repo")
            .join("deep")
            .join("nested")
            .exists());
        assert!(!output.path().join("my-repo").join("deep").exists());
    }

    #[test]
    fn test_save_conflict() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("doc.md");
        fs::write(&file, "current").unwrap();

        let conflict_path = save_conflict(&file, "old content").unwrap();
        assert!(conflict_path.exists());
        assert!(conflict_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("doc.md.conflict_"));
        assert_eq!(fs::read_to_string(&conflict_path).unwrap(), "old content");
    }
}
