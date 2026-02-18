use std::path::Path;

use ignore::gitignore::Gitignore;
use globset::GlobSet;

/// Check if a file should be mirrored based on exclude/include patterns.
///
/// Algorithm:
/// 1. Normalize path to forward slashes, strip leading `./`
/// 2. Check excludes first — if excluded, return false
/// 3. Check includes — if included, return true
/// 4. Otherwise return false
///
/// Exclude is checked FIRST so that e.g. node_modules/*.md stays excluded.
pub fn should_mirror(file_rel_path: &str, exclude: &Gitignore, include: &GlobSet) -> bool {
    let normalized = normalize_path(file_rel_path);
    if normalized.is_empty() {
        return false;
    }

    let path = Path::new(&normalized);

    // Check exclude patterns (gitignore semantics)
    // Use matched_path_or_any_parents so directory patterns like `node_modules/`
    // also exclude files within that directory
    if exclude.matched_path_or_any_parents(path, false).is_ignore() {
        return false;
    }

    // Check include patterns (glob semantics)
    include.is_match(path)
}

/// Check if the scanner should descend into a directory.
///
/// Returns false if the directory matches an exclude pattern.
pub fn should_descend(dir_rel_path: &str, exclude: &Gitignore) -> bool {
    let normalized = normalize_path(dir_rel_path);
    if normalized.is_empty() {
        return true;
    }

    let path = Path::new(&normalized);
    // The `true` flag indicates this is a directory
    !exclude.matched(path, true).is_ignore()
}

/// Normalize a relative path: forward slashes, strip leading `./`
fn normalize_path(rel_path: &str) -> String {
    let normalized = rel_path.replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
    if normalized == "." {
        return String::new();
    }
    normalized.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DEFAULT_GLOBAL_EXCLUDE, DEFAULT_GLOBAL_INCLUDE};
    use globset::{Glob, GlobSetBuilder};
    use ignore::gitignore::GitignoreBuilder;
    use std::path::PathBuf;

    fn build_exclude(patterns: &[&str]) -> Gitignore {
        let mut builder = GitignoreBuilder::new(PathBuf::from("/repo"));
        for p in patterns {
            builder.add_line(None, p).unwrap();
        }
        builder.build().unwrap()
    }

    fn build_include(patterns: &[&str]) -> GlobSet {
        let mut builder = GlobSetBuilder::new();
        for p in patterns {
            let glob_pattern = if !p.contains('/') && !p.starts_with("**/") {
                format!("**/{p}")
            } else {
                p.to_string()
            };
            builder.add(Glob::new(&glob_pattern).unwrap());
        }
        builder.build().unwrap()
    }

    fn default_exclude() -> Gitignore {
        build_exclude(&DEFAULT_GLOBAL_EXCLUDE.iter().copied().collect::<Vec<_>>())
    }

    fn default_include() -> GlobSet {
        build_include(&DEFAULT_GLOBAL_INCLUDE.iter().copied().collect::<Vec<_>>())
    }

    #[test]
    fn test_basic_md_file_included() {
        let exc = default_exclude();
        let inc = default_include();
        assert!(should_mirror("README.md", &exc, &inc));
        assert!(should_mirror("docs/guide.md", &exc, &inc));
        assert!(should_mirror("deep/nested/path/file.mdx", &exc, &inc));
    }

    #[test]
    fn test_non_matching_files_excluded() {
        let exc = default_exclude();
        let inc = default_include();
        assert!(!should_mirror("main.rs", &exc, &inc));
        assert!(!should_mirror("src/lib.rs", &exc, &inc));
        assert!(!should_mirror("Cargo.toml", &exc, &inc));
    }

    #[test]
    fn test_node_modules_excluded() {
        let exc = default_exclude();
        let inc = default_include();
        assert!(!should_mirror("node_modules/package/README.md", &exc, &inc));
        assert!(!should_mirror("node_modules/deep/nested/doc.md", &exc, &inc));
    }

    #[test]
    fn test_git_dir_excluded() {
        let exc = default_exclude();
        let inc = default_include();
        assert!(!should_mirror(".git/HEAD", &exc, &inc));
        assert!(!should_mirror(".git/objects/abc/def", &exc, &inc));
    }

    #[test]
    fn test_build_dirs_excluded() {
        let exc = default_exclude();
        let inc = default_include();
        assert!(!should_mirror("dist/README.md", &exc, &inc));
        assert!(!should_mirror("build/docs/guide.md", &exc, &inc));
        assert!(!should_mirror("target/doc/api.md", &exc, &inc));
    }

    #[test]
    fn test_extensionless_files() {
        let exc = default_exclude();
        let inc = default_include();
        assert!(should_mirror("README", &exc, &inc));
        assert!(should_mirror("LICENSE", &exc, &inc));
        assert!(should_mirror("CHANGELOG", &exc, &inc));
        assert!(should_mirror("CONTRIBUTING", &exc, &inc));
        assert!(should_mirror("AUTHORS", &exc, &inc));
        assert!(should_mirror("subdir/README", &exc, &inc));
    }

    #[test]
    fn test_other_markup_formats() {
        let exc = default_exclude();
        let inc = default_include();
        assert!(should_mirror("doc.txt", &exc, &inc));
        assert!(should_mirror("doc.rst", &exc, &inc));
        assert!(should_mirror("doc.adoc", &exc, &inc));
        assert!(should_mirror("notes.org", &exc, &inc));
    }

    #[test]
    fn test_should_descend_excludes_dirs() {
        let exc = default_exclude();
        assert!(!should_descend("node_modules", &exc));
        assert!(!should_descend(".git", &exc));
        assert!(!should_descend("__pycache__", &exc));
        assert!(!should_descend("dist", &exc));
        assert!(!should_descend(".venv", &exc));
    }

    #[test]
    fn test_should_descend_allows_normal_dirs() {
        let exc = default_exclude();
        assert!(should_descend("src", &exc));
        assert!(should_descend("docs", &exc));
        assert!(should_descend("lib", &exc));
    }

    #[test]
    fn test_custom_exclude_patterns() {
        let exc = build_exclude(&["vendor/", "docs/generated/"]);
        let inc = build_include(&["*.md"]);
        assert!(!should_mirror("vendor/README.md", &exc, &inc));
        assert!(!should_mirror("docs/generated/api.md", &exc, &inc));
        assert!(should_mirror("docs/guide.md", &exc, &inc));
    }

    #[test]
    fn test_custom_include_patterns() {
        let exc = build_exclude(&[]);
        let inc = build_include(&["*.md", "*.tex"]);
        assert!(should_mirror("paper.tex", &exc, &inc));
        assert!(should_mirror("README.md", &exc, &inc));
        assert!(!should_mirror("main.rs", &exc, &inc));
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path("./foo/bar.md"), "foo/bar.md");
        assert_eq!(normalize_path("foo\\bar.md"), "foo/bar.md");
        assert_eq!(normalize_path("."), "");
    }

    #[test]
    fn test_ide_files_excluded() {
        let exc = default_exclude();
        let inc = default_include();
        assert!(!should_mirror(".idea/workspace.xml", &exc, &inc));
        assert!(!should_mirror(".vscode/settings.json", &exc, &inc));
    }

    #[test]
    fn test_os_files_excluded() {
        let exc = default_exclude();
        let inc = default_include();
        assert!(!should_mirror(".DS_Store", &exc, &inc));
        assert!(!should_mirror("Thumbs.db", &exc, &inc));
    }
}
