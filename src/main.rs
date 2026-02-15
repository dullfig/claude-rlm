#![allow(dead_code)]

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

    /// Handle pre-tool-use (PreToolUse hook)
    PreToolUse,

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

    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Set a configuration value
    Set {
        /// The key to set (api-key, model, provider, base-url)
        key: String,
        /// The value to set
        value: String,
    },
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
        Some(Commands::PreToolUse) => run_hook(|| {
            let input = hooks::read_hook_input()?;
            hooks::pre_tool_use::handle(&input)
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
        Some(Commands::Config { action }) => run_config(action),
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

    // Recent hook activity (last 24h)
    let mut stmt = conn.prepare(
        "SELECT hook_event, COUNT(*) FROM hook_log
         WHERE created_at > datetime('now', '-1 day')
         GROUP BY hook_event
         ORDER BY COUNT(*) DESC",
    )?;
    let hook_counts: Vec<(String, i64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    if !hook_counts.is_empty() {
        println!("\nRecent hooks (last 24h):");
        for (event, count) in &hook_counts {
            println!("  {:<20} {} invocations", event, count);
        }
    } else {
        println!("\nRecent hooks (last 24h): none");
    }

    Ok(())
}

/// Ensure the installed hooks.json contains this binary's hooks.
///
/// When deployed as a plugin, the hooks.json in the plugin cache can get
/// out of sync with the binary (e.g., new hook types added). This merges
/// our hooks into the installed file, preserving entries from other plugins.
/// Takes effect on the next session (Claude Code reads hooks at init).
fn ensure_hooks_synced() {
    let plugin_root = match std::env::var("CLAUDE_PLUGIN_ROOT").ok() {
        Some(p) if p != "." && !p.is_empty() => std::path::PathBuf::from(p),
        _ => return, // Dev mode
    };

    const CANONICAL: &str = include_str!("../hooks/hooks.json");

    let ours: serde_json::Value = match serde_json::from_str(CANONICAL) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Embedded hooks.json is invalid: {}", e);
            return;
        }
    };

    let hooks_path = plugin_root.join("hooks").join("hooks.json");

    // Load existing file, or start with an empty hooks object
    let mut installed: serde_json::Value =
        std::fs::read_to_string(&hooks_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({"hooks": {}}));

    let our_hooks = match ours.get("hooks").and_then(|h| h.as_object()) {
        Some(h) => h,
        None => return,
    };
    let installed_hooks = installed
        .as_object_mut()
        .and_then(|o| {
            o.entry("hooks")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
        });
    let installed_hooks = match installed_hooks {
        Some(h) => h,
        None => return,
    };

    let mut changed = false;

    for (hook_type, our_entries) in our_hooks {
        let our_arr = match our_entries.as_array() {
            Some(a) => a,
            None => continue,
        };

        let existing = installed_hooks
            .entry(hook_type)
            .or_insert_with(|| serde_json::json!([]));
        let existing_arr = match existing.as_array_mut() {
            Some(a) => a,
            None => continue,
        };

        // Remove stale claude-rlm entries
        let before = existing_arr.len();
        existing_arr.retain(|entry| !is_claude_rlm_entry(entry));
        if existing_arr.len() != before {
            changed = true;
        }

        // Append our current entries
        for entry in our_arr {
            existing_arr.push(entry.clone());
            changed = true;
        }
    }

    if !changed {
        return;
    }

    if let Err(e) = std::fs::create_dir_all(hooks_path.parent().unwrap()) {
        tracing::warn!("Failed to create hooks dir: {}", e);
        return;
    }
    match serde_json::to_string_pretty(&installed) {
        Ok(json) => match std::fs::write(&hooks_path, json) {
            Ok(()) => tracing::info!("Merged hooks.json with binary's hooks"),
            Err(e) => tracing::warn!("Failed to write hooks.json: {}", e),
        },
        Err(e) => tracing::warn!("Failed to serialize hooks.json: {}", e),
    }
}

