# ClaudeRLM

Persistent project memory for [Claude Code](https://docs.anthropic.com/en/docs/claude-code). A Rust MCP server that transparently indexes your conversation context and code changes, then re-injects the most relevant information when context is lost to compaction.

## Why this exists

Claude Code sessions have finite context windows. As conversations grow through requests, code edits, debugging, and planning, older context gets compressed or discarded. You lose decisions, rationale, and the thread of what you were doing.

ClaudeRLM fixes this by indexing everything as it happens and surgically re-injecting what matters when context is lost.

## How it works

The core idea is borrowed from [Recursive LLMs](https://arxiv.org/abs/2512.24601) (Zhang et al., 2025), which showed that LLMs can process arbitrarily long inputs by treating them as external data to be programmatically queried rather than consumed all at once. Our insight: conversation context grows incrementally, so it can be indexed at write-time as each turn happens, rather than processed at read-time when it's already too late. This makes indexing nearly free -- each hook call takes milliseconds -- and retrieval is a fast SQLite query.

In practice, Claude Code [hooks](https://docs.anthropic.com/en/docs/claude-code/hooks) fire on every user prompt, code edit, file read, and bash command. ClaudeRLM captures each event into a local SQLite database with FTS5 full-text search. When compaction is about to happen, a `PreCompact` hook ensures everything is indexed and creates a checkpoint summary. After compaction, a `SessionStart` hook queries the index and injects the most relevant context back into the conversation -- ranked by recency, type importance, and file affinity -- so Claude picks up right where it left off.

## Features

- **Passive indexing** -- hooks fire automatically, Claude never needs to decide to use it
- **Full-text search** over conversation history (SQLite FTS5 with BM25 ranking)
- **Code structure indexing** via tree-sitter (Rust, Python, TypeScript, JavaScript, Go, C, C++)
- **Background file watcher** for incremental re-indexing on file changes
- **Ranked context injection** after compaction (type weight x recency x file affinity)
- **Knowledge distillation** at session end -- extracts decisions, preferences, conventions, and bug fixes
- **LLM-enhanced distillation** with Haiku (~1 cent/session) or any OpenAI-compatible endpoint (Ollama for free)
- **4 MCP tools** for explicit search when needed: `memory_search`, `memory_symbols`, `memory_decisions`, `memory_files`
- **Cross-session memory** -- knowledge persists and is injected at the start of every new session

## Installation

```bash
# Build from source
cargo install --path .

# Or build manually
cargo build --release
# Binary at target/release/claude-rlm
```

## Setup

### 1. Configure hooks

Copy `hooks.example.json` into your project's `.claude/settings.json` (or merge into your existing settings):

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [{ "type": "command", "command": "claude-rlm index-prompt", "timeout": 5 }]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [{ "type": "command", "command": "claude-rlm index-edit", "timeout": 5 }]
      },
      {
        "matcher": "Read",
        "hooks": [{ "type": "command", "command": "claude-rlm index-read", "timeout": 2 }]
      },
      {
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": "claude-rlm index-bash", "timeout": 2 }]
      }
    ],
    "PreCompact": [
      {
        "hooks": [{ "type": "command", "command": "claude-rlm pre-compact", "timeout": 10 }]
      }
    ],
    "SessionStart": [
      {
        "hooks": [{ "type": "command", "command": "claude-rlm session-start", "timeout": 10 }]
      }
    ],
    "Stop": [
      {
        "hooks": [{ "type": "command", "command": "claude-rlm session-end", "timeout": 30 }]
      }
    ]
  }
}
```

### 2. Configure MCP server (optional)

Add to your Claude Code MCP settings for explicit search tools:

```json
{
  "mcpServers": {
    "claude-rlm": {
      "command": "claude-rlm",
      "args": ["serve"]
    }
  }
}
```

### 3. Configure LLM distillation (optional)

For higher-quality knowledge extraction at session end, set an API key:

```bash
# Anthropic (recommended -- uses Haiku, ~1 cent per session)
export CONTEXTMEM_LLM_API_KEY="sk-ant-..."

# Or use a local model via Ollama (free)
export CONTEXTMEM_LLM_PROVIDER="ollama"
export CONTEXTMEM_LLM_MODEL="llama3"
```

Without an API key, ClaudeRLM falls back to heuristic pattern matching for distillation, which still works well for common patterns (technology choices, "always/never" preferences, build tools, test frameworks).

| Variable | Default | Description |
|---|---|---|
| `CONTEXTMEM_LLM_API_KEY` | *(none)* | API key (required for cloud, optional for Ollama) |
| `CONTEXTMEM_LLM_PROVIDER` | `anthropic` | `anthropic`, `openai`, or `ollama` |
| `CONTEXTMEM_LLM_MODEL` | `claude-haiku-4-5-20251001` | Model name |
| `CONTEXTMEM_LLM_BASE_URL` | *(provider default)* | Custom endpoint URL |

## What gets indexed

| Event | What's captured |
|---|---|
| User prompt | Full request text |
| Edit/Write | File path, old/new content, change description |
| Read | File path |
| Bash | Command and output (truncated to 2KB) |
| PreCompact | Checkpoint summary of all activity so far |
| Session end | Session summary + distilled knowledge |

## What gets injected

**At session start:** project structure, recent session summaries, distilled knowledge (decisions, conventions, preferences).

**After compaction:** checkpoint summaries, all user requests from the session, active file list, then the highest-ranked remaining turns up to a 16K character budget.

## CLI commands

```
claude-rlm serve          # Start MCP server (default)
claude-rlm status         # Show index statistics
claude-rlm index-prompt   # Hook: index user prompt (stdin)
claude-rlm index-edit     # Hook: index code edit (stdin)
claude-rlm index-read     # Hook: index file read (stdin)
claude-rlm index-bash     # Hook: index bash command (stdin)
claude-rlm pre-compact    # Hook: pre-compaction checkpoint
claude-rlm session-start  # Hook: inject context
claude-rlm session-end    # Hook: distill + summarize
```

## Data storage

All data is stored locally in `.claude/claude-rlm.db` (SQLite) inside your project directory. Nothing leaves your machine unless you configure LLM distillation with a cloud API.

## Supported languages (tree-sitter)

Rust, Python, TypeScript, TSX, JavaScript, Go, C, C++

## License

MIT
