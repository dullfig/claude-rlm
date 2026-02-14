use anyhow::Result;
use ignore::WalkBuilder;
use rusqlite::params;
use std::path::{Path, PathBuf};

use crate::db::Db;
use crate::treesitter::languages::Lang;
use crate::treesitter::symbols::{self, ExtractedSymbol};

/// Index all code files in a project directory.
/// Respects .gitignore and skips known non-code directories.
pub fn index_project(db: &Db, project_dir: &Path) -> Result<IndexStats> {
    let mut stats = IndexStats::default();

    let walker = WalkBuilder::new(project_dir)
        .hidden(true) // skip hidden files
        .git_ignore(true) // respect .gitignore
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            // Skip common non-code directories
            !matches!(
                name.as_ref(),
                "node_modules" | "target" | "dist" | "build" | ".git" | "__pycache__" | "vendor"
                    | ".venv" | "venv"
            )
        })
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }

        let path = entry.path();
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => ext,
            None => continue,
        };

        let lang = match Lang::from_extension(ext) {
            Some(l) => l,
            None => continue,
        };

        match index_file(db, path, lang) {
            Ok(count) => {
                stats.files_indexed += 1;
                stats.symbols_found += count;
            }
            Err(e) => {
                tracing::warn!("Failed to index {}: {}", path.display(), e);
                stats.files_failed += 1;
            }
        }
    }

    tracing::info!(
        "Indexed {} files, {} symbols ({} failed)",
        stats.files_indexed,
        stats.symbols_found,
        stats.files_failed
    );

    Ok(stats)
}

/// Index a single file, replacing any existing symbols for it.
pub fn index_file(db: &Db, file_path: &Path, lang: Lang) -> Result<usize> {
    let source = std::fs::read(file_path)?;
    let file_path_str = file_path.to_string_lossy();

    let extracted = symbols::extract_symbols(lang, &source)?;

    let conn = db.conn();

    // Delete existing symbols for this file
    conn.execute(
        "DELETE FROM symbols WHERE file_path = ?1",
        params![file_path_str.as_ref()],
    )?;

    // Delete existing refs from symbols in this file
    conn.execute(
        "DELETE FROM symbol_refs WHERE from_symbol_id IN
         (SELECT id FROM symbols WHERE file_path = ?1)",
        params![file_path_str.as_ref()],
    )?;

    // Insert new symbols
    for sym in &extracted {
        insert_symbol(&conn, &file_path_str, sym)?;
    }

    Ok(extracted.len())
}

/// Re-index a single file (used by hooks and file watcher).
pub fn reindex_file(db: &Db, file_path: &Path) -> Result<usize> {
    let ext = match file_path.extension().and_then(|e| e.to_str()) {
        Some(ext) => ext,
        None => return Ok(0),
    };

    let lang = match Lang::from_extension(ext) {
        Some(l) => l,
        None => return Ok(0),
    };

    index_file(db, file_path, lang)
}

/// Check if the project has been indexed (any symbols exist).
pub fn has_index(db: &Db) -> Result<bool> {
    let conn = db.conn();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
    Ok(count > 0)
}

fn insert_symbol(
    conn: &rusqlite::Connection,
    file_path: &str,
    sym: &ExtractedSymbol,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO symbols (file_path, name, kind, start_line, end_line, signature, doc_comment)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            file_path,
            sym.name,
            sym.kind,
            sym.start_line as i64,
            sym.end_line as i64,
            sym.signature,
            sym.doc_comment,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get files that have been modified since they were last indexed.
pub fn stale_files(db: &Db, project_dir: &Path) -> Result<Vec<PathBuf>> {
    let conn = db.conn();

    // Get all indexed files and their last_indexed timestamps
    let mut stmt = conn.prepare(
        "SELECT DISTINCT file_path, MAX(last_indexed) FROM symbols GROUP BY file_path",
    )?;

    let indexed: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut stale = Vec::new();

    for (file_path, last_indexed) in &indexed {
        let path = Path::new(file_path);
        if !path.exists() {
            // File was deleted — it's stale (should be removed)
            stale.push(path.to_path_buf());
            continue;
        }

        if let Ok(metadata) = std::fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                let modified_str = chrono::DateTime::<chrono::Utc>::from(modified)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string();
                if modified_str > *last_indexed {
                    stale.push(path.to_path_buf());
                }
            }
        }
    }

    // Also find new files that haven't been indexed at all
    let walker = WalkBuilder::new(project_dir)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                "node_modules" | "target" | "dist" | "build" | ".git" | "__pycache__" | "vendor"
                    | ".venv" | "venv"
            )
        })
        .build();

    let indexed_paths: std::collections::HashSet<&str> =
        indexed.iter().map(|(p, _)| p.as_str()).collect();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }

        let path = entry.path();
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => ext,
            None => continue,
        };

        if Lang::from_extension(ext).is_none() {
            continue;
        }

        let path_str = path.to_string_lossy();
        if !indexed_paths.contains(path_str.as_ref()) {
            stale.push(path.to_path_buf());
        }
    }

    Ok(stale)
}

#[derive(Debug, Default)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub symbols_found: usize,
    pub files_failed: usize,
}
