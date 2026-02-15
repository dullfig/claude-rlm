pub mod ranking;

use anyhow::Result;

use crate::db::Db;
use crate::db::search;
use crate::indexer::plans;

/// Maximum characters for injected context.
/// ~4 chars per token, aim for ~4000 tokens = 16000 chars.
const COMPACT_BUDGET: usize = 16_000;
const STARTUP_BUDGET: usize = 8_000;

const HEADER: &str = "\
[ClaudeRLM] You have persistent project memory powered by ClaudeRLM. \
Everything in this session — conversations, code edits, file reads, and shell \
commands — is being indexed automatically and persists across sessions.\n\
\n\
You have MCP tools to search your memory:\n\
- memory_search: Find past discussions, code changes, and context\n\
- memory_decisions: Recall why certain choices were made\n\
- memory_files: See change history for specific files\n\
- memory_symbols: Query code structure (functions, classes, structs)\n\
\n\
Use these proactively. Before starting a task, check if you've worked on \
something similar before. When the user references past work, search your \
memory instead of asking them to repeat themselves. When you encounter an \
unfamiliar part of the codebase, check memory_symbols and memory_files for \
prior context.\n\
\n\
Briefly greet the user and let them know project memory is loaded. Mention \
any notable context from the sections below (recent sessions, knowledge, \
git changes) if present.\n\n";

