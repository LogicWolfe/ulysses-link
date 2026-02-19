use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use notify::{Config as NotifyConfig, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{debug, error, info};
use walkdir::WalkDir;

use crate::config::RepoConfig;
use crate::linker;
use crate::matcher;

#[derive(Debug, Clone, PartialEq)]
enum EventType {
    Created,
    Deleted,
    DirDeleted,
    DirCreated,
}

struct PendingEvents {
    events: HashMap<String, EventType>,
}

pub struct RepoWatcher {
    _watcher: RecommendedWatcher,
    /// Flag to signal the debounce thread to stop
    stop: Arc<Mutex<bool>>,
    debounce_handle: Option<thread::JoinHandle<()>>,
}

impl RepoWatcher {
    pub fn cancel(&mut self) {
        {
            let mut stop = self.stop.lock().unwrap();
            *stop = true;
        }
        // Flush any remaining events won't happen — we just let the thread finish
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

/// Create a watcher for a single repo with debounced event handling.
pub fn create_watcher(
    repo_config: &RepoConfig,
    output_dir: &Path,
    debounce_seconds: f64,
) -> Result<RepoWatcher> {
    let pending = Arc::new(Mutex::new(PendingEvents {
        events: HashMap::new(),
    }));
    let stop = Arc::new(Mutex::new(false));

    let repo_path = repo_config.path.clone();
    let pending_clone = Arc::clone(&pending);

    // Create the notify watcher
    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| {
            match result {
                Ok(event) => handle_raw_event(&event, &repo_path, &pending_clone),
                Err(e) => error!("Watch error: {}", e),
            }
        },
        NotifyConfig::default(),
    )?;

    watcher.watch(&repo_config.path, RecursiveMode::Recursive)?;

    // Spawn debounce/flush thread
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
                // Flush remaining events before exiting
                flush_events(
                    &pending_flush,
                    &flush_repo_path,
                    &flush_repo_name,
                    &flush_output_dir,
                    &flush_exclude,
                    &flush_include,
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
                        flush_events(
                            &pending_flush,
                            &flush_repo_path,
                            &flush_repo_name,
                            &flush_output_dir,
                            &flush_exclude,
                            &flush_include,
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

fn handle_raw_event(event: &Event, repo_path: &Path, pending: &Arc<Mutex<PendingEvents>>) {
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
            EventKind::Modify(notify::event::ModifyKind::Name(rename_mode)) => {
                match rename_mode {
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
                        // Two paths: [from, to]. First path is delete, second is create.
                        // This case is handled by having both paths in event.paths
                        // and processing them in order. But with Both mode, we have
                        // two paths, so the loop handles them. For the first iteration,
                        // mark as deleted, for the second, mark as created.
                        // Since we're in a loop over event.paths, this works naturally
                        // if the first path is "from" and second is "to".
                        if path == &event.paths[0] {
                            p.events.insert(rel_path, EventType::Deleted);
                        } else if path.is_dir() {
                            p.events.insert(rel_path, EventType::DirCreated);
                        } else {
                            p.events.insert(rel_path, EventType::Created);
                        }
                    }
                    _ => {
                        // Unknown rename mode, treat as created
                        if path.is_dir() {
                            p.events.insert(rel_path, EventType::DirCreated);
                        } else {
                            p.events.insert(rel_path, EventType::Created);
                        }
                    }
                }
            }
            // Ignore data/metadata modifications — symlinks follow the target automatically
            EventKind::Modify(_) => {}
            _ => {}
        }
    }
}

fn flush_events(
    pending: &Arc<Mutex<PendingEvents>>,
    repo_path: &Path,
    repo_name: &str,
    output_dir: &Path,
    exclude: &ignore::gitignore::Gitignore,
    include: &globset::GlobSet,
) {
    let batch = {
        let mut p = pending.lock().unwrap();
        std::mem::take(&mut p.events)
    };

    if batch.is_empty() {
        return;
    }

    debug!("Debounced batch for {}: {} events", repo_name, batch.len());

    let mut creates = 0u32;
    let mut deletes = 0u32;

    for (rel_path, event_type) in &batch {
        match event_type {
            EventType::Deleted => {
                match linker::remove_symlink(repo_name, rel_path, output_dir) {
                    Ok(true) => deletes += 1,
                    Ok(false) => {}
                    Err(e) => error!("Error removing symlink for {}: {}", rel_path, e),
                }
            }
            EventType::Created => {
                if matcher::should_mirror(rel_path, exclude, include) {
                    match linker::ensure_symlink(repo_path, repo_name, rel_path, output_dir) {
                        Ok(true) => creates += 1,
                        Ok(false) => {}
                        Err(e) => error!("Error creating symlink for {}: {}", rel_path, e),
                    }
                }
            }
            EventType::DirDeleted => {
                match linker::remove_dir_symlinks(repo_name, rel_path, output_dir) {
                    Ok(n) => deletes += n,
                    Err(e) => error!("Error removing dir symlinks for {}: {}", rel_path, e),
                }
            }
            EventType::DirCreated => {
                let abs_dir = repo_path.join(rel_path);
                if abs_dir.is_dir() {
                    scan_new_dir(&abs_dir, repo_path, repo_name, output_dir, exclude, include, &mut creates);
                }
            }
        }
    }

    if creates > 0 || deletes > 0 {
        info!("Batch for {}: {} creates, {} deletes", repo_name, creates, deletes);
    }
}

fn scan_new_dir(
    abs_dir: &Path,
    repo_path: &Path,
    repo_name: &str,
    output_dir: &Path,
    exclude: &ignore::gitignore::Gitignore,
    include: &globset::GlobSet,
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
            match linker::ensure_symlink(repo_path, repo_name, &file_rel, output_dir) {
                Ok(true) => *creates += 1,
                Ok(false) => {}
                Err(e) => error!("Error creating symlink for {}: {}", file_rel, e),
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
        // Verify EventType variants exist and are distinct
        assert_ne!(EventType::Created, EventType::Deleted);
        assert_ne!(EventType::DirCreated, EventType::DirDeleted);
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

        let mut watcher = create_watcher(repo_config, &output, 0.1).unwrap();

        // Just verify it starts and stops without panicking
        thread::sleep(Duration::from_millis(50));
        watcher.cancel();
    }
}
