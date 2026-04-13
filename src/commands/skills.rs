use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::fs_utils::copy_dir_recursive;

// --- Unified bundled item system ---

#[derive(Clone, Copy, PartialEq, Eq)]
enum BundledKind {
    Skill,
    Agent,
}

impl BundledKind {
    fn label(self) -> &'static str {
        match self {
            Self::Skill => "Skill",
            Self::Agent => "Agent",
        }
    }

    fn claude_subdir(self) -> &'static str {
        match self {
            Self::Skill => "skills",
            Self::Agent => "agents",
        }
    }

    fn not_found_error(self, name: &str) -> PmError {
        match self {
            Self::Skill => PmError::SkillNotFound(name.to_string()),
            Self::Agent => PmError::AgentNotFound(name.to_string()),
        }
    }
}

struct BundledItem {
    kind: BundledKind,
    name: &'static str,
    /// Path relative to the install directory (e.g. "pm/SKILL.md" or "reviewer.md").
    rel_path: &'static str,
    content: &'static str,
}

const BUNDLED_ITEMS: &[BundledItem] = &[
    // Skills
    BundledItem {
        kind: BundledKind::Skill,
        name: "pm",
        rel_path: "pm/SKILL.md",
        content: include_str!("../../skills/pm/SKILL.md"),
    },
    // Agents
    BundledItem {
        kind: BundledKind::Agent,
        name: "reviewer",
        rel_path: "reviewer.md",
        content: include_str!("../../agents/reviewer.md"),
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "implementer",
        rel_path: "implementer.md",
        content: include_str!("../../agents/implementer.md"),
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "researcher",
        rel_path: "researcher.md",
        content: include_str!("../../agents/researcher.md"),
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "main",
        rel_path: "main.md",
        content: include_str!("../../agents/main.md"),
    },
];

fn items_of_kind(kind: BundledKind) -> impl Iterator<Item = &'static BundledItem> {
    BUNDLED_ITEMS.iter().filter(move |i| i.kind == kind)
}

fn is_installed(base_dir: &Path, item: &BundledItem) -> bool {
    base_dir.join(item.rel_path).exists()
}

fn is_up_to_date(base_dir: &Path, item: &BundledItem) -> bool {
    let path = base_dir.join(item.rel_path);
    match fs::read_to_string(&path) {
        Ok(installed) => installed == item.content,
        Err(_) => false,
    }
}

fn global_dir(kind: BundledKind) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or(PmError::NoHomeDir)?;
    Ok(home.join(".claude").join(kind.claude_subdir()))
}

fn project_dir(project_root: &Path, kind: BundledKind) -> PathBuf {
    project_root
        .join("main")
        .join(".claude")
        .join(kind.claude_subdir())
}

fn status_label(dir: &Path, item: &BundledItem) -> &'static str {
    if !is_installed(dir, item) {
        "not installed"
    } else if is_up_to_date(dir, item) {
        "installed"
    } else {
        "outdated"
    }
}

fn list_both(kind: BundledKind, project_root: Option<&Path>) -> Result<Vec<String>> {
    let global = global_dir(kind)?;
    let project = project_root.map(|r| project_dir(r, kind));

    let mut lines = Vec::new();
    for item in items_of_kind(kind) {
        let global_status = status_label(&global, item);

        if let Some(ref proj) = project {
            let project_status = status_label(proj, item);
            lines.push(format!(
                "  {} — project: {}, global: {}",
                item.name, project_status, global_status
            ));
        } else {
            lines.push(format!("  {} — {}", item.name, global_status));
        }
    }
    Ok(lines)
}

fn install_in(dir: &Path, kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    let to_install: Vec<&BundledItem> = match name {
        Some(n) => {
            let item = items_of_kind(kind)
                .find(|i| i.name == n)
                .ok_or_else(|| kind.not_found_error(n))?;
            vec![item]
        }
        None => items_of_kind(kind).collect(),
    };

    let label = kind.label();
    let mut messages = Vec::new();
    for item in to_install {
        if is_up_to_date(dir, item) {
            messages.push(format!("{label} '{}' is already up to date", item.name));
            continue;
        }
        let path = dir.join(item.rel_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, item.content)?;
        messages.push(format!("Installed {label} '{}'", item.name));
    }
    Ok(messages)
}

fn uninstall_in(dir: &Path, kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    let to_uninstall: Vec<&BundledItem> = match name {
        Some(n) => {
            let item = items_of_kind(kind)
                .find(|i| i.name == n)
                .ok_or_else(|| kind.not_found_error(n))?;
            vec![item]
        }
        None => items_of_kind(kind).collect(),
    };

    let label = kind.label();
    let mut messages = Vec::new();
    for item in to_uninstall {
        let path = dir.join(item.rel_path);
        if !path.exists() {
            messages.push(format!("{label} '{}' is not installed", item.name));
            continue;
        }
        std::fs::remove_file(&path)?;
        // Clean up empty parent directory (for skills which use a subdirectory)
        if let Some(parent) = path.parent()
            && parent != dir
            && parent.read_dir().is_ok_and(|mut d| d.next().is_none())
        {
            let _ = std::fs::remove_dir(parent);
        }
        messages.push(format!("Uninstalled {label} '{}'", item.name));
    }
    Ok(messages)
}

