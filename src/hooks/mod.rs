pub mod prompt;
pub mod tool_use;
pub mod compact;
pub mod session;
pub mod pre_tool_use;

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

/// Common fields present in all hook inputs from Claude Code.
#[derive(Debug, Deserialize)]
pub struct HookInput {
    /// The session ID
    pub session_id: Option<String>,

    /// The tool name (for PostToolUse hooks)
    pub tool_name: Option<String>,

    /// The tool input (for PostToolUse hooks)
    pub tool_input: Option<Value>,

    /// The tool response (for PostToolUse hooks)
    pub tool_response: Option<Value>,

    /// The prompt content (for UserPromptSubmit hooks)
    pub prompt: Option<String>,

    /// The transcript path (for SessionEnd hooks)
    pub transcript_path: Option<String>,

    /// The session source (for SessionStart: "startup" or "compact")
    pub source: Option<String>,

    /// The project directory
    pub cwd: Option<String>,

    /// All other fields
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

/// Read hook input from stdin (Claude Code sends JSON).
pub fn read_hook_input() -> Result<HookInput> {
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
    let parsed: HookInput = serde_json::from_str(&input)?;
    Ok(parsed)
}

/// Get the project directory from hook input or fall back to cwd.
pub fn project_dir(input: &HookInput) -> String {
    input
        .cwd
        .clone()
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        })
}

/// Get the session ID, generating one if not provided.
pub fn session_id(input: &HookInput) -> String {
    input
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

/// Log a hook invocation to the hook_log table.
/// Fire-and-forget — hook logging must never break a hook.
pub fn log_hook(db: &crate::db::Db, input: &HookInput, hook_event: &str, detail: &str) {
    let conn = db.conn();
    let _ = conn.execute(
        "INSERT INTO hook_log (hook_event, tool_name, detail, session_id)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            hook_event,
            input.tool_name.as_deref(),
            if detail.is_empty() { None } else { Some(detail) },
            input.session_id.as_deref(),
        ],
    );
}
