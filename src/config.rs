use std::collections::HashMap;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Deserialize;
use thiserror::Error;
use tracing::warn;

// --- Defaults ---

pub const DEFAULT_GLOBAL_EXCLUDE: &[&str] = &[
    // Version control
    ".git/",
    ".svn/",
    ".hg/",
    // Package managers / dependencies
    "node_modules/",
    "bower_components/",
    "vendor/",
    ".pnpm-store/",
    // Virtual environments
    ".venv/",
    "venv/",
    // Build output
    "dist/",
    "build/",
    "out/",
    "target/",
    "_build/",
    // Framework build caches
    ".next/",
    ".nuxt/",
    ".svelte-kit/",
    ".docusaurus/",
    // Python caches
    "__pycache__/",
    "*.pyc",
    "*.pyo",
    ".mypy_cache/",
    ".pytest_cache/",
    ".ruff_cache/",
    ".tox/",
    "*.egg-info/",
    // IDE / editor
    ".idea/",
    ".vscode/",
    "*.swp",
    "*.swo",
    "*~",
    // OS files
    ".DS_Store",
    "Thumbs.db",
    // Test coverage
    "coverage/",
    "htmlcov/",
    ".nyc_output/",
    // Misc caches
    ".cache/",
    ".gradle/",
    ".terraform/",
];

pub const DEFAULT_GLOBAL_INCLUDE: &[&str] = &[
    // Markdown
    "*.md",
    "*.mdx",
    "*.markdown",
    // Other markup / doc formats
    "*.txt",
    "*.rst",
    "*.adoc",
    "*.org",
    // Common extensionless doc files
    "README",
    "LICENSE",
    "LICENCE",
    "CHANGELOG",
    "CONTRIBUTING",
    "AUTHORS",
    "COPYING",
    "TODO",
];

pub const DEFAULT_DEBOUNCE_SECONDS: f64 = 0.5;
pub const DEFAULT_LOG_LEVEL: &str = "INFO";

// --- Errors ---

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("No config file found. Create one at ~/.config/doc-link/config.toml or pass --config PATH.")]
    NoConfigFound,

    #[error("Config file not found: {0}")]
    FileNotFound(PathBuf),

    #[error("{0}")]
    Validation(String),

    #[error("Failed to read config: {0}")]
    Io(#[from] std::io::Error),

    #[error("Failed to parse TOML: {0}")]
    Toml(#[from] toml::de::Error),
}

// --- Raw TOML schema ---

#[derive(Debug, Deserialize)]
struct RawConfig {
    version: Option<u64>,
    output_dir: Option<String>,
    global_exclude: Option<Vec<String>>,
    global_include: Option<Vec<String>>,
    debounce_seconds: Option<f64>,
    log_level: Option<String>,
    repos: Option<Vec<RawRepo>>,
}

#[derive(Debug, Deserialize)]
struct RawRepo {
    path: String,
    name: Option<String>,
    exclude: Option<Vec<String>>,
    include: Option<Vec<String>>,
}

// --- Validated config ---

#[derive(Debug, Clone)]
pub struct RepoConfig {
    pub path: PathBuf,
    pub name: String,
    pub exclude: Gitignore,
    pub include: GlobSet,
    /// Raw include patterns preserved for comparison during config reload
    pub include_patterns: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub output_dir: PathBuf,
    pub repos: Vec<RepoConfig>,
    pub debounce_seconds: f64,
    pub log_level: String,
    pub config_path: Option<PathBuf>,
}

// --- Config search ---

pub fn config_search_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("./doc-link.toml")];

    if let Some(config_dir) = dirs::config_dir() {
        paths.push(config_dir.join("doc-link").join("config.toml"));
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        paths.push(
            home.join("Library")
                .join("Application Support")
                .join("doc-link")
                .join("config.toml"),
        );
    }

    paths
}

pub fn find_config_path(explicit: Option<&Path>) -> Result<PathBuf, ConfigError> {
    if let Some(p) = explicit {
        let expanded = expand_path(&p.to_string_lossy())?;
        if expanded.is_file() {
            return Ok(expanded);
        }
        return Err(ConfigError::FileNotFound(expanded));
    }

    for candidate in config_search_paths() {
        let expanded = expand_path(&candidate.to_string_lossy())?;
        if expanded.is_file() {
            return Ok(expanded);
        }
    }

    Err(ConfigError::NoConfigFound)
}

// --- Path expansion ---

fn expand_path(p: &str) -> Result<PathBuf, ConfigError> {
    let expanded = shellexpand::full(p)
        .map_err(|e| ConfigError::Validation(format!("Failed to expand path '{p}': {e}")))?;
    let path = PathBuf::from(expanded.as_ref());
    Ok(dunce_canonicalize_or_absolute(&path))
}

