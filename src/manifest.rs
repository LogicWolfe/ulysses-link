use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MANIFEST_FILENAME: &str = ".ulysses-link";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub source: PathBuf,
    pub hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestFile {
    version: u64,
    #[serde(default)]
    files: HashMap<String, ManifestEntry>,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    files: HashMap<String, ManifestEntry>,
}

impl Manifest {
    pub fn empty() -> Self {
        Self {
            files: HashMap::new(),
        }
    }

    pub fn load(output_dir: &Path) -> Result<Self> {
        let path = output_dir.join(MANIFEST_FILENAME);
        if !path.exists() {
            return Ok(Self {
                files: HashMap::new(),
            });
        }

        let contents = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read manifest at {}", path.display()))?;
        let manifest_file: ManifestFile = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse manifest at {}", path.display()))?;

        Ok(Self {
            files: manifest_file.files,
        })
    }

    pub fn save(&self, output_dir: &Path) -> Result<()> {
        let path = output_dir.join(MANIFEST_FILENAME);
        let manifest_file = ManifestFile {
            version: 1,
            files: self.files.clone(),
        };
        let contents = toml::to_string(&manifest_file).context("Failed to serialize manifest")?;
        fs::write(&path, contents)
            .with_context(|| format!("Failed to write manifest to {}", path.display()))?;
        Ok(())
    }

    pub fn get(&self, rel_path: &str) -> Option<&ManifestEntry> {
        self.files.get(rel_path)
    }

    pub fn insert(&mut self, rel_path: String, entry: ManifestEntry) {
        self.files.insert(rel_path, entry);
    }

    pub fn remove(&mut self, rel_path: &str) -> Option<ManifestEntry> {
        self.files.remove(rel_path)
    }

    pub fn entries_for_repo(&self, repo_name: &str) -> Vec<(&String, &ManifestEntry)> {
        let prefix = format!("{repo_name}/");
        self.files
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Compute SHA-256 hex digest of a file's contents.
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("Failed to open file for hashing: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = file
            .read(&mut buffer)
            .with_context(|| format!("Failed to read file for hashing: {}", path.display()))?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Compute SHA-256 hex digest of a byte slice.
pub fn hash_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_manifest_load_save_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut manifest = Manifest::load(tmp.path()).unwrap();
        assert!(manifest.is_empty());

        manifest.insert(
            "repo/README.md".into(),
            ManifestEntry {
                source: PathBuf::from("/src/repo/README.md"),
                hash: "abc123".into(),
            },
        );
        manifest.insert(
            "repo/docs/guide.md".into(),
            ManifestEntry {
                source: PathBuf::from("/src/repo/docs/guide.md"),
                hash: "def456".into(),
            },
        );

        manifest.save(tmp.path()).unwrap();

        let loaded = Manifest::load(tmp.path()).unwrap();
        assert_eq!(loaded.get("repo/README.md").unwrap().hash, "abc123");
        assert_eq!(loaded.get("repo/docs/guide.md").unwrap().hash, "def456");
        assert_eq!(
            loaded.get("repo/README.md").unwrap().source,
            PathBuf::from("/src/repo/README.md")
        );
    }

    #[test]
    fn test_manifest_get_insert_remove() {
        let mut manifest = Manifest {
            files: HashMap::new(),
        };

        assert!(manifest.get("foo").is_none());

        manifest.insert(
            "foo".into(),
            ManifestEntry {
                source: PathBuf::from("/src/foo"),
                hash: "aaa".into(),
            },
        );
        assert!(manifest.get("foo").is_some());

        let removed = manifest.remove("foo");
        assert!(removed.is_some());
        assert!(manifest.get("foo").is_none());
    }

    #[test]
    fn test_entries_for_repo() {
        let mut manifest = Manifest {
            files: HashMap::new(),
        };

        manifest.insert(
            "repo1/a.md".into(),
            ManifestEntry {
                source: PathBuf::from("/r1/a.md"),
                hash: "a".into(),
            },
        );
        manifest.insert(
            "repo1/b.md".into(),
            ManifestEntry {
                source: PathBuf::from("/r1/b.md"),
                hash: "b".into(),
            },
        );
        manifest.insert(
            "repo2/c.md".into(),
            ManifestEntry {
                source: PathBuf::from("/r2/c.md"),
                hash: "c".into(),
            },
        );

        let repo1_entries = manifest.entries_for_repo("repo1");
        assert_eq!(repo1_entries.len(), 2);

        let repo2_entries = manifest.entries_for_repo("repo2");
        assert_eq!(repo2_entries.len(), 1);

        let repo3_entries = manifest.entries_for_repo("repo3");
        assert_eq!(repo3_entries.len(), 0);
    }

    #[test]
    fn test_hash_file() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let hash = hash_file(&file_path).unwrap();
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA-256 hex is 64 chars

        // Same content produces same hash
        let file_path2 = tmp.path().join("test2.txt");
        fs::write(&file_path2, "hello world").unwrap();
        assert_eq!(hash_file(&file_path2).unwrap(), hash);

        // Different content produces different hash
        let file_path3 = tmp.path().join("test3.txt");
        fs::write(&file_path3, "different").unwrap();
        assert_ne!(hash_file(&file_path3).unwrap(), hash);
    }

    #[test]
    fn test_hash_bytes() {
        let hash1 = hash_bytes(b"hello world");
        let hash2 = hash_bytes(b"hello world");
        let hash3 = hash_bytes(b"different");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn test_manifest_load_missing_file() {
        let tmp = TempDir::new().unwrap();
        let manifest = Manifest::load(tmp.path()).unwrap();
        assert!(manifest.is_empty());
    }
}
