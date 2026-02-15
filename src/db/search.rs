use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

/// Sanitize a user query for SQLite FTS5.
///
/// FTS5 treats characters like `-`, `*`, `OR`, `AND`, `NOT` as operators.
/// We quote each token with double quotes so they're treated as literals,
/// then join with spaces (implicit AND).
fn sanitize_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|token| {
            // Strip any existing quotes, then wrap in double quotes
            let clean = token.replace('"', "");
            format!("\"{}\"", clean)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A search result from the turns FTS index.
#[derive(Debug, Serialize)]
pub struct TurnSearchResult {
    pub turn_id: i64,
    pub session_id: String,
    pub turn_number: i64,
    pub timestamp: String,
    pub role: String,
    pub turn_type: String,
    pub content: String,
    pub content_summary: Option<String>,
    pub rank: f64,
    pub files: Vec<String>,
}

/// A search result from the knowledge FTS index.
#[derive(Debug, Serialize)]
pub struct KnowledgeSearchResult {
    pub id: i64,
    pub category: String,
    pub subject: String,
    pub content: String,
    pub confidence: f64,
    pub created_at: String,
    pub rank: f64,
}

/// Search conversation turns using FTS5 with BM25 ranking.
pub fn search_turns(
    conn: &Connection,
    query: &str,
    limit: usize,
    session_id: Option<&str>,
    turn_type: Option<&str>,
) -> Result<Vec<TurnSearchResult>> {
    let mut sql = String::from(
        "SELECT t.id, t.session_id, t.turn_number, t.timestamp,
                t.role, t.turn_type, t.content, t.content_summary,
                fts.rank
         FROM turns_fts fts
         JOIN turns t ON t.id = fts.rowid
         WHERE turns_fts MATCH ?1",
    );

    if session_id.is_some() {
        sql.push_str(" AND t.session_id = ?2");
    }
    if turn_type.is_some() {
        sql.push_str(if session_id.is_some() {
            " AND t.turn_type = ?3"
        } else {
            " AND t.turn_type = ?2"
        });
    }

    sql.push_str(" ORDER BY fts.rank LIMIT ?");
    // Append limit param number
    let limit_param = 2 + session_id.is_some() as u8 + turn_type.is_some() as u8;
    sql = sql.replace(
        " LIMIT ?",
        &format!(" LIMIT ?{}", limit_param),
    );

    let mut stmt = conn.prepare(&sql)?;

    // Build params dynamically
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    params.push(Box::new(sanitize_fts_query(query)));
    if let Some(sid) = session_id {
        params.push(Box::new(sid.to_string()));
    }
    if let Some(tt) = turn_type {
        params.push(Box::new(tt.to_string()));
    }
    params.push(Box::new(limit as i64));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(TurnSearchResult {
            turn_id: row.get(0)?,
            session_id: row.get(1)?,
            turn_number: row.get(2)?,
            timestamp: row.get(3)?,
            role: row.get(4)?,
            turn_type: row.get(5)?,
            content: row.get(6)?,
            content_summary: row.get(7)?,
            rank: row.get(8)?,
            files: Vec::new(), // populated below
        })
    })?;

    let mut results: Vec<TurnSearchResult> = Vec::new();
    for row in rows {
        results.push(row?);
    }

    // Fetch associated files for each result
    if !results.is_empty() {
        let mut file_stmt =
            conn.prepare("SELECT file_path FROM turn_files WHERE turn_id = ?1")?;
        for result in &mut results {
            let files = file_stmt.query_map([result.turn_id], |row| row.get(0))?;
            for f in files {
                result.files.push(f?);
            }
        }
    }

    Ok(results)
}

