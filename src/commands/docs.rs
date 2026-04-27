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
];

/// Generate `categories.toml` content from the default categories.
fn default_categories_toml() -> String {
    let mut out = String::new();
    for (i, cat) in DEFAULT_CATEGORIES.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        writeln!(out, "[[category]]").unwrap();
        writeln!(out, "filename = \"{}\"", cat.filename).unwrap();
        writeln!(out, "description = \"{}\"", cat.description).unwrap();
    }
    out
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

    // Write categories.toml (only if it doesn't exist, to preserve customisations)
    let categories_path = docs_dir.join("categories.toml");
    if !categories_path.exists() {
        std::fs::write(&categories_path, default_categories_toml())?;
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

        let todo = std::fs::read_to_string(docs.join("todo.md")).unwrap();
        assert!(todo.starts_with("# Todo"));
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
