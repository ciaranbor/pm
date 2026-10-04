//! One subject's problems gathered into a single blocker: its problems
//! joined into one line, its git commands into one command, and the plan
//! steps they need.

use super::{Finding, Step};

/// Shown with `--verbose`: at most this many file names per list.
const LISTED: usize = 20;

#[derive(Default)]
pub(super) struct Line {
    problems: Vec<String>,
    detail: Vec<String>,
    /// Commands run before the git commands.
    commands: Vec<String>,
    /// Git subcommands run in [`Line::dir`].
    git: Vec<String>,
    dir: String,
    steps: Vec<Step>,
}

impl Line {
    /// A line whose git commands run in `dir` (relative to the root).
    pub(super) fn in_dir(dir: &str) -> Self {
        Self {
            dir: dir.to_string(),
            ..Self::default()
        }
    }

    pub(super) fn problem(&mut self, what: impl Into<String>) {
        self.problems.push(what.into());
    }

    pub(super) fn detail(&mut self, text: impl Into<String>) {
        self.detail.push(text.into());
    }

    /// `paths`, under a `label`, as a detail line.
    pub(super) fn files(&mut self, label: &str, paths: &[String]) {
        let mut listed: Vec<&str> = paths.iter().take(LISTED).map(String::as_str).collect();
        let more = format!("{} more", paths.len().saturating_sub(LISTED));
        if paths.len() > LISTED {
            listed.push(&more);
        }
        self.detail.push(format!("{label}: {}", listed.join(", ")));
    }

    pub(super) fn command(&mut self, command: impl Into<String>) {
        self.commands.push(command.into());
    }

    pub(super) fn git(&mut self, subcommand: impl Into<String>) {
        let subcommand = subcommand.into();
        if !self.git.contains(&subcommand) {
            self.git.push(subcommand);
        }
    }

    pub(super) fn has_git(&self) -> bool {
        !self.git.is_empty()
    }

    pub(super) fn step(&mut self, step: Step) {
        if !self.steps.contains(&step) {
            self.steps.push(step);
        }
    }

    pub(super) fn steps(&mut self, steps: &[Step]) {
        for step in steps {
            self.step(*step);
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.problems.is_empty()
    }

    /// The blocker for `subject`, or `None` with no problem.
    pub(super) fn finish(self, subject: &str) -> Option<Finding> {
        if self.problems.is_empty() {
            return None;
        }
        let mut commands = self.commands;
        match self.git.as_slice() {
            [] => {}
            [one] => commands.push(format!("git -C {} {one}", self.dir)),
            many => commands.push(format!(
                "(cd {} && git {})",
                self.dir,
                many.join(" && git ")
            )),
        }
        let fix = (!commands.is_empty()).then(|| commands.join(" && "));
        let mut steps = self.steps;
        steps.sort();
        let mut finding = Finding::blocker(subject, self.problems.join(", "), fix, &steps);
        finding.detail = self.detail;
        Some(finding)
    }
}

/// `n` and `noun`, plural when `n` isn't 1.
pub(super) fn count(n: usize, noun: &str) -> String {
    let plural = if noun.ends_with('h') { "es" } else { "s" };
    format!("{n} {noun}{}", if n == 1 { "" } else { plural })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn several_git_commands_run_in_one_subshell_from_the_root() {
        let mut line = Line::in_dir("login");
        line.problem("3 modified");
        line.git("commit -a");
        line.git("push -u origin login");
        line.git("push -u origin login");
        line.step(Step::Branches);
        let finding = line.finish("login/").unwrap();
        assert_eq!(
            finding.fix.as_deref(),
            Some("(cd login && git commit -a && git push -u origin login)")
        );

        let mut line = Line::in_dir("main");
        line.problem("not on origin");
        line.git("push -u origin main");
        assert_eq!(
            line.finish("main/").unwrap().fix.as_deref(),
            Some("git -C main push -u origin main")
        );
        assert_eq!(Line::in_dir("x").finish("x/"), None);
    }
}
