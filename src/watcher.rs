use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use notify::{
    Config as NotifyConfig, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use tracing::{debug, error, info};
use walkdir::WalkDir;

use crate::config::RepoConfig;
use crate::linker::{self, SyncOutcome};
use crate::manifest::Manifest;
use crate::matcher;

#[derive(Debug, Clone, PartialEq)]
enum EventType {
    Created,
    Modified,
    Deleted,
    DirDeleted,
    DirCreated,
}

struct PendingEvents {
    events: HashMap<String, EventType>,
}

pub struct RepoWatcher {
    _watcher: RecommendedWatcher,
    stop: Arc<Mutex<bool>>,
    debounce_handle: Option<thread::JoinHandle<()>>,
}

impl RepoWatcher {
    pub fn cancel(&mut self) {
        {
            let mut stop = self.stop.lock().unwrap();
            *stop = true;
        }
        if let Some(handle) = self.debounce_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RepoWatcher {
    fn drop(&mut self) {
        let mut stop = self.stop.lock().unwrap();
        *stop = true;
    }
}

pub struct MirrorWatcher {
    _watcher: RecommendedWatcher,
    stop: Arc<Mutex<bool>>,
    debounce_handle: Option<thread::JoinHandle<()>>,
}

impl MirrorWatcher {
    pub fn cancel(&mut self) {
        {
            let mut stop = self.stop.lock().unwrap();
            *stop = true;
        }
        if let Some(handle) = self.debounce_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for MirrorWatcher {
    fn drop(&mut self) {
        let mut stop = self.stop.lock().unwrap();
        *stop = true;
    }
}

pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
    changed: Arc<AtomicBool>,
}

impl ConfigWatcher {
    /// Atomically check and clear the changed flag.
    pub fn has_changed(&self) -> bool {
        self.changed.swap(false, Ordering::SeqCst)
    }
}

/// Watch the config file for changes. Watches the parent directory (non-recursive)
/// so editors that delete+recreate the file still trigger events.
pub fn create_config_watcher(config_path: &Path) -> Result<ConfigWatcher> {
    let parent = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Config path has no parent directory"))?;
    let config_filename = config_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Config path has no filename"))?
        .to_os_string();

    let changed = Arc::new(AtomicBool::new(false));
    let changed_clone = Arc::clone(&changed);

    let watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| match result {
            Ok(event) => {
                let dominated = matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_));
                if dominated {
                    let matches_config = event.paths.iter().any(|p| {
                        p.file_name().map(OsStr::to_os_string).as_ref() == Some(&config_filename)
                    });
                    if matches_config {
                        changed_clone.store(true, Ordering::SeqCst);
                    }
                }
            }
            Err(e) => error!("Config watch error: {}", e),
        },
        NotifyConfig::default(),
    )?;

    // Watch parent directory non-recursively
    let mut watcher = watcher;
    watcher.watch(parent, RecursiveMode::NonRecursive)?;

    Ok(ConfigWatcher {
        _watcher: watcher,
        changed,
    })
}

/// Create a watcher for a single source repo with debounced event handling.
pub fn create_watcher(
    repo_config: &RepoConfig,
    output_dir: &Path,
    debounce_seconds: f64,
    manifest: Arc<Mutex<Manifest>>,
) -> Result<RepoWatcher> {
    let pending = Arc::new(Mutex::new(PendingEvents {
        events: HashMap::new(),
    }));
    let stop = Arc::new(Mutex::new(false));

    let repo_path = repo_config.path.clone();
    let pending_clone = Arc::clone(&pending);

    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| match result {
            Ok(event) => handle_raw_source_event(&event, &repo_path, &pending_clone),
            Err(e) => error!("Watch error: {}", e),
        },
        NotifyConfig::default(),
    )?;

    watcher.watch(&repo_config.path, RecursiveMode::Recursive)?;

    let pending_flush = Arc::clone(&pending);
    let stop_flush = Arc::clone(&stop);
    let flush_repo_path = repo_config.path.clone();
    let flush_repo_name = repo_config.name.clone();
    let flush_output_dir = output_dir.to_path_buf();
    let flush_exclude = repo_config.exclude.clone();
    let flush_include = repo_config.include.clone();
    let debounce_ms = (debounce_seconds * 1000.0) as u64;

    let debounce_handle = thread::spawn(move || {
        let check_interval = Duration::from_millis(100);
        let debounce_duration = Duration::from_millis(debounce_ms);
        let mut last_event_time: Option<std::time::Instant> = None;

        loop {
            if *stop_flush.lock().unwrap() {
                flush_source_events(
                    &pending_flush,
                    &flush_repo_path,
                    &flush_repo_name,
                    &flush_output_dir,
                    &flush_exclude,
                    &flush_include,
                    &manifest,
                );
                break;
            }

            let has_pending = {
                let p = pending_flush.lock().unwrap();
                !p.events.is_empty()
            };

            if has_pending {
                if last_event_time.is_none() {
                    last_event_time = Some(std::time::Instant::now());
                }

                if let Some(last) = last_event_time {
                    if last.elapsed() >= debounce_duration {
                        flush_source_events(
                            &pending_flush,
                            &flush_repo_path,
                            &flush_repo_name,
                            &flush_output_dir,
                            &flush_exclude,
                            &flush_include,
                            &manifest,
                        );
                        last_event_time = None;
                    }
                }
            } else {
                last_event_time = None;
            }

            thread::sleep(check_interval);
        }
    });

    Ok(RepoWatcher {
        _watcher: watcher,
        stop,
        debounce_handle: Some(debounce_handle),
    })
}

