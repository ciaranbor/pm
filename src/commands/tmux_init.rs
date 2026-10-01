//! `pm tmux init`: pm's tmux plugin, run from the user's config as
//! `run-shell 'pm tmux init'` so it always matches the installed binary
//! (README, "tmux plugin", has the user-facing options).
//!
//! The user's config is theirs: init sets only the `@pm_*` options pm owns,
//! appends to a format rather than replacing it, and binds only the keys the
//! user named. A config reload runs it again, so every step is idempotent:
//! the window-list badge is appended only to a format that doesn't mention
//! it yet, and a second watcher exits at once
//! ([`tmux_watch`](super::tmux_watch)).
//!
//! It runs as a `run-shell` job, often while the config is still loading
//! and no session exists yet, so it reads global options only.

use crate::error::{PmError, Result};
use crate::tmux::options::{self, Command, Scope};
use crate::tmux::shell_quote;

use super::tmux_watch::AUTO_REFRESH;

const BIN: &str = "@pm-bin";
const WINDOW_STATUS: &str = "@pm-window-status";
const TREE_KEY: &str = "@pm-tree-key";
const ATTENTION_KEY: &str = "@pm-attention-key";

pub const TREE_FORMAT_OPTION: &str = "@pm_tree_format";

/// tmux's own tree format with pm's badges added: a session's attention and
/// its reason, a window's agent badge. Theme colours (tmux 3.8) are left out
/// so every release draws it.
pub const TREE_FORMAT: &str = concat!(
    "#{?pane_format,",
    "#{?pane_marked,#[reverse],}",
    "#{pane_current_command}#{?pane_active,*,}#{?pane_marked,M,}",
    "#{?#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}},: \"#{pane_title}\",}",
    ",",
    "#{?window_format,",
    "#{?window_marked_flag,#[reverse],}",
    "#{window_name}#{window_flags}",
    "#{?@pm_agent_badge, #{@pm_agent_badge},}",
    "#{?#{&&:#{==:#{window_panes},1},#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}}},: \"#{pane_title}\",}",
    ",",
    "#{session_windows} windows",
    "#{?session_grouped, (group #{session_group}: #{session_group_list}),}",
    "#{?session_attached, (attached),}",
    "#{?@pm_badge, #{@pm_badge}#{?@pm_reason,: #{@pm_reason},},}",
    "}}",
);

/// What init appends to each window-list format.
const BADGE: &str = "#{?@pm_agent_badge, #{@pm_agent_badge},}";
const WINDOW_FORMATS: &[&str] = &["window-status-format", "window-status-current-format"];

pub fn init(tmux_server: Option<&str>) -> Result<()> {
    let settings = options::read_global(
        tmux_server,
        &[BIN, AUTO_REFRESH, WINDOW_STATUS, TREE_KEY, ATTENTION_KEY],
    )?
    .ok_or_else(|| PmError::Tmux("no tmux server running".into()))?;
    let bin = match settings.get(BIN) {
        "" => "pm".to_string(),
        bin => shell_quote(bin),
    };

    let mut commands = vec![options::set(
        Scope::Global,
        TREE_FORMAT_OPTION,
        Some(TREE_FORMAT),
    )];
    let badges = settings.get(WINDOW_STATUS) != "off";
    for name in WINDOW_FORMATS {
        let format = options::show(tmux_server, name)?;
        commands.extend(window_status(name, &format, badges));
    }
    for (option, filter) in [(TREE_KEY, None), (ATTENTION_KEY, Some("#{@pm_attention}"))] {
        let key = settings.get(option);
        if !key.is_empty() {
            commands.push(tree_binding(key, filter, &bin));
        }
    }
    if settings.get(AUTO_REFRESH) != "off" {
        commands.push(options::run_shell_background(&format!("{bin} tmux watch")));
    }
    options::run(tmux_server, &commands)
}

/// The change to the window-list format `name`, now `format`: the badge
/// appended unless the format already shows it, or, turned off, the badge
/// init appended taken off again.
fn window_status(name: &str, format: &str, badges: bool) -> Option<Command> {
    if badges {
        if format.contains("@pm_agent_badge") {
            return None;
        }
        return Some(options::append_global(name, BADGE));
    }
    let original = format.strip_suffix(BADGE)?;
    Some(options::set(Scope::Global, name, Some(original)))
}

