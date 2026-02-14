use anyhow::Result;
use rusqlite::params;
use serde_json::Value;

use crate::db::Db;

/// Ensure a session record exists, creating it if needed.
pub fn ensure_session(db: &Db, session_id: &str, project_dir: &str) -> Result<()> {
    let conn = db.conn();
    conn.execute(
        "INSERT OR IGNORE INTO sessions (id, project_dir) VALUES (?1, ?2)",
        params![session_id, project_dir],
    )?;
    Ok(())
}

/// Get the next turn number for a session.
fn next_turn_number(db: &Db, session_id: &str) -> Result<i64> {
    let conn = db.conn();
    let max: Option<i64> = conn.query_row(
        "SELECT MAX(turn_number) FROM turns WHERE session_id = ?1",
        params![session_id],
        |row| row.get(0),
    )?;
    Ok(max.unwrap_or(0) + 1)
}

/// Index a conversation turn.
pub fn index_turn(
    db: &Db,
    session_id: &str,
    role: &str,
    turn_type: &str,
    content: &str,
    metadata: Option<&Value>,
    files: &[(String, String)], // (file_path, action)
) -> Result<i64> {
    let turn_number = next_turn_number(db, session_id)?;
    let metadata_str = metadata.map(|m| m.to_string());
    let conn = db.conn();

    conn.execute(
        "INSERT INTO turns (session_id, turn_number, role, turn_type, content, metadata)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![session_id, turn_number, role, turn_type, content, metadata_str],
    )?;

    let turn_id = conn.last_insert_rowid();

    // Insert file references
    for (path, action) in files {
        conn.execute(
            "INSERT OR IGNORE INTO turn_files (turn_id, file_path, action)
             VALUES (?1, ?2, ?3)",
            params![turn_id, path, action],
        )?;
    }

    Ok(turn_id)
}

/// Mark a session as ended.
pub fn end_session(db: &Db, session_id: &str, summary: Option<&str>) -> Result<()> {
    let conn = db.conn();
    conn.execute(
        "UPDATE sessions SET ended_at = datetime('now'), summary = ?2 WHERE id = ?1",
        params![session_id, summary],
    )?;
    Ok(())
}

/// Get the total number of turns in a session.
#[allow(dead_code)]
pub fn session_turn_count(db: &Db, session_id: &str) -> Result<i64> {
    let conn = db.conn();
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM turns WHERE session_id = ?1",
        params![session_id],
        |row| row.get(0),
    )?;
    Ok(count)
}
