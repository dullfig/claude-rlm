use crate::db::search::TurnSearchResult;

/// A scored turn ready for injection.
pub struct ScoredTurn {
    pub turn: TurnSearchResult,
    pub score: f64,
}

/// Weight multiplier for different turn types.
pub fn type_weight(turn_type: &str) -> f64 {
    match turn_type {
        "decision" => 1.5,
        "checkpoint" => 1.4,
        "request" => 1.3,
        "code_edit" => 1.2,
        "explanation" => 1.0,
        "error" => 1.0,
        "plan" => 1.0,
        "file_read" => 0.5,
        "bash_cmd" => 0.3,
        _ => 0.5,
    }
}

/// Recency boost: exponential decay based on age in hours.
/// Returns a value between 0.1 and 1.0.
pub fn recency_boost(age_hours: f64) -> f64 {
    let decay = (-age_hours / 24.0).exp();
    decay.max(0.1)
}

/// File affinity boost: how many current context files overlap with this turn's files.
pub fn file_affinity(turn_files: &[String], context_files: &[String]) -> f64 {
    if context_files.is_empty() || turn_files.is_empty() {
        return 1.0;
    }

    let overlap = turn_files
        .iter()
        .filter(|f| context_files.contains(f))
        .count();

    1.0 + (overlap as f64 * 0.5)
}

/// Score a turn for ranked retrieval.
/// Higher score = more relevant for injection.
fn score_turn(turn: &TurnSearchResult, context_files: &[String], now: &str) -> f64 {
    let age_hours = hours_between(&turn.timestamp, now);
    let type_w = type_weight(&turn.turn_type);
    let recency = recency_boost(age_hours);
    let affinity = file_affinity(&turn.files, context_files);

    // Base score from type weight and recency
    let mut score = type_w * recency * affinity;

    // Bonus for turns with more substantive content
    let content_len = turn.content.len();
    if content_len > 100 {
        score *= 1.1;
    }
    if content_len > 500 {
        score *= 1.1;
    }

    score
}

/// Rank and select turns for context injection within a character budget.
///
/// Returns a formatted context string that fits within `budget_chars`.
/// Turns are selected by score, then re-ordered chronologically for coherence.
pub fn ranked_select(
    turns: &[TurnSearchResult],
    context_files: &[String],
    budget_chars: usize,
) -> String {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Score all turns
    let mut scored: Vec<ScoredTurn> = turns
        .iter()
        .map(|t| ScoredTurn {
            score: score_turn(t, context_files, &now),
            turn: TurnSearchResult {
                turn_id: t.turn_id,
                session_id: t.session_id.clone(),
                turn_number: t.turn_number,
                timestamp: t.timestamp.clone(),
                role: t.role.clone(),
                turn_type: t.turn_type.clone(),
                content: t.content.clone(),
                content_summary: t.content_summary.clone(),
                rank: t.rank,
                files: t.files.clone(),
            },
        })
        .collect();

    // Sort by score descending (best first)
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // Select turns within budget
    let mut selected: Vec<&ScoredTurn> = Vec::new();
    let mut used_chars = 0;

    for st in &scored {
        let entry_size = format_turn_for_injection(&st.turn).len();
        if used_chars + entry_size > budget_chars {
            // Try truncating this turn's content to fit
            let remaining = budget_chars.saturating_sub(used_chars);
            if remaining > 100 {
                // Worth including a truncated version
                selected.push(st);
                used_chars += remaining;
            }
            break;
        }
        selected.push(st);
        used_chars += entry_size;
    }

    // Re-sort selected turns chronologically for coherent reading
    selected.sort_by_key(|st| st.turn.turn_number);

    // Format the output
    let mut output = String::new();
    for (i, st) in selected.iter().enumerate() {
        let formatted = format_turn_for_injection(&st.turn);
        // Truncate the last entry if it exceeds the budget
        if i == selected.len() - 1 && output.len() + formatted.len() > budget_chars {
            let remaining = budget_chars.saturating_sub(output.len());
            if remaining > 50 {
                output.push_str(&formatted[..remaining.min(formatted.len())]);
            }
        } else {
            output.push_str(&formatted);
        }
    }

    output
}

/// Format a turn for injection into context.
fn format_turn_for_injection(turn: &TurnSearchResult) -> String {
    let type_label = match turn.turn_type.as_str() {
        "request" => "User",
        "code_edit" => "Edit",
        "file_read" => "Read",
        "bash_cmd" => "Cmd",
        "decision" => "Decision",
        "checkpoint" => "Checkpoint",
        "plan" => "Plan",
        "error" => "Error",
        _ => &turn.turn_type,
    };

    let files_str = if !turn.files.is_empty() {
        format!(" [{}]", turn.files.join(", "))
    } else {
        String::new()
    };

    // Truncate content for injection (individual turns shouldn't dominate)
    let content = if turn.content.len() > 800 {
        let end = turn.content.floor_char_boundary(800);
        format!("{}...", &turn.content[..end])
    } else {
        turn.content.clone()
    };

    format!("- **{}**{}: {}\n", type_label, files_str, content)
}

/// Calculate hours between two timestamp strings.
fn hours_between(earlier: &str, later: &str) -> f64 {
    let parse = |s: &str| -> Option<chrono::NaiveDateTime> {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok()
    };

    match (parse(earlier), parse(later)) {
        (Some(e), Some(l)) => {
            let duration = l.signed_duration_since(e);
            duration.num_minutes() as f64 / 60.0
        }
        _ => 1.0, // Default to 1 hour if parsing fails
    }
}
