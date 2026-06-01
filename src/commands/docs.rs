use std::fmt::Write;
use std::path::Path;

use crate::error::Result;
use crate::git;
use crate::state::paths;

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

/// Returns `true` if `.pm/docs/` is currently a nested git repo / submodule
/// that [`migrate_docs_submodule`] would migrate.
pub fn would_migrate_docs_submodule(project_root: &Path) -> bool {
    let docs_dir = paths::docs_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);
    docs_dir.join(".git").exists() && pm_dir.join(".git").exists()
}

/// Migrate `.pm/docs/` from a git submodule (nested repo) to regular files
/// tracked by the parent `.pm/` state repo.
///
/// Idempotent — does nothing if docs is already a plain directory.
/// Returns `true` if migration was performed.
pub fn migrate_docs_submodule(project_root: &Path) -> Result<bool> {
    let docs_dir = paths::docs_dir(project_root);
    let nested_git = docs_dir.join(".git");

    // Nothing to migrate if there's no nested .git
    if !nested_git.exists() {
        return Ok(false);
    }

    let pm_dir = paths::pm_dir(project_root);

    // Bail out if the state repo isn't initialised — we can't run git
    // operations without it, and removing the nested .git first would
    // leave things in a broken half-migrated state.
    if !pm_dir.join(".git").exists() {
        return Ok(false);
    }

    // 1. Remove the nested .git (could be a dir or a file depending on git version)
    if nested_git.is_dir() {
        std::fs::remove_dir_all(&nested_git)?;
    } else {
        std::fs::remove_file(&nested_git)?;
    }

    // 2. Remove .gitmodules if present
    let gitmodules = pm_dir.join(".gitmodules");
    if gitmodules.exists() {
        std::fs::remove_file(&gitmodules)?;
    }

    // 3. Remove docs from the parent index (it's currently tracked as a submodule)
    //    This may fail if docs isn't in the index yet — that's fine.
    let _ = git::rm_cached(&pm_dir, "docs");

    // 4. Stage the docs files as regular files in the parent repo
    git::add_all(&pm_dir)?;

    // 5. Commit the migration if there are staged changes
    if git::has_staged_changes(&pm_dir)? {
        git::commit_with_message(&pm_dir, "migrate docs from submodule to regular files")?;
    }

    Ok(true)
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
    let categories_path = docs_dir.join("categories.toml");
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
    let categories_path = docs_dir.join("categories.toml");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::state_cmd;
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

        let categories_path = paths::docs_dir(&root).join("categories.toml");
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
    fn migrate_docs_submodule_converts_nested_repo() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Initialise state repo
        state_cmd::init(&root).unwrap();

        // Simulate old behaviour: create a nested git repo inside docs
        let docs = paths::docs_dir(&root);
        std::fs::create_dir_all(&docs).unwrap();
        crate::git::init_repo(&docs).unwrap();
        std::fs::write(docs.join("todo.md"), "# Todo\n").unwrap();
        crate::git::add_all(&docs).unwrap();
        crate::git::commit_with_message(&docs, "init docs").unwrap();

        // Parent repo sees docs as a submodule — commit that state
        let pm_dir = paths::pm_dir(&root);
        crate::git::add_all(&pm_dir).unwrap();
        crate::git::commit_with_message(&pm_dir, "add docs submodule").unwrap();

        // Verify nested .git exists before migration
        assert!(docs.join(".git").exists());

        // Run migration
        let migrated = migrate_docs_submodule(&root).unwrap();
        assert!(migrated, "should report migration was performed");

        // Nested .git should be gone
        assert!(!docs.join(".git").exists());

        // docs files should still exist
        assert!(docs.join("todo.md").exists());

        // docs should now be tracked as regular files in parent repo
        let status = crate::git::status_short(&pm_dir).unwrap();
        assert!(
            status.is_empty(),
            "should be clean after migration commit, got: {status}"
        );
    }

    #[test]
    fn migrate_docs_submodule_is_idempotent() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        state_cmd::init(&root).unwrap();
        bootstrap(&root).unwrap();

        // No nested git — migration should be a no-op
        let migrated = migrate_docs_submodule(&root).unwrap();
        assert!(!migrated);
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
}
