use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::config::Config;

#[cfg(target_os = "macos")]
const LAUNCHD_LABEL: &str = "com.ulysses-link.agent";

#[cfg(target_os = "linux")]
const SYSTEMD_UNIT_NAME: &str = "ulysses-link.service";

fn binary_path() -> PathBuf {
    std::env::current_exe().expect("Failed to determine binary path")
}

fn log_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let dir = dirs::home_dir()
            .expect("Failed to determine home directory")
            .join("Library")
            .join("Logs")
            .join("ulysses-link");
        std::fs::create_dir_all(&dir).ok();
        dir
    }

    #[cfg(target_os = "linux")]
    {
        let dir = dirs::home_dir()
            .expect("Failed to determine home directory")
            .join(".local")
            .join("share")
            .join("ulysses-link")
            .join("logs");
        std::fs::create_dir_all(&dir).ok();
        dir
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ulysses-link")
            .join("logs");
        std::fs::create_dir_all(&dir).ok();
        dir
    }
}

pub fn install_service(config: &Config) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        install_launchd(config)
    }

    #[cfg(target_os = "linux")]
    {
        install_systemd(config)
    }

    #[cfg(target_os = "windows")]
    {
        print_windows_instructions(config);
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        anyhow::bail!("Unsupported platform for service installation")
    }
}

pub fn uninstall_service() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        uninstall_launchd()
    }

    #[cfg(target_os = "linux")]
    {
        uninstall_systemd()
    }

    #[cfg(target_os = "windows")]
    {
        println!("To remove ulysses-link from Windows Task Scheduler:");
        println!("  1. Open Task Scheduler");
        println!("  2. Find and delete the 'ulysses-link' task");
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        anyhow::bail!("Unsupported platform for service management")
    }
}

pub fn print_status() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        status_launchd()
    }

    #[cfg(target_os = "linux")]
    {
        status_systemd()
    }

    #[cfg(target_os = "windows")]
    {
        println!("Check Windows Task Scheduler for 'ulysses-link' task status.");
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        println!("Unsupported platform");
        Ok(())
    }
}

/// Check if the background service is currently running.
pub fn is_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        is_running_launchd()
    }

    #[cfg(target_os = "linux")]
    {
        is_running_systemd()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

/// Send SIGHUP to the running service to trigger a config reload.
pub fn send_reload_signal() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("launchctl")
            .args(["kill", "SIGHUP", &format!("gui/{}/{LAUNCHD_LABEL}", unsafe { libc::getuid() })])
            .output()
            .context("Failed to send SIGHUP via launchctl")?;
        if !output.status.success() {
            anyhow::bail!("launchctl kill SIGHUP failed");
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("systemctl")
            .args(["--user", "reload", SYSTEMD_UNIT_NAME])
            .status()
            .context("Failed to reload systemd unit")?;
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        anyhow::bail!("Reload signal not supported on this platform")
    }
}

// --- macOS launchd ---

#[cfg(target_os = "macos")]
fn plist_path() -> PathBuf {
    dirs::home_dir()
        .expect("Failed to determine home directory")
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

#[cfg(target_os = "macos")]
fn build_plist(config: &Config) -> String {
    let binary = binary_path();
    let log = log_dir();
    let config_path = config
        .config_path
        .as_deref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let args = [
        binary.to_string_lossy().to_string(),
        "run".to_string(),
        "--config".to_string(),
        config_path,
    ];

    let args_xml: String = args
        .iter()
        .map(|a| format!("        <string>{a}</string>"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{args_xml}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log}/ulysses-link.stdout.log</string>
    <key>StandardErrorPath</key>
    <string>{log}/ulysses-link.stderr.log</string>
    <key>ProcessType</key>
    <string>Background</string>
    <key>Nice</key>
    <integer>5</integer>
</dict>
</plist>
"#,
        log = log.display()
    )
}

#[cfg(target_os = "macos")]
fn install_launchd(config: &Config) -> Result<()> {
    let plist = plist_path();
    let content = build_plist(config);

    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&plist, &content)?;
    info!("Wrote plist to {}", plist.display());

    // Use modern bootstrap API
    let uid = unsafe { libc::getuid() };
    let domain_target = format!("gui/{uid}");

    // Try to bootout first (in case it's already loaded)
    let _ = Command::new("launchctl")
        .args(["bootout", &domain_target, &plist.to_string_lossy()])
        .output();

    let status = Command::new("launchctl")
        .args(["bootstrap", &domain_target, &plist.to_string_lossy()])
        .status()
        .context("Failed to run launchctl bootstrap")?;

    if !status.success() {
        // Fall back to legacy load
        warn!("launchctl bootstrap failed, trying legacy load");
        Command::new("launchctl")
            .args(["load", &plist.to_string_lossy()])
            .status()
            .context("Failed to run launchctl load")?;
    }

    info!("Loaded launchd agent: {}", LAUNCHD_LABEL);
    println!("Service installed and started: {LAUNCHD_LABEL}");
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_launchd() -> Result<()> {
    let plist = plist_path();
    if plist.exists() {
        let uid = unsafe { libc::getuid() };
        let domain_target = format!("gui/{uid}");

        let _ = Command::new("launchctl")
            .args(["bootout", &domain_target, &plist.to_string_lossy()])
            .output();

        std::fs::remove_file(&plist)?;
        info!("Removed launchd agent: {}", LAUNCHD_LABEL);
        println!("Service uninstalled: {LAUNCHD_LABEL}");
    } else {
        println!("Service is not installed.");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn is_running_launchd() -> bool {
    Command::new("launchctl")
        .args(["list"])
        .output()
        .ok()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.contains(LAUNCHD_LABEL))
        })
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn status_launchd() -> Result<()> {
    if is_running_launchd() {
        let output = Command::new("launchctl")
            .args(["list"])
            .output()
            .context("Failed to run launchctl list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.contains(LAUNCHD_LABEL) {
                println!("Service is running: {}", line.trim());
                return Ok(());
            }
        }
    }
    println!("Service is not running.");
    Ok(())
}

