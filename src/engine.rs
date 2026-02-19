use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::config::{load_config, Config, RepoConfig};
use crate::linker;
use crate::scanner::{full_scan, scan_repo};
use crate::watcher::{self, RepoWatcher};

pub struct MirrorEngine {
    config: Config,
    watchers: HashMap<String, RepoWatcher>,
    running: Arc<AtomicBool>,
}

impl MirrorEngine {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            watchers: HashMap::new(),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start the engine: full scan, start watchers, register signals, enter main loop.
    pub fn start(&mut self) -> Result<()> {
        info!("Starting ulysses-link engine");

        let result = full_scan(&self.config);
        info!(
            "Initial scan: {} created, {} existed, {} pruned, {} errors",
            result.created, result.already_existed, result.pruned, result.errors,
        );

        // Clone repos to avoid borrow conflict
        let repos: Vec<RepoConfig> = self.config.repos.clone();
        for repo_config in &repos {
            self.start_repo_watcher(repo_config);
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

        // Clone the data we need to avoid borrow issues
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
            let _ = linker::remove_repo_mirror(name, &self.config.output_dir);
        }

        // Added repos
        for name in new_names.difference(&old_names) {
            info!("New repo in config: {}", name);
            if let Some(repo_config) = new_repos_by_name.get(name) {
                scan_repo(repo_config, &new_config.output_dir);
                self.start_repo_watcher(repo_config);
            }
        }

        // Changed repos
        for name in old_names.intersection(&new_names) {
            let old_rc = &old_repos_by_name[name];
            let new_rc = &new_repos_by_name[name];

            if old_rc.include_patterns != new_rc.include_patterns || old_rc.path != new_rc.path {
                info!("Repo config changed, re-scanning: {}", name);
                self.stop_repo_watcher(name);
                scan_repo(new_rc, &new_config.output_dir);
                self.start_repo_watcher(new_rc);
            }
        }

        self.config = new_config;
    }

    fn start_repo_watcher(&mut self, repo_config: &RepoConfig) {
        match watcher::create_watcher(
            repo_config,
            &self.config.output_dir,
            self.config.debounce_seconds,
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

    fn stop_repo_watcher(&mut self, name: &str) {
        if let Some(mut w) = self.watchers.remove(name) {
            w.cancel();
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
        }

        self.stop();
    }
}
