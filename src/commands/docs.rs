use std::fmt::Write;
use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::{docs, paths};

struct DefaultCategory {
    filename: &'static str,
    heading: &'static str,
    description: &'static str,
}

const DEFAULT_CATEGORIES: &[DefaultCategory] = &[
    DefaultCategory {
        filename: "todo.md",
        heading: "# Todo",
        description: "Ordered task list. Actionable items with clear next steps.",
    },
    DefaultCategory {
        filename: "issues.md",
        heading: "# Issues",
        description: "Concrete bugs and unexpected behaviours discovered during usage.",
    },
    DefaultCategory {
        filename: "ideas.md",
        heading: "# Ideas",
        description: "Thoughts and design questions that aren't yet actionable.",
    },
    DefaultCategory {
        filename: "findings.md",
        heading: "# Findings",
        description: "Durable findings and learnings worth remembering — verified facts, gotchas, and external constraints discovered during work. Not tasks, bugs, or open questions.",
    },
];

/// Append a single `[[category]]` table to `out`.
fn write_category_entry(out: &mut String, cat: &DefaultCategory) {
    writeln!(out, "[[category]]").unwrap();
    writeln!(out, "filename = \"{}\"", cat.filename).unwrap();
    writeln!(out, "description = \"{}\"", cat.description).unwrap();
}

/// Generate `categories.toml` content from the default categories.
fn default_categories_toml() -> String {
    let mut out = String::new();
    for (i, cat) in DEFAULT_CATEGORIES.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        write_category_entry(&mut out, cat);
    }
    out
}

/// Reconcile an existing `categories.toml` against the defaults, appending any
/// default category whose `filename` isn't already listed.
///
/// Returns `Some(updated_content)` if entries were appended, `None` if the file
/// already lists every default (or can't be parsed — we don't clobber files we
/// don't understand).
///
/// Note: this resurrects a default category a user intentionally deleted from
/// `categories.toml`. That's deliberate and consistent with `bootstrap`, which
/// already recreates any missing default category *file*; keeping the toml and
/// the on-disk files in sync with the defaults is the simpler, predictable rule
/// this early on.
fn reconcile_categories_toml(content: &str) -> Option<String> {
    let parsed: toml::Value = toml::from_str(content).ok()?;
    let existing: std::collections::HashSet<&str> = parsed
        .get("category")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.get("filename").and_then(|f| f.as_str()))
                .collect()
        })
        .unwrap_or_default();

    let missing: Vec<&DefaultCategory> = DEFAULT_CATEGORIES
        .iter()
        .filter(|cat| !existing.contains(cat.filename))
        .collect();

    if missing.is_empty() {
        return None;
    }

    let mut out = content.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    for cat in missing {
        out.push('\n');
        write_category_entry(&mut out, cat);
    }
    Some(out)
}

/// Bootstrap the information store at `.pm/docs/`.
///
/// Creates the directory and writes default `categories.toml` and category
/// markdown files. Does NOT create a separate git repo — docs are tracked
/// by the parent `.pm/` state repo (initialised by `pm state init`).
/// Idempotent — won't overwrite existing files.
pub fn bootstrap(project_root: &Path) -> Result<()> {
    let docs_dir = paths::docs_dir(project_root);

    std::fs::create_dir_all(&docs_dir)?;

    // Write categories.toml. If it doesn't exist, seed it with the defaults.
    // If it does, preserve user customisations but reconcile in any default
    // categories it's missing (e.g. findings.md added in a later version) so
    // the orchestrator — which discovers categories by reading this file —
    // sees every default category file bootstrap creates below.
    let categories_path = paths::doc_categories(project_root);
    if !categories_path.exists() {
        std::fs::write(&categories_path, default_categories_toml())?;
    } else {
        let existing = std::fs::read_to_string(&categories_path)?;
        if let Some(updated) = reconcile_categories_toml(&existing) {
            std::fs::write(&categories_path, updated)?;
        }
    }

    // Write default category files
    for cat in DEFAULT_CATEGORIES {
        let cat_path = docs_dir.join(cat.filename);
        if !cat_path.exists() {
            std::fs::write(&cat_path, format!("{}\n", cat.heading))?;
        }
    }

    Ok(())
}

/// List paths that [`bootstrap`] would create. Empty vec means nothing to do.
pub fn bootstrap_dry_run(project_root: &Path) -> Vec<std::path::PathBuf> {
    let docs_dir = paths::docs_dir(project_root);
    let mut would_create = Vec::new();

    if !docs_dir.exists() {
        would_create.push(docs_dir.clone());
    }
    let categories_path = paths::doc_categories(project_root);
    if !categories_path.exists() {
        would_create.push(categories_path);
    }
    for cat in DEFAULT_CATEGORIES {
        let cat_path = docs_dir.join(cat.filename);
        if !cat_path.exists() {
            would_create.push(cat_path);
        }
    }
    would_create
}

/// `pm docs`: the store's categories, one per line, when `category` is
/// `None`; else that category's text. A category is named by its filename,
/// with or without `.md`.
pub fn show(project_root: &Path, category: Option<&str>) -> Result<String> {
    let listed = docs::list(project_root)?;
    let Some(category) = category else {
        if listed.is_empty() {
            return Ok("The information store has no categories.\n".to_string());
        }
        return Ok(listing(&listed));
    };
    let filename = listed
        .iter()
        .map(|c| c.filename.as_str())
        .find(|f| *f == category || f.strip_suffix(".md") == Some(category));
    match filename {
        Some(filename) => Ok(docs::read(project_root, filename)?.unwrap_or_default()),
        None => {
            let names: Vec<&str> = listed.iter().map(|c| c.filename.as_str()).collect();
            Err(PmError::Docs(if names.is_empty() {
                format!("no category '{category}': the information store has none")
            } else {
                format!(
                    "no category '{category}' in the information store; it has: {}",
                    names.join(", ")
                )
            }))
        }
    }
}

