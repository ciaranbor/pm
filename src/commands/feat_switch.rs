use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Switch to a specific feature's tmux session.
pub fn feat_switch(project_root: &Path, name: &str, tmux_server: Option<&str>) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    // Verify feature exists
    if !FeatureState::exists(&features_dir, name) {
        return Err(PmError::FeatureNotFound(name.to_string()));
    }

    let config = ProjectConfig::load(&pm_dir)?;
    let session_name = format!("{}/{name}", config.project.name);

    tmux::switch_client(tmux_server, &session_name)?;
    Ok(())
}

/// Build a tmux display-menu for feature selection within the current project.
pub fn feat_switch_menu(project_root: &Path) -> Result<Vec<(String, String)>> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    let features = FeatureState::list(&features_dir)?;

    // Include main session
    let mut items = vec![("main".to_string(), format!("{project_name}/main"))];

    for (name, _state) in &features {
        items.push((name.clone(), format!("{project_name}/{name}")));
    }

    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_new, init};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn feat_switch_nonexistent_feature_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        let result = feat_switch(&project_path, "nonexistent", server.name());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn feat_switch_existing_feature_constructs_correct_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        feat_new::feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        // switch_client will fail because we're not attached to a tmux client,
        // but the error should be a Tmux error (not a panic or FeatureNotFound)
        let result = feat_switch(&project_path, "login", server.name());
        match result {
            Ok(()) => {}                // might succeed in some tmux setups
            Err(PmError::Tmux(_)) => {} // expected: no attached client
            Err(e) => panic!("unexpected error type: {e:?}"),
        }
    }

    #[test]
    fn feat_switch_menu_includes_main_and_features() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let scoped = server.scope("myapp");
        let project_path = dir.path().join(&scoped);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        feat_new::feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();
        feat_new::feat_new(
            &project_path,
            "api",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        let items = feat_switch_menu(&project_path).unwrap();

        assert_eq!(items.len(), 3); // main + 2 features
        assert_eq!(items[0].0, "main");
        assert_eq!(items[0].1, format!("{scoped}/main"));
        assert_eq!(items[1].0, "api");
        assert_eq!(items[1].1, format!("{scoped}/api"));
        assert_eq!(items[2].0, "login");
        assert_eq!(items[2].1, format!("{scoped}/login"));
    }
}
