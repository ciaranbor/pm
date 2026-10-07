//! How `pm agent restart --all` restarts what it planned: one agent of each
//! harness first, its canary, and that harness's other agents only once the
//! canary has come up ([`launch_check`](super::super::launch_check)). A
//! harness held before its startup — a keychain that does not answer, a
//! login screen — holds every agent of it the same way, so restarting the
//! rest would leave each of them not up too, out of reach until it clears;
//! they are skipped and keep running. A canary that exited holds back no
//! one: its own config is the likelier cause. The caller is never a
//! canary, and its restart still comes last.

use std::collections::HashMap;

use crate::harness::Harness;
use crate::state::project::GlobalConfig;

use super::super::agent_restart::agent_restart_many;
use super::super::launch_check::{FailedLaunch, Launch};
use super::{Batch, Report, Scope, harnesses_of, held_back, launches, record};

/// What confirms a set of launches
/// ([`confirm_all`](super::super::launch_check::confirm_all)).
pub(super) type Confirm<'a> = &'a dyn Fn(&[Launch]) -> Vec<FailedLaunch>;

/// Restart `planned` (each scope's agents, the caller's scope last),
/// canaries first: the batches in the order they ran, and a skip report
/// for each agent a canary held back.
pub(super) fn restart_planned(
    planned: Vec<(Scope, Vec<String>, bool)>,
    force: bool,
    tmux_server: Option<&str>,
    confirm: Confirm,
) -> (Vec<Batch>, Vec<Report>) {
    let global = GlobalConfig::load_or_default();
    let harnesses: Vec<HashMap<String, Harness>> = planned
        .iter()
        .map(|(scope, names, _)| harnesses_of(scope, names, &global, tmux_server))
        .collect();
    let mut canaries: Vec<(usize, &String, Harness)> = Vec::new();
    for (at, (_, names, _)) in planned.iter().enumerate() {
        for name in names {
            if let Some(&harness) = harnesses[at].get(name)
                && !canaries.iter().any(|(.., h)| *h == harness)
            {
                canaries.push((at, name, harness));
            }
        }
    }

    let mut batches: Vec<Batch> = canaries
        .iter()
        .map(|&(at, name, _)| {
            let scope = &planned[at].0;
            Batch {
                done: agent_restart_many(
                    &scope.root,
                    &scope.name,
                    std::slice::from_ref(name),
                    force,
                    false,
                    tmux_server,
                ),
                scope: scope.clone(),
                confirmed: false,
            }
        })
        .collect();
    let failed = confirm(&launches(&batches));
    let held: Vec<(Harness, String)> = canaries
        .iter()
        .filter_map(|&(at, name, harness)| {
            let failure = failed.iter().find(|f| {
                let scope = &planned[at].0;
                f.not_up()
                    && f.launch.agent == *name
                    && f.launch.project_root == scope.root
                    && f.launch.scope == scope.name
            })?;
            let cause = if failure.output.is_empty() {
                "it has drawn nothing"
            } else {
                "its window shows a screen of its own"
            };
            Some((
                harness,
                format!(
                    "not restarted, since {}'s agent '{name}', also on {harness}, did not come \
                     up after its restart ({cause})",
                    planned[at].0.label()
                ),
            ))
        })
        .collect();
    record(&mut batches, failed);

    let mut skipped = Vec::new();
    for (at, (scope, names, _)) in planned.iter().enumerate() {
        let mut rest = Vec::new();
        for name in names {
            if canaries.iter().any(|&(c, n, _)| c == at && n == name) {
                continue;
            }
            let why = harnesses[at]
                .get(name)
                .and_then(|harness| held.iter().find(|(h, _)| h == harness));
            match why {
                Some((_, why)) => skipped.push(held_back(scope, name, why)),
                None => rest.push(name.clone()),
            }
        }
        if !rest.is_empty() {
            batches.push(Batch {
                done: agent_restart_many(
                    &scope.root,
                    &scope.name,
                    &rest,
                    force,
                    false,
                    tmux_server,
                ),
                scope: scope.clone(),
                confirmed: false,
            });
        }
    }
    (batches, skipped)
}