// --- Public API: Skills ---

pub fn skills_list(project_root: Option<&Path>) -> Result<Vec<String>> {
    list_both(BundledKind::Skill, project_root)
}

pub fn skills_install(name: Option<&str>) -> Result<Vec<String>> {
    install_in(&global_dir(BundledKind::Skill)?, BundledKind::Skill, name)
}

pub fn skills_install_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    install_in(
        &project_dir(project_root, BundledKind::Skill),
        BundledKind::Skill,
        name,
    )
}

pub fn skills_uninstall(name: Option<&str>) -> Result<Vec<String>> {
    uninstall_in(&global_dir(BundledKind::Skill)?, BundledKind::Skill, name)
}

pub fn skills_uninstall_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    uninstall_in(
        &project_dir(project_root, BundledKind::Skill),
        BundledKind::Skill,
        name,
    )
}

pub fn skills_pull(project_root: &Path, feature_name: &str) -> Result<()> {
    super::claude_settings::require_feature(project_root, feature_name)?;

    let src = project_dir(project_root, BundledKind::Skill);
    if !src.is_dir() {
        return Err(PmError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no .claude/skills/ directory in main at {}", src.display()),
        )));
    }

    let dst = project_root
        .join(feature_name)
        .join(".claude")
        .join("skills");
    copy_dir_recursive(&src, &dst)
}

// --- Public API: Agents ---

pub fn agents_list(project_root: Option<&Path>) -> Result<Vec<String>> {
    list_both(BundledKind::Agent, project_root)
}

pub fn agents_uninstall(name: Option<&str>) -> Result<Vec<String>> {
    uninstall_in(&global_dir(BundledKind::Agent)?, BundledKind::Agent, name)
}

pub fn agents_uninstall_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    uninstall_in(
        &project_dir(project_root, BundledKind::Agent),
        BundledKind::Agent,
        name,
    )
}

pub fn agents_install(name: Option<&str>) -> Result<Vec<String>> {
    install_in(&global_dir(BundledKind::Agent)?, BundledKind::Agent, name)
}

