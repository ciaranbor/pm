//! `pm config get`: one setting's effective value and where it came from.

use crate::error::{PmError, Result};
use crate::state::project::{resolve_agent_settings, resolve_harness_config, winning_row};

use super::view::{TierFile, View, lookup};
use super::{Files, Key};

/// A setting as a read resolves it.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub key: String,
    /// The effective value; `None` when unset.
    pub value: Option<toml::Value>,
    /// Where the value came from, or why it is unset.
    pub source: Option<String>,
    /// What a spawn would say about it.
    pub notes: Vec<String>,
}

impl std::fmt::Display for Setting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.value, &self.source) {
            (Some(value), Some(source)) => write!(f, "{} = {value}  ({source})", self.key)?,
            (Some(value), None) => write!(f, "{} = {value}", self.key)?,
            (None, Some(source)) => write!(f, "{} is unset ({source})", self.key)?,
            (None, None) => write!(f, "{} is unset", self.key)?,
        }
        for note in &self.notes {
            write!(f, "\n  {note}")?;
        }
        Ok(())
    }
}

/// `key`'s effective value: across the project in view and the global
/// file, or the global tier alone with `global_only`.
pub fn get(files: &Files, key: &str, global_only: bool) -> Result<Setting> {
    let key = Key::parse(key)?;
    if key.is_table() {
        return Err(PmError::Config(format!(
            "'{}' is a table: `pm config list {}` shows its settings",
            key.text, key.text
        )));
    }
    let view = View::load(files, global_only)?;
    Ok(resolve(&view, &key))
}

pub(super) fn resolve(view: &View, key: &Key<'_>) -> Setting {
    let setting = |value: Option<&toml::Value>, source: Option<String>| Setting {
        key: key.text.to_string(),
        value: value.cloned(),
        source,
        notes: Vec::new(),
    };
    let project = view
        .project
        .as_ref()
        .map(|(file, _)| file)
        .filter(|_| key.project.is_some());
    let global = key.global.is_some().then_some(&view.global.0);
    if let Some((table, agent)) = key.agent_row() {
        return agent_row(view, key, table, agent);
    }
    if key.path[0] == "harness" {
        return harness(view, key, project);
    }
    for file in [project, global].into_iter().flatten() {
        if let Some(value) = lookup(&file.raw, &key.path) {
            return setting(Some(value), Some(file.label.clone()));
        }
    }
    setting(None, None)
}

/// An `[agents.*]` row as a spawn of that agent resolves it: the row
/// [`winning_row`] settles on, unset when the agent's harness drops it.
fn agent_row(view: &View, key: &Key<'_>, table: &str, agent: &str) -> Setting {
    let mut setting = Setting {
        key: key.text.to_string(),
        value: None,
        source: None,
        notes: Vec::new(),
    };
    let project = view.project_config().agents;
    let global = &view.global.1.agents;
    let (Some(project_rows), Some(global_rows)) = (project.rows(table), global.rows(table)) else {
        return setting;
    };
    let settings = resolve_agent_settings(&project, global, agent);
    let Some((tier, name, row)) = winning_row(project_rows, global_rows, agent) else {
        if let Ok(settings) = settings
            && table == "harness"
        {
            setting.source = Some(format!("default: {}", settings.harness));
        }
        return setting;
    };
    let label = match (tier, &view.project) {
        ("project", Some((file, _))) => file.label.as_str(),
        _ => view.global.0.label.as_str(),
    };
    let from = if name == agent {
        label.to_string()
    } else {
        format!("{label}, row '{name}'")
    };
    if row.is_empty() {
        setting.source = Some(format!("{from} sets it empty"));
        return setting;
    }
    let settings = match settings {
        Ok(settings) => settings,
        Err(e) => {
            setting.value = Some(toml::Value::String(row.clone()));
            setting.source = Some(from);
            setting.notes.push(e.to_string());
            return setting;
        }
    };
    let effective = match table {
        "models" => settings.model,
        "permissions" => settings.permission_mode,
        _ => Some(settings.harness.to_string()),
    };
    match effective {
        Some(value) => {
            setting.value = Some(toml::Value::String(value));
            setting.source = Some(from);
        }
        None => {
            let table = format!("[agents.{table}]");
            setting.source = settings
                .notes
                .into_iter()
                .find(|note| note.contains(&table))
                .or(Some(from));
        }
    }
    setting
}

/// A `[harness.*]` key as [`resolve_harness_config`] resolves it.
fn harness(view: &View, key: &Key<'_>, project: Option<&TierFile>) -> Setting {
    let resolved = resolve_harness_config(&view.project_config().harness, &view.global.1.harness);
    let effective = toml::Value::try_from(&resolved)
        .ok()
        .and_then(|value| value.as_table().cloned())
        .and_then(|table| lookup(&table, &key.path[1..]).cloned());
    let set_in = [project, Some(&view.global.0)]
        .into_iter()
        .flatten()
        .find(|file| lookup(&file.raw, &key.path).is_some());
    let source = set_in.map(|file| match effective {
        Some(_) => file.label.clone(),
        None => format!("{} sets it empty", file.label),
    });
    Setting {
        key: key.text.to_string(),
        value: effective,
        source,
        notes: Vec::new(),
    }
}