/// Check if a hook entry belongs to claude-rlm (has a command containing "claude-rlm").
fn is_claude_rlm_entry(entry: &serde_json::Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains("claude-rlm"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
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
    ensure_hooks_synced();

    // Open database in current project directory
    let project_dir = std::env::current_dir()?;
    let db = db::Db::open(&project_dir)?;

    // Clear any leftover shutdown tasks from a previous session so we don't
    // immediately exit, then recover stuck tasks from crashes.
    db::tasks::clear_shutdown_tasks(&db)?;

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

    // Spawn a watchdog that detects stdin close and force-exits.
    // rmcp's async stdin reader may not detect EOF promptly on Windows,
    // causing the process to hang until Claude Code force-kills it (error).
    spawn_stdin_watchdog();

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

/// Spawn a background OS thread that monitors stdin and force-exits when it closes.
/// On Windows, rmcp's async stdin reader may not detect EOF promptly, causing the
/// process to hang past Claude Code's shutdown timeout and get force-killed (error).
#[cfg(windows)]
fn spawn_stdin_watchdog() {
    use std::os::windows::io::AsRawHandle;

    extern "system" {
        fn PeekNamedPipe(
            h_named_pipe: isize,
            lp_buffer: *mut u8,
            n_buffer_size: u32,
            lp_bytes_read: *mut u32,
            lp_total_bytes_avail: *mut u32,
            lp_bytes_left_this_message: *mut u32,
        ) -> i32;
    }

    // Grab the raw handle on the main thread before spawning
    let handle = std::io::stdin().as_raw_handle() as isize;

    std::thread::spawn(move || {
        // Let the server finish starting before we begin polling
        std::thread::sleep(std::time::Duration::from_secs(2));
        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let mut available: u32 = 0;
            let result = unsafe {
                PeekNamedPipe(
                    handle,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            };
            if result == 0 {
                // PeekNamedPipe failed → pipe is broken/closed by parent
                std::process::exit(0);
            }
        }
    });
}

#[cfg(not(windows))]
fn spawn_stdin_watchdog() {
    // On Unix, rmcp detects stdin EOF reliably. No watchdog needed.
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
                "shutdown" => {
                    tracing::info!("Task #{}: shutdown signal received, exiting", task.id);
                    let _ = db::tasks::complete_task(&db2, task.id);
                    std::process::exit(0);
                }
                "reindex_stale" => execute_reindex_stale(&db2, &task, &project_dir2),
                "distill_session" => execute_distill_session(&db2, &task),
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

/// Execute a `distill_session` background task.
/// Runs knowledge distillation and generates a session summary, then updates
/// the session record. This work was deferred from the SessionEnd hook so
/// Claude Code can shut down instantly.
fn execute_distill_session(db: &db::Db, task: &db::tasks::BackgroundTask) {
    let session_id = match &task.payload {
        Some(id) => id.as_str(),
        None => {
            let _ = db::tasks::fail_task(db, task.id, "Missing session_id in payload");
            return;
        }
    };

    // 1. Distill knowledge
    match indexer::distill::distill_session_smart(db, session_id) {
        Ok(stats) => {
            if stats.extracted > 0 {
                tracing::info!(
                    "Task #{}: distilled {} knowledge entries from session {}",
                    task.id, stats.extracted, session_id
                );
            }
        }
        Err(e) => {
            tracing::warn!("Task #{}: knowledge distillation failed: {}", task.id, e);
        }
    }

    // 2. Generate session summary and update the session record
    match generate_session_summary(db, session_id) {
        Ok(Some(summary)) => {
            let conn = db.conn();
            let _ = conn.execute(
                "UPDATE sessions SET summary = ?2 WHERE id = ?1",
                rusqlite::params![session_id, summary],
            );
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("Task #{}: summary generation failed: {}", task.id, e);
        }
    }

    let _ = db::tasks::complete_task(db, task.id);
}

/// Generate a basic session summary from the turn history.
fn generate_session_summary(db: &db::Db, session_id: &str) -> anyhow::Result<Option<String>> {
    let conn = db.conn();

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

/// Handle `config set` subcommand.
fn run_config(action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Set { key, value } => {
            // Map CLI key names to TOML field names
            let toml_key = match key.as_str() {
                "api-key" | "api_key" => "api_key",
                "model" => "model",
                "provider" => "provider",
                "base-url" | "base_url" => "base_url",
                other => {
                    anyhow::bail!(
                        "Unknown config key '{}'. Valid keys: api-key, model, provider, base-url",
                        other
                    );
                }
            };

            llm::write_global_config(toml_key, &value)?;

            let path = llm::global_config_path().unwrap_or_default();
            eprintln!("[claude-rlm] Set llm.{} in {}", toml_key, path.display());

            Ok(())
        }
    }
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