pub fn agents_install_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    install_in(
        &project_dir(project_root, BundledKind::Agent),
        BundledKind::Agent,
        name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Shared install/list tests (exercise the unified logic) ---

    #[test]
    fn status_not_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let item = items_of_kind(BundledKind::Skill).next().unwrap();
        assert_eq!(status_label(&dir, item), "not installed");
    }

    #[test]
    fn install_by_name_and_check_status() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let messages = install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("pm"));

        let item = items_of_kind(BundledKind::Skill).next().unwrap();
        assert!(is_installed(&dir, item));
        assert!(is_up_to_date(&dir, item));
        assert_eq!(status_label(&dir, item), "installed");
    }

    #[test]
    fn install_all_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let count = items_of_kind(BundledKind::Skill).count();
        let messages = install_in(&dir, BundledKind::Skill, None).unwrap();
        assert_eq!(messages.len(), count);

        for item in items_of_kind(BundledKind::Skill) {
            assert!(is_installed(&dir, item));
        }
    }

    #[test]
    fn install_all_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let count = items_of_kind(BundledKind::Agent).count();
        let messages = install_in(&dir, BundledKind::Agent, None).unwrap();
        assert_eq!(messages.len(), count);

        for item in items_of_kind(BundledKind::Agent) {
            assert!(is_installed(&dir, item));
        }
    }

    #[test]
    fn install_nonexistent_skill_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let err = install_in(&dir, BundledKind::Skill, Some("nonexistent")).unwrap_err();
        assert!(matches!(err, PmError::SkillNotFound(_)));
    }

    #[test]
    fn install_nonexistent_agent_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let err = install_in(&dir, BundledKind::Agent, Some("nonexistent")).unwrap_err();
        assert!(matches!(err, PmError::AgentNotFound(_)));
    }

    #[test]
    fn install_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let first = install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(first[0].contains("Installed"));

        let second = install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(second[0].contains("already up to date"));
    }

    #[test]
    fn outdated_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        fs::write(dir.join("pm/SKILL.md"), "old content").unwrap();

        let item = items_of_kind(BundledKind::Skill).next().unwrap();
        assert!(is_installed(&dir, item));
        assert!(!is_up_to_date(&dir, item));
        assert_eq!(status_label(&dir, item), "outdated");
    }

    #[test]
    fn agent_install_and_check_status() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let messages = install_in(&dir, BundledKind::Agent, Some("reviewer")).unwrap();
        assert!(messages[0].contains("Installed Agent 'reviewer'"));

        let reviewer = items_of_kind(BundledKind::Agent)
            .find(|i| i.name == "reviewer")
            .unwrap();
        let implementer = items_of_kind(BundledKind::Agent)
            .find(|i| i.name == "implementer")
            .unwrap();
        assert_eq!(status_label(&dir, reviewer), "installed");
        assert_eq!(status_label(&dir, implementer), "not installed");
    }

    #[test]
    fn agent_install_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        install_in(&dir, BundledKind::Agent, Some("reviewer")).unwrap();
        let second = install_in(&dir, BundledKind::Agent, Some("reviewer")).unwrap();
        assert!(second[0].contains("already up to date"));
    }

    // --- Skills-specific tests ---

    #[test]
    fn install_project_writes_to_main_claude_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(project_root.join("main")).unwrap();

        let messages = skills_install_project(project_root, Some("pm")).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Installed"));

        let skill_path = project_root
            .join("main")
            .join(".claude")
            .join("skills")
            .join("pm")
            .join("SKILL.md");
        assert!(skill_path.exists());
    }

    #[test]
    fn pull_copies_skills_from_main_to_feature() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let features_dir = project_root.join(".pm").join("features");
        fs::create_dir_all(&features_dir).unwrap();
        fs::write(features_dir.join("my-feat.toml"), "branch = \"my-feat\"\n").unwrap();

        let main_skills = project_root.join("main").join(".claude").join("skills");
        fs::create_dir_all(main_skills.join("foo")).unwrap();
        fs::write(main_skills.join("foo").join("SKILL.md"), "skill content").unwrap();

        let feature_dir = project_root.join("my-feat");
        fs::create_dir_all(&feature_dir).unwrap();

        skills_pull(project_root, "my-feat").unwrap();

        let dst = feature_dir
            .join(".claude")
            .join("skills")
            .join("foo")
            .join("SKILL.md");
        assert!(dst.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "skill content");
    }

    #[test]
    fn pull_overwrites_existing_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let features_dir = project_root.join(".pm").join("features");
        fs::create_dir_all(&features_dir).unwrap();
        fs::write(features_dir.join("my-feat.toml"), "branch = \"my-feat\"\n").unwrap();

        let main_skills = project_root.join("main").join(".claude").join("skills");
        fs::create_dir_all(main_skills.join("foo")).unwrap();
        fs::write(main_skills.join("foo").join("SKILL.md"), "updated content").unwrap();

        let feature_skills = project_root
            .join("my-feat")
            .join(".claude")
            .join("skills")
            .join("foo");
        fs::create_dir_all(&feature_skills).unwrap();
        fs::write(feature_skills.join("SKILL.md"), "old content").unwrap();

        skills_pull(project_root, "my-feat").unwrap();

        let dst = feature_skills.join("SKILL.md");
        assert_eq!(fs::read_to_string(&dst).unwrap(), "updated content");
    }

    #[test]
    fn pull_errors_when_no_main_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let features_dir = project_root.join(".pm").join("features");
        fs::create_dir_all(&features_dir).unwrap();
        fs::write(features_dir.join("my-feat.toml"), "branch = \"my-feat\"\n").unwrap();

        fs::create_dir_all(project_root.join("main")).unwrap();
        fs::create_dir_all(project_root.join("my-feat")).unwrap();

        let err = skills_pull(project_root, "my-feat").unwrap_err();
        assert!(matches!(err, PmError::Io(_)));
    }

    #[test]
    fn pull_errors_when_feature_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(project_root.join(".pm").join("features")).unwrap();

        let err = skills_pull(project_root, "nonexistent").unwrap_err();
        assert!(matches!(err, PmError::FeatureNotFound(_)));
    }

    // --- Agents-specific tests ---

    // --- Uninstall tests ---

    #[test]
    fn uninstall_removes_skill_file_and_parent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(dir.join("pm/SKILL.md").exists());

        let messages = uninstall_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(messages[0].contains("Uninstalled Skill 'pm'"));
        assert!(!dir.join("pm/SKILL.md").exists());
        // Parent dir (pm/) should be cleaned up since it's now empty
        assert!(!dir.join("pm").exists());
    }

    #[test]
    fn uninstall_removes_agent_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        install_in(&dir, BundledKind::Agent, Some("reviewer")).unwrap();
        assert!(dir.join("reviewer.md").exists());

        let messages = uninstall_in(&dir, BundledKind::Agent, Some("reviewer")).unwrap();
        assert!(messages[0].contains("Uninstalled Agent 'reviewer'"));
        assert!(!dir.join("reviewer.md").exists());
    }

    #[test]
    fn uninstall_not_installed_returns_message() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let messages = uninstall_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(messages[0].contains("is not installed"));
    }

    #[test]
    fn uninstall_all_removes_everything() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        install_in(&dir, BundledKind::Agent, None).unwrap();
        let messages = uninstall_in(&dir, BundledKind::Agent, None).unwrap();

        for item in items_of_kind(BundledKind::Agent) {
            assert!(!is_installed(&dir, item));
        }
        assert!(messages.iter().all(|m| m.contains("Uninstalled")));
    }

    // --- Agents-specific tests ---

    #[test]
    fn agents_install_project_writes_to_main_claude_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(project_root.join("main")).unwrap();

        let messages = agents_install_project(project_root, Some("reviewer")).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Installed"));

        let agent_path = project_root
            .join("main")
            .join(".claude")
            .join("agents")
            .join("reviewer.md");
        assert!(agent_path.exists());
    }
}
