//! `pm config list`: every setting the files hold, resolved as `get` does.

use std::collections::BTreeSet;

use crate::error::Result;
use crate::state::project::Shape;

use super::get::resolve;
use super::view::View;
use super::{Files, Key, Tier, shape};

/// One line per file read, then each key set in either, under `prefix` when
/// given, with its effective value.
pub fn list(files: &Files, global_only: bool, prefix: Option<&str>) -> Result<Vec<String>> {
    let prefix = prefix.map(Key::parse).transpose()?;
    let view = View::load(files, global_only)?;
    let mut out: Vec<String> = view
        .project
        .iter()
        .map(|(file, _)| file)
        .chain([&view.global.0])
        .map(|file| format!("# {}: {}", file.label, file.path.display()))
        .collect();

    let mut keys = BTreeSet::new();
    if let Some((file, _)) = &view.project {
        set_keys(&file.raw, &shape(Tier::Project), "", &mut keys);
    }
    set_keys(&view.global.0.raw, &shape(Tier::Global), "", &mut keys);
    let under = |key: &String| {
        prefix.as_ref().is_none_or(|p| {
            key == p.text
                || key
                    .strip_prefix(p.text)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
    };
    let settings: Vec<String> = keys
        .iter()
        .filter(|key| under(key))
        .filter_map(|key| Key::parse(key).ok())
        .map(|key| resolve(&view, &key).to_string())
        .collect();
    if settings.is_empty() {
        out.push("no settings".to_string());
    }
    out.extend(settings);
    Ok(out)
}

/// The keys `table` sets that `shape` knows: each leaf, and each entry of a
/// user-keyed table whole.
fn set_keys(table: &toml::Table, shape: &Shape, prefix: &str, keys: &mut BTreeSet<String>) {
    let Shape::Table(fields) = shape else {
        return;
    };
    for (name, field) in fields {
        let Some(value) = table.get(*name) else {
            continue;
        };
        let key = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}.{name}")
        };
        match (field, value.as_table()) {
            (Shape::Table(_), Some(inner)) => set_keys(inner, field, &key, keys),
            (Shape::Map(_), Some(inner)) => {
                keys.extend(inner.keys().map(|entry| format!("{key}.{entry}")));
            }
            (Shape::Table(_) | Shape::Map(_), None) => {}
            _ => {
                keys.insert(key);
            }
        }
    }
}
