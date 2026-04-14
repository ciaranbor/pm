use std::path::Path;

use crate::error::Result;
use crate::gh;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;

/// Map a GitHub PR state to a feature status.
pub fn status_from_pr(pr_info: &gh::PrInfo) -> Option<FeatureStatus> {
    match pr_info.state.to_uppercase().as_str() {
        "MERGED" => Some(FeatureStatus::Merged),
        "CLOSED" => Some(FeatureStatus::Stale),
        "OPEN" if pr_info.is_draft => Some(FeatureStatus::Wip),
        "OPEN" if pr_info.review_decision == "APPROVED" => Some(FeatureStatus::Approved),
        "OPEN" => Some(FeatureStatus::Review),
        _ => None,
    }
}

/// Query the PR linked to a feature and update its status in place.
/// Saves to disk if the status changed. Caller must ensure `state.pr` is non-empty.
pub fn sync_one(
    state: &mut FeatureState,
    features_dir: &Path,
    name: &str,
    repo_dir: &Path,
) -> Result<()> {
    let info = gh::pr_info(repo_dir, &state.pr)?;
    let new_status = status_from_pr(&info).unwrap_or(state.status);
    if new_status != state.status {
        state.status = new_status;
        if new_status.is_active() {
            state.last_active = chrono::Utc::now();
        }
        state.save(features_dir, name)?;
    }
    Ok(())
}

/// Sync feature statuses with their linked GitHub PRs.
///
/// For each feature with a non-empty `pr` field, queries GitHub for the PR
/// state and updates the local feature status accordingly:
/// - MERGED -> Merged
/// - CLOSED -> Stale
/// - OPEN + draft -> Wip
/// - OPEN + approved -> Approved
/// - OPEN + ready -> Review
///
/// If `name` is Some, syncs only that feature; otherwise syncs all features.
/// Returns a list of human-readable status change messages.
pub fn feat_sync(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    let features_dir = paths::features_dir(project_root);
    let main_worktree = project_root.join("main");

    let features: Vec<(String, FeatureState)> = if let Some(name) = name {
        let state = FeatureState::load(&features_dir, name)?;
        vec![(name.to_string(), state)]
    } else {
        FeatureState::list(&features_dir)?
    };

    let mut messages = Vec::new();
    let mut merged_features = Vec::new();

    for (feat_name, mut state) in features {
        if state.pr.is_empty() {
            continue;
        }

        let old_status = state.status;

        match sync_one(&mut state, &features_dir, &feat_name, &main_worktree) {
            Ok(()) => {
                if state.status != old_status {
                    messages.push(format!("  {feat_name}: {old_status} -> {}", state.status));
                }
            }
            Err(e) => {
                messages.push(format!(
                    "  {feat_name}: failed to query PR #{} — {e}",
                    state.pr
                ));
                continue;
            }
        }

        if state.status == FeatureStatus::Merged {
            merged_features.push(feat_name);
        }
    }

    if messages.is_empty() {
        messages.push("All features up to date".to_string());
    }

    if !merged_features.is_empty() {
        messages.push(String::new());
        messages.push("Merged features (clean up with `pm feat delete`):".to_string());
        for name in &merged_features {
            messages.push(format!("  pm feat delete {name}"));
        }
    }

    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh::PrInfo;
    use crate::state::feature::FeatureStatus;
    use chrono::Utc;
    use tempfile::tempdir;

    fn make_feature(status: FeatureStatus, pr: &str) -> FeatureState {
        FeatureState {
            status,
            branch: "test-branch".to_string(),
            worktree: "test-branch".to_string(),
            base: String::new(),
            pr: pr.to_string(),
            context: String::new(),
            created: Utc::now(),
            last_active: Utc::now(),
        }
    }

    // -- Pure status mapping tests (no subprocess calls) --

    #[test]
    fn merged_pr_maps_to_merged() {
        let info = PrInfo {
            state: "MERGED".to_string(),
            is_draft: false,
            review_decision: String::new(),
        };
        assert_eq!(status_from_pr(&info), Some(FeatureStatus::Merged));
    }

    #[test]
    fn closed_pr_maps_to_stale() {
        let info = PrInfo {
            state: "CLOSED".to_string(),
            is_draft: false,
            review_decision: String::new(),
        };
        assert_eq!(status_from_pr(&info), Some(FeatureStatus::Stale));
    }

    #[test]
    fn open_draft_pr_maps_to_wip() {
        let info = PrInfo {
            state: "OPEN".to_string(),
            is_draft: true,
            review_decision: String::new(),
        };
        assert_eq!(status_from_pr(&info), Some(FeatureStatus::Wip));
    }

    #[test]
    fn open_approved_pr_maps_to_approved() {
        let info = PrInfo {
            state: "OPEN".to_string(),
            is_draft: false,
            review_decision: "APPROVED".to_string(),
        };
        assert_eq!(status_from_pr(&info), Some(FeatureStatus::Approved));
    }

    #[test]
    fn open_ready_pr_maps_to_review() {
        let info = PrInfo {
            state: "OPEN".to_string(),
            is_draft: false,
            review_decision: String::new(),
        };
        assert_eq!(status_from_pr(&info), Some(FeatureStatus::Review));
    }

    #[test]
    fn unknown_state_maps_to_none() {
        let info = PrInfo {
            state: "UNKNOWN".to_string(),
            is_draft: false,
            review_decision: String::new(),
        };
        assert_eq!(status_from_pr(&info), None);
    }

    #[test]
    fn lowercase_state_maps_correctly() {
        // gh CLI may return lowercase state in some versions
        let info = PrInfo {
            state: "open".to_string(),
            is_draft: false,
            review_decision: String::new(),
        };
        assert_eq!(status_from_pr(&info), Some(FeatureStatus::Review));

        let info = PrInfo {
            state: "merged".to_string(),
            is_draft: false,
            review_decision: String::new(),
        };
        assert_eq!(status_from_pr(&info), Some(FeatureStatus::Merged));

        let info = PrInfo {
            state: "closed".to_string(),
            is_draft: false,
            review_decision: String::new(),
        };
        assert_eq!(status_from_pr(&info), Some(FeatureStatus::Stale));
    }

    // -- Integration-level tests (filesystem, no gh calls) --

    #[test]
    fn sync_skips_features_without_pr() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let features_dir = paths::features_dir(project_root);
        std::fs::create_dir_all(project_root.join("main")).unwrap();

        let state = make_feature(FeatureStatus::Wip, "");
        state.save(&features_dir, "no-pr").unwrap();

        let messages = feat_sync(project_root, Some("no-pr")).unwrap();
        assert_eq!(messages, vec!["All features up to date"]);

        let reloaded = FeatureState::load(&features_dir, "no-pr").unwrap();
        assert_eq!(reloaded.status, FeatureStatus::Wip);
    }

    #[test]
    fn sync_nonexistent_feature_returns_error() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let features_dir = paths::features_dir(project_root);
        std::fs::create_dir_all(&features_dir).unwrap();

        let result = feat_sync(project_root, Some("nonexistent"));
        assert!(result.is_err());
    }

    #[test]
    fn sync_empty_project_reports_up_to_date() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let features_dir = paths::features_dir(project_root);
        std::fs::create_dir_all(&features_dir).unwrap();
        std::fs::create_dir_all(project_root.join("main")).unwrap();

        let messages = feat_sync(project_root, None).unwrap();
        assert_eq!(messages, vec!["All features up to date"]);
    }
}