/// Canonicalize if path exists, otherwise make absolute without requiring existence.
fn dunce_canonicalize_or_absolute(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        canonical
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    }
}

// --- Loading ---

pub fn load_config(config_path: Option<&Path>) -> Result<Config, ConfigError> {
    let resolved = find_config_path(config_path)?;
    let contents = std::fs::read_to_string(&resolved)?;
    let raw: RawConfig = toml::from_str(&contents)?;
    parse_config(raw, Some(resolved))
}

fn parse_config(raw: RawConfig, config_path: Option<PathBuf>) -> Result<Config, ConfigError> {
    // Version check
    match raw.version {
        Some(1) => {}
        other => {
            return Err(ConfigError::Validation(format!(
                "Config version must be 1, got {other:?}"
            )));
        }
    }

    // Output dir
    let output_dir_raw = raw
        .output_dir
        .as_deref()
        .ok_or_else(|| ConfigError::Validation("'output_dir' is required".into()))?;
    let output_dir = expand_path(output_dir_raw)?;
    std::fs::create_dir_all(&output_dir)?;
    // Re-canonicalize now that the directory exists (resolves macOS /var -> /private/var)
    let output_dir = std::fs::canonicalize(&output_dir).unwrap_or(output_dir);

    // Debounce
    let debounce = raw.debounce_seconds.unwrap_or(DEFAULT_DEBOUNCE_SECONDS);
    if !(0.0..=30.0).contains(&debounce) {
        return Err(ConfigError::Validation(format!(
            "'debounce_seconds' must be between 0.0 and 30.0, got {debounce}"
        )));
    }

    // Log level
    let log_level = raw.log_level.unwrap_or_else(|| DEFAULT_LOG_LEVEL.into());
    let valid_levels = ["DEBUG", "INFO", "WARNING", "ERROR", "TRACE"];
    if !valid_levels.contains(&log_level.as_str()) {
        return Err(ConfigError::Validation(format!(
            "'log_level' must be one of {valid_levels:?}, got '{log_level}'"
        )));
    }

    // Global patterns
    let global_exclude: Vec<String> = raw.global_exclude.unwrap_or_else(|| {
        DEFAULT_GLOBAL_EXCLUDE.iter().map(|s| s.to_string()).collect()
    });
    let global_include: Vec<String> = raw.global_include.unwrap_or_else(|| {
        DEFAULT_GLOBAL_INCLUDE.iter().map(|s| s.to_string()).collect()
    });
    let global_include = if global_include.is_empty() {
        DEFAULT_GLOBAL_INCLUDE.iter().map(|s| s.to_string()).collect()
    } else {
        global_include
    };

    // Repos
    let repos_raw = raw.repos.unwrap_or_default();
    let named_repos = resolve_repo_names(&repos_raw)?;

    let mut repos = Vec::new();
    for (repo_raw, path, name) in named_repos {
        if !path.is_dir() {
            warn!("Repo path does not exist, skipping: {}", path.display());
            continue;
        }

        // Check output_dir not inside repo
        if output_dir.starts_with(&path) {
            return Err(ConfigError::Validation(format!(
                "output_dir '{}' is inside repo '{}'. This would create an infinite loop.",
                output_dir.display(),
                path.display(),
            )));
        }

        let repo_exclude: Vec<String> = repo_raw.exclude.clone().unwrap_or_default();
        let repo_include: Vec<String> = repo_raw.include.clone().unwrap_or_default();

        let all_exclude: Vec<String> = global_exclude.iter().chain(repo_exclude.iter()).cloned().collect();
        let all_include: Vec<String> = global_include.iter().chain(repo_include.iter()).cloned().collect();

        let exclude = compile_exclude(&all_exclude, &path)?;
        let include = compile_include(&all_include)?;

        repos.push(RepoConfig {
            path,
            name,
            exclude,
            include,
            include_patterns: all_include,
        });
    }

    Ok(Config {
        output_dir,
        repos,
        debounce_seconds: debounce,
        log_level,
        config_path,
    })
}

fn resolve_repo_names(repos: &[RawRepo]) -> Result<Vec<(&RawRepo, PathBuf, String)>, ConfigError> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut result = Vec::new();

    for repo in repos {
        let path = expand_path(&repo.path)?;
        let base_name = repo.name.clone().unwrap_or_else(|| {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unnamed".into())
        });

        let count = seen.entry(base_name.clone()).or_insert(0);
        *count += 1;

        let name = if *count > 1 {
            let suffixed = format!("{}-{}", base_name, count);
            warn!(
                "Repo name collision for '{}', using '{}'",
                base_name, suffixed
            );
            suffixed
        } else {
            base_name
        };

        result.push((repo, path, name));
    }

    Ok(result)
}

