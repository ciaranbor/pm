use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::fs_utils::copy_dir_recursive;

struct BundledSkill {
    name: &'static str,
    content: &'static str,
}

const BUNDLED_SKILLS: &[BundledSkill] = &[BundledSkill {
    name: "pm",
    content: include_str!("../../skills/pm/SKILL.md"),
}];

fn skills_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or(PmError::NoHomeDir)?;
    Ok(home.join(".claude").join("skills"))
}

fn is_installed(skills_dir: &Path, name: &str) -> bool {
    skills_dir.join(name).join("SKILL.md").exists()
}

fn is_up_to_date(skills_dir: &Path, skill: &BundledSkill) -> bool {
    let path = skills_dir.join(skill.name).join("SKILL.md");
    match fs::read_to_string(&path) {
        Ok(installed) => installed == skill.content,
        Err(_) => false,
    }
}

fn skills_list_in(dir: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    for skill in BUNDLED_SKILLS {
        let status = if !is_installed(dir, skill.name) {
            "not installed"
        } else if is_up_to_date(dir, skill) {
            "installed"
        } else {
            "installed (outdated)"
        };
        lines.push(format!("  {} — {}", skill.name, status));
    }
    lines
}

fn skills_install_in(dir: &Path, name: Option<&str>) -> Result<Vec<String>> {
    let to_install: Vec<&BundledSkill> = match name {
        Some(n) => {
            let skill = BUNDLED_SKILLS
                .iter()
                .find(|s| s.name == n)
                .ok_or_else(|| PmError::SkillNotFound(n.to_string()))?;
            vec![skill]
        }
        None => BUNDLED_SKILLS.iter().collect(),
    };

    let mut messages = Vec::new();
    for skill in to_install {
        if is_up_to_date(dir, skill) {
            messages.push(format!("Skill '{}' is already up to date", skill.name));
            continue;
        }
        let skill_dir = dir.join(skill.name);
        fs::create_dir_all(&skill_dir)?;
        fs::write(skill_dir.join("SKILL.md"), skill.content)?;
        messages.push(format!("Installed skill '{}'", skill.name));
    }
    Ok(messages)
}

pub fn skills_list() -> Result<Vec<String>> {
    Ok(skills_list_in(&skills_dir()?))
}

pub fn skills_install(name: Option<&str>) -> Result<Vec<String>> {
    skills_install_in(&skills_dir()?, name)
}

pub fn skills_install_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    let dir = project_root.join("main").join(".claude").join("skills");
    skills_install_in(&dir, name)
}

pub fn skills_pull(project_root: &Path, feature_name: &str) -> Result<()> {
    super::claude_settings::require_feature(project_root, feature_name)?;

    let src = project_root.join("main").join(".claude").join("skills");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_not_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");
        let lines = skills_list_in(&dir);
        assert!(lines.iter().any(|l| l.contains("pm")));
        assert!(lines.iter().any(|l| l.contains("not installed")));
    }

    #[test]
    fn install_by_name_and_list() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");

        assert!(!is_installed(&dir, "pm"));

        let messages = skills_install_in(&dir, Some("pm")).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("pm"));

        assert!(is_installed(&dir, "pm"));
        assert!(is_up_to_date(&dir, &BUNDLED_SKILLS[0]));

        let lines = skills_list_in(&dir);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("installed") && !l.contains("not installed"))
        );
    }

    #[test]
    fn install_all() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");

        let messages = skills_install_in(&dir, None).unwrap();
        assert_eq!(messages.len(), BUNDLED_SKILLS.len());

        for skill in BUNDLED_SKILLS {
            assert!(is_installed(&dir, skill.name));
        }
    }

    #[test]
    fn install_nonexistent_skill_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");

        let err = skills_install_in(&dir, Some("nonexistent")).unwrap_err();
        assert!(matches!(err, PmError::SkillNotFound(_)));
    }

    #[test]
    fn install_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");

        let first = skills_install_in(&dir, Some("pm")).unwrap();
        assert!(first[0].contains("Installed"));

        let second = skills_install_in(&dir, Some("pm")).unwrap();
        assert!(second[0].contains("already up to date"));

        assert!(is_installed(&dir, "pm"));
        assert!(is_up_to_date(&dir, &BUNDLED_SKILLS[0]));
    }

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
        assert_eq!(
            fs::read_to_string(&skill_path).unwrap(),
            BUNDLED_SKILLS[0].content
        );
    }

    #[test]
    fn pull_copies_skills_from_main_to_feature() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        // Set up project structure with .pm/features and a feature state file
        let features_dir = project_root.join(".pm").join("features");
        fs::create_dir_all(&features_dir).unwrap();
        fs::write(features_dir.join("my-feat.toml"), "branch = \"my-feat\"\n").unwrap();

        // Set up main skills
        let main_skills = project_root.join("main").join(".claude").join("skills");
        fs::create_dir_all(main_skills.join("foo")).unwrap();
        fs::write(main_skills.join("foo").join("SKILL.md"), "skill content").unwrap();

        // Set up feature worktree dir
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

        // Set up main skills
        let main_skills = project_root.join("main").join(".claude").join("skills");
        fs::create_dir_all(main_skills.join("foo")).unwrap();
        fs::write(main_skills.join("foo").join("SKILL.md"), "updated content").unwrap();

        // Set up feature with stale skill
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

    #[test]
    fn outdated_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");

        skills_install_in(&dir, Some("pm")).unwrap();
        fs::write(dir.join("pm").join("SKILL.md"), "old content").unwrap();

        assert!(is_installed(&dir, "pm"));
        assert!(!is_up_to_date(&dir, &BUNDLED_SKILLS[0]));

        let lines = skills_list_in(&dir);
        assert!(lines.iter().any(|l| l.contains("outdated")));
    }
}
