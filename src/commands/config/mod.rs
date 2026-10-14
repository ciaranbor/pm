//! `pm config`: read and change the settings of the global `config.toml` and
//! a project's `.pm/config.toml` by command. The config structs are the one
//! list of keys and their types ([`shape_of`]). A write edits the file in
//! place, keeping its comments and other content, and is refused unless the
//! file still loads as its struct afterwards; `[agents.*]` rows get the
//! checks a spawn makes, reported as warnings since a valid setup can take
//! several writes to reach.
//!
//! A key the structs give both files is written to the project's unless
//! `--global` says otherwise, and read as a spawn resolves it: project over
//! global, an `[agents.*]` row through the `"*"` row and its harness
//! binding ([`resolve_agent_settings`]), `[harness.*]` through
//! [`resolve_harness_config`].
//!
//! [`resolve_agent_settings`]: crate::state::project::resolve_agent_settings
//! [`resolve_harness_config`]: crate::state::project::resolve_harness_config

mod get;
mod list;
mod set;
mod view;

use std::path::PathBuf;

use crate::error::{PmError, Result};
use crate::state::paths;
use crate::state::project::{CodexConfig, GlobalConfig, ProjectConfig, Shape, shape_of};

pub use get::{Setting, get};
pub use list::list;
pub use set::{set, unset};

const CONFIG_FILE: &str = "config.toml";

/// The config files a command reads: the project's in view, if any, and the
/// global one.
pub struct Files {
    pub project_root: Option<PathBuf>,
    pub global_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Project,
    Global,
}

impl Files {
    fn path(&self, tier: Tier) -> Result<PathBuf> {
        match tier {
            Tier::Global => Ok(self.global_dir.join(CONFIG_FILE)),
            Tier::Project => self
                .project_root
                .as_ref()
                .map(|root| paths::pm_dir(root).join(CONFIG_FILE))
                .ok_or(PmError::NotInProject),
        }
    }
}

fn shape(tier: Tier) -> Shape {
    match tier {
        Tier::Project => {
            let mut shape = shape_of::<ProjectConfig>();
            drop_fields(&mut shape, &["harness", "codex"], CodexConfig::GLOBAL_ONLY);
            shape
        }
        Tier::Global => shape_of::<GlobalConfig>(),
    }
}

/// Remove `names` from the table at `path`: keys the struct shares with
/// the global file but only the global one is read for.
fn drop_fields(shape: &mut Shape, path: &[&str], names: &[&str]) {
    let Shape::Table(fields) = shape else {
        return;
    };
    match path.split_first() {
        None => fields.retain(|(name, _)| !names.contains(name)),
        Some((first, rest)) => {
            if let Some((_, inner)) = fields.iter_mut().find(|(name, _)| name == first) {
                drop_fields(inner, rest, names);
            }
        }
    }
}

/// A key the structs know, split into its path.
struct Key<'a> {
    text: &'a str,
    path: Vec<&'a str>,
    /// Its shape in each tier that has it.
    project: Option<Shape>,
    global: Option<Shape>,
    /// Whether it names an entry of a user-keyed table (an agent row, a
    /// provider) rather than a fixed key.
    entry: bool,
}

impl<'a> Key<'a> {
    fn parse(text: &'a str) -> Result<Self> {
        let path: Vec<&str> = text.split('.').collect();
        if path.iter().any(|segment| segment.is_empty()) {
            return Err(PmError::Config(format!(
                "invalid key '{text}': dotted names like agents.models.reviewer"
            )));
        }
        let (p, g) = (shape(Tier::Project), shape(Tier::Global));
        let project = p.at(&path).cloned();
        let global = g.at(&path).cloned();
        if project.is_none() && global.is_none() {
            return Err(unknown_key(text, &path, &p, &g));
        }
        let parent = &path[..path.len() - 1];
        let entry = [&p, &g]
            .iter()
            .any(|s| matches!(s.at(parent), Some(Shape::Map(_))));
        Ok(Key {
            text,
            path,
            project,
            global,
            entry,
        })
    }

    fn shape(&self) -> &Shape {
        self.project
            .as_ref()
            .or(self.global.as_ref())
            .expect("a parsed key has a shape in some tier")
    }

    /// Whether it names a table of fixed or user-chosen keys rather than one
    /// setting.
    fn is_table(&self) -> bool {
        match self.shape() {
            Shape::Table(_) => true,
            Shape::Map(_) => !self.entry,
            _ => false,
        }
    }

    fn in_tier(&self, tier: Tier) -> bool {
        match tier {
            Tier::Project => self.project.is_some(),
            Tier::Global => self.global.is_some(),
        }
    }

    /// The agent definition an `[agents.*]` row is for, if it is one.
    fn agent_row(&self) -> Option<(&'a str, &'a str)> {
        match self.path[..] {
            ["agents", table, agent] => Some((table, agent)),
            _ => None,
        }
    }

    /// Every settable key under this one, as [`keys_under`] lists them.
    fn keys_below(&self) -> String {
        keys_under(&self.path, &shape(Tier::Project), &shape(Tier::Global)).join("\n  ")
    }
}

/// The error for a key no struct has, listing the keys of the nearest table
/// it is under.
fn unknown_key(text: &str, path: &[&str], project: &Shape, global: &Shape) -> PmError {
    let known = (0..path.len()).rev().find(|&depth| {
        [project, global]
            .iter()
            .any(|s| matches!(s.at(&path[..depth]), Some(Shape::Table(_))))
    });
    let prefix = &path[..known.unwrap_or(0)];
    let under = if prefix.is_empty() {
        String::new()
    } else {
        format!(" under {}", prefix.join("."))
    };
    PmError::Config(format!(
        "unknown key '{text}'; the keys{under} are:\n  {}",
        keys_under(prefix, project, global).join("\n  ")
    ))
}

/// The settable keys under `prefix`, the global file's first, each marked
/// when only one file has it.
fn keys_under(prefix: &[&str], project: &Shape, global: &Shape) -> Vec<String> {
    let text = prefix.join(".");
    let list = |s: &Shape| s.at(prefix).map(|s| s.keys(&text)).unwrap_or_default();
    let (p, g) = (list(project), list(global));
    let mut keys: Vec<String> = g
        .iter()
        .map(|k| {
            if p.contains(k) {
                k.clone()
            } else {
                format!("{k} (global only)")
            }
        })
        .collect();
    keys.extend(
        p.iter()
            .filter(|k| !g.contains(k))
            .map(|k| format!("{k} (project only)")),
    );
    keys
}

/// What a change to settings under `section` reaches, for the line after a
/// write.
fn pickup_note(section: &str) -> Option<&'static str> {
    match section {
        "agents" | "harness" => Some(
            "Running agents pick this up when restarted (`pm agent restart`); `pm update` \
             restarts the idle ones it leaves stale.",
        ),
        "bundled" => Some("Takes effect at the next `pm update`."),
        "serve" => Some("A running `pm serve` picks this up when restarted."),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
