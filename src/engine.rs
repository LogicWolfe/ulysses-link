use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::config::{load_config, Config, RepoConfig, RescanInterval};
use crate::linker;
use crate::manifest::Manifest;
use crate::scanner::{full_scan, scan_repo};
use crate::upgrade::{self, VersionCheck};
use crate::watcher::{self, ConfigWatcher, MirrorWatcher, RepoWatcher};

const UPGRADE_CHECK_INTERVAL: Duration = Duration::from_secs(3600);

pub struct MirrorEngine {
    config: Config,
    watchers: HashMap<String, RepoWatcher>,
    mirror_watchers: HashMap<PathBuf, MirrorWatcher>,
    config_watcher: Option<ConfigWatcher>,
    manifests: HashMap<PathBuf, Arc<Mutex<Manifest>>>,
    running: Arc<AtomicBool>,
    last_scan_at: Instant,
    last_scan_duration: Duration,
    last_upgrade_check: Instant,
    last_etag: Option<String>,
}

impl MirrorEngine {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            watchers: HashMap::new(),
            mirror_watchers: HashMap::new(),
            config_watcher: None,
            manifests: HashMap::new(),
            running: Arc::new(AtomicBool::new(false)),
            last_scan_at: Instant::now(),
            last_scan_duration: Duration::ZERO,
            last_upgrade_check: Instant::now(),
            last_etag: None,
        }
    }

    /// Start the engine: load manifests, full scan, start watchers, enter main loop.
    pub fn start(&mut self) -> Result<()> {
        info!("Starting ulysses-link engine");

        // Load one manifest per unique output_dir
        for output_dir in self.config.active_output_dirs() {
            let loaded = Manifest::load(&output_dir)?;
            self.manifests
                .insert(output_dir, Arc::new(Mutex::new(loaded)));
        }

        // Initial full scan
        let scan_start = Instant::now();
        let result = {
            let mut unlocked: HashMap<PathBuf, Manifest> = self
                .manifests
                .iter()
                .map(|(k, v)| (k.clone(), v.lock().unwrap().clone()))
                .collect();
            let r = full_scan(&self.config, &mut unlocked);
            for (k, v) in unlocked {
                if let Some(arc) = self.manifests.get(&k) {
                    *arc.lock().unwrap() = v;
                } else {
                    self.manifests.insert(k, Arc::new(Mutex::new(v)));
                }
            }
            r
        };
        self.last_scan_duration = scan_start.elapsed();
        self.last_scan_at = Instant::now();
        info!(
            "Initial scan: {} created, {} existed, {} pruned, {} errors in {:?}",
            result.created,
            result.already_existed,
            result.pruned,
            result.errors,
            self.last_scan_duration,
        );

        // Start per-repo source watchers
        let repos: Vec<RepoConfig> = self.config.repos.clone();
        for repo_config in &repos {
            self.start_repo_watcher(repo_config);
        }

        // Start one mirror watcher per unique output_dir
        for output_dir in self.config.active_output_dirs() {
            self.start_mirror_watcher(&output_dir);
        }

        // Start config file watcher
        if let Some(ref config_path) = self.config.config_path {
            match watcher::create_config_watcher(config_path) {
                Ok(w) => {
                    debug!("Started config watcher on {}", config_path.display());
                    self.config_watcher = Some(w);
                }
                Err(e) => {
                    warn!("Failed to start config watcher: {}", e);
                }
            }
        }

        self.running.store(true, Ordering::SeqCst);

        // Register signal handlers
        let running = Arc::clone(&self.running);
        ctrlc::set_handler(move || {
            info!("Received shutdown signal");
            running.store(false, Ordering::SeqCst);
        })?;

        info!(
            "Started watching {} repos, {} files mirrored",
            self.watchers.len(),
            result.created + result.already_existed,
        );

        self.main_loop();
        Ok(())
    }

    /// Stop all watchers and clean up.
    pub fn stop(&mut self) {
        info!("Stopping ulysses-link engine");
        self.running.store(false, Ordering::SeqCst);

        for (name, watcher) in &mut self.watchers {
            debug!("Stopping watcher for {}", name);
            watcher.cancel();
        }
        self.watchers.clear();

        for (dir, watcher) in &mut self.mirror_watchers {
            debug!("Stopping mirror watcher on {}", dir.display());
            watcher.cancel();
        }
        self.mirror_watchers.clear();
        self.config_watcher = None;

        info!("Engine stopped");
    }

    /// Reload config: diff repos, add/remove/update watchers.
    pub fn reload_config(&mut self) {
        let config_path = match &self.config.config_path {
            Some(p) => p.clone(),
            None => {
                warn!("No config path available for reload");
                return;
            }
        };

        info!("Reloading config from {}", config_path.display());

        let new_config = match load_config(Some(&config_path)) {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to reload config: {}", e);
                return;
            }
        };

        let old_active = self.config.active_output_dirs();
        let new_active = new_config.active_output_dirs();

        let old_active_set: HashSet<PathBuf> = old_active.iter().cloned().collect();
        let new_active_set: HashSet<PathBuf> = new_active.iter().cloned().collect();

        // Determine if this is a simple global move:
        // ALL old repos shared one output_dir and ALL new repos share one (different) output_dir.
        let is_simple_global_move =
            old_active.len() == 1 && new_active.len() == 1 && old_active[0] != new_active[0];

        if is_simple_global_move {
            let old_dir = &old_active[0];
            let new_dir = &new_active[0];
            info!(
                "Global output_dir changed: {} -> {}",
                old_dir.display(),
                new_dir.display(),
            );

            // Stop mirror watcher on old dir to prevent deletions from propagating
            if let Some(mut mw) = self.mirror_watchers.remove(old_dir) {
                mw.cancel();
            }

            // Try to move the old output_dir to the new location
            let mut moved = false;
            match linker::move_output_dir(old_dir, new_dir) {
                Ok(true) => {
                    moved = true;
                    info!("Output directory moved successfully");
                }
                Ok(false) => {}
                Err(e) => {
                    warn!("Failed to move output_dir, will re-scan: {}", e);
                }
            }

            // Load manifest from new location
            match Manifest::load(new_dir) {
                Ok(m) => {
                    self.manifests.remove(old_dir);
                    self.manifests
                        .insert(new_dir.clone(), Arc::new(Mutex::new(m)));
                }
                Err(e) => {
                    error!("Failed to load manifest from new output_dir: {}", e);
                    return;
                }
            }

            if moved {
                info!("Running reconciliation scan after move");
            } else {
                info!("Re-scanning all repos into new output_dir");
            }
        }

        // Build repo name maps for diffing
        let old_names: HashSet<String> = self.config.repos.iter().map(|r| r.name.clone()).collect();
        let new_names: HashSet<String> = new_config.repos.iter().map(|r| r.name.clone()).collect();

        let new_repos_by_name: HashMap<String, RepoConfig> = new_config
            .repos
            .iter()
            .map(|r| (r.name.clone(), r.clone()))
            .collect();
        let old_repos_by_name: HashMap<String, RepoConfig> = self
            .config
            .repos
            .iter()
            .map(|r| (r.name.clone(), r.clone()))
            .collect();

        // Removed repos: prune mirrors from their old output_dir
        for name in old_names.difference(&new_names) {
            info!("Repo removed from config: {}", name);
            self.stop_repo_watcher(name);
            let old_rc = &old_repos_by_name[name];
            if let Some(manifest_arc) = self.manifests.get(&old_rc.output_dir) {
                let mut manifest = manifest_arc.lock().unwrap();
                let _ = linker::remove_repo_mirror(name, &old_rc.output_dir, &mut manifest);
            }
        }

        // Load manifests for newly active output_dirs
        for dir in new_active_set.difference(&old_active_set) {
            match Manifest::load(dir) {
                Ok(m) => {
                    self.manifests.insert(dir.clone(), Arc::new(Mutex::new(m)));
                }
                Err(e) => {
                    error!(
                        "Failed to load manifest for new output_dir {}: {}",
                        dir.display(),
                        e
                    );
                }
            }
        }

        let mut repos_changed = false;

        // Added repos
        for name in new_names.difference(&old_names) {
            info!("New repo in config: {}", name);
            if let Some(repo_config) = new_repos_by_name.get(name) {
                if let Some(manifest_arc) = self.manifests.get(&repo_config.output_dir) {
                    let mut manifest = manifest_arc.lock().unwrap();
                    scan_repo(repo_config, &repo_config.output_dir, &mut manifest);
                }
                self.start_repo_watcher(repo_config);
                repos_changed = true;
            }
        }

        // Changed repos (includes output_dir changes)
        for name in old_names.intersection(&new_names) {
            let old_rc = &old_repos_by_name[name];
            let new_rc = &new_repos_by_name[name];

            let output_dir_changed = old_rc.output_dir != new_rc.output_dir;
            let patterns_changed =
                old_rc.include_patterns != new_rc.include_patterns || old_rc.path != new_rc.path;

            if output_dir_changed {
                info!(
                    "Repo '{}' output_dir changed: {} -> {}, re-scanning",
                    name,
                    old_rc.output_dir.display(),
                    new_rc.output_dir.display()
                );
                self.stop_repo_watcher(name);

                // Prune old mirror (don't move — could share output_dir with other repos)
                if let Some(manifest_arc) = self.manifests.get(&old_rc.output_dir) {
                    let mut manifest = manifest_arc.lock().unwrap();
                    let _ = linker::remove_repo_mirror(name, &old_rc.output_dir, &mut manifest);
                }

                // Scan into new output_dir
                if let Some(manifest_arc) = self.manifests.get(&new_rc.output_dir) {
                    let mut manifest = manifest_arc.lock().unwrap();
                    scan_repo(new_rc, &new_rc.output_dir, &mut manifest);
                }

                self.start_repo_watcher(new_rc);
                repos_changed = true;
            } else if patterns_changed {
                info!("Repo config changed, re-scanning: {}", name);
                self.stop_repo_watcher(name);
                if let Some(manifest_arc) = self.manifests.get(&new_rc.output_dir) {
                    let mut manifest = manifest_arc.lock().unwrap();
                    scan_repo(new_rc, &new_rc.output_dir, &mut manifest);
                }
                self.start_repo_watcher(new_rc);
                repos_changed = true;
            }
        }

        self.config = new_config;

        // If this was a simple global move, do a full re-scan for reconciliation
        if is_simple_global_move {
            let scan_start = Instant::now();
            let result = {
                let mut unlocked: HashMap<PathBuf, Manifest> = self
                    .manifests
                    .iter()
                    .map(|(k, v)| (k.clone(), v.lock().unwrap().clone()))
                    .collect();
                let r = full_scan(&self.config, &mut unlocked);
                for (k, v) in unlocked {
                    if let Some(arc) = self.manifests.get(&k) {
                        *arc.lock().unwrap() = v;
                    }
                }
                r
            };
            self.last_scan_duration = scan_start.elapsed();
            self.last_scan_at = Instant::now();
            info!(
                "Scan after output_dir change: {} created, {} existed, {} pruned in {:?}",
                result.created, result.already_existed, result.pruned, self.last_scan_duration,
            );

            // Restart all repo watchers with new output_dir
            let repo_names: Vec<String> = self.watchers.keys().cloned().collect();
            for name in &repo_names {
                self.stop_repo_watcher(name);
            }
            let repos: Vec<RepoConfig> = self.config.repos.clone();
            for repo_config in &repos {
                self.start_repo_watcher(repo_config);
            }
        } else if repos_changed {
            self.last_scan_at = Instant::now();
        }

        // Reconcile mirror watchers: stop removed, start added
        let final_active: HashSet<PathBuf> = self.config.active_output_dirs().into_iter().collect();
        let current_watched: HashSet<PathBuf> = self.mirror_watchers.keys().cloned().collect();

        for dir in current_watched.difference(&final_active) {
            if let Some(mut mw) = self.mirror_watchers.remove(dir) {
                debug!("Stopping mirror watcher on {}", dir.display());
                mw.cancel();
            }
        }
        for dir in final_active.difference(&current_watched) {
            self.start_mirror_watcher(dir);
        }

        // Drop manifests for output_dirs no longer in use
        let stale_dirs: Vec<PathBuf> = self
            .manifests
            .keys()
            .filter(|k| !final_active.contains(*k))
            .cloned()
            .collect();
        for dir in stale_dirs {
            self.manifests.remove(&dir);
        }
    }

    fn start_repo_watcher(&mut self, repo_config: &RepoConfig) {
        let manifest_arc = match self.manifests.get(&repo_config.output_dir) {
            Some(m) => Arc::clone(m),
            None => {
                error!(
                    "No manifest for output_dir {} when starting watcher for {}",
                    repo_config.output_dir.display(),
                    repo_config.name
                );
                return;
            }
        };

        match watcher::create_watcher(
            repo_config,
            &repo_config.output_dir,
            self.config.debounce_seconds,
            manifest_arc,
        ) {
            Ok(w) => {
                debug!("Started watcher for {}", repo_config.name);
                self.watchers.insert(repo_config.name.clone(), w);
            }
            Err(e) => {
                let err_str = e.to_string().to_lowercase();
                if err_str.contains("inotify") {
                    warn!(
                        "inotify watch limit reached. Run:\n  \
                        echo fs.inotify.max_user_watches=524288 | \
                        sudo tee -a /etc/sysctl.conf\n  \
                        sudo sysctl -p"
                    );
                } else {
                    error!("Failed to start watcher for {}: {}", repo_config.name, e);
                }
            }
        }
    }

    fn start_mirror_watcher(&mut self, output_dir: &Path) {
        let manifest_arc = match self.manifests.get(output_dir) {
            Some(m) => Arc::clone(m),
            None => {
                error!(
                    "No manifest for output_dir {} when starting mirror watcher",
                    output_dir.display()
                );
                return;
            }
        };

        match watcher::create_mirror_watcher(output_dir, self.config.debounce_seconds, manifest_arc)
        {
            Ok(w) => {
                debug!("Started mirror watcher on {}", output_dir.display());
                self.mirror_watchers.insert(output_dir.to_path_buf(), w);
            }
            Err(e) => {
                error!(
                    "Failed to start mirror watcher on {}: {}",
                    output_dir.display(),
                    e
                );
            }
        }
    }

    fn stop_repo_watcher(&mut self, name: &str) {
        if let Some(mut w) = self.watchers.remove(name) {
            w.cancel();
        }
    }

    fn check_for_upgrade(&mut self) {
        match upgrade::check_latest_version(self.last_etag.as_deref()) {
            Ok(VersionCheck::NotModified) => {
                debug!("Upgrade check: index not modified");
            }
            Ok(VersionCheck::UpToDate { etag }) => {
                debug!("Upgrade check: already up to date");
                self.last_etag = Some(etag);
            }
            Ok(VersionCheck::UpdateAvailable { version, etag }) => {
                info!("New version available: {version}");
                self.last_etag = Some(etag);

                let cargo = match upgrade::find_cargo() {
                    Ok(c) => c,
                    Err(e) => {
                        error!("Cannot find cargo for auto-upgrade: {e}");
                        return;
                    }
                };

                match upgrade::run_cargo_install(&cargo) {
                    Ok(()) => {
                        info!("Upgraded to {version}, restarting");
                        std::process::exit(0);
                    }
                    Err(e) => {
                        error!("Auto-upgrade failed: {e}");
                    }
                }
            }
            Err(e) => {
                warn!("Upgrade check failed: {e}");
            }
        }
    }

    fn rescan_interval(&self) -> Option<Duration> {
        match &self.config.rescan_interval {
            RescanInterval::Never => None,
            RescanInterval::Auto => {
                let computed = self.last_scan_duration * 1000;
                Some(computed.max(Duration::from_secs(60)))
            }
            RescanInterval::Fixed(d) => Some(*d),
        }
    }

    fn main_loop(&mut self) {
        #[cfg(unix)]
        let mut sighup_signals = {
            use signal_hook::iterator::Signals;
            Signals::new([signal_hook::consts::SIGHUP]).ok()
        };

        while self.running.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_secs(1));

            #[cfg(unix)]
            if let Some(ref mut signals) = sighup_signals {
                for sig in signals.pending() {
                    if sig == signal_hook::consts::SIGHUP {
                        info!("Received SIGHUP, reloading config");
                        self.reload_config();
                    }
                }
            }

            if let Some(ref cw) = self.config_watcher {
                if cw.has_changed() {
                    info!("Config file changed, reloading");
                    self.reload_config();
                }
            }

            if let Some(interval) = self.rescan_interval() {
                if self.last_scan_at.elapsed() >= interval {
                    info!("Periodic rescan");
                    let scan_start = Instant::now();
                    let result = {
                        let mut unlocked: HashMap<PathBuf, Manifest> = self
                            .manifests
                            .iter()
                            .map(|(k, v)| (k.clone(), v.lock().unwrap().clone()))
                            .collect();
                        let r = full_scan(&self.config, &mut unlocked);
                        for (k, v) in unlocked {
                            if let Some(arc) = self.manifests.get(&k) {
                                *arc.lock().unwrap() = v;
                            }
                        }
                        r
                    };
                    self.last_scan_duration = scan_start.elapsed();
                    self.last_scan_at = Instant::now();
                    info!(
                        "Rescan: {} created, {} pruned in {:?}",
                        result.created, result.pruned, self.last_scan_duration,
                    );
                }
            }

            if self.config.auto_upgrade
                && self.last_upgrade_check.elapsed() >= UPGRADE_CHECK_INTERVAL
            {
                self.last_upgrade_check = Instant::now();
                self.check_for_upgrade();
            }
        }

        self.stop();
    }
}