/// Create a watcher on the output (mirror) directory for bidirectional sync.
pub fn create_mirror_watcher(
    output_dir: &Path,
    debounce_seconds: f64,
    manifest: Arc<Mutex<Manifest>>,
) -> Result<MirrorWatcher> {
    let pending = Arc::new(Mutex::new(PendingEvents {
        events: HashMap::new(),
    }));
    let stop = Arc::new(Mutex::new(false));

    let watch_dir = output_dir.to_path_buf();
    let pending_clone = Arc::clone(&pending);

    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| match result {
            Ok(event) => handle_raw_mirror_event(&event, &watch_dir, &pending_clone),
            Err(e) => error!("Mirror watch error: {}", e),
        },
        NotifyConfig::default(),
    )?;

    watcher.watch(output_dir, RecursiveMode::Recursive)?;

    let pending_flush = Arc::clone(&pending);
    let stop_flush = Arc::clone(&stop);
    let flush_output_dir = output_dir.to_path_buf();
    let debounce_ms = (debounce_seconds * 1000.0) as u64;

    let debounce_handle = thread::spawn(move || {
        let check_interval = Duration::from_millis(100);
        let debounce_duration = Duration::from_millis(debounce_ms);
        let mut last_event_time: Option<std::time::Instant> = None;

        loop {
            if *stop_flush.lock().unwrap() {
                flush_mirror_events(&pending_flush, &flush_output_dir, &manifest);
                break;
            }

            let has_pending = {
                let p = pending_flush.lock().unwrap();
                !p.events.is_empty()
            };

            if has_pending {
                if last_event_time.is_none() {
                    last_event_time = Some(std::time::Instant::now());
                }

                if let Some(last) = last_event_time {
                    if last.elapsed() >= debounce_duration {
                        flush_mirror_events(&pending_flush, &flush_output_dir, &manifest);
                        last_event_time = None;
                    }
                }
            } else {
                last_event_time = None;
            }

            thread::sleep(check_interval);
        }
    });

    Ok(MirrorWatcher {
        _watcher: watcher,
        stop,
        debounce_handle: Some(debounce_handle),
    })
}

fn handle_raw_source_event(event: &Event, repo_path: &Path, pending: &Arc<Mutex<PendingEvents>>) {
    let mut p = pending.lock().unwrap();

    for path in &event.paths {
        let rel_path = match path.strip_prefix(repo_path) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => continue,
        };

        match event.kind {
            EventKind::Create(_) => {
                if path.is_dir() {
                    p.events.insert(rel_path, EventType::DirCreated);
                } else {
                    p.events.insert(rel_path, EventType::Created);
                }
            }
            EventKind::Remove(notify::event::RemoveKind::Folder) => {
                p.events.insert(rel_path, EventType::DirDeleted);
            }
            EventKind::Remove(_) => {
                p.events.insert(rel_path, EventType::Deleted);
            }
            EventKind::Modify(notify::event::ModifyKind::Name(rename_mode)) => match rename_mode {
                notify::event::RenameMode::From => {
                    p.events.insert(rel_path, EventType::Deleted);
                }
                notify::event::RenameMode::To => {
                    if path.is_dir() {
                        p.events.insert(rel_path, EventType::DirCreated);
                    } else {
                        p.events.insert(rel_path, EventType::Created);
                    }
                }
                notify::event::RenameMode::Both => {
                    if path == &event.paths[0] {
                        p.events.insert(rel_path, EventType::Deleted);
                    } else if path.is_dir() {
                        p.events.insert(rel_path, EventType::DirCreated);
                    } else {
                        p.events.insert(rel_path, EventType::Created);
                    }
                }
                _ => {
                    if path.is_dir() {
                        p.events.insert(rel_path, EventType::DirCreated);
                    } else {
                        p.events.insert(rel_path, EventType::Created);
                    }
                }
            },
            EventKind::Modify(notify::event::ModifyKind::Data(_)) => {
                if !path.is_dir() {
                    p.events.insert(rel_path, EventType::Modified);
                }
            }
            _ => {}
        }
    }
}

