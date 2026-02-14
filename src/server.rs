use rmcp::{
    handler::server::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::*,
    tool, tool_handler, tool_router, ServerHandler,
    ErrorData as McpError,
};
use serde::Deserialize;
use std::borrow::Cow;

use crate::db::Db;
use crate::db::search;

/// The ContextMem MCP server.
#[derive(Clone)]
pub struct ClaudeRlmServer {
    db: Db,
    tool_router: ToolRouter<Self>,
}

// --- Tool parameter types ---

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MemorySearchParams {
    /// The search query (supports natural language and keywords)
    #[schemars(description = "Search query for conversation history")]
    pub query: String,

    /// Maximum number of results to return (default: 10)
    #[schemars(description = "Maximum results to return")]
    pub limit: Option<usize>,

    /// Filter by session ID
    #[schemars(description = "Optional session ID to search within")]
    pub session_id: Option<String>,

    /// Filter by turn type (request, code_edit, decision, etc.)
    #[schemars(description = "Optional turn type filter")]
    pub turn_type: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MemoryDecisionsParams {
    /// Search query for decisions
    #[schemars(description = "Search query for past decisions")]
    pub query: String,

    /// Maximum number of results
    #[schemars(description = "Maximum results to return")]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MemoryFilesParams {
    /// The file path to get history for
    #[schemars(description = "File path to look up change history")]
    pub file_path: String,

    /// Maximum number of results
    #[schemars(description = "Maximum results to return")]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MemorySymbolsParams {
    /// Symbol name to search for
    #[schemars(description = "Symbol name (function, class, struct, etc.)")]
    pub name: String,

    /// Symbol kind filter (function, class, struct, type, etc.)
    #[schemars(description = "Optional symbol kind filter")]
    pub kind: Option<String>,
}

// --- Helper: run DB work on a blocking thread ---

fn mcp_err(msg: String) -> McpError {
    McpError {
        code: ErrorCode::INTERNAL_ERROR,
        message: Cow::from(msg),
        data: None,
    }
}

// --- Server implementation ---

#[tool_router]
impl ClaudeRlmServer {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Search conversation history and project memory. Use this to find past discussions, decisions, code changes, and context from previous sessions.")]
    async fn memory_search(
        &self,
        Parameters(params): Parameters<MemorySearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let result = tokio::task::spawn_blocking(move || {
            let limit = params.limit.unwrap_or(10);
            let conn = db.conn();
            let results = search::search_turns(
                &conn,
                &params.query,
                limit,
                params.session_id.as_deref(),
                params.turn_type.as_deref(),
            )?;
            Ok::<_, anyhow::Error>(results)
        })
        .await
        .map_err(|e| mcp_err(format!("Task join error: {e}")))?
        .map_err(|e| mcp_err(format!("Search failed: {e}")))?;

        if result.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No matching results found.",
            )]));
        }

        let mut output = String::new();
        for r in &result {
            output.push_str(&format!(
                "---\n**Turn #{} ({})** [{}] session:{}\n",
                r.turn_number, r.turn_type, r.timestamp, r.session_id
            ));
            if !r.files.is_empty() {
                output.push_str(&format!("Files: {}\n", r.files.join(", ")));
            }
            let content = if r.content.len() > 1000 {
                let end = r.content.floor_char_boundary(1000);
                format!("{}...", &r.content[..end])
            } else {
                r.content.clone()
            };
            output.push_str(&content);
            output.push_str("\n\n");
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(description = "Search past decisions and their rationale. Use this to understand why certain choices were made.")]
    async fn memory_decisions(
        &self,
        Parameters(params): Parameters<MemoryDecisionsParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let result = tokio::task::spawn_blocking(move || {
            let limit = params.limit.unwrap_or(10);
            let conn = db.conn();

            let knowledge_results = search::search_knowledge(
                &conn,
                &params.query,
                limit,
                Some("decision"),
            )?;

            let turn_results = search::search_turns(
                &conn,
                &params.query,
                limit,
                None,
                Some("decision"),
            )?;

            Ok::<_, anyhow::Error>((knowledge_results, turn_results))
        })
        .await
        .map_err(|e| mcp_err(format!("Task join error: {e}")))?
        .map_err(|e| mcp_err(format!("Search failed: {e}")))?;

        let (knowledge_results, turn_results) = result;

        if knowledge_results.is_empty() && turn_results.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No matching decisions found.",
            )]));
        }

        let mut output = String::new();

        if !knowledge_results.is_empty() {
            output.push_str("## Distilled Knowledge\n");
            for k in &knowledge_results {
                output.push_str(&format!(
                    "- **{}** (confidence: {:.1}): {}\n",
                    k.subject, k.confidence, k.content
                ));
            }
            output.push('\n');
        }

        if !turn_results.is_empty() {
            output.push_str("## Decision Turns\n");
            for r in &turn_results {
                output.push_str(&format!("- [{}] {}\n", r.timestamp, r.content));
            }
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(description = "Get the change history for a specific file. Shows what was modified and when.")]
    async fn memory_files(
        &self,
        Parameters(params): Parameters<MemoryFilesParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let result = tokio::task::spawn_blocking(move || {
            let limit = params.limit.unwrap_or(20);
            let conn = db.conn();
            let results = search::file_history(&conn, &params.file_path, limit)?;
            Ok::<_, anyhow::Error>(results)
        })
        .await
        .map_err(|e| mcp_err(format!("Task join error: {e}")))?
        .map_err(|e| mcp_err(format!("Search failed: {e}")))?;

        if result.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No file history found.",
            )]));
        }

        let mut output = String::from("## File History\n\n");
        for r in &result {
            let content = if r.content.len() > 500 {
                let end = r.content.floor_char_boundary(500);
                format!("{}...", &r.content[..end])
            } else {
                r.content.clone()
            };
            output.push_str(&format!(
                "- [{}] ({}): {}\n",
                r.timestamp, r.turn_type, content
            ));
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(description = "Query code symbols (functions, classes, structs) indexed by tree-sitter. Phase 2 feature.")]
    async fn memory_symbols(
        &self,
        Parameters(params): Parameters<MemorySymbolsParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let result = tokio::task::spawn_blocking(move || {
            let conn = db.conn();
            let kind_filter = params.kind.as_deref().unwrap_or("%");
            let mut stmt = conn.prepare(
                "SELECT file_path, name, kind, start_line, end_line, signature
                 FROM symbols
                 WHERE name LIKE ?1 AND kind LIKE ?2
                 ORDER BY file_path, start_line
                 LIMIT 50",
            )?;

            let results: Vec<String> = stmt
                .query_map(
                    rusqlite::params![format!("%{}%", params.name), kind_filter],
                    |row| {
                        let file_path: String = row.get(0)?;
                        let name: String = row.get(1)?;
                        let kind: String = row.get(2)?;
                        let start_line: i64 = row.get(3)?;
                        let end_line: i64 = row.get(4)?;
                        let signature: Option<String> = row.get(5)?;

                        let sig_str = signature
                            .map(|s| format!(" - `{}`", s))
                            .unwrap_or_default();
                        Ok(format!(
                            "- {} `{}` at {}:{}-{}{}",
                            kind, name, file_path, start_line, end_line, sig_str
                        ))
                    },
                )?
                .filter_map(|r| r.ok())
                .collect();
            Ok::<_, anyhow::Error>(results)
        })
        .await
        .map_err(|e| mcp_err(format!("Task join error: {e}")))?
        .map_err(|e| mcp_err(format!("Query failed: {e}")))?;

        if result.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No symbols found.",
            )]));
        }

        let output = format!("## Symbols\n\n{}", result.join("\n"));
        Ok(CallToolResult::success(vec![Content::text(output)]))
    }
}

#[tool_handler]
impl ServerHandler for ClaudeRlmServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "ClaudeRLM: Persistent project memory for Claude Code. \
                 Automatically indexes conversation history and code changes. \
                 Use memory_search to find past discussions, memory_decisions \
                 for past decisions, memory_files for file change history, \
                 and memory_symbols for code structure queries."
                    .to_string(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
