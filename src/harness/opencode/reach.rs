//! Which providers pm's opencode agents can reach, and what in opencode's
//! own config needs one they cannot. An agent reaches the providers pm
//! config defines and the one its own row names (`enabled_providers`), so a
//! provider defined only in the user's opencode config, and a definition
//! whose `model` names one, fail every turn with `Model unavailable` — in a
//! subagent, where only the parent agent sees it, and paraphrased.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::ModelRef;

/// The directories under a config directory opencode reads definitions
/// from, recursively (`{agent,agents}/**/*.md`, opencode 2.0.23).
const DEFINITION_DIRS: &[&str] = &["agent", "agents"];

/// Which providers pm's opencode agents can reach: every one pm config
/// defines, and each row's own for the agents on that row.
pub(super) struct Reach<'a> {
    pub(super) defined: &'a [&'a str],
    pub(super) rows: &'a [&'a str],
}

impl Reach<'_> {
    fn defined(&self, id: &str) -> bool {
        self.defined.contains(&id)
    }

    /// A remark for each provider a config document `shown` defines that
    /// no pm agent can reach.
    pub(super) fn unreachable_providers(&self, info: &Value, shown: &str) -> Vec<String> {
        info["providers"]
            .as_object()
            .into_iter()
            .flat_map(|providers| providers.keys())
            .filter(|id| !self.defined(id) && !self.rows.contains(&id.as_str()))
            .map(|id| {
                format!(
                    "opencode provider '{id}' is defined only in {shown}, so no pm agent or \
                     subagent can use it: an opencode agent reaches only the providers pm \
                     config defines and the one its own [agents.models] row names; define it \
                     as [harness.opencode.providers.{id}] to make it reachable"
                )
            })
            .collect()
    }

    /// A remark for each definition in config directory `dir` whose `model`
    /// names a provider pm config does not define.
    pub(super) fn unreachable_definitions(&self, dir: &Path) -> Vec<String> {
        let mut files = Vec::new();
        for name in DEFINITION_DIRS {
            markdown_files(&dir.join(name), &mut files);
        }
        files.sort();
        let mut notes = Vec::new();
        for file in files {
            let Some(row) = std::fs::read_to_string(&file)
                .ok()
                .and_then(|text| frontmatter_model(&text))
            else {
                continue;
            };
            let Ok(model) = ModelRef::parse(&row) else {
                continue;
            };
            if self.defined(model.provider) {
                continue;
            }
            let who = if self.rows.contains(&model.provider) {
                format!(
                    "only an agent whose own [agents.models] row is on '{}' can run it",
                    model.provider
                )
            } else {
                "no pm agent can run it".to_string()
            };
            notes.push(format!(
                "opencode definition {} runs on '{row}', and pm config does not define \
                 provider '{}', so {who}: as a subagent it fails with `Model unavailable`; \
                 define it as [harness.opencode.providers.{}]",
                crate::path_utils::to_portable(&file),
                model.provider,
                model.provider
            ));
        }
        notes
    }
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.filter_map(|entry| Some(entry.ok()?.path())) {
        if path.is_dir() {
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

/// The `model:` of a markdown definition's frontmatter.
fn frontmatter_model(text: &str) -> Option<String> {
    let rest = text.strip_prefix("---")?;
    let (front, _) = rest.split_once("\n---")?;
    front.lines().find_map(|line| {
        let value = line.strip_prefix("model:")?.trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'');
        (!value.is_empty()).then(|| value.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_are_found_under_both_dir_names_at_any_depth() {
        let dir = tempfile::tempdir().unwrap();
        for (file, model) in [
            ("agents/top.md", "model: far/one"),
            ("agent/nested/deep.md", "model: \"far/two\""),
            ("agent/near.md", "model: near/three"),
            ("agents/none.md", "description: no model"),
            ("agents/body.md", "description: x\n---\nmodel: far/body"),
        ] {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("---\n{model}\n---\nbody\n")).unwrap();
        }
        let reach = Reach {
            defined: &["near"],
            rows: &[],
        };
        let notes = reach.unreachable_definitions(dir.path());
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[0].contains("agent/nested/deep.md runs on 'far/two'"),
            "{notes:?}"
        );
        assert!(
            notes[1].contains("agents/top.md runs on 'far/one'"),
            "{notes:?}"
        );
    }
}
