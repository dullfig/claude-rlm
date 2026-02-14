use anyhow::Result;
use serde_json::Value;

use crate::db::Db;
use crate::hooks::{self, HookInput};
use crate::indexer::{code, conversation};

/// Handle PostToolUse for Edit/Write tools.
pub fn handle_edit(input: &HookInput) -> Result<()> {
    let project_dir = hooks::project_dir(input);
    let session_id = hooks::session_id(input);

    let db = Db::open(std::path::Path::new(&project_dir))?;
    conversation::ensure_session(&db, &session_id, &project_dir)?;

    let tool_name = input.tool_name.as_deref().unwrap_or("unknown");
    let tool_input = &input.tool_input;

    // Extract file path from tool input
    let file_path = tool_input
        .as_ref()
        .and_then(|v| v.get("file_path"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Build a concise content description
    let content = if let Some(ti) = tool_input {
        format_edit_content(tool_name, ti)
    } else {
        format!("{tool_name}: {file_path}")
    };

    let action = match tool_name {
        "Write" => "write",
        "Edit" => "edit",
        _ => "edit",
    };

    conversation::index_turn(
        &db,
        &session_id,
        "assistant",
        "code_edit",
        &content,
        tool_input.as_ref(),
        &[(file_path.to_string(), action.to_string())],
    )?;

    // Re-index the changed file for tree-sitter symbols
    let path = std::path::Path::new(file_path);
    if path.exists() {
        if let Err(e) = code::reindex_file(&db, path) {
            tracing::warn!("Failed to reindex {}: {}", file_path, e);
        }
    }

    Ok(())
}

/// Handle PostToolUse for Read tool.
pub fn handle_read(input: &HookInput) -> Result<()> {
    let project_dir = hooks::project_dir(input);
    let session_id = hooks::session_id(input);

    let db = Db::open(std::path::Path::new(&project_dir))?;
    conversation::ensure_session(&db, &session_id, &project_dir)?;

    let file_path = input
        .tool_input
        .as_ref()
        .and_then(|v| v.get("file_path"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let content = format!("Read file: {file_path}");

    conversation::index_turn(
        &db,
        &session_id,
        "assistant",
        "file_read",
        &content,
        None,
        &[(file_path.to_string(), "read".to_string())],
    )?;

    Ok(())
}

/// Handle PostToolUse for Bash tool.
pub fn handle_bash(input: &HookInput) -> Result<()> {
    let project_dir = hooks::project_dir(input);
    let session_id = hooks::session_id(input);

    let db = Db::open(std::path::Path::new(&project_dir))?;
    conversation::ensure_session(&db, &session_id, &project_dir)?;

    let command = input
        .tool_input
        .as_ref()
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("[unknown command]");

    // Extract output: tool_response can be a string or {"stdout": "...", ...}
    let output = input
        .tool_response
        .as_ref()
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.get("stdout").and_then(|s| s.as_str()).map(|s| s.to_string()))
        })
        .unwrap_or_default();
    let output = output.as_str();

    let truncated_output = if output.len() > 2000 {
        &output[..output.floor_char_boundary(2000)]
    } else {
        output
    };

    let content = format!("$ {command}\n{truncated_output}");

    conversation::index_turn(
        &db,
        &session_id,
        "assistant",
        "bash_cmd",
        &content,
        None,
        &[],
    )?;

    Ok(())
}

/// Format edit content concisely.
fn format_edit_content(tool_name: &str, tool_input: &Value) -> String {
    let file_path = tool_input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    match tool_name {
        "Edit" => {
            let old = tool_input
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = tool_input
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Truncate long strings
            let old_trunc = truncate_str(old, 500);
            let new_trunc = truncate_str(new, 500);

            format!("Edit {file_path}:\n- {old_trunc}\n+ {new_trunc}")
        }
        "Write" => {
            let content = tool_input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let preview = truncate_str(content, 500);
            format!("Write {file_path}: {preview}")
        }
        _ => format!("{tool_name} {file_path}"),
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max);
        format!("{}...[truncated]", &s[..end])
    }
}
