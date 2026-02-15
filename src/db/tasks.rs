use anyhow::Result;
use rusqlite::params;

use super::Db;

/// A background task row.
pub struct BackgroundTask {
    pub id: i64,
    pub task_type: String,
    pub project_dir: String,
    pub payload: Option<String>,
}

/// Enqueue a new background task.
pub fn enqueue_task(db: &Db, task_type: &str, project_dir: &str, payload: Option<&str>) -> Result<i64> {
    let conn = db.conn();
    conn.execute(
        "INSERT INTO background_tasks (task_type, project_dir, payload) VALUES (?1, ?2, ?3)",
        params![task_type, project_dir, payload],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Atomically claim the next pending task.
/// Returns `None` if no tasks are pending.
pub fn claim_next_task(db: &Db) -> Result<Option<BackgroundTask>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "UPDATE background_tasks
         SET status = 'running', started_at = datetime('now')
         WHERE id = (
             SELECT id FROM background_tasks
             WHERE status = 'pending'
             ORDER BY id ASC
             LIMIT 1
         )
         RETURNING id, task_type, project_dir, payload",
    )?;

    let mut rows = stmt.query([])?;
    match rows.next()? {
        Some(row) => Ok(Some(BackgroundTask {
            id: row.get(0)?,
            task_type: row.get(1)?,
            project_dir: row.get(2)?,
            payload: row.get(3)?,
        })),
        None => Ok(None),
    }
}

/// Mark a task as completed.
pub fn complete_task(db: &Db, task_id: i64) -> Result<()> {
    let conn = db.conn();
    conn.execute(
        "UPDATE background_tasks SET status = 'completed', completed_at = datetime('now') WHERE id = ?1",
        params![task_id],
    )?;
    Ok(())
}

/// Mark a task as failed with an error message.
pub fn fail_task(db: &Db, task_id: i64, error: &str) -> Result<()> {
    let conn = db.conn();
    conn.execute(
        "UPDATE background_tasks SET status = 'failed', completed_at = datetime('now'), error = ?2 WHERE id = ?1",
        params![task_id, error],
    )?;
    Ok(())
}

/// Recover tasks stuck in 'running' state (e.g. from a crash).
/// Resets them back to 'pending' so they'll be retried.
pub fn recover_stuck_tasks(db: &Db) -> Result<u64> {
    let conn = db.conn();
    let count = conn.execute(
        "UPDATE background_tasks SET status = 'pending', started_at = NULL
         WHERE status = 'running'",
        [],
    )?;
    Ok(count as u64)
}

/// Delete completed/failed tasks older than the given number of seconds.
pub fn prune_old_tasks(db: &Db, max_age_secs: u64) -> Result<u64> {
    let conn = db.conn();
    let count = conn.execute(
        "DELETE FROM background_tasks
         WHERE status IN ('completed', 'failed')
         AND completed_at < datetime('now', ?1)",
        params![format!("-{} seconds", max_age_secs)],
    )?;
    Ok(count as u64)
}
