//! Reading the information store (`.pm/docs/`). Only the files
//! `categories.toml` lists can be read, each by its listed filename, so a
//! reader names a category and never a path: nothing else under `.pm/` can
//! be reached.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::state::paths;

#[derive(Deserialize)]
struct Categories {
    #[serde(default)]
    category: Vec<Category>,
}

#[derive(Deserialize)]
struct Category {
    filename: String,
    #[serde(default)]
    description: String,
}

/// A category of the store, as `categories.toml` lists it.
#[derive(Debug, Serialize)]
pub struct Listed {
    pub filename: String,
    pub description: String,
    /// Bytes; 0 when the file doesn't exist yet.
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
}

/// The categories `categories.toml` lists, in its order, keeping only
/// those whose filename names a file directly in the docs dir and, if it
/// is a link, one that stays there; none when it is missing.
fn categories(root: &Path) -> Result<Vec<Category>> {
    let path = paths::doc_categories(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let parsed: Categories = toml::from_str(&text)?;
    Ok(parsed
        .category
        .into_iter()
        .filter(|c| plain_name(&c.filename) && stays_in(root, &c.filename))
        .collect())
}

/// Whether `filename` in the docs dir resolves to a file directly inside
/// it; a file not written yet does.
fn stays_in(root: &Path, filename: &str) -> bool {
    let dir = paths::docs_dir(root);
    match (dir.join(filename).canonicalize(), dir.canonicalize()) {
        (Ok(file), Ok(dir)) => file.parent() == Some(dir.as_path()) && file.is_file(),
        (Err(e), _) => e.kind() == std::io::ErrorKind::NotFound,
        (Ok(_), Err(_)) => false,
    }
}

fn plain_name(name: &str) -> bool {
    !matches!(name, "" | "." | ".." | "categories.toml") && !name.contains(['/', '\\'])
}

/// Every readable category of the store at `root`, in listed order.
pub fn list(root: &Path) -> Result<Vec<Listed>> {
    let dir = paths::docs_dir(root);
    Ok(categories(root)?
        .into_iter()
        .map(|c| {
            let meta = std::fs::metadata(dir.join(&c.filename)).ok();
            Listed {
                size: meta.as_ref().map_or(0, std::fs::Metadata::len),
                modified: meta
                    .and_then(|m| m.modified().ok())
                    .map(DateTime::<Utc>::from),
                filename: c.filename,
                description: c.description,
            }
        })
        .collect())
}

/// The listed category `filename`'s text, empty when its file doesn't
/// exist yet; `None` when no readable category has that filename.
pub fn read(root: &Path, filename: &str) -> Result<Option<String>> {
    if !categories(root)?.iter().any(|c| c.filename == filename) {
        return Ok(None);
    }
    match std::fs::read_to_string(paths::docs_dir(root).join(filename)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Some(String::new())),
        Err(e) => Err(e.into()),
    }
}
