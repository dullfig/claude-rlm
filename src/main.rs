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
#[command(name = "claude-rlm", about = "Persistent project memory for Claude Code")]
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
                if s.len() > 60 { format!("{}...", &s[..60]) } else { s }
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
                    if c.len() > 80 { format!("{}...", &c[..80]) } else { c }
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
    let _watcher = match watcher::start_watcher(db.clone(), project_dir) {
        Ok(w) => {
            tracing::info!("Background file watcher started");
            Some(w)
        }
        Err(e) => {
            tracing::warn!("Failed to start file watcher: {}", e);
            None
        }
    };

    let server = server::ClaudeRlmServer::new(db);

    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| {
            eprintln!("Error starting ClaudeRLM server: {}", e);
        })?;

    service.waiting().await?;
    Ok(())
}

/// Run a hook handler, catching and logging errors.
fn run_hook(f: impl FnOnce() -> Result<()>) -> Result<()> {
    match f() {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("[claude-rlm] Hook error: {}", e);
            // Don't fail the hook — return Ok so Claude Code continues
            Ok(())
        }
    }
}
