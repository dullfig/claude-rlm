use anyhow::Result;

use crate::db::Db;
use crate::hooks::{self, HookInput};
use crate::indexer::code;

/// Handle PreCompact hook: ensure all context is indexed before compaction.
///
/// This is critical — compaction will compress/discard older context, so we
/// need to make sure everything valuable has been captured in the index.
pub fn handle(input: &HookInput) -> Result<()> {
    let project_dir = hooks::project_dir(input);
    let session_id = hooks::session_id(input);
    let db = Db::open(std::path::Path::new(&project_dir))?;

    eprintln!("[claude-rlm] PreCompact: ensuring index is current for session {session_id}");

    // 1. Re-index any stale code files (modified since last indexed)
    let project_path = std::path::Path::new(&project_dir);
    let stale = code::stale_files(&db, project_path)?;
    if !stale.is_empty() {
        eprintln!("[claude-rlm] PreCompact: re-indexing {} stale files", stale.len());
        for path in &stale {
            if path.exists() {
                if let Err(e) = code::reindex_file(&db, path) {
                    eprintln!("[claude-rlm] PreCompact: failed to reindex {}: {}", path.display(), e);
                }
            } else {
                // File deleted — remove its symbols
                let conn = db.conn();
                let _ = conn.execute(
                    "DELETE FROM symbols WHERE file_path = ?1",
                    rusqlite::params![path.to_string_lossy().as_ref()],
                );
            }
        }
    }

    // 2. Generate a mid-session summary and store it as a turn
    //    This gives us a compact representation of what's happened so far,
    //    which survives compaction even if individual turns are lost.
    generate_checkpoint_summary(&db, &session_id)?;

    eprintln!("[claude-rlm] PreCompact: indexing complete");
    Ok(())
}

/// Generate a checkpoint summary of the session so far and store it as a special turn.
fn generate_checkpoint_summary(db: &Db, session_id: &str) -> Result<()> {
    let conn = db.conn();

    // Gather stats
    let turn_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM turns WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )?;

    if turn_count == 0 {
        return Ok(());
    }

    // Get all user requests
    let mut stmt = conn.prepare(
        "SELECT content FROM turns WHERE session_id = ?1 AND turn_type = 'request'
         ORDER BY turn_number ASC",
    )?;
    let requests: Vec<String> = stmt
        .query_map([session_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    // Get files modified
    let mut stmt = conn.prepare(
        "SELECT DISTINCT tf.file_path, tf.action
         FROM turn_files tf
         JOIN turns t ON t.id = tf.turn_id
         WHERE t.session_id = ?1 AND tf.action IN ('edit', 'write', 'create')
         ORDER BY tf.file_path",
    )?;
    let modified_files: Vec<(String, String)> = stmt
        .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    // Get edit summaries (most recent edits, condensed)
    let mut stmt = conn.prepare(
        "SELECT content FROM turns WHERE session_id = ?1 AND turn_type = 'code_edit'
         ORDER BY turn_number DESC LIMIT 10",
    )?;
    let recent_edits: Vec<String> = stmt
        .query_map([session_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    // Build the checkpoint summary
    let mut summary = String::from("[Pre-Compaction Checkpoint]\n");

    summary.push_str("Tasks:\n");
    for (i, req) in requests.iter().enumerate() {
        let truncated = if req.len() > 200 {
            format!("{}...", &req[..200])
        } else {
            req.clone()
        };
        summary.push_str(&format!("  {}. {}\n", i + 1, truncated));
    }

    if !modified_files.is_empty() {
        summary.push_str("\nFiles modified:\n");
        for (path, action) in &modified_files {
            summary.push_str(&format!("  - {} ({})\n", path, action));
        }
    }

    if !recent_edits.is_empty() {
        summary.push_str("\nRecent edits:\n");
        for edit in recent_edits.iter().rev() {
            let truncated = if edit.len() > 300 {
                format!("{}...", &edit[..300])
            } else {
                edit.clone()
            };
            summary.push_str(&format!("  - {}\n", truncated));
        }
    }

    // Store as a special "checkpoint" turn
    crate::indexer::conversation::index_turn(
        db,
        session_id,
        "system",
        "checkpoint",
        &summary,
        None,
        &[],
    )?;

    Ok(())
}
