use anyhow::Result;

use crate::db::Db;

/// Info about an active plan, returned by `active_plan()`.
pub struct PlanInfo {
    pub id: i64,
    pub session_id: String,
    pub plan_file_path: String,
    pub title: Option<String>,
    pub content: String,
    pub status: String,
    pub target_files: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub progress: Vec<ProgressEntry>,
}

pub struct ProgressEntry {
    pub file_path: String,
    pub edit_count: i64,
}

/// Check if a path is a Claude Code plan file.
pub fn is_plan_file(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.contains(".claude/plans/") && normalized.ends_with(".md")
}

/// Extract the title from plan markdown content.
/// Uses the first `# ` heading, falls back to the filename.
pub fn extract_title(content: &str, path: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("# ") {
            let title = heading.trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
    }
    // Fallback: filename without extension
    let normalized = path.replace('\\', "/");
    normalized
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .strip_suffix(".md")
        .unwrap_or(path)
        .to_string()
}

/// Extract target file paths from plan markdown content.
/// Looks for backtick-wrapped paths and bare paths with known extensions.
pub fn extract_target_files(content: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let known_extensions = [
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".c", ".cpp", ".h", ".hpp",
        ".cs", ".rb", ".swift", ".kt", ".scala", ".toml", ".yaml", ".yml", ".json", ".sql",
        ".sh", ".bash", ".zsh", ".ps1", ".md",
    ];

    for line in content.lines() {
        // Match backtick-wrapped paths: `src/foo/bar.rs`
        let mut rest = line;
        while let Some(start) = rest.find('`') {
            rest = &rest[start + 1..];
            if let Some(end) = rest.find('`') {
                let candidate = &rest[..end];
                rest = &rest[end + 1..];
                if is_file_path(candidate, &known_extensions) && seen.insert(candidate.to_string())
                {
                    files.push(candidate.to_string());
                }
            } else {
                break;
            }
        }
    }

    files
}

/// Check if a string looks like a file path with a known extension.
fn is_file_path(s: &str, extensions: &[&str]) -> bool {
    if s.is_empty() || s.len() > 200 {
        return false;
    }
    // Must contain a slash or dot to look like a path
    if !s.contains('/') && !s.contains('.') {
        return false;
    }
    // Must not contain spaces (likely prose, not a path)
    if s.contains(' ') {
        return false;
    }
    extensions.iter().any(|ext| s.ends_with(ext))
}

/// Create or update a plan in the database.
/// When creating a new plan, supersedes any other active plans.
pub fn upsert_plan(db: &Db, session_id: &str, path: &str, content: &str) -> Result<i64> {
    let conn = db.conn();
    let title = extract_title(content, path);
    let target_files = extract_target_files(content);
    let target_files_json = serde_json::to_string(&target_files)?;

    // Check if a plan with this path already exists and is active
    let existing_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM plans WHERE plan_file_path = ?1 AND status IN ('created', 'in_progress')",
            [path],
            |row| row.get(0),
        )
        .ok();

    if let Some(id) = existing_id {
        // Update existing plan
        conn.execute(
            "UPDATE plans SET content = ?1, title = ?2, target_files = ?3, updated_at = datetime('now')
             WHERE id = ?4",
            rusqlite::params![content, title, target_files_json, id],
        )?;
        Ok(id)
    } else {
        // Supersede any other active plans
        let active_ids: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT id FROM plans WHERE status IN ('created', 'in_progress') AND plan_file_path != ?1",
            )?;
            let rows = stmt.query_map([path], |row| row.get(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };

        // Insert new plan
        conn.execute(
            "INSERT INTO plans (session_id, plan_file_path, title, content, status, target_files)
             VALUES (?1, ?2, ?3, ?4, 'created', ?5)",
            rusqlite::params![session_id, path, title, content, target_files_json],
        )?;
        let new_id = conn.last_insert_rowid();

        // Mark old active plans as superseded
        for old_id in active_ids {
            conn.execute(
                "UPDATE plans SET status = 'superseded', superseded_by = ?1, updated_at = datetime('now')
                 WHERE id = ?2",
                rusqlite::params![new_id, old_id],
            )?;
        }

        Ok(new_id)
    }
}