fn handle_raw_mirror_event(event: &Event, output_dir: &Path, pending: &Arc<Mutex<PendingEvents>>) {
    let mut p = pending.lock().unwrap();

    for path in &event.paths {
        let rel_path = match path.strip_prefix(output_dir) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => continue,
        };

        // Ignore manifest and base cache files
        if rel_path.starts_with(".ulysses-link") {
            continue;
        }

        match event.kind {
            EventKind::Modify(notify::event::ModifyKind::Data(_)) => {
                if !path.is_dir() {
                    p.events.insert(rel_path, EventType::Modified);
                }
            }
            EventKind::Remove(_) if !path.is_dir() => {
                p.events.insert(rel_path, EventType::Deleted);
            }
            // Ignore creates in mirror — not our file unless already in manifest
            _ => {}
        }
    }
}

fn flush_source_events(
    pending: &Arc<Mutex<PendingEvents>>,
    repo_path: &Path,
    repo_name: &str,
    output_dir: &Path,
    exclude: &ignore::gitignore::Gitignore,
    include: &globset::GlobSet,
    manifest: &Arc<Mutex<Manifest>>,
) {
    let batch = {
        let mut p = pending.lock().unwrap();
        std::mem::take(&mut p.events)
    };

    if batch.is_empty() {
        return;
    }

    debug!("Debounced batch for {}: {} events", repo_name, batch.len());

    let mut manifest = manifest.lock().unwrap();
    let mut creates = 0u32;
    let mut deletes = 0u32;

    for (rel_path, event_type) in &batch {
        match event_type {
            EventType::Deleted => {
                let manifest_rel = format!("{repo_name}/{rel_path}");
                match linker::propagate_delete(&manifest_rel, &mut manifest, output_dir) {
                    Ok(true) => deletes += 1,
                    Ok(false) => {}
                    Err(e) => error!("Error propagating delete for {}: {}", rel_path, e),
                }
            }
            EventType::Created | EventType::Modified => {
                if matcher::should_mirror(rel_path, exclude, include) {
                    let source = repo_path.join(rel_path);
                    let manifest_rel = format!("{repo_name}/{rel_path}");
                    let mirror = output_dir.join(&manifest_rel);
                    match linker::sync_file(
                        &source,
                        &mirror,
                        &mut manifest,
                        &manifest_rel,
                        output_dir,
                    ) {
                        Ok(SyncOutcome::Copied) => creates += 1,
                        Ok(
                            SyncOutcome::AlreadyInSync
                            | SyncOutcome::Claimed
                            | SyncOutcome::Skipped,
                        ) => {}
                        Ok(SyncOutcome::Merged) => creates += 1,
                        Ok(SyncOutcome::Conflict) => {
                            info!("Conflict detected for {}", rel_path);
                        }
                        Err(e) => error!("Error syncing {}: {}", rel_path, e),
                    }
                }
            }
            EventType::DirDeleted => {
                match linker::remove_dir_mirrors(repo_name, rel_path, output_dir, &mut manifest) {
                    Ok(n) => deletes += n,
                    Err(e) => error!("Error removing dir mirrors for {}: {}", rel_path, e),
                }
            }
            EventType::DirCreated => {
                let abs_dir = repo_path.join(rel_path);
                if abs_dir.is_dir() {
                    scan_new_dir(
                        &abs_dir,
                        repo_path,
                        repo_name,
                        output_dir,
                        exclude,
                        include,
                        &mut manifest,
                        &mut creates,
                    );
                }
            }
        }
    }

    if creates > 0 || deletes > 0 {
        if let Err(e) = manifest.save(output_dir) {
            error!("Failed to save manifest: {}", e);
        }
        info!(
            "Batch for {}: {} creates, {} deletes",
            repo_name, creates, deletes
        );
    }
}

