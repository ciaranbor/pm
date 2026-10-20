//! Every agent definition a spawn can name: the reserved vanilla name, then
//! the files of the stores [`definition_paths`](super::definition_paths)
//! searches, the project's shadowing the global one's by name.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::Result;

use super::{VANILLA_AGENT, definition_stores, front_matter, is_vanilla};

/// What [`list_definitions`] gives `plain` for a description.
const VANILLA_DESCRIPTION: &str = "no definition: a vanilla session";

/// One definition a spawn can name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub name: String,
    /// Its front matter's `description`, if it has one.
    pub description: Option<String>,
}

/// `plain` first, then every definition file in the project's store and
/// the global one under `home`, sorted by name. A file whose name no agent
/// may take, or that a vanilla alias shadows, is left out: no spawn could
/// launch it.
pub fn list_definitions(project_root: &Path, home: Option<&Path>) -> Result<Vec<Definition>> {
    let mut found: BTreeMap<String, Option<String>> = BTreeMap::new();
    for store in definition_stores(project_root, home) {
        if !store.is_absolute() {
            continue;
        }
        let entries = match std::fs::read_dir(&store) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "md") || !path.is_file() {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if is_vanilla(name)
                || found.contains_key(name)
                || crate::messages::validate_name(name, "agent").is_err()
            {
                continue;
            }
            let description = description(&std::fs::read_to_string(&path)?);
            found.insert(name.to_string(), description);
        }
    }
    let vanilla = Definition {
        name: VANILLA_AGENT.to_string(),
        description: Some(VANILLA_DESCRIPTION.to_string()),
    };
    Ok(std::iter::once(vanilla)
        .chain(
            found
                .into_iter()
                .map(|(name, description)| Definition { name, description }),
        )
        .collect())
}

/// The `description` of `content`'s front matter.
fn description(content: &str) -> Option<String> {
    let (block, _) = front_matter(content)?;
    block.lines().find_map(|line| {
        let value = line.strip_prefix("description:")?.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        (!value.is_empty()).then(|| value.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::paths;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, content: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn plain_comes_first_and_the_project_tier_shadows_the_global_one() {
        let project = tempdir().unwrap();
        let home = tempdir().unwrap();
        let ours = paths::main_worktree(project.path()).join(".agents/agents");
        let global = home.path().join(".agents/agents");
        write(
            &global,
            "reviewer.md",
            "---\nname: reviewer\ndescription: global\n---\n",
        );
        write(
            &global,
            "auditor.md",
            "---\nname: auditor\ndescription: 'Audits'\n---\nx\n",
        );
        write(
            &ours,
            "reviewer.md",
            "---\nname: reviewer\ndescription: \"ours\"\n---\n",
        );
        write(
            &ours,
            "bare.md",
            "# no front matter\ndescription: not this\n",
        );
        write(
            &ours,
            "plain.md",
            "---\ndescription: shadowed by vanilla\n---\n",
        );
        write(&ours, "bad name.md", "---\ndescription: unspawnable\n---\n");
        write(&ours, "notes.txt", "not a definition");

        let listed = list_definitions(project.path(), Some(home.path())).unwrap();

        let pair = |d: &Definition| (d.name.clone(), d.description.clone());
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            listed.iter().map(pair).collect::<Vec<_>>(),
            vec![
                ("plain".into(), some(VANILLA_DESCRIPTION)),
                ("auditor".into(), some("Audits")),
                ("bare".into(), None),
                ("reviewer".into(), some("ours")),
            ]
        );
    }
}