fn compile_exclude(patterns: &[String], repo_path: &Path) -> Result<Gitignore, ConfigError> {
    let mut builder = GitignoreBuilder::new(repo_path);
    for pattern in patterns {
        builder
            .add_line(None, pattern)
            .map_err(|e| ConfigError::Validation(format!("Invalid exclude pattern '{pattern}': {e}")))?;
    }
    builder
        .build()
        .map_err(|e| ConfigError::Validation(format!("Failed to compile exclude patterns: {e}")))
}

fn compile_include(patterns: &[String]) -> Result<GlobSet, ConfigError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        // For patterns without path separators, match against filename only
        // by prepending **/ to make them match at any depth
        let glob_pattern = if !pattern.contains('/') && !pattern.starts_with("**/") {
            format!("**/{pattern}")
        } else {
            pattern.clone()
        };
        let glob = Glob::new(&glob_pattern)
            .map_err(|e| ConfigError::Validation(format!("Invalid include pattern '{pattern}': {e}")))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| ConfigError::Validation(format!("Failed to compile include patterns: {e}")))
}

// --- Default config generation ---

pub fn generate_default_config(path: &Path) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, DEFAULT_CONFIG_CONTENT)?;
    Ok(())
}

pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("doc-link")
        .join("config.toml")
}

const DEFAULT_CONFIG_CONTENT: &str = r#"# doc-link configuration
version = 1

# Where the symlink mirror tree is rooted.
# Tilde and env vars are expanded.
output_dir = "~/doc-link"

# Debounce window in seconds for filesystem events.
# After a burst of events (e.g. git pull), wait this long before syncing.
debounce_seconds = 0.5

# Logging level: TRACE, DEBUG, INFO, WARNING, ERROR
log_level = "INFO"

# Global exclude patterns applied to ALL repos (gitignore syntax).
# These are checked BEFORE includes, so node_modules/*.md stays excluded.
# Uncomment to override defaults (version control dirs, node_modules,
# build output, IDE files, OS files, etc. are excluded by default).
# global_exclude = [".git/", "node_modules/"]

# Global include patterns — files matching these are mirrored.
# Uncomment to override defaults (*.md, *.mdx, *.markdown, *.txt, *.rst,
# *.adoc, *.org, README, LICENSE, CHANGELOG, etc. are included by default).
# global_include = ["*.md", "*.mdx"]

