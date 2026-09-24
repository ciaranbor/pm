use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::state::paths;
use crate::tmux;

const HOOK_WINDOW_NAME: &str = "hook";

pub const POST_CREATE_PATH: &str = ".pm/hooks/post-create.sh";
pub const POST_MERGE_PATH: &str = ".pm/hooks/post-merge.sh";
pub const RESTORE_PATH: &str = ".pm/hooks/restore.sh";

/// The `PM_*` contract as a comment block; a macro so `concat!` can splice
/// it into each template.
macro_rules! env_comment {
    () => {
        "\
# pm exports the hook's context as environment variables:
#   PM_PROJECT_ROOT    project root (holds .pm/ and the worktrees)
#   PM_MAIN_WORKTREE   the main worktree
#   PM_WORKTREE        the worktree this hook concerns (also the cwd)
#   PM_SESSION         the tmux session this hook runs in
#   PM_FEATURE         feature owning PM_WORKTREE; empty in main scope
"
    };
}

pub const DEFAULT_POST_CREATE: &str = concat!(
    "\
#!/bin/sh
# post-create hook — runs in the new feature's session after 'pm feat new'
",
    env_comment!(),
    "\
# Uncomment and customize for your project:
#
# # Copy gitignored files from the main worktree into the new feature
# cp \"$PM_MAIN_WORKTREE/.env\" \"$PM_WORKTREE/\"
#
# # Install dependencies
# npm install
echo \"post-create hook: edit .pm/hooks/post-create.sh to customize\"
"
);

pub const DEFAULT_POST_MERGE: &str = concat!(
    "\
#!/bin/sh
# post-merge hook — runs in the base session after 'pm feat merge'
",
    env_comment!(),
    "\
#   PM_MERGED_FEATURE  the feature that was just merged
# Uncomment and customize for your project:
#
# # Deploy, notify, ...
# echo \"merged $PM_MERGED_FEATURE into ${PM_FEATURE:-main}\"
echo \"post-merge hook: edit .pm/hooks/post-merge.sh to customize\"
"
);

pub const DEFAULT_RESTORE: &str = concat!(
    "\
#!/bin/sh
# restore hook — runs in each session after 'pm open' recreates it
",
    env_comment!(),
    "\
# Uncomment and customize for your project:
#
# # Reinstall dependencies
# npm install
#
# # Start dev server in a split pane
# tmux split-window -h -t \"$PM_SESSION\" -c \"$PM_WORKTREE\" 'npm run dev'
#
# # Copy a gitignored secret from the main worktree into a feature
# [ -n \"$PM_FEATURE\" ] && cp \"$PM_MAIN_WORKTREE/.env\" \"$PM_WORKTREE/\"
#
# # Reattach to a running process
# # (agents are respawned automatically by pm open)
echo \"restore hook: edit .pm/hooks/restore.sh to customize\"
"
);

/// What a hook invocation concerns, exported to the script as `PM_*`
/// environment variables (see README, "Lifecycle hooks").
pub struct HookContext {
    pub project_root: PathBuf,
    /// tmux session the hook window lives in.
    pub session: String,
    /// Worktree the session belongs to; also the hook's working directory.
    pub worktree: PathBuf,
    /// Feature the session belongs to; `None` in main scope, exported as
    /// an empty `PM_FEATURE`.
    pub feature: Option<String>,
    /// post-merge only: the feature that was merged into `worktree`.
    pub merged_feature: Option<String>,
}

