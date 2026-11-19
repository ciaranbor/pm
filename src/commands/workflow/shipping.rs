//! The `## Shipping to GitHub` section `pm workflow show` appends where
//! `commands::ship_gate::applies`, which also enforces it.

pub(super) const SHIPPING_GUIDANCE: &str = "\
## Shipping to GitHub

This project ships each feature as a GitHub pull request. These steps are
the summary owner's, and are the instruction to use git for them. A feature
with nothing to ship, such as research alone, skips them.

Once the team's review loop has approved the work:

1. Commit it on the feature branch with imperative messages; the first
   commit's subject becomes the PR title. Never commit to or push the base
   branch, never force-push, and never merge the PR.
2. Write the PR description to a file outside the worktree: the repo's
   `.github/pull_request_template.md` filled in if it has one, else a short
   description for the PR's reviewers. It is not the pm summary, and
   nothing about pm belongs in it.
3. Run `pm feat pr create --ready --body <file>`. It pushes the branch and
   opens the PR.
4. Wait for CI with `gh pr checks --watch`. On a failure, read
   `gh run view --log-failed`, fix it, send a non-trivial fix back through
   the review loop, commit, `git push`, and wait again.

`pm feat status ready` refuses until the PR holds the branch's latest
commit and all its checks pass.
";
