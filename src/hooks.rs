use std::path::Path;

use crate::error::Result;
use crate::tmux;

const HOOK_WINDOW_NAME: &str = "hook";

pub const POST_CREATE_PATH: &str = ".pm/hooks/post-create.sh";
pub const POST_MERGE_PATH: &str = ".pm/hooks/post-merge.sh";
pub const RESTORE_PATH: &str = ".pm/hooks/restore.sh";

pub const DEFAULT_POST_CREATE: &str = "\
#!/bin/sh
# post-create hook — runs in the feature session after 'pm feat new'
# Edit this script to add your own setup logic (install deps, start dev server, etc.)
echo \"post-create hook: edit .pm/hooks/post-create.sh to customize\"
";

pub const DEFAULT_POST_MERGE: &str = "\
#!/bin/sh
# post-merge hook — runs in the main session after 'pm feat merge'
# Edit this script to add your own post-merge logic (deploy, notify, etc.)
echo \"post-merge hook: edit .pm/hooks/post-merge.sh to customize\"
";

pub const DEFAULT_RESTORE: &str = "\
#!/bin/sh
# restore hook — runs in each session after 'pm open' recreates it
# Uncomment and customize for your project:
#
# # Reinstall dependencies
# npm install
#
# # Start dev server in a split pane
# tmux split-window -h -t \"$PM_SESSION\" 'npm run dev'
#
# # Reattach to a running process
# # (agents are respawned automatically by pm open)
echo \"restore hook: edit .pm/hooks/restore.sh to customize\"
";

/// Bootstrap default hook scripts into a project's .pm/hooks/ directory.
pub fn bootstrap(project_root: &Path) -> Result<()> {
    write_default_hook(project_root, POST_CREATE_PATH, DEFAULT_POST_CREATE)?;
    write_default_hook(project_root, POST_MERGE_PATH, DEFAULT_POST_MERGE)?;
    write_default_hook(project_root, RESTORE_PATH, DEFAULT_RESTORE)?;
    Ok(())
}

fn write_default_hook(project_root: &Path, rel_path: &str, content: &str) -> Result<()> {
    let path = project_root.join(rel_path);
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Run a hook in a named "hook" window within the given session (non-fatal).
pub fn run_hook(tmux_server: Option<&str>, session: &str, working_dir: &Path, hook_path: &Path) {
    if !hook_path.is_file() {
        return;
    }
    let quoted = tmux::shell_quote(&hook_path.to_string_lossy());
    match tmux::find_or_create_window(tmux_server, session, HOOK_WINDOW_NAME, working_dir)
        .and_then(|target| tmux::send_keys(tmux_server, &target, &quoted))
    {
        Ok(()) => {}
        Err(e) => eprintln!("warning: hook failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn bootstrap_creates_executable_hook_scripts() {
        let dir = tempdir().unwrap();
        bootstrap(dir.path()).unwrap();

        let post_create = dir.path().join(POST_CREATE_PATH);
        let post_merge = dir.path().join(POST_MERGE_PATH);

        assert!(post_create.is_file());
        assert!(post_merge.is_file());

        let content = std::fs::read_to_string(&post_create).unwrap();
        assert!(content.starts_with("#!/bin/sh"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&post_create)
                .unwrap()
                .permissions()
                .mode();
            assert!(mode & 0o111 != 0, "post-create hook should be executable");
            let mode = std::fs::metadata(&post_merge).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "post-merge hook should be executable");
        }
    }

    #[test]
    fn bootstrap_is_idempotent() {
        let dir = tempdir().unwrap();
        bootstrap(dir.path()).unwrap();
        bootstrap(dir.path()).unwrap();

        assert!(dir.path().join(POST_CREATE_PATH).is_file());
        assert!(dir.path().join(POST_MERGE_PATH).is_file());
    }

    #[test]
    fn bootstrap_preserves_user_customized_hooks() {
        let dir = tempdir().unwrap();
        bootstrap(dir.path()).unwrap();

        // User customizes the hook
        let hook_path = dir.path().join(POST_CREATE_PATH);
        std::fs::write(&hook_path, "#!/bin/sh\necho custom\n").unwrap();

        // Bootstrap again (e.g. from pm register on same project)
        bootstrap(dir.path()).unwrap();

        let content = std::fs::read_to_string(&hook_path).unwrap();
        assert_eq!(content, "#!/bin/sh\necho custom\n");
    }
}
