//! `pm config set` and `unset`: one key of one config file, edited in place.

use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use crate::commands::bundled_disable::Disabled;
use crate::commands::harness_check;
use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;
use crate::harness::Harness;
use crate::messages::validate_name;
use crate::state::project::{
    GlobalConfig, ProjectConfig, Shape, WILDCARD_AGENT, resolve_agent_settings,
    resolve_harness_config,
};

use super::view::View;
use super::{Files, Key, Tier, pickup_note, shape};

/// Set `key` to `raw` in the `tier` file, returning what to print.
pub fn set(files: &Files, tier: Tier, key: &str, raw: &str) -> Result<Vec<String>> {
    let key = settable(key, tier)?;
    if let Some((table, agent)) = key.agent_row() {
        check_agent_row(table, agent, raw)?;
    }
    let value = to_value(key.text, key_shape(&key, tier), raw)?;
    let shown = value.to_string().trim().to_string();
    let path = files.path(tier)?;
    let mut doc = read_doc(&path, tier)?;
    insert(&mut doc, &key, value)?;
    write_checked(&path, tier, &doc, key.text)?;
    let label = tier_label(&doc, tier);

    let mut out = vec![format!("{label}: {} = {shown}", key.text)];
    out.extend(warnings(files, &key)?);
    out.extend(pickup_note(key.path[0]).map(str::to_string));
    Ok(out)
}

/// Remove `key` from the `tier` file, returning what to print.
pub fn unset(files: &Files, tier: Tier, key: &str) -> Result<Vec<String>> {
    let key = settable(key, tier)?;
    let path = files.path(tier)?;
    let mut doc = read_doc(&path, tier)?;
    let (leaf, parents) = key.path.split_last().expect("a key has a segment");
    let removed = item_mut(&mut doc, parents)
        .and_then(Item::as_table_like_mut)
        .and_then(|table| table.remove(leaf));
    if removed.is_none() {
        return Ok(vec![format!(
            "{}: {} was not set",
            tier_label(&doc, tier),
            key.text
        )]);
    }
    prune(&mut doc, &key.path, tier);
    write_checked(&path, tier, &doc, key.text)?;
    let label = tier_label(&doc, tier);

    let mut out = vec![format!("{label}: unset {}", key.text)];
    out.extend(warnings(files, &key)?);
    out.extend(pickup_note(key.path[0]).map(str::to_string));
    Ok(out)
}

/// `text` as a key the `tier` file can hold one value at.
fn settable(text: &str, tier: Tier) -> Result<Key<'_>> {
    let key = Key::parse(text)?;
    if key.is_table() {
        return Err(PmError::Config(format!(
            "'{text}' is a table; set one of its keys:\n  {}",
            key.keys_below()
        )));
    }
    if key.path == ["project", "name"] {
        return Err(PmError::Config(
            "project.name is the name the registry knows the project by; it can't be changed \
             here"
                .to_string(),
        ));
    }
    if !key.in_tier(tier) {
        let fix = match tier {
            Tier::Project => "is global only: pass --global",
            Tier::Global => "is project only: drop --global",
        };
        return Err(PmError::Config(format!("'{text}' {fix}")));
    }
    Ok(key)
}

fn key_shape<'k>(key: &'k Key<'_>, tier: Tier) -> &'k Shape {
    match tier {
        Tier::Project => key.project.as_ref(),
        Tier::Global => key.global.as_ref(),
    }
    .expect("settable checked the tier")
}

/// Refuse a row a spawn can never use: a key that can't name an agent, or
/// a harness pm doesn't support. Model ids and permission modes are the
/// harness's own vocabulary and pass through.
fn check_agent_row(table: &str, agent: &str, raw: &str) -> Result<()> {
    if agent != WILDCARD_AGENT {
        validate_name(agent, "agent")?;
    }
    if table == "harness" && !raw.is_empty() {
        raw.parse::<Harness>()?;
    }
    Ok(())
}

/// `raw` as a TOML value of `shape`.
fn to_value(key: &str, shape: &Shape, raw: &str) -> Result<Value> {
    let invalid = |expected: &str| PmError::Config(format!("{key} takes {expected}, not '{raw}'"));
    Ok(match shape {
        Shape::String => Value::from(raw),
        Shape::Bool => match raw {
            "true" => Value::from(true),
            "false" => Value::from(false),
            _ => return Err(invalid("true or false")),
        },
        Shape::Integer => Value::from(raw.parse::<i64>().map_err(|_| invalid("a whole number"))?),
        Shape::Float => Value::from(raw.parse::<f64>().map_err(|_| invalid("a number"))?),
        Shape::List(element) => {
            if raw.trim_start().starts_with('[') {
                match raw.parse::<Value>() {
                    Ok(array @ Value::Array(_)) => array,
                    _ => return Err(invalid("a TOML array")),
                }
            } else {
                let mut array = toml_edit::Array::new();
                for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    array.push(to_value(key, element, item)?);
                }
                Value::Array(array)
            }
        }
        Shape::Map(_) => match raw.parse::<Value>() {
            Ok(table @ Value::InlineTable(_)) => table,
            _ => return Err(invalid("an inline table like '{ package = \"…\" }'")),
        },
        Shape::Any => raw.parse::<Value>().unwrap_or_else(|_| Value::from(raw)),
        Shape::Table(_) => unreachable!("settable refuses tables"),
    })
}

