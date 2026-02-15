mod db;
mod hooks;
mod indexer;
mod inject;
mod llm;
mod server;
mod treesitter;
mod watcher;

use anyhow::Result;
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;

#[derive(Parser)]
#[command(name = "claude-rlm", about = "Persistent project memory for Claude Code", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP server (default mode)
    Serve,

    /// Index a user prompt (UserPromptSubmit hook)
    IndexPrompt,

    /// Index a code edit (PostToolUse Edit/Write hook)
    IndexEdit,

    /// Index a file read (PostToolUse Read hook)
    IndexRead,

    /// Index a bash command (PostToolUse Bash hook)
    IndexBash,

    /// Handle pre-compaction (PreCompact hook)
    PreCompact,

    /// Handle session start (SessionStart hook)
    SessionStart,

    /// Handle session end (SessionEnd hook)
    SessionEnd {
        /// Trigger knowledge distillation
        #[arg(long)]
        distill: bool,
    },

    /// Show index status and statistics
    Status,

    /// Disable all hooks (emergency kill switch)
    Disable,

    /// Re-enable hooks after disable
    Enable,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Serve) => run_server().await,
        Some(Commands::IndexPrompt) => run_hook(|| {
            let input = hooks::read_hook_input()?;
            hooks::prompt::handle(&input)
        }),
        Some(Commands::IndexEdit) => run_hook(|| {
            let input = hooks::read_hook_input()?;
            hooks::tool_use::handle_edit(&input)
        }),
        Some(Commands::IndexRead) => run_hook(|| {
            let input = hooks::read_hook_input()?;
            hooks::tool_use::handle_read(&input)
        }),
        Some(Commands::IndexBash) => run_hook(|| {
            let input = hooks::read_hook_input()?;
            hooks::tool_use::handle_bash(&input)
        }),
        Some(Commands::PreCompact) => run_hook(|| {
            let input = hooks::read_hook_input()?;
            hooks::compact::handle(&input)
        }),
        Some(Commands::SessionStart) => run_hook(|| {
            let input = hooks::read_hook_input()?;
            hooks::session::handle_start(&input)
        }),
        Some(Commands::SessionEnd { distill: _ }) => run_hook(|| {
            let input = hooks::read_hook_input()?;
            hooks::session::handle_end(&input)
        }),
        Some(Commands::Status) => run_status(),
        Some(Commands::Disable) => run_disable(),
        Some(Commands::Enable) => run_enable(),
    }
}

