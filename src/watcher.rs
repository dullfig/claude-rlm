use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use crate::db::Db;
use crate::indexer::code;
use crate::treesitter::languages::Lang;

/// Start a background file watcher that re-indexes files when they change.
/// Returns a handle that keeps the watcher alive.
pub fn start_watcher(
    db: Db,
    project_dir: PathBuf,
) -> anyhow::Result<WatcherHandle> {
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())?;
    watcher.watch(&project_dir, RecursiveMode::Recursive)?;

    // Spawn a thread to process file events (notify uses std channels)
    let handle = std::thread::spawn(move || {
        process_events(rx, db);
    });

    Ok(WatcherHandle {
        _watcher: watcher,
        _thread: handle,
    })
}

/// Handle that keeps the watcher alive. Drop it to stop watching.
pub struct WatcherHandle {
    _watcher: RecommendedWatcher,
    _thread: std::thread::JoinHandle<()>,
}

fn process_events(rx: mpsc::Receiver<notify::Result<Event>>, db: Db) {
    // Debounce: collect events for a short period before re-indexing
    let mut pending_files: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let debounce_duration = Duration::from_millis(500);

    loop {
        // Wait for the first event
        match rx.recv() {
            Ok(Ok(event)) => {
                collect_changed_files(&event, &mut pending_files);
            }
            Ok(Err(e)) => {
                tracing::warn!("File watcher error: {}", e);
                continue;
            }
            Err(_) => {
                // Channel closed, watcher was dropped
                tracing::info!("File watcher channel closed, stopping");
                return;
            }
        }

        // Drain additional events within the debounce window
        loop {
            match rx.recv_timeout(debounce_duration) {
                Ok(Ok(event)) => {
                    collect_changed_files(&event, &mut pending_files);
                }
                Ok(Err(e)) => {
                    tracing::warn!("File watcher error: {}", e);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }

        // Process all pending files
        if !pending_files.is_empty() {
            let files: Vec<PathBuf> = pending_files.drain().collect();
            for file_path in &files {
                if !file_path.exists() {
                    // File was deleted, remove its symbols
                    let conn = db.conn();
                    let path_str = file_path.to_string_lossy();
                    if let Err(e) = conn.execute(
                        "DELETE FROM symbols WHERE file_path = ?1",
                        rusqlite::params![path_str.as_ref()],
                    ) {
                        tracing::warn!("Failed to remove symbols for {}: {}", path_str, e);
                    }
                    continue;
                }

                match code::reindex_file(&db, file_path) {
                    Ok(count) => {
                        if count > 0 {
                            tracing::debug!(
                                "Re-indexed {}: {} symbols",
                                file_path.display(),
                                count
                            );
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Skip reindex {}: {}", file_path.display(), e);
                    }
                }
            }
        }
    }
}

/// Extract relevant file paths from a notify event.
fn collect_changed_files(event: &Event, pending: &mut std::collections::HashSet<PathBuf>) {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
            for path in &event.paths {
                // Only care about files with recognized extensions
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if Lang::from_extension(ext).is_some() {
                        // Skip files in directories we don't care about
                        let path_str = path.to_string_lossy();
                        if !should_skip_path(&path_str) {
                            pending.insert(path.clone());
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn should_skip_path(path: &str) -> bool {
    let skip_dirs = [
        "node_modules",
        "target",
        "dist",
        "build",
        ".git",
        "__pycache__",
        "vendor",
        ".venv",
        "venv",
    ];

    for dir in &skip_dirs {
        // Check for both forward and back slashes
        if path.contains(&format!("/{}/", dir))
            || path.contains(&format!("\\{}\\", dir))
            || path.contains(&format!("\\{}/", dir))
            || path.contains(&format!("/{}\\", dir))
        {
            return true;
        }
    }
    false
}
