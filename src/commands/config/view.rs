//! The config files as a read sees them.

use std::path::PathBuf;

use crate::error::{PmError, Result};
use crate::state::project::{GlobalConfig, ProjectConfig};

use super::{Files, Tier};

/// One config file, read.
pub(super) struct TierFile {
    pub label: String,
    pub path: PathBuf,
    pub raw: toml::Table,
}

/// The files a read resolves across: the project's in view (none under
/// `--global`) and the global one.
pub(super) struct View {
    pub project: Option<(TierFile, ProjectConfig)>,
    pub global: (TierFile, GlobalConfig),
}

impl View {
    pub fn load(files: &Files, global_only: bool) -> Result<Self> {
        let project = match &files.project_root {
            Some(_) if !global_only => {
                let path = files.path(Tier::Project)?;
                if !path.exists() {
                    return Err(PmError::NotInProject);
                }
                let raw = read(&path)?;
                let config: ProjectConfig = toml::Value::Table(raw.clone()).try_into()?;
                let label = format!("project '{}'", config.project.name);
                Some((TierFile { label, path, raw }, config))
            }
            _ => None,
        };
        let path = files.path(Tier::Global)?;
        let raw = if path.exists() {
            read(&path)?
        } else {
            toml::Table::new()
        };
        let config: GlobalConfig = toml::Value::Table(raw.clone()).try_into()?;
        let global = TierFile {
            label: "global".to_string(),
            path,
            raw,
        };
        Ok(View {
            project,
            global: (global, config),
        })
    }

    /// The project's config, or an empty one when none is in view.
    pub fn project_config(&self) -> ProjectConfig {
        self.project.as_ref().map_or_else(
            || toml::from_str("[project]\nname = \"\"\n").expect("a minimal project config"),
            |(_, config)| config.clone(),
        )
    }
}

fn read(path: &std::path::Path) -> Result<toml::Table> {
    Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
}

/// The value at `path` in `table`.
pub(super) fn lookup<'t>(table: &'t toml::Table, path: &[&str]) -> Option<&'t toml::Value> {
    let (last, parents) = path.split_last()?;
    let mut table = table;
    for segment in parents {
        table = table.get(*segment)?.as_table()?;
    }
    table.get(*last)
}
