use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};

fn run_gh(repo_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(repo_dir)
        .output()?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(PmError::Gh(stderr))
    }
}

/// Check if a PR already exists for the given branch head.
/// Returns the PR number if one exists, None otherwise.
pub fn existing_pr_number(repo_dir: &Path, branch: &str) -> Result<Option<String>> {
    let output = run_gh(
        repo_dir,
        &[
            "pr",
            "list",
            "--head",
            branch,
            "--json",
            "number",
            "--jq",
            ".[0].number",
        ],
    )?;
    if output.is_empty() {
        Ok(None)
    } else {
        Ok(Some(output))
    }
}

/// Create a PR for the given branch. Returns the PR number.
/// Uses `--fill-first` to auto-populate the title from the first commit
/// without dumping all commit messages into the body.
/// If `draft` is true, creates a draft PR.
/// If `body` is Some, uses it as the PR body.
pub fn create_pr(repo_dir: &Path, branch: &str, draft: bool, body: Option<&str>) -> Result<String> {
    let mut args = vec!["pr", "create", "--fill-first", "--head", branch];
    if draft {
        args.push("--draft");
    }
    if let Some(body) = body {
        args.push("--body");
        args.push(body);
    }
    let url = run_gh(repo_dir, &args)?;
    // gh pr create returns the URL; extract the number from the end
    Ok(pr_number_from_url(&url))
}

/// Check if a PR has been merged on GitHub.
pub fn pr_is_merged(repo_dir: &Path, pr_number: &str) -> Result<bool> {
    let output = run_gh(
        repo_dir,
        &["pr", "view", pr_number, "--json", "state", "--jq", ".state"],
    )?;
    Ok(output == "MERGED")
}

/// Mark an existing PR as ready for review.
pub fn mark_pr_ready(repo_dir: &Path, branch: &str) -> Result<()> {
    run_gh(repo_dir, &["pr", "ready", branch])?;
    Ok(())
}

/// Extract the PR number from a gh PR URL (the last path segment).
fn pr_number_from_url(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_number_from_url_extracts_trailing_number() {
        assert_eq!(
            pr_number_from_url("https://github.com/owner/repo/pull/42"),
            "42"
        );
    }

    #[test]
    fn pr_number_from_url_handles_bare_number() {
        assert_eq!(pr_number_from_url("99"), "99");
    }

    #[test]
    fn pr_number_from_url_handles_trailing_slash() {
        // Shouldn't happen in practice, but verify no panic
        assert_eq!(
            pr_number_from_url("https://github.com/owner/repo/pull/7/"),
            ""
        );
    }
}