# Per-repo definitions
# [[repos]]
# path = "~/code/my-project"
# name = "my-project"           # optional, defaults to directory basename
# exclude = ["docs/generated/"] # merged with global_exclude
# include = ["*.tex"]           # merged with global_include
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_config(dir: &Path, content: &str) -> PathBuf {
        let path = dir.join("doc-link.toml");
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_valid_minimal_config() {
        let tmp = TempDir::new().unwrap();
        let repo_dir = tmp.path().join("my-repo");
        fs::create_dir(&repo_dir).unwrap();
        let output_dir = tmp.path().join("output");

        let config_path = write_config(
            tmp.path(),
            &format!(
                "version = 1\noutput_dir = \"{}\"\n\n[[repos]]\npath = \"{}\"",
                output_dir.display(),
                repo_dir.display()
            ),
        );

        let config = load_config(Some(&config_path)).unwrap();
        assert_eq!(config.repos.len(), 1);
        assert_eq!(config.repos[0].name, "my-repo");
        assert_eq!(config.debounce_seconds, DEFAULT_DEBOUNCE_SECONDS);
        assert_eq!(config.log_level, "INFO");
    }

    #[test]
    fn test_missing_version() {
        let tmp = TempDir::new().unwrap();
        let config_path = write_config(tmp.path(), "output_dir = \"/tmp/out\"");

        let err = load_config(Some(&config_path)).unwrap_err();
        assert!(err.to_string().contains("version must be 1"));
    }

    #[test]
    fn test_wrong_version() {
        let tmp = TempDir::new().unwrap();
        let config_path = write_config(tmp.path(), "version = 2\noutput_dir = \"/tmp/out\"");

        let err = load_config(Some(&config_path)).unwrap_err();
        assert!(err.to_string().contains("version must be 1"));
    }

    #[test]
    fn test_missing_output_dir() {
        let tmp = TempDir::new().unwrap();
        let config_path = write_config(tmp.path(), "version = 1");

        let err = load_config(Some(&config_path)).unwrap_err();
        assert!(err.to_string().contains("output_dir"));
    }

    #[test]
    fn test_debounce_out_of_range() {
        let tmp = TempDir::new().unwrap();
        let output_dir = tmp.path().join("output");
        let config_path = write_config(
            tmp.path(),
            &format!(
                "version = 1\noutput_dir = \"{}\"\ndebounce_seconds = 50.0",
                output_dir.display()
            ),
        );

        let err = load_config(Some(&config_path)).unwrap_err();
        assert!(err.to_string().contains("debounce_seconds"));
    }

    #[test]
    fn test_invalid_log_level() {
        let tmp = TempDir::new().unwrap();
        let output_dir = tmp.path().join("output");
        let config_path = write_config(
            tmp.path(),
            &format!(
                "version = 1\noutput_dir = \"{}\"\nlog_level = \"VERBOSE\"",
                output_dir.display()
            ),
        );

        let err = load_config(Some(&config_path)).unwrap_err();
        assert!(err.to_string().contains("log_level"));
    }

    #[test]
    fn test_repo_name_deduplication() {
        let tmp = TempDir::new().unwrap();
        let repo1 = tmp.path().join("repos").join("project");
        let repo2 = tmp.path().join("other").join("project");
        fs::create_dir_all(&repo1).unwrap();
        fs::create_dir_all(&repo2).unwrap();
        let output_dir = tmp.path().join("output");

        let config_path = write_config(
            tmp.path(),
            &format!(
                "version = 1\noutput_dir = \"{}\"\n\n[[repos]]\npath = \"{}\"\n\n[[repos]]\npath = \"{}\"",
                output_dir.display(),
                repo1.display(),
                repo2.display()
            ),
        );

        let config = load_config(Some(&config_path)).unwrap();
        assert_eq!(config.repos.len(), 2);
        assert_eq!(config.repos[0].name, "project");
        assert_eq!(config.repos[1].name, "project-2");
    }

    #[test]
    fn test_output_dir_inside_repo() {
        let tmp = TempDir::new().unwrap();
        let repo_dir = tmp.path().join("my-repo");
        fs::create_dir(&repo_dir).unwrap();
        let output_dir = repo_dir.join("mirror");

        let config_path = write_config(
            tmp.path(),
            &format!(
                "version = 1\noutput_dir = \"{}\"\n\n[[repos]]\npath = \"{}\"",
                output_dir.display(),
                repo_dir.display()
            ),
        );

        let err = load_config(Some(&config_path)).unwrap_err();
        assert!(err.to_string().contains("infinite loop"));
    }

    #[test]
    fn test_missing_repo_skipped() {
        let tmp = TempDir::new().unwrap();
        let output_dir = tmp.path().join("output");

        let config_path = write_config(
            tmp.path(),
            &format!(
                "version = 1\noutput_dir = \"{}\"\n\n[[repos]]\npath = \"/nonexistent/repo/path\"",
                output_dir.display()
            ),
        );

        let config = load_config(Some(&config_path)).unwrap();
        assert_eq!(config.repos.len(), 0);
    }

    #[test]
    fn test_custom_patterns() {
        let tmp = TempDir::new().unwrap();
        let repo_dir = tmp.path().join("my-repo");
        fs::create_dir(&repo_dir).unwrap();
        let output_dir = tmp.path().join("output");

        let config_path = write_config(
            tmp.path(),
            &format!(
                "version = 1\noutput_dir = \"{}\"\nglobal_exclude = [\".git/\"]\nglobal_include = [\"*.md\"]\n\n[[repos]]\npath = \"{}\"\nexclude = [\"vendor/\"]\ninclude = [\"*.rst\"]",
                output_dir.display(),
                repo_dir.display()
            ),
        );

        let config = load_config(Some(&config_path)).unwrap();
        assert_eq!(config.repos.len(), 1);
        assert!(config.repos[0].include_patterns.contains(&"*.md".to_string()));
        assert!(config.repos[0].include_patterns.contains(&"*.rst".to_string()));
    }

    #[test]
    fn test_generate_default_config() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("subdir").join("config.toml");

        generate_default_config(&config_path).unwrap();
        assert!(config_path.exists());

        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("version = 1"));
        assert!(content.contains("output_dir"));
    }

    #[test]
    fn test_explicit_config_not_found() {
        let err = find_config_path(Some(Path::new("/nonexistent/config.toml"))).unwrap_err();
        assert!(matches!(err, ConfigError::FileNotFound(_)));
    }

    #[test]
    fn test_no_config_found() {
        let tmp = TempDir::new().unwrap();
        let _guard = std::env::set_current_dir(tmp.path());
        let err = find_config_path(None);
        assert!(matches!(err, Err(ConfigError::NoConfigFound)));
    }
}
