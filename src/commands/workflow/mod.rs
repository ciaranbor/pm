//! `pm workflow show` and `pm workflow list`.
//!
//! - `show` prints the scope's active `workflow.md` plus the appended
//!   `## Status and summary` guidance (so agents see it through the command
//!   they actually run), and in a shipping feature the `shipping` section.
//!   Used by the bundled
//!   `pm-workflow` skill so agents can discover their scope's routing
//!   at the start of every turn.
//! - `list` enumerates installed workflows across both tiers with one-line
//!   descriptions, tagged by tier and origin.

use std::path::Path;

use crate::commands::bundled_disable::Disabled;
use crate::error::{PmError, Result};
use crate::state::paths;
use crate::state::scope;
use crate::state::workflow::{self, WorkflowDef};

mod shipping;

/// Appended to every `pm workflow show` so the status and summary guidance
/// reaches the agents through the channel they actually use. Lives here
/// (not in each `workflow.md`) to keep a single source of truth rather than
/// duplicating the rule across every bundled workflow.
const SUMMARY_GUIDANCE: &str = "\
## Status and summary

Put every decision about this feature to the user in your own session,
never through `main`. While you wait, run
`pm feat status blocked -m \"<what you need>\"`. Their reply sets it back to
wip on its own; if it is still blocked once they have answered
(`pm feat status` shows it), run `pm feat status wip`.

If the active workflow names you the summary owner, write the summary at the
path `pm feat summary path` prints, editing it like any other file. It is
kept in pm state, never in the worktree, for `main`, which never saw the
feature, to triage after the branch is gone. If git history, the code, or
the merge already records something, leave it out; if it would be lost with
the branch, write it down. Not a running log: write it when you know what
the work leaves behind, and add to it if more emerges.

Open the summary with one or two plain sentences on a single unwrapped line,
before any heading, saying what the feature is and what it changed or found.
Write them for someone who doesn't know this project: describe things in
everyday words, not module, command, flag or internal feature names, unless
the name *is* the change. Then cover, in order of value:

- out-of-scope bugs, gaps, and ideas you did not fix — self-contained
  enough to act on without the branch
- decisions the brief didn't make or diverged from, with the reason
- durable gotchas: external constraints, verified behaviour, dead-ends
- user-facing consequences: breaking changes, required actions, migrations

