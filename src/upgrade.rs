use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CRATE_NAME: &str = "ulysses-link";
const SPARSE_INDEX_URL: &str = "https://index.crates.io/ul/ys/ulysses-link";

#[derive(Debug, PartialEq)]
pub enum VersionCheck {
    NotModified,
    UpToDate { etag: String },
    UpdateAvailable { version: String, etag: String },
}

pub fn check_latest_version(last_etag: Option<&str>) -> Result<VersionCheck> {
    let mut request = ureq::get(SPARSE_INDEX_URL)
        .header("user-agent", &format!("{CRATE_NAME}/{CURRENT_VERSION}"));

    if let Some(etag) = last_etag {
        request = request.header("if-none-match", etag);
    }

    let mut response = match request.call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(304)) => return Ok(VersionCheck::NotModified),
        Err(e) => return Err(e).context("Failed to fetch crate index"),
    };

    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = response
        .body_mut()
        .read_to_string()
        .context("Failed to read index response")?;

    let latest = parse_latest_version(&body)?;
    let check = compare_versions(&latest, CURRENT_VERSION, etag);
    Ok(check)
}

fn parse_latest_version(body: &str) -> Result<String> {
    let last_line = body
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .context("Empty index response body")?;

    let parsed: serde_json::Value =
        serde_json::from_str(last_line).context("Failed to parse index JSON")?;

    parsed["vers"]
        .as_str()
        .map(|s| s.to_string())
        .context("No 'vers' field in index entry")
}

fn compare_versions(latest: &str, current: &str, etag: String) -> VersionCheck {
    let latest_parts = parse_semver(latest);
    let current_parts = parse_semver(current);

    if latest_parts > current_parts {
        VersionCheck::UpdateAvailable {
            version: latest.to_string(),
            etag,
        }
    } else {
        VersionCheck::UpToDate { etag }
    }
}

fn parse_semver(v: &str) -> (u64, u64, u64) {
    let parts: Vec<u64> = v.split('.').filter_map(|s| s.parse().ok()).collect();
    (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    )
}

pub fn find_cargo() -> Result<PathBuf> {
    if let Some(home) = dirs::home_dir() {
        let cargo_home = home.join(".cargo").join("bin").join("cargo");
        if cargo_home.is_file() {
            return Ok(cargo_home);
        }
    }

    which::which("cargo").context("cargo not found in ~/.cargo/bin or PATH")
}

pub fn run_cargo_install(cargo: &std::path::Path) -> Result<()> {
    let status = Command::new(cargo)
        .args(["install", CRATE_NAME])
        .status()
        .context("Failed to run cargo install")?;

    if !status.success() {
        bail!("cargo install exited with status {status}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_index_body() -> String {
        [
            r#"{"name":"ulysses-link","vers":"0.9.5","deps":[],"cksum":"abc123","features":{},"yanked":false}"#,
            r#"{"name":"ulysses-link","vers":"0.9.6","deps":[],"cksum":"def456","features":{},"yanked":false}"#,
            r#"{"name":"ulysses-link","vers":"0.9.8","deps":[],"cksum":"ghi789","features":{},"yanked":false}"#,
        ]
        .join("\n")
    }

    #[test]
    fn test_parse_latest_version() {
        let body = sample_index_body();
        let ver = parse_latest_version(&body).unwrap();
        assert_eq!(ver, "0.9.8");
    }

    #[test]
    fn test_parse_empty_body() {
        let err = parse_latest_version("").unwrap_err();
        assert!(err.to_string().contains("Empty"));
    }

    #[test]
    fn test_parse_malformed_json() {
        let err = parse_latest_version("not json at all").unwrap_err();
        assert!(err.to_string().contains("parse"));
    }

    #[test]
    fn test_parse_single_line() {
        let body = r#"{"name":"ulysses-link","vers":"0.1.0","deps":[],"cksum":"aaa","features":{},"yanked":false}"#;
        let ver = parse_latest_version(body).unwrap();
        assert_eq!(ver, "0.1.0");
    }

    #[test]
    fn test_version_comparison_newer() {
        let check = compare_versions("0.9.8", "0.9.7", "etag-1".into());
        assert_eq!(
            check,
            VersionCheck::UpdateAvailable {
                version: "0.9.8".into(),
                etag: "etag-1".into()
            }
        );
    }

    #[test]
    fn test_version_comparison_equal() {
        let check = compare_versions("0.9.7", "0.9.7", "etag-2".into());
        assert_eq!(
            check,
            VersionCheck::UpToDate {
                etag: "etag-2".into()
            }
        );
    }

    #[test]
    fn test_version_comparison_older() {
        let check = compare_versions("0.9.6", "0.9.7", "etag-3".into());
        assert_eq!(
            check,
            VersionCheck::UpToDate {
                etag: "etag-3".into()
            }
        );
    }

    #[test]
    fn test_find_cargo_in_cargo_home() {
        if let Some(home) = dirs::home_dir() {
            let cargo_path = home.join(".cargo").join("bin").join("cargo");
            if cargo_path.is_file() {
                let found = find_cargo().unwrap();
                assert_eq!(found, cargo_path);
            }
        }
    }

    #[test]
    fn test_find_cargo_returns_valid_path() {
        let found = find_cargo().unwrap();
        assert!(
            found.is_file(),
            "cargo path should exist: {}",
            found.display()
        );
    }
}
