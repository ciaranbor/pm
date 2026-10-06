//! The global registry repo, the pm config dir.

use std::path::Path;

use crate::error::Result;
use crate::git;
use crate::state::paths;

use super::init::{InitConfig, init_repo_managed};
use super::remote::{prompt_remote_setup_common, remote_branch};
use super::repo::{RepoContext, pull_repo, push_repo, set_remote, status_repo};

fn global_ctx(dir: &Path) -> RepoContext<'_> {
    RepoContext {
        dir,
        label: "global registry",
        init_hint: "pm state init --global",
        remote_hint: "pm state remote --global <url>",
    }
}

/// Initialise a git repo in ~/.config/pm/ for global registry backup.
/// Non-interactive variant for programmatic use (e.g. `pm upgrade`).
pub fn global_init() -> Result<String> {
    let dir = paths::global_config_dir()?;
    global_init_at(&dir, false, None)
}

/// Initialise with an explicit remote URL (combines init + remote + pull).
pub fn global_init_with_remote(remote_url: Option<&str>) -> Result<String> {
    let dir = paths::global_config_dir()?;
    match remote_url {
        Some(url) => global_init_at(&dir, false, Some(url)),
        None => global_init_at(&dir, true, None),
    }
}

fn global_init_at(dir: &Path, interactive: bool, remote_url: Option<&str>) -> Result<String> {
    // On a fresh machine the registry is pulled before any project exists.
    let created = remote_url.is_some() && !dir.exists();
    if created {
        std::fs::create_dir_all(dir)?;
    }
    let local_projects = match remote_url {
        Some(_) => registered_here(dir)?,
        None => Vec::new(),
    };
    let dir_missing_error = format!(
        "{} does not exist — run `pm init` first to create a project, or pass --remote",
        dir.display()
    );
    let cfg = InitConfig {
        dir,
        label: "global registry",
        dir_missing_error: &dir_missing_error,
        init_commit_msg: "init global registry repo",
        init_success_msg: format!("Initialised global registry repo in {}", dir.display()),
        already_init_msg: "Global registry repo already initialised",
        pull_hint: "pm state pull --global",
        reset_when_diverged: true,
        pre_init: Some(Box::new(|dir: &Path| {
            crate::commands::state_gitignore::write_global_gitignore(dir, false)?;
            Ok(())
        })),
        post_remote: None,
        prompt_remote: Some(Box::new(|dir: &Path| {
            prompt_remote_setup_common(dir, "global registry", "pm-global-registry")
        })),
    };
    let (mut result, reset) = match init_repo_managed(cfg, interactive, remote_url) {
        Ok(done) => done,
        Err(e) => {
            if created {
                let _ = std::fs::remove_dir_all(dir);
            }
            return Err(e);
        }
    };
    // Only a reset can drop an entry registered here; a fast-forward that
    // removes one is the remote deleting it.
    let (kept, set_aside) = if reset {
        keep_registry_entries(dir, local_projects)?
    } else {
        Default::default()
    };
    if !kept.is_empty() {
        result.push_str(&format!(
            "\nKept projects registered only on this machine: {} (`pm state push --global` \
             shares them)",
            kept.join(", ")
        ));
    }
    if !set_aside.is_empty() {
        result.push_str(&format!(
            "\nThe remote's entries replaced this machine's for {}; this machine's are in {}",
            set_aside.join(", "),
            set_aside_dir(dir).display()
        ));
    }
    Ok(result)
}

/// The config-dir-relative dir that registry entries a pull replaced are
/// kept in; machine-local, so the registry repo ignores it.
pub(crate) const SET_ASIDE_DIR_NAME: &str = "registry-before-pull";

fn set_aside_dir(dir: &Path) -> std::path::PathBuf {
    dir.join(SET_ASIDE_DIR_NAME)
}

/// The registry entries under the config dir `dir` that did not come from
/// its remote: an entry the last fetch of the remote held is the remote's
/// to keep or delete.
fn registered_here(dir: &Path) -> Result<Vec<(std::ffi::OsString, Vec<u8>)>> {
    let mut entries = registry_entries(dir);
    if dir.join(".git").exists()
        && git::has_remote(dir, "origin")?
        && let Some(remote_ref) = remote_branch(dir)?
    {
        let pulled = git::tree_files(dir, &remote_ref, paths::PROJECTS_DIR_NAME)?;
        entries.retain(|(file, _)| {
            let path = Path::new(paths::PROJECTS_DIR_NAME).join(file);
            !pulled.iter().any(|p| Path::new(p) == path)
        });
    }
    Ok(entries)
}