fn read_doc(path: &std::path::Path, tier: Tier) -> Result<DocumentMut> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse()
            .map_err(|e| PmError::Config(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && tier == Tier::Global => {
            Ok(DocumentMut::new())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(PmError::NotInProject),
        Err(e) => Err(e.into()),
    }
}

fn item_mut<'d>(doc: &'d mut DocumentMut, path: &[&str]) -> Option<&'d mut Item> {
    let mut item = doc.as_item_mut();
    for segment in path {
        item = item.as_table_like_mut()?.get_mut(segment)?;
    }
    Some(item)
}

/// Put `value` at `key`, creating the tables above it.
fn insert(doc: &mut DocumentMut, key: &Key<'_>, value: Value) -> Result<()> {
    let (leaf, parents) = key.path.split_last().expect("a key has a segment");
    let mut item = doc.as_item_mut();
    for (depth, segment) in parents.iter().enumerate() {
        let inline = item.is_inline_table();
        let not_table = || {
            PmError::Config(format!(
                "{} is not a table in the file",
                key.path[..=depth].join(".")
            ))
        };
        let table = item.as_table_like_mut().ok_or_else(not_table)?;
        if table.get(segment).is_none() {
            let child = if inline {
                Item::Value(Value::InlineTable(InlineTable::new()))
            } else {
                let mut table = Table::new();
                table.set_implicit(true);
                Item::Table(table)
            };
            table.insert(segment, child);
        }
        item = table.get_mut(segment).expect("just inserted");
    }
    let parent = item.as_table_like_mut().ok_or_else(|| {
        PmError::Config(format!("{} is not a table in the file", parents.join(".")))
    })?;
    parent.insert(leaf, Item::Value(value));
    Ok(())
}

/// Drop the tables an unset left empty along `path`, up to the first that
/// isn't, keeping a user-keyed entry (an empty provider masks the global
/// one).
fn prune(doc: &mut DocumentMut, path: &[&str], tier: Tier) {
    let shape = shape(tier);
    for depth in (1..path.len()).rev() {
        let prunable = match shape.at(&path[..depth]) {
            Some(Shape::Table(_)) => true,
            Some(Shape::Map(_)) => !matches!(shape.at(&path[..depth - 1]), Some(Shape::Map(_))),
            _ => false,
        };
        let empty = item_mut(doc, &path[..depth])
            .and_then(|item| item.as_table_like())
            .is_some_and(|table| table.is_empty());
        if !prunable || !empty {
            return;
        }
        if let Some(parent) = item_mut(doc, &path[..depth - 1]).and_then(Item::as_table_like_mut) {
            parent.remove(path[depth - 1]);
        }
    }
}

/// Write `doc` to `path` if it still loads as the `tier` struct.
fn write_checked(path: &std::path::Path, tier: Tier, doc: &DocumentMut, key: &str) -> Result<()> {
    let text = doc.to_string();
    let loaded = match tier {
        Tier::Project => toml::from_str::<ProjectConfig>(&text).map(drop),
        Tier::Global => toml::from_str::<GlobalConfig>(&text).map(drop),
    };
    loaded.map_err(|e| PmError::Config(format!("{key}: {}", e.message())))?;
    write_atomic(path, text.as_bytes())
}

/// How a report names the `tier` file `doc` holds.
fn tier_label(doc: &DocumentMut, tier: Tier) -> String {
    match (
        tier,
        doc.get("project")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str()),
    ) {
        (Tier::Project, Some(name)) => format!("project '{name}'"),
        (Tier::Project, None) => "project".to_string(),
        (Tier::Global, _) => "global".to_string(),
    }
}

/// What a spawn or install would say about the files as they now stand.
fn warnings(files: &Files, key: &Key<'_>) -> Result<Vec<String>> {
    let warn = |text: String| format!("warning: {text}");
    if key.path[0] == "bundled" {
        let unknown = Disabled::load_in(&files.global_dir)?.unknown();
        return Ok(unknown
            .into_iter()
            .map(|item| warn(format!("[bundled.disable] {item} names nothing pm bundles")))
            .collect());
    }
    let Some((_, agent)) = key.agent_row() else {
        return Ok(Vec::new());
    };
    let view = View::load(files, false)?;
    let project = view.project_config();
    let who = if agent == WILDCARD_AGENT {
        "agents without a row of their own".to_string()
    } else {
        format!("agent '{agent}'")
    };
    let settings = match resolve_agent_settings(&project.agents, &view.global.1.agents, agent) {
        Ok(settings) => settings,
        Err(e) => return Ok(vec![warn(format!("{who}: {e}"))]),
    };
    let missing = harness_check::missing_model_row(&settings);
    let has_missing = missing.is_some();
    let harness_config = resolve_harness_config(&project.harness, &view.global.1.harness);
    let notes = settings
        .notes
        .iter()
        .filter(|note| !has_missing || Some(*note) != settings.dropped_model.as_ref())
        .cloned()
        .chain(missing.map(|dropped| {
            format!(
                "runs on {} and has no [agents.models] row, so it will not spawn{dropped}",
                settings.harness
            )
        }))
        .chain(
            harness_check::row_issues(&settings)
                .into_iter()
                .map(|issue| format!("will not spawn: {issue}")),
        )
        .chain(
            settings
                .harness
                .row_notes(&harness_config, settings.model.as_deref()),
        );
    Ok(notes.map(|note| warn(format!("{who}: {note}"))).collect())
}
