use anyhow::Result;
use rusqlite::Connection;

/// Create all tables and FTS indexes.
pub fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        -- Sessions
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            project_dir TEXT NOT NULL,
            started_at TEXT DEFAULT (datetime('now')),
            ended_at TEXT,
            summary TEXT
        );

        -- Conversation turns (core index)
        CREATE TABLE IF NOT EXISTS turns (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(id),
            turn_number INTEGER NOT NULL,
            timestamp TEXT DEFAULT (datetime('now')),
            role TEXT NOT NULL,
            turn_type TEXT NOT NULL,
            content TEXT NOT NULL,
            content_summary TEXT,
            metadata TEXT
        );

        -- FTS5 for turns
        CREATE VIRTUAL TABLE IF NOT EXISTS turns_fts USING fts5(
            content, content_summary, tokenize='porter unicode61'
        );

        -- Triggers to keep FTS in sync
        CREATE TRIGGER IF NOT EXISTS turns_ai AFTER INSERT ON turns BEGIN
            INSERT INTO turns_fts(rowid, content, content_summary)
            VALUES (new.id, new.content, COALESCE(new.content_summary, ''));
        END;

        CREATE TRIGGER IF NOT EXISTS turns_au AFTER UPDATE OF content, content_summary ON turns BEGIN
            UPDATE turns_fts SET
                content = new.content,
                content_summary = COALESCE(new.content_summary, '')
            WHERE rowid = new.id;
        END;

        CREATE TRIGGER IF NOT EXISTS turns_ad AFTER DELETE ON turns BEGIN
            DELETE FROM turns_fts WHERE rowid = old.id;
        END;

        -- Files referenced in turns
        CREATE TABLE IF NOT EXISTS turn_files (
            turn_id INTEGER NOT NULL REFERENCES turns(id),
            file_path TEXT NOT NULL,
            action TEXT NOT NULL,
            PRIMARY KEY (turn_id, file_path)
        );

        CREATE INDEX IF NOT EXISTS idx_turn_files_path ON turn_files(file_path);

        -- Code symbols (tree-sitter, Phase 2)
        CREATE TABLE IF NOT EXISTS symbols (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_path TEXT NOT NULL,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            parent_id INTEGER REFERENCES symbols(id),
            signature TEXT,
            doc_comment TEXT,
            last_indexed TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_path);
        CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
        CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind);

        -- Symbol cross-references (Phase 2)
        CREATE TABLE IF NOT EXISTS symbol_refs (
            from_symbol_id INTEGER NOT NULL REFERENCES symbols(id),
            to_symbol_id INTEGER NOT NULL REFERENCES symbols(id),
            ref_type TEXT NOT NULL,
            PRIMARY KEY (from_symbol_id, to_symbol_id, ref_type)
        );

        -- Git state tracking (for session-start catch-up)
        CREATE TABLE IF NOT EXISTS git_state (
            project_dir TEXT PRIMARY KEY,
            last_commit_hash TEXT NOT NULL,
            updated_at TEXT DEFAULT (datetime('now'))
        );

        -- File content hashes (for non-git catch-up)
        CREATE TABLE IF NOT EXISTS file_hashes (
            project_dir TEXT NOT NULL,
            file_path TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            updated_at TEXT DEFAULT (datetime('now')),
            PRIMARY KEY (project_dir, file_path)
        );

        -- Distilled knowledge (Phase 4, but create table now)
        CREATE TABLE IF NOT EXISTS knowledge (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT REFERENCES sessions(id),
            category TEXT NOT NULL,
            subject TEXT NOT NULL,
            content TEXT NOT NULL,
            confidence REAL DEFAULT 1.0,
            created_at TEXT DEFAULT (datetime('now')),
            last_confirmed TEXT,
            superseded_by INTEGER REFERENCES knowledge(id)
        );

        -- FTS5 for knowledge
        CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_fts USING fts5(
            subject, content, tokenize='porter unicode61'
        );

        CREATE TRIGGER IF NOT EXISTS knowledge_ai AFTER INSERT ON knowledge BEGIN
            INSERT INTO knowledge_fts(rowid, subject, content)
            VALUES (new.id, new.subject, new.content);
        END;

        CREATE TRIGGER IF NOT EXISTS knowledge_au AFTER UPDATE OF subject, content ON knowledge BEGIN
            UPDATE knowledge_fts SET subject = new.subject, content = new.content
            WHERE rowid = new.id;
        END;

        CREATE TRIGGER IF NOT EXISTS knowledge_ad AFTER DELETE ON knowledge BEGIN
            DELETE FROM knowledge_fts WHERE rowid = old.id;
        END;

        -- Background task queue (cross-process via SQLite)
        CREATE TABLE IF NOT EXISTS background_tasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            project_dir TEXT NOT NULL,
            payload TEXT,
            created_at TEXT DEFAULT (datetime('now')),
            started_at TEXT,
            completed_at TEXT,
            error TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_bg_tasks_status ON background_tasks(status);
        ",
    )?;
    Ok(())
}