/// Search knowledge entries using FTS5.
pub fn search_knowledge(
    conn: &Connection,
    query: &str,
    limit: usize,
    category: Option<&str>,
) -> Result<Vec<KnowledgeSearchResult>> {
    let sql = if category.is_some() {
        "SELECT k.id, k.category, k.subject, k.content, k.confidence, k.created_at,
                fts.rank
         FROM knowledge_fts fts
         JOIN knowledge k ON k.id = fts.rowid
         WHERE knowledge_fts MATCH ?1
           AND k.category = ?2
           AND k.superseded_by IS NULL
         ORDER BY fts.rank
         LIMIT ?3"
    } else {
        "SELECT k.id, k.category, k.subject, k.content, k.confidence, k.created_at,
                fts.rank
         FROM knowledge_fts fts
         JOIN knowledge k ON k.id = fts.rowid
         WHERE knowledge_fts MATCH ?1
           AND k.superseded_by IS NULL
         ORDER BY fts.rank
         LIMIT ?2"
    };

    let mut stmt = conn.prepare(sql)?;

    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    params.push(Box::new(sanitize_fts_query(query)));
    if let Some(cat) = category {
        params.push(Box::new(cat.to_string()));
    }
    params.push(Box::new(limit as i64));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(KnowledgeSearchResult {
            id: row.get(0)?,
            category: row.get(1)?,
            subject: row.get(2)?,
            content: row.get(3)?,
            confidence: row.get(4)?,
            created_at: row.get(5)?,
            rank: row.get(6)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get recent session summaries.
pub fn recent_sessions(conn: &Connection, limit: usize) -> Result<Vec<SessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_dir, started_at, ended_at, summary
         FROM sessions
         ORDER BY started_at DESC
         LIMIT ?1",
    )?;

    let rows = stmt.query_map([limit as i64], |row| {
        Ok(SessionSummary {
            id: row.get(0)?,
            project_dir: row.get(1)?,
            started_at: row.get(2)?,
            ended_at: row.get(3)?,
            summary: row.get(4)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub project_dir: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub summary: Option<String>,
}

/// Retrieve all turns from a session, ordered by turn number.
/// Used for ranked retrieval during compaction injection.
pub fn session_turns(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<TurnSearchResult>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.session_id, t.turn_number, t.timestamp,
                t.role, t.turn_type, t.content, t.content_summary, 0.0 as rank
         FROM turns t
         WHERE t.session_id = ?1
         ORDER BY t.turn_number ASC",
    )?;

    let rows = stmt.query_map([session_id], |row| {
        Ok(TurnSearchResult {
            turn_id: row.get(0)?,
            session_id: row.get(1)?,
            turn_number: row.get(2)?,
            timestamp: row.get(3)?,
            role: row.get(4)?,
            turn_type: row.get(5)?,
            content: row.get(6)?,
            content_summary: row.get(7)?,
            rank: row.get(8)?,
            files: Vec::new(),
        })
    })?;

    let mut results: Vec<TurnSearchResult> = Vec::new();
    for row in rows {
        results.push(row?);
    }

    // Populate files
    if !results.is_empty() {
        let mut file_stmt =
            conn.prepare("SELECT file_path FROM turn_files WHERE turn_id = ?1")?;
        for result in &mut results {
            let files = file_stmt.query_map([result.turn_id], |row| row.get(0))?;
            for f in files {
                result.files.push(f?);
            }
        }
    }

    Ok(results)
}

/// A symbol entry in the codebase map.
#[derive(Debug)]
pub struct SymbolMapEntry {
    pub name: String,
    pub kind: String,
}

/// A file entry in the codebase map, with its symbols.
#[derive(Debug)]
pub struct FileMapEntry {
    pub file_path: String,
    pub symbols: Vec<SymbolMapEntry>,
    pub truncated: bool,
    pub score: u32,
}

/// Build a codebase map: symbols grouped by file, ranked by importance.
///
/// Filters out imports and variables, skips redundant `impl Foo` when
/// `struct Foo` exists in the same file, and caps symbols per file.
pub fn codebase_map(conn: &Connection) -> Result<Vec<FileMapEntry>> {
    let mut stmt = conn.prepare(
        "SELECT file_path, name, kind
         FROM symbols
         WHERE kind NOT IN ('import', 'variable')
         ORDER BY file_path, start_line",
    )?;

    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if rows.is_empty() {
        return Ok(Vec::new());
    }

    // Group by file
    let mut files: Vec<FileMapEntry> = Vec::new();
    let mut current_path = String::new();
    let mut current_symbols: Vec<SymbolMapEntry> = Vec::new();

    for (path, name, kind) in &rows {
        if path != &current_path {
            if !current_path.is_empty() {
                files.push(finish_file_entry(
                    std::mem::take(&mut current_path),
                    std::mem::take(&mut current_symbols),
                ));
            }
            current_path = path.clone();
        }
        current_symbols.push(SymbolMapEntry {
            name: name.clone(),
            kind: kind.clone(),
        });
    }
    if !current_path.is_empty() {
        files.push(finish_file_entry(current_path, current_symbols));
    }

    // Sort by score descending (most important files first)
    files.sort_by(|a, b| b.score.cmp(&a.score));

    Ok(files)
}

/// Score a file and deduplicate/cap its symbols.
fn finish_file_entry(file_path: String, mut symbols: Vec<SymbolMapEntry>) -> FileMapEntry {
    // Collect struct/enum/trait names to filter redundant impl blocks
    let type_names: std::collections::HashSet<String> = symbols
        .iter()
        .filter(|s| matches!(s.kind.as_str(), "struct" | "enum" | "trait"))
        .map(|s| s.name.clone())
        .collect();

    // Remove `impl Foo` when `struct/enum/trait Foo` exists
    symbols.retain(|s| !(s.kind == "impl" && type_names.contains(&s.name)));

    // Score: structs/traits/enums=3, functions=2, rest=1
    let score: u32 = symbols
        .iter()
        .map(|s| match s.kind.as_str() {
            "struct" | "trait" | "enum" => 3,
            "function" => 2,
            _ => 1,
        })
        .sum();

    const MAX_SYMBOLS: usize = 8;
    let truncated = symbols.len() > MAX_SYMBOLS;
    symbols.truncate(MAX_SYMBOLS);

    FileMapEntry {
        file_path,
        symbols,
        truncated,
        score,
    }
}

/// Get files actively being worked on in a session (edited/written, most recent first).
pub fn active_files(conn: &Connection, session_id: &str, limit: usize) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT tf.file_path
         FROM turn_files tf
         JOIN turns t ON t.id = tf.turn_id
         WHERE t.session_id = ?1 AND tf.action IN ('edit', 'write', 'create')
         ORDER BY t.turn_number DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![session_id, limit as i64], |row| {
        row.get(0)
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get a project structure summary: languages used, top-level directories with code, file counts.
pub fn project_structure(conn: &Connection) -> Result<ProjectStructure> {
    // Count symbols by kind
    let mut kind_stmt = conn.prepare(
        "SELECT kind, COUNT(*) FROM symbols GROUP BY kind ORDER BY COUNT(*) DESC",
    )?;
    let symbol_kinds: Vec<(String, i64)> = kind_stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    // Count files by extension (inferred from file_path)
    let mut file_stmt = conn.prepare(
        "SELECT DISTINCT file_path FROM symbols",
    )?;
    let file_paths: Vec<String> = file_stmt
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    let total_files = file_paths.len();
    let total_symbols: i64 = symbol_kinds.iter().map(|(_, c)| c).sum();

    // Count top-level directories
    let mut dir_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for path in &file_paths {
        // Get the first path component after any common prefix
        let parts: Vec<&str> = path.split(['/', '\\']).collect();
        if parts.len() >= 2 {
            // Find the "src" or first meaningful directory
            let dir = if let Some(pos) = parts.iter().position(|&p| p == "src") {
                if pos + 1 < parts.len() - 1 {
                    format!("src/{}", parts[pos + 1])
                } else {
                    "src".to_string()
                }
            } else {
                parts[parts.len().saturating_sub(2)].to_string()
            };
            *dir_counts.entry(dir).or_default() += 1;
        }
    }

    let mut directories: Vec<(String, usize)> = dir_counts.into_iter().collect();
    directories.sort_by(|a, b| b.1.cmp(&a.1));

    Ok(ProjectStructure {
        total_files,
        total_symbols: total_symbols as usize,
        symbol_kinds,
        directories,
    })
}

#[derive(Debug, Serialize)]
pub struct ProjectStructure {
    pub total_files: usize,
    pub total_symbols: usize,
    pub symbol_kinds: Vec<(String, i64)>,
    pub directories: Vec<(String, usize)>,
}

/// Get the history of changes to a specific file.
pub fn file_history(
    conn: &Connection,
    file_path: &str,
    limit: usize,
) -> Result<Vec<TurnSearchResult>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.session_id, t.turn_number, t.timestamp,
                t.role, t.turn_type, t.content, t.content_summary, 0.0 as rank
         FROM turns t
         JOIN turn_files tf ON tf.turn_id = t.id
         WHERE tf.file_path = ?1
         ORDER BY t.timestamp DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![file_path, limit as i64], |row| {
        Ok(TurnSearchResult {
            turn_id: row.get(0)?,
            session_id: row.get(1)?,
            turn_number: row.get(2)?,
            timestamp: row.get(3)?,
            role: row.get(4)?,
            turn_type: row.get(5)?,
            content: row.get(6)?,
            content_summary: row.get(7)?,
            rank: row.get(8)?,
            files: vec![file_path.to_string()],
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// A symbol matched by keyword search.
#[derive(Debug)]
pub struct SymbolMatch {
    pub file_path: String,
    pub name: String,
    pub kind: String,
    pub start_line: i64,
    pub end_line: i64,
    pub signature: Option<String>,
    pub parent_name: Option<String>,
    pub doc_comment: Option<String>,
}

/// Search symbols by keywords across name, signature, doc_comment, and file_path.
///
/// Each keyword generates OR conditions across all columns.
/// Multiple keywords are OR'd together (any keyword match counts).
pub fn search_symbols_by_keywords(
    conn: &Connection,
    keywords: &[String],
    limit: usize,
) -> Result<Vec<SymbolMatch>> {
    if keywords.is_empty() {
        return Ok(Vec::new());
    }

    let mut conditions = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_num = 1;

    for kw in keywords {
        let pattern = format!("%{}%", kw);
        conditions.push(format!(
            "(name LIKE ?{0} OR file_path LIKE ?{0} \
             OR COALESCE(signature, '') LIKE ?{0} \
             OR COALESCE(doc_comment, '') LIKE ?{0})",
            param_num
        ));
        params.push(Box::new(pattern));
        param_num += 1;
    }

    let sql = format!(
        "SELECT file_path, name, kind, start_line, end_line, \
                signature, parent_name, doc_comment \
         FROM symbols \
         WHERE kind NOT IN ('import', 'variable') \
           AND ({}) \
         ORDER BY file_path, start_line \
         LIMIT ?{}",
        conditions.join(" OR "),
        param_num,
    );

    params.push(Box::new(limit as i64));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(SymbolMatch {
            file_path: row.get(0)?,
            name: row.get(1)?,
            kind: row.get(2)?,
            start_line: row.get(3)?,
            end_line: row.get(4)?,
            signature: row.get(5)?,
            parent_name: row.get(6)?,
            doc_comment: row.get(7)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}
