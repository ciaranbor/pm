use std::fs;
use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::agent::{AgentRegistry, AgentType};
use crate::state::paths;

/// Parse the YAML frontmatter from an agent definition and extract checklist items.
///
/// Expects `---`-delimited frontmatter with a `checklist:` field containing
/// YAML list entries (`- item`). Returns an empty vec if no checklist field
/// is present.
pub fn parse_frontmatter_checklist(content: &str) -> Vec<String> {
    let mut lines = content.lines();

    // Must start with ---
    match lines.next() {
        Some(line) if line.trim() == "---" => {}
        _ => return Vec::new(),
    }

    let mut in_checklist = false;
    let mut items = Vec::new();

    for line in lines {
        // End of frontmatter
        if line.trim() == "---" {
            break;
        }

        if in_checklist {
            // A checklist item: "  - some text"
            let trimmed = line.trim();
            if let Some(item) = trimmed.strip_prefix("- ").map(str::trim) {
                if !item.is_empty() {
                    items.push(item.to_string());
                }
            } else if !trimmed.is_empty() {
                // Non-list line means we've exited the checklist block
                break;
            }
        } else if line.starts_with("checklist:") {
            in_checklist = true;
        }
    }

    items
}

/// Parse a project-specific checklist file. One item per line, blank lines
/// and `#` comment lines are ignored.
pub fn parse_project_checklist(content: &str) -> Vec<String> {
    content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.to_string())
        .collect()
}

/// Assemble a checklist from the agent definition frontmatter and a
/// project-specific checklist file, then send it as a message to the
/// target agent.
///
/// Resolves aliases: when the registry has an entry for `agent_name` with
/// an `agent_definition` set, the *definition* file is used to source the
/// frontmatter checklist. The project-specific checklist (`.pm/checklist/
/// <name>.txt`) is keyed on the display name so two aliases of the same
/// definition can have distinct project-level extras.
pub fn agent_check(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    sender: &str,
    tmux_server: Option<&str>,
) -> Result<String> {
    // Resolve the agent definition name: registry's stored definition (if
    // aliased) > display name (back-compat / unregistered names).
    let agents_dir = paths::agents_dir(project_root);
    let registry = AgentRegistry::load(&agents_dir, feature)?;
    let definition_name = registry
        .get(agent_name)
        .map(|entry| entry.effective_definition(agent_name).to_string())
        .unwrap_or_else(|| agent_name.to_string());

    // 1. Find and read the agent definition (looked up by definition name)
    let def_path =
        super::agent_send::find_agent_definition_path(project_root, feature, &definition_name)
            .ok_or_else(|| {
                PmError::AgentNotFound(if definition_name == agent_name {
                    format!(
                        "No agent definition found for '{agent_name}'. \
                     Cannot assemble a checklist without an agent definition."
                    )
                } else {
                    format!(
                        "No agent definition found for '{definition_name}' \
                         (alias for '{agent_name}'). \
                         Cannot assemble a checklist without an agent definition."
                    )
                })
            })?;

    let def_content = fs::read_to_string(&def_path).map_err(|e| {
        PmError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to read agent definition at {}: {e}",
                def_path.display()
            ),
        ))
    })?;

    // 2. Parse checklist from frontmatter
    let mut items = parse_frontmatter_checklist(&def_content);

    // 3. Read project-specific checklist if it exists
    let project_checklist_path = project_root
        .join(".pm")
        .join("checklist")
        .join(format!("{agent_name}.txt"));
    if project_checklist_path.exists() {
        let project_content = fs::read_to_string(&project_checklist_path).map_err(|e| {
            PmError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to read project checklist at {}: {e}",
                    project_checklist_path.display()
                ),
            ))
        })?;
        items.extend(parse_project_checklist(&project_content));
    }

    // 4. Error if no items found
    if items.is_empty() {
        return Err(PmError::Messaging(format!(
            "No checklist items found for '{agent_name}'. \
             Add a `checklist:` field to the agent definition frontmatter \
             or create {}",
            project_checklist_path.display()
        )));
    }

    // 5. Compose the message body
    let checklist_lines: Vec<String> = items.iter().map(|item| format!("- [ ] {item}")).collect();
    let body = format!(
        "Please verify each of the following before this feature is considered complete. \
         Check each item and produce a brief pass/fail report.\n\n{}",
        checklist_lines.join("\n")
    );

    // 6. Delegate to agent_send
    super::agent_send::agent_send(
        project_root,
        feature,
        None,
        agent_name,
        sender,
        &body,
        tmux_server,
    )
}

