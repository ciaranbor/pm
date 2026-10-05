//! `/v1/projects/{project}/notes`: a project's notes ([`notes`]) as
//! Markdown, each reply tagged with its version (`ETag`). A save names the
//! version it started from in `If-Match` and is refused (409) when the
//! notes have changed since, with the current notes for the device to
//! reconcile against.

use std::path::Path;

use crate::commands::notes::{self, Saved};
use crate::error::Result;

use super::routes::{JSON, MARKDOWN, Reply, error};

/// The longest notes a device may save. The terminal has no limit, so
/// notes grown past it there can be read here but not saved.
pub(super) const MAX_TEXT: usize = 256 * 1024;

pub(super) fn get(root: &Path) -> Result<Reply> {
    let current = notes::read(root)?;
    Ok(Reply::Body {
        status: 200,
        content_type: MARKDOWN,
        body: current.text,
        etag: Some(current.version),
    })
}

/// Save `body` as the notes; the reply, and what the request log says.
pub(super) fn put(
    root: &Path,
    if_match: Option<&str>,
    body: &str,
) -> Result<(Reply, Option<String>)> {
    let Some(base) = if_match.map(version) else {
        return Ok((
            error(428, "If-Match names the version the edit started from"),
            None,
        ));
    };
    if body.len() > MAX_TEXT {
        return Ok((too_long(), None));
    }
    Ok(match notes::save(root, body, base)? {
        Saved::Written(version) => (
            Reply::Body {
                status: 200,
                content_type: JSON,
                body: serde_json::json!({ "version": version }).to_string(),
                etag: Some(version.clone()),
            },
            Some(format!("notes {version}")),
        ),
        Saved::Changed(current) => (
            Reply::Body {
                status: 409,
                content_type: JSON,
                body: serde_json::json!({
                    "error": "the notes changed since that version",
                    "refused": "changed",
                    "text": current.text,
                    "version": current.version,
                })
                .to_string(),
                etag: Some(current.version),
            },
            Some("notes refused: changed".to_string()),
        ),
    })
}

/// The refusal of notes longer than [`MAX_TEXT`].
pub(super) fn too_long() -> Reply {
    error(
        413,
        &format!("notes over {MAX_TEXT} bytes are edited with `pm notes` only"),
    )
}

/// The version an `If-Match` value names, its quotes and any weak marker
/// dropped.
fn version(if_match: &str) -> &str {
    let value = if_match.trim();
    let value = value.strip_prefix("W/").unwrap_or(value);
    value.trim_matches('"')
}