impl HookContext {
    /// A hook running in `scope`'s session and worktree (`"main"` is main scope).
    pub fn scope(project_root: &Path, project_name: &str, scope: &str) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            session: tmux::session_name(project_name, scope),
            worktree: project_root.join(scope),
            feature: (scope != "main").then(|| scope.to_string()),
            merged_feature: None,
        }
    }

    /// The post-merge hook: runs in `base`'s scope after `merged` landed there.
    pub fn post_merge(project_root: &Path, project_name: &str, base: &str, merged: &str) -> Self {
        Self {
            merged_feature: Some(merged.to_string()),
            ..Self::scope(project_root, project_name, base)
        }
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        let lossy = |p: &Path| p.to_string_lossy().into_owned();
        let mut vars = vec![
            ("PM_PROJECT_ROOT", lossy(&self.project_root)),
            (
                "PM_MAIN_WORKTREE",
                lossy(&paths::main_worktree(&self.project_root)),
            ),
            ("PM_WORKTREE", lossy(&self.worktree)),
            ("PM_SESSION", self.session.clone()),
            ("PM_FEATURE", self.feature.clone().unwrap_or_default()),
        ];
        if let Some(merged) = &self.merged_feature {
            vars.push(("PM_MERGED_FEATURE", merged.clone()));
        }
        vars
    }
}

/// The shell line sent to the hook window: `env PM_…=… <hook>`.
fn hook_command(ctx: &HookContext, hook_path: &Path) -> String {
    let mut cmd = String::from("env");
    for (name, value) in ctx.env() {
        cmd.push(' ');
        cmd.push_str(name);
        cmd.push('=');
        cmd.push_str(&tmux::shell_quote(&value));
    }
    cmd.push(' ');
    cmd.push_str(&tmux::shell_quote(&hook_path.to_string_lossy()));
    cmd
}

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

/// Run a hook in a named "hook" window within the context's session (non-fatal).
pub fn run_hook(tmux_server: Option<&str>, ctx: &HookContext, hook_path: &Path) {
    if !hook_path.is_file() {
        return;
    }
    let cmd = hook_command(ctx, hook_path);
    match tmux::find_or_create_window(tmux_server, &ctx.session, HOOK_WINDOW_NAME, &ctx.worktree)
        .and_then(|target| tmux::send_keys(tmux_server, &target, &cmd))
    {
        Ok(()) => {}
        Err(e) => eprintln!("warning: hook failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Run `hook_command` through `sh` against a script that prints its
    /// `PM_*` environment, one value per line (`set`/unset probed via
    /// `${VAR+set}`).
    fn hook_env_seen_by(ctx: &HookContext) -> Vec<String> {
        let script = ctx.project_root.join("dump.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' \"$PM_PROJECT_ROOT\" \"$PM_MAIN_WORKTREE\" \
             \"$PM_WORKTREE\" \"$PM_SESSION\" \"${PM_FEATURE+set}:$PM_FEATURE\" \
             \"${PM_MERGED_FEATURE+set}:$PM_MERGED_FEATURE\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(hook_command(ctx, &script))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn hook_process_sees_pm_vars_round_tripped_through_shell_quoting() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("it's a/proj");
        std::fs::create_dir_all(&root).unwrap();
        let ctx = HookContext::scope(&root, "it's a/proj", "log in");
        assert_eq!(
            hook_env_seen_by(&ctx),
            [
                root.display().to_string(),
                root.join("main").display().to_string(),
                root.join("log in").display().to_string(),
                "it's a/proj/log in".to_string(),
                "set:log in".to_string(),
                ":".to_string(),
            ]
        );
    }

    #[test]
    fn main_scope_hook_sees_empty_feature_and_post_merge_sees_merged_feature() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let env = hook_env_seen_by(&HookContext::post_merge(&root, "proj", "main", "login"));
        assert_eq!(env[4], "set:");
        assert_eq!(env[5], "set:login");
    }

    #[test]
    fn post_merge_context_is_the_base_scope() {
        let root = Path::new("/p");
        let ctx = HookContext::post_merge(root, "proj", "main", "login");
        assert_eq!(ctx.feature, None);
        assert_eq!(ctx.session, "proj/main");
        assert_eq!(ctx.worktree, root.join("main"));

        let ctx = HookContext::post_merge(root, "proj", "parent", "login");
        assert_eq!(ctx.feature.as_deref(), Some("parent"));
        assert_eq!(ctx.merged_feature.as_deref(), Some("login"));
        assert_eq!(ctx.session, "proj/parent");
        assert_eq!(ctx.worktree, root.join("parent"));
    }

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