/// Send checklists to all active agents that have checklist items configured.
/// Returns a list of status lines (one per agent checked) and a list of errors.
/// Agents with no checklist items are silently skipped.
pub fn agent_check_all(
    project_root: &Path,
    feature: &str,
    sender: &str,
    tmux_server: Option<&str>,
) -> Result<(Vec<String>, Vec<String>)> {
    let agents_dir = paths::agents_dir(project_root);
    let registry = AgentRegistry::load(&agents_dir, feature)?;

    let active_agents: Vec<&str> = registry
        .agents
        .iter()
        .filter(|(_, entry)| entry.agent_type == AgentType::Agent && entry.active)
        .map(|(name, _)| name.as_str())
        .collect();

    if active_agents.is_empty() {
        return Err(PmError::Messaging(
            "No active agents to check. Spawn agents first or specify an agent name.".to_string(),
        ));
    }

    let mut successes = Vec::new();
    let mut errors = Vec::new();

    for agent_name in active_agents {
        match agent_check(project_root, feature, agent_name, sender, tmux_server) {
            Ok(msg) => successes.push(msg),
            Err(PmError::Messaging(msg)) if msg.contains("No checklist items found") => {
                // Silently skip agents with no checklist configured
            }
            Err(PmError::AgentNotFound(msg)) if msg.contains("No agent definition found") => {
                // Silently skip agents without definitions (shouldn't happen often)
            }
            Err(e) => errors.push(format!("{agent_name}: {e}")),
        }
    }

    if successes.is_empty() && errors.is_empty() {
        return Err(PmError::Messaging(
            "No active agents have checklists configured. Add a `checklist:` field \
             to agent definition frontmatter or create .pm/checklist/<agent>.txt files."
                .to_string(),
        ));
    }

    Ok((successes, errors))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::feature::{FeatureState, FeatureStatus};
    use crate::state::project::{ProjectConfig, ProjectInfo};
    use crate::testing::TestServer;
    use crate::{state::paths, tmux};
    use chrono::Utc;
    use tempfile::tempdir;

    // --- Frontmatter parsing ---

    #[test]
    fn parse_frontmatter_with_checklist() {
        let content = r#"---
name: implementer
description: test
checklist:
  - summary.md exists
  - All tests pass
  - All changes are committed
---

# Body
"#;
        let items = parse_frontmatter_checklist(content);
        assert_eq!(
            items,
            vec![
                "summary.md exists",
                "All tests pass",
                "All changes are committed",
            ]
        );
    }

    #[test]
    fn parse_frontmatter_without_checklist() {
        let content = r#"---
name: implementer
description: test
tools: Read, Write
---

# Body
"#;
        let items = parse_frontmatter_checklist(content);
        assert!(items.is_empty());
    }

    #[test]
    fn parse_frontmatter_no_frontmatter() {
        let content = "# Just a markdown file\n\nNo frontmatter here.\n";
        let items = parse_frontmatter_checklist(content);
        assert!(items.is_empty());
    }

    #[test]
    fn parse_frontmatter_checklist_followed_by_other_field() {
        let content = r#"---
checklist:
  - item one
  - item two
tools: Read, Write
---
"#;
        let items = parse_frontmatter_checklist(content);
        assert_eq!(items, vec!["item one", "item two"]);
    }

    // --- Project checklist parsing ---

    #[test]
    fn parse_project_checklist_items() {
        let content = "# Comments\n\nFirst item\nSecond item\n\n# Another comment\nThird item\n";
        let items = parse_project_checklist(content);
        assert_eq!(items, vec!["First item", "Second item", "Third item"]);
    }

    #[test]
    fn parse_project_checklist_empty() {
        let items = parse_project_checklist("");
        assert!(items.is_empty());
    }

    #[test]
    fn parse_project_checklist_only_comments() {
        let items = parse_project_checklist("# comment\n# another\n\n");
        assert!(items.is_empty());
    }

    // --- Integration tests ---

    fn setup_project(dir: &std::path::Path, server: &TestServer) -> (std::path::PathBuf, String) {
        let root = dir.to_path_buf();
        let pm_dir = root.join(".pm");
        let project_name = server.scope("proj");
        let feature_name = "login";

        std::fs::create_dir_all(pm_dir.join("features")).unwrap();

        let config = ProjectConfig {
            project: ProjectInfo {
                name: project_name.clone(),
                max_features: None,
            },
            setup: Default::default(),
            github: Default::default(),
            agents: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        let now = Utc::now();
        let state = FeatureState {
            status: FeatureStatus::Wip,
            branch: feature_name.to_string(),
            worktree: feature_name.to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            created: now,
            last_active: now,
        };
        state.save(&pm_dir.join("features"), feature_name).unwrap();

        let worktree = root.join(feature_name);
        std::fs::create_dir_all(&worktree).unwrap();

        let session_name = tmux::session_name(&project_name, feature_name);
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        (root, feature_name.to_string())
    }

    fn create_agent_definition_with_checklist(
        root: &std::path::Path,
        agent_name: &str,
        checklist: &[&str],
    ) {
        let agent_def = paths::main_worktree(root)
            .join(".claude/agents")
            .join(format!("{agent_name}.md"));
        std::fs::create_dir_all(agent_def.parent().unwrap()).unwrap();

        let checklist_yaml = if checklist.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = checklist.iter().map(|item| format!("  - {item}")).collect();
            format!("checklist:\n{}\n", items.join("\n"))
        };

        let content = format!(
            "---\nname: {agent_name}\ndescription: test\n{checklist_yaml}---\n\n# {agent_name}\n"
        );
        std::fs::write(&agent_def, content).unwrap();
    }

    #[test]
    fn agent_check_sends_checklist_message() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, feature) = setup_project(dir.path(), &server);

        create_agent_definition_with_checklist(
            &root,
            "implementer",
            &["summary.md exists", "All tests pass"],
        );

        let msg = agent_check(&root, &feature, "implementer", "user", server.name()).unwrap();
        assert!(msg.contains("Message 001 sent to 'implementer'"));

        // Verify the message content in the inbox
        let messages_dir = paths::messages_dir(&root);
        let delivered = crate::messages::read_at(&messages_dir, &feature, "implementer", "user", 1)
            .unwrap()
            .unwrap();
        assert!(delivered.body.contains("- [ ] summary.md exists"));
        assert!(delivered.body.contains("- [ ] All tests pass"));
    }

    #[test]
    fn agent_check_with_project_specific_items() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, feature) = setup_project(dir.path(), &server);

        create_agent_definition_with_checklist(&root, "implementer", &["Global item"]);

        // Create project-specific checklist
        let checklist_dir = root.join(".pm/checklist");
        std::fs::create_dir_all(&checklist_dir).unwrap();
        std::fs::write(
            checklist_dir.join("implementer.txt"),
            "# Project specific\nProject item one\nProject item two\n",
        )
        .unwrap();

        agent_check(&root, &feature, "implementer", "user", server.name()).unwrap();

        let messages_dir = paths::messages_dir(&root);
        let delivered = crate::messages::read_at(&messages_dir, &feature, "implementer", "user", 1)
            .unwrap()
            .unwrap();
        assert!(delivered.body.contains("- [ ] Global item"));
        assert!(delivered.body.contains("- [ ] Project item one"));
        assert!(delivered.body.contains("- [ ] Project item two"));
    }

    #[test]
    fn agent_check_no_definition_errors() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, feature) = setup_project(dir.path(), &server);

        let result = agent_check(&root, &feature, "nonexistent", "user", server.name());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No agent definition found for 'nonexistent'"));
    }

    #[test]
    fn agent_check_no_checklist_items_errors() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, feature) = setup_project(dir.path(), &server);

        // Agent definition without checklist
        create_agent_definition_with_checklist(&root, "implementer", &[]);

        let result = agent_check(&root, &feature, "implementer", "user", server.name());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No checklist items found"));
    }

    #[test]
    fn agent_check_resolves_aliased_agent_definition() {
        // Regression: `pm agent spawn frontend-dev --agent implementer` then
        // `pm agent check frontend-dev` must look up the implementer
        // definition, not search for a non-existent frontend-dev.md.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, feature) = setup_project(dir.path(), &server);

        // Definition file lives at `.claude/agents/implementer.md`
        create_agent_definition_with_checklist(
            &root,
            "implementer",
            &["summary.md exists", "All tests pass"],
        );

        // Register the alias: display name 'frontend-dev', definition 'implementer'.
        // Use spawn (which writes the registry entry).
        super::super::agent_spawn::agent_spawn(
            &root,
            &feature,
            "frontend-dev",
            Some("implementer"),
            None,
            false,
            server.name(),
        )
        .unwrap();

        let msg = agent_check(&root, &feature, "frontend-dev", "user", server.name()).unwrap();
        assert!(msg.contains("Message 001 sent to 'frontend-dev'"));

        // Body sourced from implementer's frontmatter and delivered to frontend-dev's inbox
        let messages_dir = paths::messages_dir(&root);
        let delivered =
            crate::messages::read_at(&messages_dir, &feature, "frontend-dev", "user", 1)
                .unwrap()
                .unwrap();
        assert!(delivered.body.contains("- [ ] summary.md exists"));
        assert!(delivered.body.contains("- [ ] All tests pass"));
    }

    #[test]
    fn agent_check_alias_with_missing_definition_errors_clearly() {
        // If the alias points at a definition that's not installed, the
        // error mentions both the alias and the definition.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, feature) = setup_project(dir.path(), &server);

        // Register an alias whose definition file does not exist
        let agents_dir = paths::agents_dir(&root);
        let mut registry = AgentRegistry::default();
        registry.register(
            "frontend-dev",
            crate::state::agent::AgentEntry {
                agent_type: crate::state::agent::AgentType::Agent,
                session_id: String::new(),
                window_name: "frontend-dev".to_string(),
                active: false,
                agent_definition: Some("ghost-definition".to_string()),
            },
        );
        registry.save(&agents_dir, &feature).unwrap();

        let result = agent_check(&root, &feature, "frontend-dev", "user", server.name());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("ghost-definition"), "got: {err}");
        assert!(err.contains("frontend-dev"), "got: {err}");
    }

    #[test]
    fn agent_check_custom_agent_name() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, feature) = setup_project(dir.path(), &server);

        create_agent_definition_with_checklist(&root, "reviewer", &["Sent final approval"]);

        let msg = agent_check(&root, &feature, "reviewer", "user", server.name()).unwrap();
        assert!(msg.contains("Message 001 sent to 'reviewer'"));
    }

    // --- agent_check_all tests ---

    fn register_active_agent(
        root: &std::path::Path,
        feature: &str,
        agent_name: &str,
        server: &TestServer,
    ) {
        let pm_dir = paths::pm_dir(root);
        let config = ProjectConfig::load(&pm_dir).unwrap();
        let session_name = tmux::session_name(&config.project.name, feature);
        server.spawn_fake_agent(root, &session_name, feature, agent_name);
    }

    #[test]
    fn check_all_sends_to_active_agents_with_checklists() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, feature) = setup_project(dir.path(), &server);

        create_agent_definition_with_checklist(&root, "implementer", &["Tests pass"]);
        create_agent_definition_with_checklist(&root, "reviewer", &["Sent approval"]);

        register_active_agent(&root, &feature, "implementer", &server);
        register_active_agent(&root, &feature, "reviewer", &server);

        let (successes, errors) = agent_check_all(&root, &feature, "user", server.name()).unwrap();
        assert_eq!(successes.len(), 2);
        assert!(errors.is_empty());
        assert!(successes.iter().any(|s| s.contains("implementer")));
        assert!(successes.iter().any(|s| s.contains("reviewer")));
    }

    #[test]
    fn check_all_skips_agents_without_checklist() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, feature) = setup_project(dir.path(), &server);

        create_agent_definition_with_checklist(&root, "implementer", &["Tests pass"]);
        // reviewer has no checklist
        create_agent_definition_with_checklist(&root, "reviewer", &[]);

        register_active_agent(&root, &feature, "implementer", &server);
        register_active_agent(&root, &feature, "reviewer", &server);

        let (successes, errors) = agent_check_all(&root, &feature, "user", server.name()).unwrap();
        assert_eq!(successes.len(), 1);
        assert!(successes[0].contains("implementer"));
        assert!(errors.is_empty());
    }

    #[test]
    fn check_all_no_active_agents_errors() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, feature) = setup_project(dir.path(), &server);

        let result = agent_check_all(&root, &feature, "user", server.name());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No active agents"));
    }

    #[test]
    fn check_all_active_agents_but_no_checklists_errors() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, feature) = setup_project(dir.path(), &server);

        create_agent_definition_with_checklist(&root, "implementer", &[]);
        register_active_agent(&root, &feature, "implementer", &server);

        let result = agent_check_all(&root, &feature, "user", server.name());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No active agents have checklists configured"));
    }
}
