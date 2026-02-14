use anyhow::Result;

use crate::db::Db;
use crate::hooks::{self, HookInput};
use crate::indexer::conversation;

/// Handle UserPromptSubmit hook: index the user's prompt.
pub fn handle(input: &HookInput) -> Result<()> {
    let project_dir = hooks::project_dir(input);
    let session_id = hooks::session_id(input);

    let db = Db::open(std::path::Path::new(&project_dir))?;
    conversation::ensure_session(&db, &session_id, &project_dir)?;

    let content = input
        .prompt
        .as_deref()
        .unwrap_or("[empty prompt]");

    conversation::index_turn(
        &db,
        &session_id,
        "user",
        "request",
        content,
        None,
        &[],
    )?;

    Ok(())
}
