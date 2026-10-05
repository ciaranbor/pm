//! The user's editor: `$VISUAL`, else `$EDITOR`, else `vi`, run through
//! `sh` as git runs it, so a value with arguments (`code --wait`) works.

use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};

/// Edit `path` in the user's editor, on this terminal, until it exits.
pub fn edit(path: &Path) -> Result<()> {
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "vi".to_string());
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(&editor)
        .arg(path)
        .status()
        .map_err(|e| PmError::Editor(format!("could not run {editor}: {e}")))?;
    if !status.success() {
        return Err(PmError::Editor(format!("{editor} exited with {status}")));
    }
    Ok(())
}