/// Show index status and statistics.
fn run_status() -> Result<()> {
    let project_dir = std::env::current_dir()?;
    let db = db::Db::open(&project_dir)?;
    let conn = db.conn();

    let symbol_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
    let file_count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT file_path) FROM symbols",
        [],
        |row| row.get(0),
    )?;
    let turn_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM turns", [], |row| row.get(0))?;
    let session_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
    let knowledge_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM knowledge", [], |row| row.get(0))?;

    println!("ClaudeRLM Status");
    println!("=================");
    if is_disabled() {
        println!("State:     DISABLED (run `claude-rlm enable` to re-enable)");
    } else {
        println!("State:     enabled");
    }
    println!("Sessions:  {}", session_count);
    println!("Turns:     {}", turn_count);
    println!("Knowledge: {}", knowledge_count);
    println!("Symbols:   {} (across {} files)", symbol_count, file_count);

    // Show symbol breakdown by kind
    let mut stmt = conn.prepare(
        "SELECT kind, COUNT(*) FROM symbols GROUP BY kind ORDER BY COUNT(*) DESC",
    )?;
    let kinds: Vec<(String, i64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    if !kinds.is_empty() {
        println!("\nSymbols by kind:");
        for (kind, count) in &kinds {
            println!("  {}: {}", kind, count);
        }
    }

    // Show top 10 symbols
    let mut stmt = conn.prepare(
        "SELECT file_path, name, kind, start_line, signature FROM symbols
         ORDER BY file_path, start_line LIMIT 20",
    )?;
    let syms: Vec<String> = stmt
        .query_map([], |row| {
            let sig: Option<String> = row.get(4)?;
            let sig_str = sig.map(|s| {
                let s = s.replace('\n', " ");
                if s.len() > 60 { let e = s.floor_char_boundary(60); format!("{}...", &s[..e]) } else { s }
            }).unwrap_or_default();
            Ok(format!(
                "  {} `{}` at {}:{} {}",
                row.get::<_, String>(2)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(0)?,
                row.get::<_, i64>(3)?,
                sig_str,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();
    if !syms.is_empty() {
        println!("\nSample symbols:");
        for s in &syms {
            println!("{}", s);
        }
    }

    // Show distilled knowledge
    let mut stmt = conn.prepare(
        "SELECT category, subject, content, confidence FROM knowledge
         WHERE superseded_by IS NULL AND confidence > 0.3
         ORDER BY confidence DESC, created_at DESC
         LIMIT 20",
    )?;
    let knowledge: Vec<String> = stmt
        .query_map([], |row| {
            Ok(format!(
                "  [{}] {} ({:.0}%): {}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(3)? * 100.0,
                {
                    let c: String = row.get(2)?;
                    if c.len() > 80 { let e = c.floor_char_boundary(80); format!("{}...", &c[..e]) } else { c }
                },
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();
    if !knowledge.is_empty() {
        println!("\nDistilled knowledge:");
        for k in &knowledge {
            println!("{}", k);
        }
    }

    Ok(())
}

/// Run the MCP server over stdio.
async fn run_server() -> Result<()> {
    // Log to stderr to keep stdout clean for MCP protocol
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("claude_rlm=info".parse()?),
        )
        .init();

    tracing::info!("Starting ClaudeRLM MCP server");

    // Open database in current project directory
    let project_dir = std::env::current_dir()?;
    let db = db::Db::open(&project_dir)?;

    // Recover any tasks stuck in 'running' from a previous crash
    match db::tasks::recover_stuck_tasks(&db) {
        Ok(0) => {}
        Ok(n) => tracing::info!("Recovered {} stuck background tasks", n),
        Err(e) => tracing::warn!("Failed to recover stuck tasks: {}", e),
    }

    // Run initial code indexing if needed
    if !indexer::code::has_index(&db)? {
        tracing::info!("No code index found, running initial scan...");
        match indexer::code::index_project(&db, &project_dir) {
            Ok(stats) => tracing::info!(
                "Initial indexing: {} files, {} symbols",
                stats.files_indexed,
                stats.symbols_found
            ),
            Err(e) => tracing::warn!("Initial indexing failed: {}", e),
        }
    }

    // Start background file watcher
    let _watcher = match watcher::start_watcher(db.clone(), project_dir.clone()) {
        Ok(w) => {
            tracing::info!("Background file watcher started");
            Some(w)
        }
        Err(e) => {
            tracing::warn!("Failed to start file watcher: {}", e);
            None
        }
    };

    // Start background task poller
    tokio::spawn(run_task_poller(db.clone(), project_dir));

    let server = server::ClaudeRlmServer::new(db);

    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| {
            eprintln!("Error starting ClaudeRLM server: {}", e);
        })?;

    // Wait for the MCP service to finish.
    match service.waiting().await {
        Ok(reason) => tracing::info!("MCP service stopped: {:?}", reason),
        Err(e) => tracing::info!("MCP service stopped with join error: {}", e),
    }

    // Force-exit immediately. Tokio runtime shutdown can hang waiting for
    // spawn_blocking tasks (file watcher, task poller). Claude Code kills
    // MCP servers that don't exit promptly and reports them as failed.
    std::process::exit(0);
}

/// Poll for background tasks and execute them.
async fn run_task_poller(db: db::Db, project_dir: std::path::PathBuf) {
    use tokio::time::{interval, Duration};

    let mut poll_interval = interval(Duration::from_millis(300));
    let mut prune_counter: u32 = 0;

    loop {
        poll_interval.tick().await;

        // Prune old completed/failed tasks roughly every 30s (300ms * 100)
        prune_counter += 1;
        if prune_counter >= 100 {
            prune_counter = 0;
            let db2 = db.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(e) = db::tasks::prune_old_tasks(&db2, 3600) {
                    tracing::warn!("Failed to prune old tasks: {}", e);
                }
            })
            .await;
        }

        // Try to claim and execute a task
        let db2 = db.clone();
        let project_dir2 = project_dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            let task = match db::tasks::claim_next_task(&db2) {
                Ok(Some(t)) => t,
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!("Failed to claim task: {}", e);
                    return;
                }
            };

            tracing::info!(
                "Executing background task #{}: {} (project: {})",
                task.id,
                task.task_type,
                task.project_dir
            );

            match task.task_type.as_str() {
                "reindex_stale" => execute_reindex_stale(&db2, &task, &project_dir2),
                other => {
                    let msg = format!("Unknown task type: {}", other);
                    tracing::warn!("{}", msg);
                    let _ = db::tasks::fail_task(&db2, task.id, &msg);
                }
            }
        })
        .await;

        if let Err(e) = result {
            tracing::warn!("Task executor panicked: {}", e);
        }
    }
}

/// Execute a `reindex_stale` background task.
fn execute_reindex_stale(db: &db::Db, task: &db::tasks::BackgroundTask, project_dir: &std::path::Path) {
    // Use the project_dir from the task if it differs (future-proofing),
    // but fall back to the server's project_dir for the DB connection.
    let target_dir = std::path::Path::new(&task.project_dir);
    let scan_dir = if target_dir.exists() { target_dir } else { project_dir };

    match indexer::code::stale_files(db, scan_dir) {
        Ok(stale) => {
            if stale.is_empty() {
                tracing::info!("Task #{}: no stale files to reindex", task.id);
                let _ = db::tasks::complete_task(db, task.id);
                return;
            }

            tracing::info!("Task #{}: reindexing {} stale files", task.id, stale.len());
            let mut reindexed = 0usize;
            let mut failed = 0usize;

            for path in &stale {
                if path.exists() {
                    match indexer::code::reindex_file(db, path) {
                        Ok(_) => reindexed += 1,
                        Err(e) => {
                            tracing::warn!("Task #{}: failed to reindex {}: {}", task.id, path.display(), e);
                            failed += 1;
                        }
                    }
                } else {
                    // File deleted — remove its symbols
                    let conn = db.conn();
                    let _ = conn.execute(
                        "DELETE FROM symbols WHERE file_path = ?1",
                        rusqlite::params![path.to_string_lossy().as_ref()],
                    );
                    reindexed += 1;
                }
            }

            tracing::info!(
                "Task #{}: reindex complete ({} ok, {} failed)",
                task.id,
                reindexed,
                failed
            );
            let _ = db::tasks::complete_task(db, task.id);
        }
        Err(e) => {
            let msg = format!("Failed to find stale files: {}", e);
            tracing::warn!("Task #{}: {}", task.id, msg);
            let _ = db::tasks::fail_task(db, task.id, &msg);
        }
    }
}

/// Path to the disable flag file.
fn disable_flag_path() -> std::path::PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    std::path::Path::new(&home).join(".claude-rlm-disabled")
}

/// Check if claude-rlm is disabled.
fn is_disabled() -> bool {
    disable_flag_path().exists()
}

/// Disable all hooks.
fn run_disable() -> Result<()> {
    let path = disable_flag_path();
    std::fs::write(&path, "disabled\n")?;
    eprintln!("claude-rlm disabled. All hooks will be skipped.");
    eprintln!("Run `claude-rlm enable` to re-enable.");
    Ok(())
}

/// Re-enable hooks.
fn run_enable() -> Result<()> {
    let path = disable_flag_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
        eprintln!("claude-rlm enabled. Hooks are active again.");
    } else {
        eprintln!("claude-rlm is already enabled.");
    }
    Ok(())
}

/// Run a hook handler, catching errors and panics.
fn run_hook(f: impl FnOnce() -> Result<()>) -> Result<()> {
    if is_disabled() {
        return Ok(());
    }

    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            eprintln!("[claude-rlm] Hook error: {}", e);
            // Don't fail the hook — return Ok so Claude Code continues
            Ok(())
        }
        Err(panic) => {
            let msg = panic
                .downcast_ref::<String>()
                .map(|s| s.as_str())
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic");
            eprintln!("[claude-rlm] Hook panicked: {}", msg);
            Ok(())
        }
    }
}
