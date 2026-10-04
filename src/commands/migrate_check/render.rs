//! The report as text: a verdict, the plan that clears every blocker, one
//! line per subject in each section, and a checklist of manual steps.
//! Colour and symbols only on a terminal.

use std::io::IsTerminal;

use super::line::count;
use super::plan::{self, PlanStep, Step};
use super::{Finding, Report, Section, SectionKind, Severity, THIS_HOST};

/// The widest subject column; a longer subject pushes its line out.
const SUBJECT_WIDTH: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub color: bool,
}

impl Style {
    /// Colour when stdout is a terminal and `NO_COLOR` is unset.
    pub fn detect() -> Self {
        Self {
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn paint(self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn blocker(self) -> String {
        if self.color {
            self.paint("31", "✗")
        } else {
            "x".to_string()
        }
    }

    fn todo(self) -> String {
        if self.color {
            self.paint("33", "☐")
        } else {
            "[ ]".to_string()
        }
    }

    fn ok(self) -> String {
        if self.color {
            self.paint("32", "✓ ok")
        } else {
            "ok".to_string()
        }
    }

    fn note(self, text: &str) -> String {
        let bullet = if self.color { "·" } else { "-" };
        self.paint("2", &format!("{bullet} {text}"))
    }

    fn bold(self, text: &str) -> String {
        self.paint("1", text)
    }

    fn cmd(self, text: &str) -> String {
        self.paint("36", text)
    }
}

pub(super) fn lines(report: &Report, style: Style, verbose: bool) -> Vec<String> {
    let mut out = verdict(report, style);
    let plan = plan::plan(report);
    if !plan.is_empty() {
        out.push(String::new());
        out.extend(plan_lines(report, &plan, style));
    }
    for section in &report.sections {
        if section.kind != SectionKind::Machine {
            out.push(String::new());
            out.extend(section_lines(section, style, verbose));
        }
    }
    out.push(String::new());
    out.extend(manual_lines(report, style, verbose));
    out
}

fn verdict(report: &Report, style: Style) -> Vec<String> {
    let manual = report
        .findings()
        .filter(|f| f.severity == Severity::Manual)
        .count();
    let blockers = report.blockers();
    if blockers == 0 {
        return vec![style.paint(
            "1;32",
            &format!(
                "Ready to migrate: nothing blocks. {} below.",
                count(manual, "manual step")
            ),
        )];
    }
    let per_section: Vec<String> = report
        .sections
        .iter()
        .filter_map(|s| {
            let n = s
                .findings
                .iter()
                .filter(|f| f.severity == Severity::Blocker)
                .count();
            (n > 0).then(|| format!("{} {n}", s.name))
        })
        .collect();
    vec![style.paint(
        "1;31",
        &format!(
            "Not ready to migrate: {} ({}).",
            count(blockers, "blocker"),
            per_section.join(", ")
        ),
    )]
}

fn plan_lines(report: &Report, plan: &[PlanStep], style: Style) -> Vec<String> {
    let mut out = vec![
        style.bold("Plan — run each command from the project's root, then run this check again:"),
    ];
    let backfill = plan.iter().any(|p| p.step == Step::Backfill);
    let mut n = 0;
    for step in plan {
        let text = match step.step {
            Step::StopAgents => format!("Stop agents: {}", style.cmd("pm close --all")),
            Step::Repair => format!(
                "Resolve {} first, as each line below says",
                count(step.blockers, "problem")
            ),
            Step::Branches => format!(
                "Commit and push {}: the git command under each",
                count(step.blockers, "branch")
            ),
            Step::GlobalPush if backfill => continue,
            Step::Backfill => style.cmd("pm state backfill && pm state push --global"),
            _ => {
                let command = style.cmd(step.command.unwrap_or_default());
                if step.projects.is_empty() {
                    command
                } else {
                    format!("{command} in {}", step.projects.join(", "))
                }
            }
        };
        n += 1;
        out.push(format!("  {n}. {text}"));
    }
    let in_flight: Vec<String> = report
        .sections
        .iter()
        .flat_map(|s| {
            s.findings
                .iter()
                .filter(|f| f.in_flight)
                .map(move |f| format!("{} {}", s.name, f.subject.trim_end_matches('/')))
        })
        .collect();
    if !in_flight.is_empty() {
        out.push(style.paint(
            "33",
            &format!(
                "  In flight: {} — agents active, work uncommitted. Finishing and merging \
                 before the move beats committing work in progress.",
                in_flight.join(", ")
            ),
        ));
    }
    out
}

fn section_lines(section: &Section, style: Style, verbose: bool) -> Vec<String> {
    let mut head = style.bold(&section.name);
    if !section.path.is_empty() {
        head.push_str(&format!("  {}", style.paint("2", &section.path)));
    }
    let blockers: Vec<&Finding> = section
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Blocker)
        .collect();
    if blockers.is_empty() {
        head.push_str(&format!("  {}", style.ok()));
    }
    let mut out = vec![head];
    let width = blockers
        .iter()
        .map(|f| f.subject.chars().count())
        .filter(|&w| w <= SUBJECT_WIDTH)
        .max()
        .unwrap_or(0);
    for f in blockers {
        let subject = format!("{:width$}", f.subject);
        let mut line = format!("  {} {}  {}", style.blocker(), subject, f.what);
        if f.in_flight {
            line.push_str(&format!("  {}", style.paint("33", "(in flight)")));
        }
        out.push(line.trim_end().to_string());
        if let Some(fix) = &f.fix {
            out.push(format!("      → {}", style.cmd(fix)));
        }
        if verbose {
            out.extend(f.detail.iter().map(|d| format!("      {}", style.note(d))));
        }
    }
    for f in &section.findings {
        if f.severity == Severity::Note {
            let text = match f.subject.as_str() {
                "" => f.what.clone(),
                subject => format!("{subject}: {}", f.what),
            };
            out.push(format!("  {}", style.note(&text)));
        }
    }
    out
}

fn manual_lines(report: &Report, style: Style, verbose: bool) -> Vec<String> {
    let mut here = Vec::new();
    let mut there = Vec::new();
    let mut carried = Vec::new();
    for section in &report.sections {
        for f in &section.findings {
            if f.severity != Severity::Manual {
                continue;
            }
            let text = match section.kind {
                SectionKind::Project => format!("{}: {}", section.name, f.what),
                _ => f.what.clone(),
            };
            let mut item = vec![format!("    {} {text}", style.todo())];
            if verbose {
                item.extend(
                    f.detail
                        .iter()
                        .map(|d| format!("        {}", style.note(d))),
                );
            }
            match section.kind {
                SectionKind::Machine if f.subject == THIS_HOST => here.extend(item),
                SectionKind::Machine => there.extend(item),
                _ => carried.extend(item),
            }
        }
    }
    let mut out = vec![style.bold("Manual steps")];
    if !here.is_empty() {
        out.push("  On this host, once agents are stopped:".to_string());
        out.extend(here);
    }
    out.push("  On the new host:".to_string());
    out.extend(there);
    out.extend(carried);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocker(subject: &str, what: &str, fix: Option<&str>, steps: &[Step]) -> Finding {
        Finding::blocker(subject, what.to_string(), fix.map(str::to_string), steps)
    }

    fn project(name: &str, findings: Vec<Finding>) -> Section {
        Section {
            kind: SectionKind::Project,
            name: name.to_string(),
            path: format!("~/Projects/{name}"),
            findings,
        }
    }

    /// Three projects: running agents, a feature with work in progress, an
    /// unpushed branch, `.pm/` changes, and a `.pm/` with no remote.
    fn report() -> Report {
        let push = [Step::StatePush];
        let mut wip = blocker(
            "android-plumbing/",
            "13 modified, 21 untracked, not on origin",
            Some(
                "(cd android-plumbing && git add <file>… && git commit -a && git push -u origin android-plumbing)",
            ),
            &[Step::Branches],
        );
        wip.in_flight = true;
        wip.detail.push("modified: src/a.rs, src/b.rs".to_string());
        Report {
            sections: vec![
                Section {
                    kind: SectionKind::Registry,
                    name: "global registry".to_string(),
                    path: "~/.config/pm".to_string(),
                    findings: Vec::new(),
                },
                project(
                    "pm",
                    vec![
                        blocker("agents", "2 running: main/main, android-plumbing/implementer", None, &[Step::StopAgents]),
                        wip,
                        blocker("serve/", "2 commits not pushed", Some("git -C serve push -u origin serve"), &[Step::Branches]),
                        blocker(".pm/", "7 changes not committed", None, &push),
                    ],
                ),
                project("eco", vec![blocker(".pm/", "3 changes not committed", None, &push)]),
                project(
                    "exo-model-provider",
                    vec![blocker(
                        ".pm/",
                        "no remote",
                        None,
                        &[Step::StateRemote, Step::StatePush, Step::Backfill, Step::GlobalPush],
                    )],
                ),
                Section {
                    kind: SectionKind::Machine,
                    name: "this machine".to_string(),
                    path: String::new(),
                    findings: vec![
                        Finding::manual(THIS_HOST, "pm harness export --all --harness claude-code -o pm-claude-code.tar.gz".to_string(), None),
                        Finding::manual("", "install pm 1.0.0".to_string(), Some("why".to_string())),
                    ],
                },
            ],
        }
    }

    #[test]
    fn the_plan_runs_each_step_once_for_every_project_that_needs_it() {
        let out = report().lines(Style { color: false }, false);
        let plan: Vec<&str> = out
            .iter()
            .skip_while(|l| !l.starts_with("Plan"))
            .skip(1)
            .take_while(|l| !l.is_empty())
            .map(String::as_str)
            .collect();
        assert_eq!(
            plan,
            [
                "  1. Stop agents: pm close --all",
                "  2. Commit and push 2 branches: the git command under each",
                "  3. pm state remote <new empty repo url> in exo-model-provider",
                "  4. pm state push in pm, eco, exo-model-provider",
                "  5. pm state backfill && pm state push --global",
                "  In flight: pm android-plumbing — agents active, work uncommitted. Finishing \
                 and merging before the move beats committing work in progress.",
            ]
        );
        assert_eq!(
            out[0],
            "Not ready to migrate: 6 blockers (pm 4, eco 1, exo-model-provider 1)."
        );
    }

    #[test]
    fn each_subject_is_one_line_and_file_names_wait_for_verbose() {
        let out = report().lines(Style { color: false }, false);
        let pm: Vec<&str> = out
            .iter()
            .skip_while(|l| !l.starts_with("pm  "))
            .take_while(|l| !l.is_empty())
            .map(String::as_str)
            .collect();
        assert_eq!(
            pm,
            [
                "pm  ~/Projects/pm",
                "  x agents             2 running: main/main, android-plumbing/implementer",
                "  x android-plumbing/  13 modified, 21 untracked, not on origin  (in flight)",
                "      → (cd android-plumbing && git add <file>… && git commit -a && git push -u origin android-plumbing)",
                "  x serve/             2 commits not pushed",
                "      → git -C serve push -u origin serve",
                "  x .pm/               7 changes not committed",
            ]
        );
        assert!(out.contains(&"global registry  ~/.config/pm  ok".to_string()));
        assert!(
            !out.iter()
                .any(|l| l.contains("src/a.rs") || l.contains('\x1b'))
        );

        let verbose = report().lines(Style { color: false }, true);
        assert!(verbose.iter().any(|l| l.contains("src/a.rs")));
        let manual: Vec<&str> = verbose
            .iter()
            .skip_while(|l| *l != "Manual steps")
            .map(String::as_str)
            .collect();
        assert_eq!(
            manual,
            [
                "Manual steps",
                "  On this host, once agents are stopped:",
                "    [ ] pm harness export --all --harness claude-code -o pm-claude-code.tar.gz",
                "  On the new host:",
                "    [ ] install pm 1.0.0",
                "        - why",
            ]
        );
    }
}