/// The registry entries under the config dir `dir`: file name and content.
fn registry_entries(dir: &Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
    let Ok(entries) = std::fs::read_dir(dir.join(paths::PROJECTS_DIR_NAME)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
        .filter_map(|e| Some((e.file_name(), std::fs::read(e.path()).ok()?)))
        .collect()
}

/// Write back each of `entries` that taking the remote's registry removed,
/// so a project registered on this machine before it pulled the registry
/// stays registered. Where both have an entry the remote's wins, and this
/// machine's differing one is set aside in [`set_aside_dir`]. Returns the
/// names written back and the names set aside.
fn keep_registry_entries(
    dir: &Path,
    entries: Vec<(std::ffi::OsString, Vec<u8>)>,
) -> Result<(Vec<String>, Vec<String>)> {
    let projects = dir.join(paths::PROJECTS_DIR_NAME);
    let (mut kept, mut set_aside) = (Vec::new(), Vec::new());
    let name = |file: &std::ffi::OsString| {
        Path::new(file)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    };
    for (file, content) in entries {
        let path = projects.join(&file);
        match std::fs::read(&path) {
            Ok(remote) if remote == content => {}
            Ok(_) => {
                let aside = set_aside_dir(dir);
                std::fs::create_dir_all(&aside)?;
                std::fs::write(aside.join(&file), content)?;
                set_aside.push(name(&file));
            }
            Err(_) => {
                std::fs::create_dir_all(&projects)?;
                std::fs::write(&path, content)?;
                kept.push(name(&file));
            }
        }
    }
    kept.sort();
    set_aside.sort();
    Ok((kept, set_aside))
}

/// Set the remote URL for the global registry repo.
pub fn global_remote(url: &str) -> Result<String> {
    let dir = paths::global_config_dir()?;
    set_remote(&global_ctx(&dir), url)
}

/// Auto-commit and push the global registry repo.
pub fn global_push() -> Result<String> {
    let dir = paths::global_config_dir()?;
    push_repo(&global_ctx(&dir))
}

/// Pull global registry from the remote.
pub fn global_pull() -> Result<String> {
    let dir = paths::global_config_dir()?;
    pull_repo(&global_ctx(&dir))
}

/// Show git status of the global registry repo.
pub fn global_status() -> Result<String> {
    let dir = paths::global_config_dir()?;
    status_repo(&global_ctx(&dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::state_cmd::test_support::*;
    use tempfile::tempdir;

    // -- init --remote tests (global) --

    #[test]
    fn global_init_with_remote_empty_bare() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("config-pm");
        std::fs::create_dir_all(global.join("projects")).unwrap();

        let bare = dir.path().join("registry-remote.git");
        git::init_bare(&bare).unwrap();

        let msg = global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(msg.contains("Initialised"));
        assert!(git::has_remote(&global, "origin").unwrap());
    }

    #[test]
    fn global_init_with_remote_populated_bare() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("config-pm");
        std::fs::create_dir_all(global.join("projects")).unwrap();

        let bare = dir.path().join("registry-remote.git");
        create_populated_bare(&bare);

        let msg = global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(msg.contains("Initialised"), "unexpected: {msg}");
        assert!(msg.contains("pulled"), "unexpected: {msg}");
        assert!(git::has_remote(&global, "origin").unwrap());
        assert!(
            global.join("remote-file.txt").exists(),
            "remote content should have been pulled"
        );
    }

    #[test]
    fn global_init_with_remote_pulls_the_registry_onto_a_fresh_machine() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("config-pm");
        let bare = dir.path().join("registry-remote.git");
        create_populated_bare(&bare);

        global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(global.join("remote-file.txt").exists());

        let without = dir.path().join("other");
        assert!(global_init_at(&without, false, None).is_err());
    }

    /// A machine that already pulled the registry from `bare`; the old
    /// machine then pushes a new project entry from its own clone.
    fn pulled_registry_and_a_later_push(dir: &Path, bare: &Path) -> std::path::PathBuf {
        let global = dir.join("new-host");
        create_populated_bare(bare);
        global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        let old = dir.join("old-host");
        git::clone_repo(&bare.to_string_lossy(), &old).unwrap();
        std::fs::create_dir_all(old.join("projects")).unwrap();
        std::fs::write(old.join("projects/pushed.toml"), "root = \"~/pushed\"\n").unwrap();
        git::add_all(&old).unwrap();
        git::commit_with_message(&old, "pushed").unwrap();
        git::push(&old, "origin", "main").unwrap();
        global
    }

    #[test]
    fn global_init_with_the_same_remote_again_keeps_what_was_registered_meanwhile() {
        let dir = tempdir().unwrap();
        let bare = dir.path().join("registry.git");
        let global = pulled_registry_and_a_later_push(dir.path(), &bare);
        // Registered here meanwhile, as `pm init` on the new host does.
        std::fs::create_dir_all(global.join("projects")).unwrap();
        std::fs::write(global.join("projects/local.toml"), "root = \"~/local\"\n").unwrap();

        global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(global.join("projects/pushed.toml").exists());
        assert!(global.join("projects/local.toml").exists());
    }

    #[test]
    fn global_init_with_the_same_remote_again_drops_an_entry_the_remote_deleted() {
        let dir = tempdir().unwrap();
        let bare = dir.path().join("registry.git");
        let global = pulled_registry_and_a_later_push(dir.path(), &bare);
        global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(global.join("projects/pushed.toml").exists());

        let old = dir.path().join("old-host");
        git::run_git(&old, &["rm", "-q", "projects/pushed.toml"]).unwrap();
        git::commit_with_message(&old, "pm delete pushed").unwrap();
        git::push(&old, "origin", "main").unwrap();

        let msg = global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(!global.join("projects/pushed.toml").exists(), "{msg}");
        assert!(!msg.contains("Kept"), "{msg}");
    }

    #[test]
    fn global_init_takes_the_remote_into_a_registry_set_up_here_first() {
        // As `pm register` on the new host left it: a repo with the right
        // remote, never pulled, and an entry for a project cloned here.
        let dir = tempdir().unwrap();
        let global = dir.path().join("config-pm");
        std::fs::create_dir_all(global.join("projects")).unwrap();
        global_init_at(&global, false, None).unwrap();
        let bare = dir.path().join("registry.git");
        create_populated_bare(&bare);
        git::add_remote(&global, "origin", &bare.to_string_lossy()).unwrap();
        std::fs::write(global.join("projects/here.toml"), "root = \"~/here\"\n").unwrap();

        let msg = global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(global.join("remote-file.txt").exists(), "{msg}");
        assert!(global.join("projects/here.toml").exists(), "{msg}");
    }

    #[test]
    fn global_init_taking_the_remote_drops_what_it_deleted_and_keeps_what_is_new_here() {
        let dir = tempdir().unwrap();
        let bare = dir.path().join("registry.git");
        let global = pulled_registry_and_a_later_push(dir.path(), &bare);
        global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        // Registered and committed here, so the next pull can't fast-forward.
        std::fs::write(global.join("projects/here.toml"), "root = \"~/here\"\n").unwrap();
        git::add_all(&global).unwrap();
        git::commit_with_message(&global, "here").unwrap();

        let old_host = dir.path().join("old-host");
        git::run_git(&old_host, &["rm", "-q", "projects/pushed.toml"]).unwrap();
        git::commit_with_message(&old_host, "pm delete pushed").unwrap();
        git::push(&old_host, "origin", "main").unwrap();

        let msg = global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(!global.join("projects/pushed.toml").exists(), "{msg}");
        assert!(global.join("projects/here.toml").exists(), "{msg}");
    }

    #[test]
    fn global_init_with_another_remote_says_how_to_repoint() {
        let dir = tempdir().unwrap();
        let bare = dir.path().join("registry.git");
        let global = pulled_registry_and_a_later_push(dir.path(), &bare);

        let err = global_init_at(&global, false, Some("git@example.com:other.git"))
            .unwrap_err()
            .to_string();
        assert!(err.contains(&*bare.to_string_lossy()), "{err}");
        assert!(
            err.contains("remote set-url origin git@example.com:other.git"),
            "{err}"
        );
        assert!(err.contains("pm state pull --global"), "{err}");
    }

    #[test]
    fn global_init_with_remote_keeps_projects_registered_before_the_pull() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("config-pm");
        std::fs::create_dir_all(global.join("projects")).unwrap();
        std::fs::write(global.join("projects/local.toml"), "root = \"~/local\"\n").unwrap();
        global_init_at(&global, false, None).unwrap();
        let bare = dir.path().join("registry-remote.git");
        create_populated_bare(&bare);

        let msg = global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(global.join("remote-file.txt").exists());
        assert!(global.join("projects/local.toml").exists());
        assert!(msg.contains("only on this machine: local"), "{msg}");
    }

    #[test]
    fn global_init_with_an_unreachable_remote_leaves_nothing_behind() {
        let dir = tempdir().unwrap();
        let bad = dir.path().join("typo.giT").to_string_lossy().to_string();

        let fresh = dir.path().join("fresh");
        let err = global_init_at(&fresh, false, Some(&bad)).unwrap_err();
        assert!(err.to_string().contains(&bad), "{err}");
        assert!(!fresh.exists());

        let existing = dir.path().join("existing");
        std::fs::create_dir_all(existing.join("projects")).unwrap();
        std::fs::write(existing.join("projects/local.toml"), "root = \"~/l\"\n").unwrap();
        global_init_at(&existing, false, Some(&bad)).unwrap_err();
        let mut left: Vec<_> = std::fs::read_dir(&existing)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["projects"]);

        // The retry with the URL fixed just works.
        let bare = dir.path().join("registry.git");
        create_populated_bare(&bare);
        global_init_at(&existing, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(existing.join("remote-file.txt").exists());
        assert!(existing.join("projects/local.toml").exists());
    }

    #[test]
    fn global_init_sets_aside_a_local_entry_the_remote_replaces() {
        let dir = tempdir().unwrap();
        let bare = dir.path().join("registry.git");
        pulled_registry_and_a_later_push(dir.path(), &bare);
        let old_host = dir.path().join("old-host");
        let global = dir.path().join("third-host");
        std::fs::create_dir_all(global.join("projects")).unwrap();
        std::fs::write(global.join("projects/pushed.toml"), "root = \"~/mine\"\n").unwrap();

        let msg = global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert_eq!(
            std::fs::read_to_string(global.join("projects/pushed.toml")).unwrap(),
            std::fs::read_to_string(old_host.join("projects/pushed.toml")).unwrap()
        );
        let aside = set_aside_dir(&global).join("pushed.toml");
        assert_eq!(
            std::fs::read_to_string(aside).unwrap(),
            "root = \"~/mine\"\n"
        );
        assert!(msg.contains("replaced this machine's for pushed"), "{msg}");
    }

    #[test]
    fn global_init_with_remote_on_existing_repo_without_remote() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("config-pm");
        std::fs::create_dir_all(global.join("projects")).unwrap();

        global_init_at(&global, false, None).unwrap();

        let bare = dir.path().join("registry-remote.git");
        git::init_bare(&bare).unwrap();

        let msg = global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(msg.contains("already initialised"));
        assert!(git::has_remote(&global, "origin").unwrap());
    }

    #[test]
    fn global_init_with_remote_on_existing_repo_populated_bare() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("config-pm");
        std::fs::create_dir_all(global.join("projects")).unwrap();

        global_init_at(&global, false, None).unwrap();

        let bare = dir.path().join("registry-remote.git");
        create_populated_bare(&bare);

        let msg = global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(msg.contains("already initialised"), "unexpected: {msg}");
        assert!(git::has_remote(&global, "origin").unwrap());
        assert!(
            global.join("remote-file.txt").exists(),
            "remote content should have been pulled/reset"
        );
    }

    #[test]
    fn global_init_with_remote_errors_when_origin_already_set() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("config-pm");
        std::fs::create_dir_all(global.join("projects")).unwrap();

        let bare = dir.path().join("registry-remote.git");
        git::init_bare(&bare).unwrap();
        global_init_at(&global, false, Some(&bare.to_string_lossy())).unwrap();

        let bare2 = dir.path().join("other-remote.git");
        git::init_bare(&bare2).unwrap();
        let result = global_init_at(&global, false, Some(&bare2.to_string_lossy()));
        assert!(result.is_err());
    }

    // -- Global registry tests --

    fn setup_global_dir(dir: &std::path::Path) -> std::path::PathBuf {
        let global = dir.join("config-pm");
        std::fs::create_dir_all(global.join("projects")).unwrap();
        global
    }

    #[test]
    fn global_init_creates_git_repo() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        let msg = global_init_at(&global, false, None).unwrap();
        assert!(msg.contains("Initialised"));
        assert!(global.join(".git").exists());
        assert!(global.join(".gitignore").exists());
    }

    #[test]
    fn global_init_adds_the_bundled_block_to_a_pre_existing_gitignore() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        std::fs::write(global.join(".gitignore"), "*.lock\n").unwrap();
        let bundled = crate::commands::skills::bundled_workflow_names()[0];
        let wf = global.join("workflows").join(bundled);
        std::fs::create_dir_all(&wf).unwrap();
        std::fs::write(wf.join("config.toml"), "x").unwrap();

        global_init_at(&global, false, None).unwrap();

        let committed = git::cat_file(&global, "HEAD:.gitignore").unwrap();
        assert!(committed.starts_with("*.lock\n"));
        assert!(git::ls_files(&global, "workflows").unwrap().is_empty());
        assert!(wf.join("config.toml").is_file());
    }

    #[test]
    fn global_init_is_idempotent() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        global_init_at(&global, false, None).unwrap();
        let msg = global_init_at(&global, false, None).unwrap();
        assert!(msg.contains("already initialised"));
    }

    #[test]
    fn global_init_errors_without_dir() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("nonexistent");

        let result = global_init_at(&global, false, None);
        assert!(result.is_err());
    }

    #[test]
    fn global_status_shows_clean_after_init() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        global_init_at(&global, false, None).unwrap();
        let ctx = global_ctx(&global);
        let msg = status_repo(&ctx).unwrap();
        assert!(msg.contains("clean"));
    }

    #[test]
    fn global_status_shows_changes_after_modification() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        global_init_at(&global, false, None).unwrap();
        std::fs::write(global.join("projects").join("test.toml"), "x").unwrap();

        let ctx = global_ctx(&global);
        let msg = status_repo(&ctx).unwrap();
        assert!(!msg.contains("clean"), "should show changes, got: {msg}");
    }

    #[test]
    fn global_status_errors_without_init() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        let ctx = global_ctx(&global);
        let result = status_repo(&ctx);
        assert!(result.is_err());
    }

    #[test]
    fn global_remote_sets_origin() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false, None).unwrap();

        let ctx = global_ctx(&global);
        let msg = set_remote(&ctx, "https://example.com/registry.git").unwrap();
        assert!(msg.contains("https://example.com/registry.git"));
        assert!(git::has_remote(&global, "origin").unwrap());
    }

    #[test]
    fn global_remote_errors_if_already_set() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false, None).unwrap();

        let ctx = global_ctx(&global);
        set_remote(&ctx, "https://example.com/registry.git").unwrap();
        let result = set_remote(&ctx, "https://other.com/registry.git");
        assert!(result.is_err());
    }

    #[test]
    fn global_push_without_remote_commits_locally() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false, None).unwrap();

        std::fs::write(global.join("projects").join("new.toml"), "x").unwrap();

        let ctx = global_ctx(&global);
        let msg = push_repo(&ctx).unwrap();
        assert!(msg.contains("locally"), "unexpected: {msg}");
        assert!(msg.contains("no remote configured"), "unexpected: {msg}");
    }

    #[test]
    fn global_pull_without_remote_is_noop() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false, None).unwrap();

        let ctx = global_ctx(&global);
        let msg = pull_repo(&ctx).unwrap();
        assert!(msg.contains("nothing to pull"), "unexpected: {msg}");
    }

    #[test]
    fn global_push_commits_and_pushes() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false, None).unwrap();

        // Create a bare remote
        let bare = dir.path().join("registry-remote.git");
        git::init_bare(&bare).unwrap();
        git::add_remote(&global, "origin", &bare.to_string_lossy()).unwrap();

        // Push initial state
        let branch = git::current_branch(&global).unwrap();
        git::push(&global, "origin", &branch).unwrap();

        // Make a change
        std::fs::write(global.join("projects").join("new.toml"), "x").unwrap();

        let ctx = global_ctx(&global);
        let msg = push_repo(&ctx).unwrap();
        assert!(msg.contains("Committed and pushed"));
    }

    #[test]
    fn global_pull_fetches_remote_changes() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false, None).unwrap();

        // Create bare remote and push
        let bare = dir.path().join("registry-remote.git");
        git::init_bare(&bare).unwrap();
        git::add_remote(&global, "origin", &bare.to_string_lossy()).unwrap();
        let branch = git::current_branch(&global).unwrap();
        git::push(&global, "origin", &branch).unwrap();

        // Clone bare elsewhere, push a change
        let other = dir.path().join("other-clone");
        git::clone_repo(&bare.to_string_lossy(), &other).unwrap();
        std::fs::write(other.join("extra.txt"), "remote data").unwrap();
        git::add_all(&other).unwrap();
        git::commit_with_message(&other, "remote change").unwrap();
        let other_branch = git::current_branch(&other).unwrap();
        git::push(&other, "origin", &other_branch).unwrap();

        // Pull
        let ctx = global_ctx(&global);
        let msg = pull_repo(&ctx).unwrap();
        assert!(msg.contains("Pulled"));
        assert!(global.join("extra.txt").exists());
    }

    #[test]
    fn global_init_commits_existing_state() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        // Create some state before init
        std::fs::write(
            global.join("projects").join("myproject.toml"),
            "[project]\nname = \"myproject\"\n",
        )
        .unwrap();

        global_init_at(&global, false, None).unwrap();

        let ctx = global_ctx(&global);
        let msg = status_repo(&ctx).unwrap();
        assert!(msg.contains("clean"), "state should be committed: {msg}");
    }
}
