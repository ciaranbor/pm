use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};

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