fn flush_mirror_events(
    pending: &Arc<Mutex<PendingEvents>>,
    output_dir: &Path,
    manifest: &Arc<Mutex<Manifest>>,
) {
    let batch = {
        let mut p = pending.lock().unwrap();
        std::mem::take(&mut p.events)
    };

    if batch.is_empty() {
        return;
    }

    debug!("Mirror debounced batch: {} events", batch.len());

    let mut manifest = manifest.lock().unwrap();
    let mut syncs = 0u32;
    let mut deletes = 0u32;

    for (rel_path, event_type) in &batch {
        match event_type {
            EventType::Modified => {
                if let Some(entry) = manifest.get(rel_path).cloned() {
                    let source = entry.source.clone();
                    let mirror = output_dir.join(rel_path);
                    match linker::sync_file(&source, &mirror, &mut manifest, rel_path, output_dir) {
                        Ok(SyncOutcome::Copied) => syncs += 1,
                        Ok(SyncOutcome::AlreadyInSync) => {}
                        Ok(SyncOutcome::Merged) => syncs += 1,
                        Ok(SyncOutcome::Conflict) => {
                            info!("Conflict detected for mirror edit: {}", rel_path);
                        }
                        Ok(_) => {}
                        Err(e) => error!("Error syncing mirror edit for {}: {}", rel_path, e),
                    }
                }
            }
            EventType::Deleted => {
                match linker::propagate_mirror_delete(rel_path, &mut manifest, output_dir) {
                    Ok(true) => deletes += 1,
                    Ok(false) => {}
                    Err(e) => error!("Error propagating mirror delete for {}: {}", rel_path, e),
                }
            }
            _ => {}
        }
    }

    if syncs > 0 || deletes > 0 {
        if let Err(e) = manifest.save(output_dir) {
            error!("Failed to save manifest: {}", e);
        }
        info!("Mirror batch: {} syncs, {} deletes", syncs, deletes);
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_new_dir(
    abs_dir: &Path,
    repo_path: &Path,
    repo_name: &str,
    output_dir: &Path,
    exclude: &ignore::gitignore::Gitignore,
    include: &globset::GlobSet,
    manifest: &mut Manifest,
    creates: &mut u32,
) {
    for entry in WalkDir::new(abs_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir() || entry.path_is_symlink() {
            continue;
        }

        let file_rel = match entry.path().strip_prefix(repo_path) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };

        if matcher::should_mirror(&file_rel, exclude, include) {
            let source = repo_path.join(&file_rel);
            let manifest_rel = format!("{repo_name}/{file_rel}");
            let mirror = output_dir.join(&manifest_rel);
            match linker::sync_file(&source, &mirror, manifest, &manifest_rel, output_dir) {
                Ok(SyncOutcome::Copied) => *creates += 1,
                Ok(SyncOutcome::AlreadyInSync | SyncOutcome::Claimed | SyncOutcome::Skipped) => {}
                Ok(SyncOutcome::Merged | SyncOutcome::Conflict) => *creates += 1,
                Err(e) => error!("Error syncing {}: {}", file_rel, e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_event_type_mapping() {
        assert_ne!(EventType::Created, EventType::Deleted);
        assert_ne!(EventType::DirCreated, EventType::DirDeleted);
        assert_ne!(EventType::Modified, EventType::Created);
    }

    #[test]
    fn test_watcher_creates_and_cancels() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let output = tmp.path().join("output");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&output).unwrap();

        let toml = format!(
            "version = 1\noutput_dir = \"{}\"\n\n[[repos]]\npath = \"{}\"",
            output.display(),
            repo.display()
        );
        let config_file = tmp.path().join("config.toml");
        fs::write(&config_file, toml).unwrap();
        let cfg = config::load_config(Some(&config_file)).unwrap();
        let repo_config = &cfg.repos[0];

        let manifest = Arc::new(Mutex::new(Manifest::load(&output).unwrap()));
        let mut watcher = create_watcher(repo_config, &output, 0.1, manifest).unwrap();

        thread::sleep(Duration::from_millis(50));
        watcher.cancel();
    }

    #[test]
    fn test_config_watcher_creates_and_detects_change() {
        let tmp = TempDir::new().unwrap();
        let config_file = tmp.path().join("config.toml");
        fs::write(&config_file, "version = 1").unwrap();

        let watcher = create_config_watcher(&config_file).unwrap();
        assert!(!watcher.has_changed());

        // Modify the config file
        thread::sleep(Duration::from_millis(100));
        fs::write(&config_file, "version = 1\nlog_level = \"DEBUG\"").unwrap();
        thread::sleep(Duration::from_millis(500));

        assert!(watcher.has_changed());
        // Second call should return false (flag was cleared)
        assert!(!watcher.has_changed());
    }

    #[test]
    fn test_mirror_watcher_creates_and_cancels() {
        let tmp = TempDir::new().unwrap();
        let output = tmp.path().join("output");
        fs::create_dir_all(&output).unwrap();

        let manifest = Arc::new(Mutex::new(Manifest::load(&output).unwrap()));
        let mut watcher = create_mirror_watcher(&output, 0.1, manifest).unwrap();

        thread::sleep(Duration::from_millis(50));
        watcher.cancel();
    }
}
