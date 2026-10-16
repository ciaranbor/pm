//! A project named by flag, reached from another project or from none.

mod commands;
mod messages;

use super::*;

/// Register a project `name` under `home`'s registry, rooted at
/// `<home>/<name>` with a main worktree and a `reviewer` agent in main.
fn registered_project(home: &std::path::Path, name: &str) -> std::path::PathBuf {
    use pm::state::project::{ProjectConfig, ProjectEntry, ProjectInfo};
    let root = home.join(name);
    std::fs::create_dir_all(root.join("main")).unwrap();
    std::fs::create_dir_all(root.join(".pm/agents")).unwrap();
    std::fs::create_dir_all(root.join(".pm/features")).unwrap();
    ProjectConfig {
        project: ProjectInfo {
            name: name.to_string(),
            max_features: None,
            fresh_base: None,
        },
        agents: Default::default(),
        harness: Default::default(),
    }
    .save(&root.join(".pm"))
    .unwrap();
    std::fs::write(
        root.join(".pm/agents/main.toml"),
        "[agents.reviewer]\ntype = \"agent\"\nsession_id = \"\"\nwindow_name = \"reviewer\"\n\
         active = false\nharness = \"claude-code\"\n",
    )
    .unwrap();
    ProjectEntry {
        root: root.to_string_lossy().into_owned(),
        main_branch: "main".to_string(),
        repo_url: None,
        state_remote: None,
    }
    .save(
        &pm::state::paths::dirs_under(home).config.join("projects"),
        name,
    )
    .unwrap();
    root
}