fn listing(listed: &[docs::Listed]) -> String {
    let width = listed.iter().map(|c| c.filename.len()).max().unwrap_or(0);
    let mut out = String::new();
    for c in listed {
        let state = match c.modified {
            Some(modified) if c.size > 0 => format!(
                "updated {}",
                modified.with_timezone(&chrono::Local).format("%Y-%m-%d")
            ),
            _ => "empty".to_string(),
        };
        writeln!(
            out,
            "{:<width$}  {state:<18}  {}",
            c.filename, c.description
        )
        .unwrap();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_project(dir: &std::path::Path) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm").join("features")).unwrap();
        std::fs::create_dir_all(paths::main_worktree(&root)).unwrap();
        root
    }
    #[test]
    fn bootstrap_creates_docs_directory() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        bootstrap(&root).unwrap();

        assert!(paths::docs_dir(&root).exists());
    }

    #[test]
    fn bootstrap_creates_categories_toml() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        bootstrap(&root).unwrap();

        let categories_path = paths::doc_categories(&root);
        assert!(categories_path.exists());

        let content = std::fs::read_to_string(&categories_path).unwrap();
        assert!(content.contains("todo.md"));
        assert!(content.contains("issues.md"));
        assert!(content.contains("ideas.md"));
        assert!(content.contains("findings.md"));

        // The orchestrator reads filename + description from this file, so make
        // sure the findings entry round-trips through a TOML parse intact.
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        let findings = parsed
            .get("category")
            .and_then(|c| c.as_array())
            .unwrap()
            .iter()
            .find(|e| e.get("filename").and_then(|f| f.as_str()) == Some("findings.md"))
            .expect("findings category present");
        let desc = findings
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap();
        assert!(desc.contains("Durable findings"));
    }

    #[test]
    fn bootstrap_reconciles_missing_default_into_existing_categories_toml() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Simulate a project created before findings existed: categories.toml
        // lists only the original three categories.
        let docs = paths::docs_dir(&root);
        std::fs::create_dir_all(&docs).unwrap();
        let legacy = "[[category]]\nfilename = \"todo.md\"\ndescription = \"Ordered task list.\"\n\n\
[[category]]\nfilename = \"issues.md\"\ndescription = \"Bugs.\"\n\n\
[[category]]\nfilename = \"ideas.md\"\ndescription = \"Ideas.\"\n";
        std::fs::write(docs.join("categories.toml"), legacy).unwrap();

        bootstrap(&root).unwrap();

        let content = std::fs::read_to_string(docs.join("categories.toml")).unwrap();
        // Original entries preserved (custom descriptions untouched).
        assert!(content.contains("Ordered task list."));
        assert!(content.contains("\"Bugs.\""));
        // findings appended both to the toml and on disk.
        assert!(content.contains("findings.md"));
        assert!(docs.join("findings.md").exists());

        // Still valid TOML with exactly one findings entry.
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        let count = parsed
            .get("category")
            .and_then(|c| c.as_array())
            .unwrap()
            .iter()
            .filter(|e| e.get("filename").and_then(|f| f.as_str()) == Some("findings.md"))
            .count();
        assert_eq!(count, 1);

        // Idempotent: a second bootstrap doesn't append a duplicate.
        bootstrap(&root).unwrap();
        let content2 = std::fs::read_to_string(docs.join("categories.toml")).unwrap();
        assert_eq!(content, content2);
    }

    #[test]
    fn bootstrap_creates_default_category_files() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        bootstrap(&root).unwrap();

        let docs = paths::docs_dir(&root);
        assert!(docs.join("todo.md").exists());
        assert!(docs.join("issues.md").exists());
        assert!(docs.join("ideas.md").exists());
        assert!(docs.join("findings.md").exists());

        let todo = std::fs::read_to_string(docs.join("todo.md")).unwrap();
        assert!(todo.starts_with("# Todo"));

        let findings = std::fs::read_to_string(docs.join("findings.md")).unwrap();
        assert!(findings.starts_with("# Findings"));
    }

    #[test]
    fn bootstrap_does_not_create_git_repo() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        bootstrap(&root).unwrap();

        // No separate git repo in docs
        assert!(!paths::docs_dir(&root).join(".git").exists());
    }

    #[test]
    fn bootstrap_is_idempotent() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        bootstrap(&root).unwrap();

        // Modify a file to verify bootstrap doesn't overwrite
        let docs = paths::docs_dir(&root);
        std::fs::write(docs.join("todo.md"), "# Todo\n- custom item\n").unwrap();

        bootstrap(&root).unwrap();

        // Content should be preserved
        let content = std::fs::read_to_string(docs.join("todo.md")).unwrap();
        assert!(content.contains("custom item"));
    }

    #[test]
    fn show_names_a_category_with_or_without_its_extension() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        bootstrap(&root).unwrap();
        std::fs::write(paths::docs_dir(&root).join("todo.md"), "# Todo\n- a\n").unwrap();

        assert_eq!(show(&root, Some("todo")).unwrap(), "# Todo\n- a\n");
        assert_eq!(show(&root, Some("todo.md")).unwrap(), "# Todo\n- a\n");
        let listing = show(&root, None).unwrap();
        let names: Vec<&str> = listing
            .lines()
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(names, ["todo.md", "issues.md", "ideas.md", "findings.md"]);
        let err = show(&root, Some("categories.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("todo.md, issues.md"), "{err}");
    }
}
