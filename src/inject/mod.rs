pub mod ranking;

use anyhow::Result;

use crate::db::Db;
use crate::db::search;

/// Maximum characters for injected context.
/// ~4 chars per token, aim for ~4000 tokens = 16000 chars.
const COMPACT_BUDGET: usize = 16_000;
const STARTUP_BUDGET: usize = 8_000;

const HEADER: &str = "\
[ClaudeRLM] Project memory retrieved. You have context from previous sessions \
including conversation history, code structure, and distilled knowledge. \
Briefly inform the user that ClaudeRLM has loaded project memory.\n\n";

/// Build context to inject at session startup.
/// Includes: project structure, recent session summaries, active knowledge.
pub fn build_startup_context(db: &Db) -> Result<String> {
    let conn = db.conn();
    let mut parts: Vec<String> = vec![HEADER.to_string()];
    let mut budget_remaining = STARTUP_BUDGET - HEADER.len();

    // 1. Project structure summary (if code has been indexed)
    let structure = search::project_structure(&conn)?;
    if structure.total_symbols > 0 {
        let mut section = format!(
            "## Project Structure ({} symbols across {} files)\n",
            structure.total_symbols, structure.total_files
        );

        // Symbol breakdown
        let kinds_str: Vec<String> = structure
            .symbol_kinds
            .iter()
            .take(6)
            .map(|(k, c)| format!("{} {}", c, k))
            .collect();
        section.push_str(&format!("Symbols: {}\n", kinds_str.join(", ")));

        // Directory breakdown
        if !structure.directories.is_empty() {
            let dirs_str: Vec<String> = structure
                .directories
                .iter()
                .take(8)
                .map(|(d, c)| format!("{} ({})", d, c))
                .collect();
            section.push_str(&format!("Directories: {}\n", dirs_str.join(", ")));
        }

        budget_remaining = budget_remaining.saturating_sub(section.len());
        parts.push(section);
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

    // 3. Active knowledge (decisions, conventions, preferences)
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