// --- Linux systemd ---

#[cfg(target_os = "linux")]
fn unit_path() -> PathBuf {
    dirs::home_dir()
        .expect("Failed to determine home directory")
        .join(".config")
        .join("systemd")
        .join("user")
        .join(SYSTEMD_UNIT_NAME)
}

#[cfg(target_os = "linux")]
fn build_unit(config: &Config) -> String {
    let binary = binary_path();
    let config_path = config
        .config_path
        .as_deref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    format!(
        r#"[Unit]
Description=ulysses-link — documentation symlink sync service
After=default.target

[Service]
Type=simple
ExecStart={binary} run --config {config_path}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#,
        binary = binary.display(),
    )
}

#[cfg(target_os = "linux")]
fn install_systemd(config: &Config) -> Result<()> {
    let unit = unit_path();
    let content = build_unit(config);

    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&unit, &content)?;
    info!("Wrote systemd unit to {}", unit.display());

    Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .context("Failed to run systemctl daemon-reload")?;

    Command::new("systemctl")
        .args(["--user", "enable", "--now", SYSTEMD_UNIT_NAME])
        .status()
        .context("Failed to enable systemd unit")?;

    info!("Enabled and started systemd unit: {}", SYSTEMD_UNIT_NAME);
    println!("Service installed and started: {SYSTEMD_UNIT_NAME}");
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_systemd() -> Result<()> {
    let unit = unit_path();
    if unit.exists() {
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", SYSTEMD_UNIT_NAME])
            .output();
        std::fs::remove_file(&unit)?;
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();
        info!("Removed systemd unit: {}", SYSTEMD_UNIT_NAME);
        println!("Service uninstalled: {SYSTEMD_UNIT_NAME}");
    } else {
        println!("Service is not installed.");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn is_running_systemd() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", SYSTEMD_UNIT_NAME])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn status_systemd() -> Result<()> {
    let output = Command::new("systemctl")
        .args(["--user", "status", SYSTEMD_UNIT_NAME])
        .output()
        .context("Failed to run systemctl status")?;

    println!("{}", String::from_utf8_lossy(&output.stdout));
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            println!("{}", stderr);
        }
    }
    Ok(())
}

// --- Windows ---

#[cfg(target_os = "windows")]
fn print_windows_instructions(config: &Config) {
    let binary = binary_path();
    let config_path = config
        .config_path
        .as_deref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "PATH_TO_CONFIG".into());

    println!("Windows service setup instructions:");
    println!();
    println!("Option 1: Task Scheduler");
    println!("  1. Open Task Scheduler (taskschd.msc)");
    println!("  2. Create a Basic Task named 'ulysses-link'");
    println!("  3. Set trigger: 'When I log on'");
    println!("  4. Set action: Start a program");
    println!("     Program: {}", binary.display());
    println!("     Arguments: run --config {config_path}");
    println!();
    println!("Option 2: NSSM (Non-Sucking Service Manager)");
    println!("  1. Download NSSM from https://nssm.cc/");
    println!(
        "  2. Run: nssm install ulysses-link {} run --config {}",
        binary.display(),
        config_path
    );
    println!("  3. Run: nssm start ulysses-link");
    println!();
    println!("Note: Symlinks on Windows require Developer Mode enabled.");
    println!("  Settings > Update & Security > For developers > Developer Mode");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_binary_path_resolves() {
        let path = binary_path();
        // Should return some path (the test binary)
        assert!(!path.as_os_str().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_plist_content_generation() {
        let tmp = TempDir::new().unwrap();
        let config = Config {
            output_dir: tmp.path().join("output"),
            repos: vec![],
            debounce_seconds: 0.5,
            log_level: "INFO".into(),
            config_path: Some(tmp.path().join("config.yaml")),
        };

        let plist = build_plist(&config);
        assert!(plist.contains(LAUNCHD_LABEL));
        assert!(plist.contains("RunAtLoad"));
        assert!(plist.contains("KeepAlive"));
        assert!(plist.contains("ulysses-link.stdout.log"));
        assert!(plist.contains("ulysses-link.stderr.log"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_unit_content_generation() {
        let tmp = TempDir::new().unwrap();
        let config = Config {
            output_dir: tmp.path().join("output"),
            repos: vec![],
            debounce_seconds: 0.5,
            log_level: "INFO".into(),
            config_path: Some(tmp.path().join("config.yaml")),
        };

        let unit = build_unit(&config);
        assert!(unit.contains("ulysses-link"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("Restart=on-failure"));
    }
}
