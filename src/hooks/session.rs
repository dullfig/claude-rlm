use anyhow::Result;
use serde_json::json;

use crate::db::Db;
use crate::hooks::{self, HookInput};
use crate::indexer::{code, conversation, distill, files, git};
use crate::inject;

/// Handle SessionStart hook.
/// - source="startup": inject project memory (recent sessions, knowledge)
/// - source="compact": inject context relevant to current task
pub fn handle_start(input: &HookInput) -> Result<()> {
    let project_dir = hooks::project_dir(input);
    let session_id = hooks::session_id(input);
    let source = input.source.as_deref().unwrap_or("startup");

    let db = Db::open(std::path::Path::new(&project_dir))?;
    conversation::ensure_session(&db, &session_id, &project_dir)?;

    // Catch up on git changes since last session
    if source == "startup" {
        match git::catchup(&db, std::path::Path::new(&project_dir), &session_id) {
            Ok(stats) if stats.commits > 0 => {
                eprintln!(
                    "[claude-rlm] Git catch-up: {} commits, {} files changed",
                    stats.commits, stats.files_changed
                );
            }
            Err(e) => eprintln!("[claude-rlm] Git catch-up skipped: {}", e),
            _ => {}
        }
    }

    // File-hash catch-up for non-git projects
    if source == "startup" && !git::is_git_repo(std::path::Path::new(&project_dir)) {
        match files::catchup(&db, std::path::Path::new(&project_dir), &session_id) {
            Ok(stats) if stats.files_changed + stats.files_added + stats.files_deleted > 0 => {
                eprintln!(
                    "[claude-rlm] File catch-up: {} changed, {} added, {} deleted",
                    stats.files_changed, stats.files_added, stats.files_deleted
                );
            }
            Err(e) => eprintln!("[claude-rlm] File catch-up failed: {}", e),
            _ => {}
        }
    }

    // On startup, run initial code indexing if no index exists
    if source == "startup" && !code::has_index(&db)? {
        let dir = std::path::Path::new(&project_dir);
        if let Err(e) = code::index_project(&db, dir) {
            eprintln!("[claude-rlm] Initial code indexing failed: {}", e);
        }
    }

    let context = match source {
        "compact" => inject::build_compact_context(&db, &session_id)?,
        _ => inject::build_startup_context(&db)?,
    };

    // Print startup banner with quick stats
    if source == "startup" {
        let conn = db.conn();
        let sessions: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE ended_at IS NOT NULL",
            [], |row| row.get(0)
        ).unwrap_or(0);
        let symbols: i64 = conn.query_row(
            "SELECT COUNT(*) FROM symbols", [], |row| row.get(0)
        ).unwrap_or(0);
        let knowledge: i64 = conn.query_row(
            "SELECT COUNT(*) FROM knowledge WHERE superseded_by IS NULL",
            [], |row| row.get(0)
        ).unwrap_or(0);

        let mut parts = Vec::new();
        if sessions > 0 { parts.push(format!("{} sessions", sessions)); }
        if symbols > 0 { parts.push(format!("{} symbols", symbols)); }
        if knowledge > 0 { parts.push(format!("{} knowledge", knowledge)); }

        if parts.is_empty() {
            eprintln!("[ClaudeRLM] Project memory initialized");
        } else {
            eprintln!("[ClaudeRLM] Project memory loaded ({})", parts.join(", "));
        }
    }

    if !context.is_empty() {
        let output = json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": context
            }
        });
        println!("{}", serde_json::to_string(&output)?);
    }

    Ok(())
}

/// Handle SessionEnd hook: mark session as ended, distill knowledge.
pub fn handle_end(input: &HookInput) -> Result<()> {
    let project_dir = hooks::project_dir(input);
    let session_id = hooks::session_id(input);

    let db = Db::open(std::path::Path::new(&project_dir))?;

    // 1. Distill knowledge from the session (LLM if configured, else heuristic)
    match distill::distill_session_smart(&db, &session_id) {
        Ok(stats) => {
            if stats.extracted > 0 {
                eprintln!(
                    "[claude-rlm] Distilled {} knowledge entries from session {}",
                    stats.extracted, session_id
                );
            }
        }
        Err(e) => eprintln!("[claude-rlm] Knowledge distillation failed: {}", e),
    }

    // 2. Generate summary and mark session as ended
    let summary = generate_session_summary(&db, &session_id)?;
    conversation::end_session(&db, &session_id, summary.as_deref())?;

    Ok(())
}

/// Generate a basic session summary from the turn history.
fn generate_session_summary(db: &Db, session_id: &str) -> Result<Option<String>> {
    let conn = db.conn();

    // Get request turns for this session
    let mut stmt = conn.prepare(
        "SELECT content FROM turns
         WHERE session_id = ?1 AND turn_type = 'request'
         ORDER BY turn_number ASC
         LIMIT 20",
    )?;

    let requests: Vec<String> = stmt
        .query_map([session_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    if requests.is_empty() {
        return Ok(None);
    }

    // Get count of different turn types
    let edit_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM turns WHERE session_id = ?1 AND turn_type = 'code_edit'",
        [session_id],
        |row| row.get(0),
    )?;

    let file_count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT file_path) FROM turn_files
         JOIN turns ON turns.id = turn_files.turn_id
         WHERE turns.session_id = ?1",
        [session_id],
        |row| row.get(0),
    )?;

    // Build summary from user requests
    let mut summary = String::from("User requests:\n");
    for (i, req) in requests.iter().enumerate() {
        let truncated = if req.len() > 200 {
            let end = req.floor_char_boundary(200);
            format!("{}...", &req[..end])
        } else {
            req.clone()
        };
        summary.push_str(&format!("{}. {}\n", i + 1, truncated));
    }
    summary.push_str(&format!(
        "\nStats: {} code edits across {} files",
        edit_count, file_count
    ));

    Ok(Some(summary))
}