/// Bind `key` to pm's tree, showing only the sessions `filter` matches.
fn tree_binding(key: &str, filter: Option<&str>, bin: &str) -> Command {
    options::bind_key(
        key,
        options::choose_tree(
            &format!("#{{E:{TREE_FORMAT_OPTION}}}"),
            filter,
            &format!("run-shell \"{bin} tmux jump --client '#{{client_name}}' '%%'\""),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::OwnServer;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::tempdir;

    fn tmux(server: &OwnServer, args: &[&str]) -> String {
        let out = std::process::Command::new("tmux")
            .args(["-L", server.name().unwrap()])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn binding(server: &OwnServer, key: &str) -> String {
        tmux(server, &["list-keys", "-T", "prefix"])
            .lines()
            .find(|l| l.split_whitespace().nth(3) == Some(key))
            .unwrap_or_default()
            .to_string()
    }

    /// A stand-in for pm that records each invocation's arguments.
    fn recording_bin(dir: &Path) -> (String, std::path::PathBuf) {
        let log = dir.join("calls");
        let bin = dir.join("pm-bin with space");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\necho \"$@\" >> '{}'\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        (bin.display().to_string(), log)
    }

    #[test]
    fn init_adds_to_the_users_config_once_however_often_it_runs() {
        let dir = tempdir().unwrap();
        let server = OwnServer::start("init");
        let (bin, calls) = recording_bin(dir.path());
        tmux(&server, &["set", "-g", BIN, &bin]);
        tmux(&server, &["set", "-g", "status-right", "mine %H:%M"]);
        tmux(&server, &["setw", "-g", "window-status-format", "#I #W"]);
        tmux(
            &server,
            &["setw", "-g", "window-status-current-format", "#I*#W "],
        );
        tmux(&server, &["bind", "s", "choose-tree", "-Zs", "-O", "name"]);
        let users_s = binding(&server, "s");
        assert!(users_s.contains("choose-tree"), "{users_s}");
        tmux(&server, &["set", "-g", TREE_KEY, "T"]);

        init(server.name()).unwrap();
        init(server.name()).unwrap();

        assert_eq!(
            options::show(server.name(), "window-status-format").unwrap(),
            format!("#I #W{BADGE}")
        );
        assert_eq!(
            options::show(server.name(), "window-status-current-format").unwrap(),
            format!("#I*#W {BADGE}")
        );
        assert_eq!(
            options::show(server.name(), "status-right").unwrap(),
            "mine %H:%M"
        );
        assert_eq!(binding(&server, "s"), users_s);
        assert_eq!(
            options::show(server.name(), TREE_FORMAT_OPTION).unwrap(),
            TREE_FORMAT
        );
        // What choosing a session in the tree runs: the template as tmux
        // printed it, unquoted, with the item's target for `%%`.
        let tree = binding(&server, "T");
        let template = tree[tree.find("\"run-shell").unwrap()..]
            .trim_end()
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .unwrap()
            .replace("\\\"", "\"")
            .replace("%%", "=app/login:");
        let chosen = dir.path().join("chosen.conf");
        std::fs::write(&chosen, template).unwrap();
        tmux(&server, &["source-file", chosen.to_str().unwrap()]);
        assert_eq!(
            std::fs::read_to_string(&calls)
                .unwrap()
                .lines()
                .filter(|l| l.contains("jump"))
                .collect::<Vec<_>>(),
            ["tmux jump --client  =app/login:"],
            "no client in a sourced file"
        );
        assert_eq!(
            tmux(&server, &["list-keys", "-T", "prefix"])
                .lines()
                .filter(|l| l.contains("@pm_attention"))
                .count(),
            0,
            "a key the user didn't name"
        );
        let watches = || {
            std::fs::read_to_string(&calls)
                .unwrap_or_default()
                .lines()
                .filter(|l| *l == "tmux watch")
                .count()
        };
        let start = std::time::Instant::now();
        while watches() < 2 {
            assert!(start.elapsed() < Duration::from_secs(10), "watchers");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(watches(), 2, "each run starts a watcher");
    }

    #[test]
    fn opting_out_takes_off_only_what_init_added() {
        let server = OwnServer::start("init-off");
        tmux(&server, &["set", "-g", AUTO_REFRESH, "off"]);
        tmux(&server, &["setw", "-g", "window-status-format", "#I #W"]);
        tmux(
            &server,
            &[
                "setw",
                "-g",
                "window-status-current-format",
                "#I #{@pm_agent_badge} #W",
            ],
        );
        init(server.name()).unwrap();
        assert_eq!(
            options::show(server.name(), "window-status-current-format").unwrap(),
            "#I #{@pm_agent_badge} #W",
            "the user placed the badge"
        );

        tmux(&server, &["set", "-g", WINDOW_STATUS, "off"]);
        init(server.name()).unwrap();

        for name in WINDOW_FORMATS {
            assert_eq!(
                options::show(server.name(), name).unwrap(),
                if *name == "window-status-format" {
                    "#I #W"
                } else {
                    "#I #{@pm_agent_badge} #W"
                }
            );
        }
    }
}
