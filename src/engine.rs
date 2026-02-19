use std::collections::{HashMap, HashSet};
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
use crate::watcher::{self, MirrorWatcher, RepoWatcher};

pub struct MirrorEngine {
    config: Config,
    watchers: HashMap<String, RepoWatcher>,
    mirror_watcher: Option<MirrorWatcher>,
    manifest: Arc<Mutex<Manifest>>,
    running: Arc<AtomicBool>,
    last_scan_at: Instant,
    last_scan_duration: Duration,
}

impl MirrorEngine {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            watchers: HashMap::new(),
            mirror_watcher: None,
            manifest: Arc::new(Mutex::new(Manifest::empty())),
            running: Arc::new(AtomicBool::new(false)),
            last_scan_at: Instant::now(),
            last_scan_duration: Duration::ZERO,
        }
    }

    /// Start the engine: load manifest, full scan, start watchers, enter main loop.
    pub fn start(&mut self) -> Result<()> {
        info!("Starting ulysses-link engine");

        // Load manifest
        let loaded_manifest = Manifest::load(&self.config.output_dir)?;
        self.manifest = Arc::new(Mutex::new(loaded_manifest));

        // Initial full scan
        let scan_start = Instant::now();
        let result = {
            let mut manifest = self.manifest.lock().unwrap();
            full_scan(&self.config, &mut manifest)
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

        // Start mirror watcher on output_dir
        self.start_mirror_watcher();

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

        if let Some(ref mut mw) = self.mirror_watcher {
            debug!("Stopping mirror watcher");
            mw.cancel();
        }
        self.mirror_watcher = None;

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

        let old_names: HashSet<String> = self.config.repos.iter().map(|r| r.name.clone()).collect();
        let new_names: HashSet<String> = new_config.repos.iter().map(|r| r.name.clone()).collect();

        let new_repos: Vec<RepoConfig> = new_config.repos.clone();
        let new_repos_by_name: HashMap<String, RepoConfig> =
            new_repos.into_iter().map(|r| (r.name.clone(), r)).collect();

        let old_repos: Vec<RepoConfig> = self.config.repos.clone();
        let old_repos_by_name: HashMap<String, RepoConfig> =
            old_repos.into_iter().map(|r| (r.name.clone(), r)).collect();

        // Removed repos
        for name in old_names.difference(&new_names) {
            info!("Repo removed from config: {}", name);
            self.stop_repo_watcher(name);
            let mut manifest = self.manifest.lock().unwrap();
            let _ = linker::remove_repo_mirror(name, &self.config.output_dir, &mut manifest);
        }

        // Added repos
        let mut repos_changed = false;
        for name in new_names.difference(&old_names) {
            info!("New repo in config: {}", name);
            if let Some(repo_config) = new_repos_by_name.get(name) {
                let mut manifest = self.manifest.lock().unwrap();
                scan_repo(repo_config, &new_config.output_dir, &mut manifest);
                drop(manifest);
                self.start_repo_watcher(repo_config);
                repos_changed = true;
            }
        }

        // Changed repos
        for name in old_names.intersection(&new_names) {
            let old_rc = &old_repos_by_name[name];
            let new_rc = &new_repos_by_name[name];

            if old_rc.include_patterns != new_rc.include_patterns || old_rc.path != new_rc.path {
                info!("Repo config changed, re-scanning: {}", name);
                self.stop_repo_watcher(name);
                let mut manifest = self.manifest.lock().unwrap();
                scan_repo(new_rc, &new_config.output_dir, &mut manifest);
                drop(manifest);
                self.start_repo_watcher(new_rc);
                repos_changed = true;
            }
        }

        self.config = new_config;

        if repos_changed {
            self.last_scan_at = Instant::now();
        }
    }

    fn start_repo_watcher(&mut self, repo_config: &RepoConfig) {
        match watcher::create_watcher(
            repo_config,
            &self.config.output_dir,
            self.config.debounce_seconds,
            Arc::clone(&self.manifest),
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

    fn start_mirror_watcher(&mut self) {
        match watcher::create_mirror_watcher(
            &self.config.output_dir,
            self.config.debounce_seconds,
            Arc::clone(&self.manifest),
        ) {
            Ok(w) => {
                debug!(
                    "Started mirror watcher on {}",
                    self.config.output_dir.display()
                );
                self.mirror_watcher = Some(w);
            }
            Err(e) => {
                error!(
                    "Failed to start mirror watcher on {}: {}",
                    self.config.output_dir.display(),
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

            if let Some(interval) = self.rescan_interval() {
                if self.last_scan_at.elapsed() >= interval {
                    info!("Periodic rescan");
                    let scan_start = Instant::now();
                    let result = {
                        let mut manifest = self.manifest.lock().unwrap();
                        full_scan(&self.config, &mut manifest)
                    };
                    self.last_scan_duration = scan_start.elapsed();
                    self.last_scan_at = Instant::now();
                    info!(
                        "Rescan: {} created, {} pruned in {:?}",
                        result.created, result.pruned, self.last_scan_duration,
                    );
                }
            }
        }

        self.stop();
    }
}
