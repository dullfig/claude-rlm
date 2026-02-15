use anyhow::Result;
use serde_json::json;

use crate::hooks::HookInput;

const MEMORY_PROMPT_PREFIX: &str = "\
You have access to project memory tools that contain indexed history from all past sessions.
ALWAYS try these tools FIRST before falling back to Glob/Grep/Read:
- memory_search(query): Search past discussions, code changes, context
- memory_symbols(name, kind): Find code symbols with full location breadcrumbs
- memory_files(file_path): Get change history for a file
- memory_decisions(query): Find past decisions and rationale

Use these tools to answer questions about the project before doing raw file exploration.

";

/// Handle PreToolUse hook.
/// Intercepts Task tool calls with subagent_type="Explore" and augments the
/// prompt with instructions to use memory MCP tools first.
pub fn handle(input: &HookInput) -> Result<()> {
    let tool_name = match &input.tool_name {
        Some(name) => name.as_str(),
        None => return Ok(()),
    };

    if tool_name != "Task" {
        return Ok(());
    }

    let tool_input = match &input.tool_input {
        Some(v) => v,
        None => return Ok(()),
    };

    let subagent_type = match tool_input.get("subagent_type").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return Ok(()),
    };

    if subagent_type != "Explore" {
        return Ok(());
    }

    // Extract the original prompt and prepend memory instructions
    let original_prompt = tool_input
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let augmented_prompt = format!("{}{}", MEMORY_PROMPT_PREFIX, original_prompt);

    let output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {
                "prompt": augmented_prompt
            }
        }
    });
    println!("{}", serde_json::to_string(&output)?);

    Ok(())
}