Leave out review rounds and approvals, status lines, test counts or tool
output, file-by-file change lists, smoke-test walkthroughs, and narration of
how the work went (the baseline's `## Comments and docs` rule).

`pm feat status ready` means the work is done and reviewed and no decision
waits on the user. The summary owner runs it; pm tells `main` once no agent
of the team is busy, and `main` may message you about gaps: reply to it. Put
any decision it raises to the user first, and write into the summary
anything worth keeping. Run it again after every substantive summary change
(new work, a user's decision, an answer to `main`), not a wording fix; pm
decides whether `main` needs telling. Before more work, run
`pm feat status wip`. Message `main` only to answer its messages.
";

/// Resolve the active workflow for the current scope and return its
/// `workflow.md` content. Returns `Ok(None)` for a feature created without
/// a workflow.
pub fn show(project_root: &Path, scope: &str) -> Result<Option<String>> {
    let Some(workflow_name) = scope::workflow(project_root, scope)? else {
        return Ok(None);
    };

    let md_path = match workflow::workflow_md_path(project_root, &workflow_name) {
        Some(p) if p.is_file() => p,
        Some(p) => {
            return Err(PmError::WorkflowNotFound(format!(
                "{workflow_name} (workflow.md missing at {})",
                p.display()
            )));
        }
        None => {
            return Err(
                Disabled::load().explain(project_root, PmError::WorkflowNotFound(workflow_name))
            );
        }
    };
    let mut body = std::fs::read_to_string(&md_path)?;
    // Append the status and summary guidance so it reaches whoever runs the
    // command. A single blank line separates it from the workflow's
    // own prose regardless of how `workflow.md` ends.
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push('\n');
    body.push_str(SUMMARY_GUIDANCE);
    if scope != scope::MAIN
        && crate::commands::ship_gate::applies(project_root, Some(&workflow_name))
    {
        body.push('\n');
        body.push_str(shipping::SHIPPING_GUIDANCE);
    }
    Ok(Some(body))
}

/// Output of [`list_rows`]: stdout rows (successfully-parsed workflows)
/// plus stderr warnings (workflows whose `config.toml` failed to parse).
pub struct ListOutput {
    pub rows: Vec<String>,
    pub warnings: Vec<String>,
}

/// Build the column-aligned listing used by `pm workflow list`. Returns
/// one row per installed workflow, sorted by name and tagged
/// `[<tier>, bundled|user]` — `bundled` meaning pm-owned and rewritten by
/// `pm update`, and a project entry shadowing a same-named global one —
/// plus a warning per broken `config.toml` so users don't discover the
/// breakage only when `pm feat new --workflow <name>` fails. Outside a
/// project (`None`) only the global tier is listed. Main's workflow is
/// marked as not for features. A workflow whose team names an agent
/// `[bundled.disable]` lists, with no custom resolving it, is marked
/// unavailable.
pub fn list_rows(project_root: Option<&Path>) -> Result<ListOutput> {
    list_rows_in(
        project_root,
        &workflow::GlobalTier::resolve()?,
        paths::home_dir().ok().as_deref(),
        &Disabled::load(),
    )
}

/// [`list_rows`] against explicit tiers.
pub fn list_rows_in(
    project_root: Option<&Path>,
    global: &workflow::GlobalTier,
    home: Option<&Path>,
    disabled: &Disabled,
) -> Result<ListOutput> {
    let installed = workflow::list_installed_in(project_root, global)?;
    let resolves = |agent: &str| match project_root {
        Some(root) => workflow::definition_exists(root, agent, home),
        None => home.is_some_and(|h| workflow::global_definition_path(h, agent).exists()),
    };

    let max_name = installed
        .workflows
        .iter()
        .map(|w| w.name.len())
        .max()
        .unwrap_or(0);
    let tags: Vec<String> = installed
        .workflows
        .iter()
        .map(|w| {
            let origin = if w.tier == workflow::Tier::Global
                && crate::commands::skills::is_bundled_workflow(&w.name)
            {
                "bundled"
            } else {
                "user"
            };
            format!("[{}, {origin}]", w.tier)
        })
        .collect();
    let max_tag = tags.iter().map(|t| t.len()).max().unwrap_or(0);

    // Indent the optional "use when:" line to align under the description
    // column: 2 leading spaces + name column + 2 spaces + tag column + 2
    // spaces + "— " (2 chars).
    let hint_indent = " ".repeat(max_name + max_tag + 8);

    let mut rows = Vec::new();
    for (w, tag) in installed.workflows.iter().zip(&tags) {
        let mut row = format!(
            "  {:<name_w$}  {:<tag_w$}  — {}",
            w.name,
            tag,
            w.def.description,
            name_w = max_name,
            tag_w = max_tag,
        );
        if w.name == scope::MAIN_WORKFLOW {
            row.push_str(" (main's scope, not for features)");
        }
        if let Some(agent) = w
            .def
            .agents
            .iter()
            .find(|a| disabled.agent(a) && !resolves(a))
        {
            row.push_str(&format!(" (unavailable: agent '{agent}' disabled)"));
        }
        rows.push(row);
        if let Some(hint) = &w.def.when_to_use {
            rows.push(format!("{hint_indent}use when: {hint}"));
        }
    }

    let warnings = installed
        .errors
        .into_iter()
        .map(|(name, err)| format!("warning: skipping workflow '{name}' ({err})"))
        .collect();

    Ok(ListOutput { rows, warnings })
}

/// Look up a single workflow's definition for display (used by
/// `pm feat info` to show the workflow row alongside its description).
/// Returns `None` if the workflow isn't installed in either tier; an `Err`
/// only on a genuine I/O or parse failure for an installed workflow.
pub fn get(project_root: &Path, name: &str) -> Result<Option<WorkflowDef>> {
    match WorkflowDef::load(project_root, name) {
        Ok(def) => Ok(Some(def)),
        Err(PmError::WorkflowNotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests;
