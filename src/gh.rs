use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};
use serde::Deserialize;

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
pub fn create_pr(
    repo_dir: &Path,
    branch: &str,
    draft: bool,
    body: Option<&str>,
    base: Option<&str>,
) -> Result<String> {
    let mut args = vec!["pr", "create", "--fill-first", "--head", branch];
    if let Some(base) = base {
        args.push("--base");
        args.push(base);
    }
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

/// PR status info returned by a single `gh pr view` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    /// "OPEN", "MERGED", or "CLOSED"
    pub state: String,
    /// Whether the PR is a draft (only meaningful when state is "OPEN")
    pub is_draft: bool,
}

/// Raw JSON shape returned by `gh pr view --json state,isDraft`.
#[derive(Deserialize)]
struct PrInfoJson {
    state: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
}

/// Get the state and draft status of a PR in a single gh call.
pub fn pr_info(repo_dir: &Path, pr_number: &str) -> Result<PrInfo> {
    let output = run_gh(
        repo_dir,
        &["pr", "view", pr_number, "--json", "state,isDraft"],
    )?;
    let parsed: PrInfoJson =
        serde_json::from_str(&output).map_err(|e| PmError::Gh(format!("parse PR info: {e}")))?;
    Ok(PrInfo {
        state: parsed.state.to_uppercase(),
        is_draft: parsed.is_draft,
    })
}

/// Check if a PR has been merged on GitHub.
pub fn pr_is_merged(repo_dir: &Path, pr_number: &str) -> Result<bool> {
    Ok(pr_info(repo_dir, pr_number)?.state == "MERGED")
}

/// Mark an existing PR as ready for review.
pub fn mark_pr_ready(repo_dir: &Path, branch: &str) -> Result<()> {
    run_gh(repo_dir, &["pr", "ready", branch])?;
    Ok(())
}

/// PR details returned by `gh pr view`.
#[derive(Debug, Clone)]
pub struct PrDetails {
    pub number: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub head_ref: String,
}

/// Fetch full details for a PR by number or URL.
pub fn pr_details(repo_dir: &Path, pr: &str) -> Result<PrDetails> {
    let output = run_gh(
        repo_dir,
        &[
            "pr",
            "view",
            pr,
            "--json",
            "number,title,body,url,headRefName",
        ],
    )?;
    let parsed: serde_json::Value =
        serde_json::from_str(&output).map_err(|e| PmError::Gh(format!("parse PR JSON: {e}")))?;
    let number = parsed["number"]
        .as_u64()
        .map(|n| n.to_string())
        .ok_or_else(|| PmError::Gh("PR response missing 'number' field".to_string()))?;
    let head_ref = parsed["headRefName"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| PmError::Gh("PR response missing 'headRefName' field".to_string()))?
        .to_string();
    Ok(PrDetails {
        number,
        title: parsed["title"].as_str().unwrap_or("").to_string(),
        body: parsed["body"].as_str().unwrap_or("").to_string(),
        url: parsed["url"].as_str().unwrap_or("").to_string(),
        head_ref,
    })
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

    #[test]
    fn pr_info_json_parses_open_non_draft() {
        let json = r#"{"state":"OPEN","isDraft":false}"#;
        let parsed: PrInfoJson = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.state, "OPEN");
        assert!(!parsed.is_draft);
    }

    #[test]
    fn pr_info_json_parses_open_draft() {
        let json = r#"{"state":"OPEN","isDraft":true}"#;
        let parsed: PrInfoJson = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.state, "OPEN");
        assert!(parsed.is_draft);
    }

    #[test]
    fn pr_info_json_parses_merged() {
        let json = r#"{"state":"MERGED","isDraft":false}"#;
        let parsed: PrInfoJson = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.state, "MERGED");
        assert!(!parsed.is_draft);
    }
}