/// Record a source file edit as progress on the active plan.
/// Transitions plan from `created` → `in_progress` on first source edit.
pub fn record_progress(db: &Db, session_id: &str, file_path: &str) -> Result<()> {
    let conn = db.conn();

    // Find active plan for this session (or any active plan)
    let plan_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM plans
             WHERE status IN ('created', 'in_progress')
             ORDER BY CASE WHEN session_id = ?1 THEN 0 ELSE 1 END, updated_at DESC
             LIMIT 1",
            [session_id],
            |row| row.get(0),
        )
        .ok();

    let Some(plan_id) = plan_id else {
        return Ok(());
    };

    // Upsert into plan_progress
    conn.execute(
        "INSERT INTO plan_progress (plan_id, file_path)
         VALUES (?1, ?2)
         ON CONFLICT(plan_id, file_path) DO UPDATE SET
             edit_count = edit_count + 1,
             last_edited = datetime('now')",
        rusqlite::params![plan_id, file_path],
    )?;

    // Transition from created → in_progress
    conn.execute(
        "UPDATE plans SET status = 'in_progress', updated_at = datetime('now')
         WHERE id = ?1 AND status = 'created'",
        [plan_id],
    )?;

    Ok(())
}

/// Get the most recent active plan (created or in_progress).
pub fn active_plan(db: &Db) -> Result<Option<PlanInfo>> {
    let conn = db.conn();

    let row: Option<(i64, String, String, Option<String>, String, String, Option<String>, String, String)> = conn
        .query_row(
            "SELECT id, session_id, plan_file_path, title, content, status, target_files, created_at, updated_at
             FROM plans
             WHERE status IN ('created', 'in_progress')
             ORDER BY updated_at DESC
             LIMIT 1",
            [],
            |row| Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
            )),
        )
        .ok();

    let Some((id, session_id, plan_file_path, title, content, status, target_files_json, created_at, updated_at)) = row else {
        return Ok(None);
    };

    let target_files: Vec<String> = target_files_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    // Get progress entries
    let progress = {
        let mut stmt = conn.prepare(
            "SELECT file_path, edit_count FROM plan_progress WHERE plan_id = ?1 ORDER BY last_edited DESC",
        )?;
        let rows = stmt.query_map([id], |row| Ok(ProgressEntry {
            file_path: row.get(0)?,
            edit_count: row.get(1)?,
        }))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    Ok(Some(PlanInfo {
        id,
        session_id,
        plan_file_path,
        title,
        content,
        status,
        target_files,
        created_at,
        updated_at,
        progress,
    }))
}

/// Evaluate plan completion at session end.
/// Heuristic: ≥60% of target files touched, or 3+ edits if no targets parsed.
pub fn evaluate_completion(db: &Db, session_id: &str) -> Result<()> {
    let conn = db.conn();

    // Find active plans for this session
    let plans: Vec<(i64, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, target_files FROM plans
             WHERE session_id = ?1 AND status IN ('created', 'in_progress')",
        )?;
        let rows = stmt.query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    for (plan_id, target_files_json) in plans {
        let target_files: Vec<String> = target_files_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        let progress_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM plan_progress WHERE plan_id = ?1",
            [plan_id],
            |row| row.get(0),
        )?;

        let completed = if target_files.is_empty() {
            // No targets parsed — use edit count heuristic
            progress_count >= 3
        } else {
            // Count how many target files were touched
            let touched: i64 = conn.query_row(
                "SELECT COUNT(DISTINCT pp.file_path) FROM plan_progress pp
                 WHERE pp.plan_id = ?1
                   AND EXISTS (
                       SELECT 1 FROM json_each(?2) je
                       WHERE pp.file_path LIKE '%' || je.value
                          OR je.value LIKE '%' || pp.file_path
                   )",
                rusqlite::params![plan_id, serde_json::to_string(&target_files)?],
                |row| row.get(0),
            )?;

            let ratio = touched as f64 / target_files.len() as f64;
            ratio >= 0.6
        };

        if completed {
            conn.execute(
                "UPDATE plans SET status = 'completed', completed_at = datetime('now'), updated_at = datetime('now')
                 WHERE id = ?1",
                [plan_id],
            )?;
        }
    }

    Ok(())
}

/// Mark old untouched plans as abandoned.
pub fn abandon_stale_plans(db: &Db, max_age_days: i64) -> Result<()> {
    let conn = db.conn();
    conn.execute(
        "UPDATE plans SET status = 'abandoned', updated_at = datetime('now')
         WHERE status IN ('created', 'in_progress')
           AND updated_at < datetime('now', ?1)",
        [format!("-{} days", max_age_days)],
    )?;
    Ok(())
}