/// Build context to inject at session startup.
/// Includes: project structure, recent session summaries, active knowledge.
pub fn build_startup_context(db: &Db) -> Result<String> {
    let conn = db.conn();
    let mut parts: Vec<String> = vec![HEADER.to_string()];
    let mut budget_remaining = STARTUP_BUDGET - HEADER.len();

    // 0. Active plan (highest priority — crash recovery)
    if let Ok(Some(plan)) = plans::active_plan(&db) {
        let section = format_plan_section(&plan, budget_remaining);
        budget_remaining = budget_remaining.saturating_sub(section.len());
        parts.push(section);
    }

    // 1. Codebase map (symbols grouped by file) or fallback to stats
    let map_section = format_codebase_map(&conn, &db.project_dir(), budget_remaining / 2)?;
    if !map_section.is_empty() {
        budget_remaining = budget_remaining.saturating_sub(map_section.len());
        parts.push(map_section);
    }

    // 2. Recent session summaries (last 3)
    let sessions = search::recent_sessions(&conn, 3)?;
    if !sessions.is_empty() {
        let mut section = String::from("## Recent Sessions\n");
        for s in &sessions {
            let summary = s.summary.as_deref().unwrap_or("(no summary)");
            let ended = s.ended_at.as_deref().unwrap_or("(in progress)");
            let id_end = s.id.floor_char_boundary(8.min(s.id.len()));
            let entry = format!(
                "- {} (started: {}, ended: {}): {}\n",
                &s.id[..id_end],
                s.started_at,
                ended,
                truncate(summary, 200)
            );
            if section.len() + entry.len() > budget_remaining / 2 {
                break;
            }
            section.push_str(&entry);
        }
        budget_remaining = budget_remaining.saturating_sub(section.len());
        parts.push(section);
    }

    // 3. Recent git/file catch-up (if from this session start, within last 30 seconds)
    {
        let mut stmt = conn.prepare(
            "SELECT content FROM turns
             WHERE turn_type IN ('git_catchup', 'file_catchup')
               AND timestamp >= datetime('now', '-30 seconds')
             ORDER BY timestamp DESC
             LIMIT 1",
        )?;

        let catchup: Option<String> = stmt
            .query_row([], |row| row.get(0))
            .ok();

        if let Some(content) = catchup {
            let section = format!("## Recent Git Changes\n{}\n", truncate(&content, 800));
            budget_remaining = budget_remaining.saturating_sub(section.len());
            parts.push(section);
        }
    }

    // 4. Active knowledge (decisions, conventions, preferences)
    let knowledge_categories = [
        "decision",
        "preference",
        "convention",
        "pattern",
        "architecture",
        "bug_fix",
    ];
    let mut knowledge_section = String::new();

    for category in &knowledge_categories {
        let mut stmt = conn.prepare(
            "SELECT subject, content, confidence FROM knowledge
             WHERE category = ?1 AND superseded_by IS NULL AND confidence > 0.5
             ORDER BY confidence DESC, created_at DESC
             LIMIT 10",
        )?;

        let entries: Vec<(String, String, f64)> = stmt
            .query_map([category], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .filter_map(|r| r.ok())
            .collect();

        if !entries.is_empty() {
            knowledge_section.push_str(&format!("### {}\n", capitalize(category)));
            for (subject, content, confidence) in &entries {
                let entry = format!(
                    "- **{}** ({:.0}%): {}\n",
                    subject,
                    confidence * 100.0,
                    truncate(content, 150)
                );
                if knowledge_section.len() + entry.len() > budget_remaining {
                    break;
                }
                knowledge_section.push_str(&entry);
            }
        }
    }

    if !knowledge_section.is_empty() {
        parts.push(format!("## Project Knowledge\n{}", knowledge_section));
    }

    Ok(parts.join("\n"))
}

/// Build context to inject after compaction.
///
/// This is the critical path — compaction just discarded most of the conversation.
/// We need to reconstruct the most important context:
/// 1. What the user is working on (recent requests)
/// 2. What files are being modified (working set)
/// 3. Key decisions made in this session
/// 4. Recent code changes (what was done, not just file names)
/// 5. Any checkpoint summaries from PreCompact
pub fn build_compact_context(db: &Db, session_id: &str) -> Result<String> {
    let conn = db.conn();
    let mut parts: Vec<String> = vec![HEADER.to_string()];

    // Active plan (must survive compaction)
    if let Ok(Some(plan)) = plans::active_plan(&db) {
        parts.push(format_plan_section(&plan, COMPACT_BUDGET / 4));
    }

    // Get the active file set for file-affinity scoring
    let active_files = search::active_files(&conn, session_id, 20)?;

    // Get all turns from this session
    let all_turns = search::session_turns(&conn, session_id)?;

    if all_turns.is_empty() {
        return Ok(String::new());
    }

    // 1. Checkpoint summaries (generated by PreCompact) — these are gold
    let checkpoints: Vec<&search::TurnSearchResult> = all_turns
        .iter()
        .filter(|t| t.turn_type == "checkpoint")
        .collect();

    if !checkpoints.is_empty() {
        let mut section = String::from("## Session Checkpoint\n");
        for cp in &checkpoints {
            section.push_str(&truncate(&cp.content, 2000));
            section.push('\n');
        }
        parts.push(section);
    }

    // 2. All user requests (these define the task — never skip these)
    let requests: Vec<&search::TurnSearchResult> = all_turns
        .iter()
        .filter(|t| t.turn_type == "request")
        .collect();

    if !requests.is_empty() {
        let mut section = String::from("## User Requests\n");
        for r in &requests {
            section.push_str(&format!("{}. {}\n", r.turn_number, truncate(&r.content, 300)));
        }
        parts.push(section);
    }

    // 3. Files being worked on with their change summaries
    if !active_files.is_empty() {
        let mut section = String::from("## Active Files\n");
        for file in &active_files {
            section.push_str(&format!("- {}\n", file));
        }
        parts.push(section);
    }

    // 4. Use ranked retrieval for the remaining budget
    //    Filter out requests and checkpoints (already included above)
    let current_size: usize = parts.iter().map(|p| p.len()).sum();
    let remaining_budget = COMPACT_BUDGET.saturating_sub(current_size);

    let rankable_turns: Vec<search::TurnSearchResult> = all_turns
        .into_iter()
        .filter(|t| t.turn_type != "request" && t.turn_type != "checkpoint")
        .collect();

    if !rankable_turns.is_empty() && remaining_budget > 200 {
        let ranked_context =
            ranking::ranked_select(&rankable_turns, &active_files, remaining_budget);
        if !ranked_context.is_empty() {
            parts.push(format!("## Session Activity\n{}", ranked_context));
        }
    }

    Ok(parts.join("\n"))
}

/// Format a codebase map showing symbols grouped by file.
/// Falls back to aggregate stats if no symbols are indexed.
fn format_codebase_map(
    conn: &rusqlite::Connection,
    project_dir: &str,
    budget: usize,
) -> Result<String> {
    let structure = search::project_structure(conn)?;
    if structure.total_symbols == 0 {
        return Ok(String::new());
    }

    let map = search::codebase_map(conn)?;

    // Header line with aggregate stats
    let kinds_str: Vec<String> = structure
        .symbol_kinds
        .iter()
        .take(6)
        .map(|(k, c)| format!("{} {}", c, k))
        .collect();
    let dirs_str: Vec<String> = structure
        .directories
        .iter()
        .take(8)
        .map(|(d, c)| format!("{} ({})", d, c))
        .collect();

    let mut section = format!(
        "## Project Structure ({} symbols across {} files)\n",
        structure.total_symbols, structure.total_files
    );
    section.push_str(&format!("Symbols: {}\n", kinds_str.join(", ")));
    if !dirs_str.is_empty() {
        section.push_str(&format!("Directories: {}\n", dirs_str.join(", ")));
    }

    if map.is_empty() {
        return Ok(section);
    }

    section.push('\n');

    // Normalize project_dir for prefix stripping
    let prefix = project_dir.replace('\\', "/");
    let prefix = prefix.trim_end_matches('/');

    // Deduplicate entries that map to the same relative path
    let mut seen = std::collections::HashSet::new();
    let mut deduped: Vec<&search::FileMapEntry> = Vec::new();
    for entry in &map {
        let rel = make_relative(&entry.file_path, prefix);
        if seen.insert(rel) {
            deduped.push(entry);
        }
    }

    let mut files_shown = 0;
    let total_files = deduped.len();

    for entry in &deduped {
        let rel_path = make_relative(&entry.file_path, prefix);
        let sym_strs: Vec<String> = entry
            .symbols
            .iter()
            .map(|s| format_symbol(&s.name, &s.kind))
            .collect();
        let mut line = format!("{}\n  {}", rel_path, sym_strs.join(", "));
        if entry.truncated {
            line.push_str(", ...");
        }
        line.push('\n');

        if section.len() + line.len() > budget {
            break;
        }
        section.push_str(&line);
        files_shown += 1;
    }

    let remaining = total_files - files_shown;
    if remaining > 0 {
        section.push_str(&format!("...and {} more files\n", remaining));
    }

    Ok(section)
}

/// Format a symbol name with its kind prefix.
fn format_symbol(name: &str, kind: &str) -> String {
    match kind {
        "struct" => format!("struct {}", name),
        "enum" => format!("enum {}", name),
        "trait" => format!("trait {}", name),
        "function" => format!("fn {}()", name),
        "const" => format!("const {}", name),
        "impl" => format!("impl {}", name),
        "type" => format!("type {}", name),
        _ => name.to_string(),
    }
}

/// Strip the project directory prefix and normalize to forward slashes.
fn make_relative(path: &str, prefix: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .strip_prefix(prefix)
        .unwrap_or(&normalized)
        .trim_start_matches('/')
        .to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max);
        format!("{}...", &s[..end])
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

/// Format an active plan for injection into startup/compact context.
fn format_plan_section(plan: &plans::PlanInfo, budget: usize) -> String {
    let title = plan.title.as_deref().unwrap_or("Untitled Plan");
    let mut section = format!(
        "## Active Plan: {} [{}]\n\
         Plan file: {}\n\
         Created: {} | Updated: {}\n",
        title, plan.status, plan.plan_file_path, plan.created_at, plan.updated_at,
    );

    // Progress files
    if !plan.progress.is_empty() {
        section.push_str("\nFiles edited:\n");
        for p in &plan.progress {
            section.push_str(&format!("- {} ({} edits)\n", p.file_path, p.edit_count));
        }
    }

    // Target file completion ratio
    if !plan.target_files.is_empty() {
        let touched = plan
            .progress
            .iter()
            .filter(|p| {
                plan.target_files.iter().any(|t| {
                    p.file_path.ends_with(t) || t.ends_with(&p.file_path)
                })
            })
            .count();
        section.push_str(&format!(
            "\nTarget progress: {}/{} files ({:.0}%)\n",
            touched,
            plan.target_files.len(),
            touched as f64 / plan.target_files.len() as f64 * 100.0,
        ));
    }

    // Plan content (truncated)
    let content_budget = budget.saturating_sub(section.len()).min(2000);
    if content_budget > 100 {
        section.push_str("\nPlan content:\n");
        section.push_str(&truncate(&plan.content, content_budget));
        section.push('\n');
    }

    section
}
