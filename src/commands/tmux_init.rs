//! `pm tmux init`: pm's tmux plugin, run from the user's config as
//! `run-shell 'pm tmux init'` so it always matches the installed binary
//! (README, "tmux integration", has the user-facing options).
//!
//! The user's config is theirs: init sets only the `@pm_*` options pm owns,
//! adds to a format rather than replacing it, and changes a key the user
//! didn't name only by adding pm's format and Enter action to a plain
//! `choose-tree` on `s` or `w`, keeping its flags, note and repeat flag, and
//! sorting it by name if it sets no order. The attention key, `a` unless
//! named, is bound only while unbound or pm's (`attention_key`). It reads
//! the bindings as they stand when it runs, so it must run after the config
//! binds them.
//!
//! A config reload runs it again, so every step is idempotent: the
//! window-list badge goes in once, and one an earlier pm placed elsewhere
//! is moved; pm's tree binding is recognised and rebuilt from the flags under
//! it (which also picks up a changed `@pm-bin`); pm's attention binding is
//! recognised and unbound from a key `@pm-attention-key` no longer names;
//! and a second watcher exits at once ([`tmux_watch`](super::tmux_watch)).
//!
//! It runs as a `run-shell` job, often while the config is still loading
//! and no session exists yet, so it reads global options only.

use crate::error::{PmError, Result};
use crate::tmux::keys::{self, Binding};
use crate::tmux::options::{self, Command, Scope};
use crate::tmux::shell_quote;

use super::tmux_watch::AUTO_REFRESH;

mod attention_key;
mod window_status;

pub(super) const BIN: &str = "@pm-bin";
const WINDOW_STATUS: &str = "@pm-window-status";
const BIND_TREE: &str = "@pm-bind-tree";
const ATTENTION_KEY: &str = "@pm-attention-key";

pub const TREE_FORMAT_OPTION: &str = "@pm_tree_format";

/// tmux's own tree format with pm's badges added: a session's activity,
/// attention and reason, a window's agent badge. Pane titles, usually a
/// path, and theme colours (tmux 3.8) are left out.
pub const TREE_FORMAT: &str = concat!(
    "#{?pane_format,",
    "#{?pane_marked,#[reverse],}",
    "#{pane_current_command}#{?pane_active,*,}#{?pane_marked,M,}",
    ",",
    "#{?window_format,",
    "#{?window_marked_flag,#[reverse],}",
    "#{window_name}#{window_flags}",
    "#{?@pm_agent_badge, #{@pm_agent_badge},}",
    ",",
    "#{session_windows} windows",
    "#{?session_grouped, (group #{session_group}: #{session_group_list}),}",
    "#{?session_attached, (attached),}",
    "#{?@pm_activity, #{@pm_activity},}",
    "#{?@pm_badge, #{@pm_badge}#{?@pm_reason, #{@pm_reason},},}",
    "}}",
);

/// The keys whose `choose-tree` init gives pm's format and Enter action:
/// tmux's session and window trees.
const TREE_KEYS: &[&str] = &["s", "w"];

const WINDOW_FORMATS: &[&str] = &["window-status-format", "window-status-current-format"];

pub fn init(tmux_server: Option<&str>) -> Result<()> {
    let settings = options::read_global(
        tmux_server,
        &[BIN, AUTO_REFRESH, WINDOW_STATUS, BIND_TREE, ATTENTION_KEY],
    )?
    .ok_or_else(|| PmError::Tmux("no tmux server running".into()))?;
    let bin = match settings.get(BIN) {
        "" => "pm".to_string(),
        bin => shell_quote(bin),
    };

    let mut commands = format_commands(tmux_server, settings.get(WINDOW_STATUS) != "off")?;
    let template = jump_template(&bin);
    let bind_tree = settings.get(BIND_TREE) != "off";
    let table = keys::prefix_table(tmux_server)?;
    for key in TREE_KEYS {
        if let Some(bound) = table.get(*key) {
            commands.extend(tree_key(key, bound, bind_tree, &template));
        }
    }
    let defaults = std::cell::OnceCell::new();
    commands.extend(attention_key::commands(
        settings.get(ATTENTION_KEY),
        &table,
        &tree_format(),
        &template,
        |key| {
            defaults
                .get_or_init(|| keys::default_prefix_table().unwrap_or_default())
                .get(key)
                .cloned()
        },
    ));
    if settings.get(AUTO_REFRESH) != "off" {
        commands.push(options::run_shell_background(&format!("{bin} tmux watch")));
    }
    options::run(tmux_server, &commands)
}

/// Re-set the formats pm owns, so a running server picks up the ones this
/// binary has without a config reload. Bindings are left to init: they
/// read the tree format through `@pm_tree_format`. No server, no change.
pub(super) fn formats(tmux_server: Option<&str>) -> Result<()> {
    let Some(settings) = options::read_global(tmux_server, &[WINDOW_STATUS])? else {
        return Ok(());
    };
    let commands = format_commands(tmux_server, settings.get(WINDOW_STATUS) != "off")?;
    options::run(tmux_server, &commands)
}

/// pm's tree format, and the window-list badge in or out per `badges`.
fn format_commands(tmux_server: Option<&str>, badges: bool) -> Result<Vec<Command>> {
    let mut commands = vec![options::set(
        Scope::Global,
        TREE_FORMAT_OPTION,
        Some(TREE_FORMAT),
    )];
    for name in WINDOW_FORMATS {
        let format = options::show(tmux_server, name)?;
        commands.extend(window_status(name, &format, badges));
    }
    Ok(commands)
}

/// The change to the window-list format `name`, now `format`, that
/// places pm's badge ([`window_status`]), or with `badges` off removes it.
fn window_status(name: &str, format: &str, badges: bool) -> Option<Command> {
    let wanted = window_status::wanted(format, badges);
    (wanted != format).then(|| options::set(Scope::Global, name, Some(&wanted)))
}

fn tree_format() -> String {
    format!("#{{E:{TREE_FORMAT_OPTION}}}")
}

/// What choosing an item in pm's tree runs: `pm tmux jump` to its target.
fn jump_template(bin: &str) -> String {
    format!("run-shell \"{bin} tmux jump --client '#{{client_name}}' '%%'\"")
}

/// The rebinding of `key`, now `bound`: with `on`, a `choose-tree` with
/// neither a format nor a template of the user's own gets pm's format and
/// `template`, and sorts by name unless it sets an order, so each project's
/// `{project}/{scope}` sessions sit together. Turned off, pm's format and
/// template come off again and so does `-O name`: a binding still pm's after
/// the config has run is one the config no longer sets, so its name order is
/// taken to be pm's default rather than the user's.
fn tree_key(key: &str, bound: &Binding, on: bool, template: &str) -> Option<Command> {
    let (command, args) = bound.command.as_ref()?.split_first()?;
    if command != "choose-tree" {
        return None;
    }
    let mut flags = Vec::new();
    let mut sort = None;
    let mut format = None;
    let mut user_template = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if user_template.is_some() {
            return None;
        }
        match arg.as_str() {
            "-F" => format = Some(args.next()?.clone()),
            "-O" => sort = Some(args.next()?.clone()),
            "-f" | "-K" | "-t" => flags.extend([arg.clone(), args.next()?.clone()]),
            a if a.len() > 1
                && a.starts_with('-')
                && a[1..].chars().all(|c| "GNrswyZ".contains(c)) =>
            {
                flags.push(arg.clone())
            }
            _ => user_template = Some(arg.clone()),
        }
    }
    let pms = format.as_deref() == Some(tree_format().as_str())
        && user_template
            .as_deref()
            .is_some_and(|t| t.contains(" tmux jump "));
    if pms {
        format = None;
        user_template = None;
    }
    let mut command = vec!["choose-tree".to_string()];
    command.extend(flags);
    if on {
        if format.is_some() || user_template.is_some() {
            return None;
        }
        let sort = sort.unwrap_or_else(|| "name".to_string());
        command.extend(["-O".to_string(), sort, "-F".to_string(), tree_format()]);
        command.push(template.to_string());
    } else if !pms {
        return None;
    } else if let Some(sort) = sort.filter(|s| s != "name") {
        command.extend(["-O".to_string(), sort]);
    }
    Some(bound.rebind(key, command))
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

    fn binding(server: &OwnServer, key: &str) -> Command {
        keys::prefix_table(server.name()).unwrap()[key]
            .command
            .clone()
            .unwrap()
    }

    fn args(args: &[&str]) -> Command {
        args.iter().map(|a| a.to_string()).collect()
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
        tmux(&server, &["bind", "s", "choose-tree", "-Zs", "-O", "index"]);
        let users_w = ["choose-tree", "-Zw", "-F", "#{window_name}"];
        tmux(&server, &[&["bind", "w"], &users_w[..]].concat());

        init(server.name()).unwrap();
        let first = binding(&server, "s");
        init(server.name()).unwrap();

        assert_eq!(
            options::show(server.name(), "status-right").unwrap(),
            "mine %H:%M"
        );
        let template = jump_template(&shell_quote(&bin));
        assert_eq!(
            binding(&server, "s"),
            args(&[
                "choose-tree",
                "-Zs",
                "-F",
                &tree_format(),
                "-O",
                "index",
                &template
            ]),
            "the user's flags kept, in tmux's order"
        );
        assert_eq!(binding(&server, "s"), first, "nothing stacked by a rerun");
        assert_eq!(
            binding(&server, "w"),
            args(&users_w),
            "the user's own format"
        );
        assert_eq!(
            options::show(server.name(), TREE_FORMAT_OPTION).unwrap(),
            TREE_FORMAT
        );
        // What choosing a session in the tree runs, as tmux stored it, with
        // the item's target for `%%`.
        let template = first.last().unwrap().replace("%%", "=app/login:");
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
        tmux(&server, &["setw", "-g", "window-status-format", "#I #W "]);
        tmux(
            &server,
            &[
                "setw",
                "-g",
                "window-status-current-format",
                "#I #{@pm_agent_badge} #W",
            ],
        );
        let users_w = ["choose-tree", "-Zw", "switch-client -t '%%'"];
        tmux(&server, &[&["bind", "w"], &users_w[..]].concat());
        init(server.name()).unwrap();
        assert_eq!(
            options::show(server.name(), "window-status-format").unwrap(),
            "#I #{?@pm_agent_badge,#{@pm_agent_badge} ,}#W "
        );
        assert_eq!(
            options::show(server.name(), "window-status-current-format").unwrap(),
            "#I #{@pm_agent_badge} #W",
            "the user placed the badge"
        );
        assert_eq!(
            binding(&server, "s"),
            args(&[
                "choose-tree",
                "-Zs",
                "-F",
                &tree_format(),
                "-O",
                "name",
                &jump_template("pm")
            ]),
            "tmux's own binding, sorted by name"
        );
        assert_eq!(
            binding(&server, "w"),
            args(&users_w),
            "the user's own template"
        );

        tmux(&server, &["set", "-g", WINDOW_STATUS, "off"]);
        tmux(&server, &["set", "-g", BIND_TREE, "off"]);
        init(server.name()).unwrap();

        assert_eq!(binding(&server, "s"), args(&["choose-tree", "-Zs"]));
        assert_eq!(binding(&server, "w"), args(&users_w));

        for name in WINDOW_FORMATS {
            assert_eq!(
                options::show(server.name(), name).unwrap(),
                if *name == "window-status-format" {
                    "#I #W "
                } else {
                    "#I #{@pm_agent_badge} #W"
                }
            );
        }
    }

    /// The gruvbox-material theme's formats, as an earlier pm left them:
    /// the badge prepended to one, appended to the other.
    const THEME: &str = "#[fg=#928374,bg=#32302f] #I #[fg=#928374,bg=#32302f] #W ";
    const THEME_CURRENT: &str = "#[fg=#32302f,bg=#32302f,nobold,nounderscore,noitalics]#[fg=#ddc7a1,bg=#32302f] #I #[fg=#ddc7a1,bg=#32302f] #W #[fg=#32302f,bg=#32302f,nobold,nounderscore,noitalics]";
    const PREPENDED: &str = "#{?@pm_agent_badge,#{@pm_agent_badge} ,}";
    const APPENDED: &str = "#{?@pm_agent_badge, #{@pm_agent_badge},}";

    #[test]
    fn the_window_badge_sits_before_the_name_in_the_themes_style() {
        let server = OwnServer::start("init-badge");
        tmux(&server, &["set", "-g", AUTO_REFRESH, "off"]);
        tmux(
            &server,
            &["new-window", "-d", "-t", "keepalive:", "-n", "main"],
        );
        let window = "keepalive:main";
        let show = |name: &str| options::show(server.name(), name).unwrap();
        let drawn = |name: &str| {
            tmux(
                &server,
                &["display", "-p", "-t", window, &format!("#{{E:{name}}}")],
            )
            .trim_end_matches('\n')
            .to_string()
        };
        let theme_drawn = |name: &str, theme: &str| {
            tmux(&server, &["setw", "-g", name, theme]);
            drawn(name)
        };
        let plain = theme_drawn("window-status-format", THEME);
        let plain_current = theme_drawn("window-status-current-format", THEME_CURRENT);
        tmux(
            &server,
            &[
                "setw",
                "-g",
                "window-status-format",
                &format!("{PREPENDED}{THEME}"),
            ],
        );
        tmux(
            &server,
            &[
                "setw",
                "-g",
                "window-status-current-format",
                &format!("{THEME_CURRENT}{APPENDED}"),
            ],
        );

        init(server.name()).unwrap();
        let first = (
            show("window-status-format"),
            show("window-status-current-format"),
        );
        init(server.name()).unwrap();

        assert_eq!(
            first,
            (
                show("window-status-format"),
                show("window-status-current-format")
            ),
            "a rerun changes nothing"
        );
        assert_eq!(
            first.0,
            "#[fg=#928374,bg=#32302f] #I #[fg=#928374,bg=#32302f] \
             #{?@pm_agent_badge,#{@pm_agent_badge}#[fg=#928374#,bg=#32302f]#[fg=#928374#,bg=#32302f] ,}#W ",
            "a prepended badge moved before the name"
        );
        assert_eq!(
            first.1,
            "#[fg=#32302f,bg=#32302f,nobold,nounderscore,noitalics]#[fg=#ddc7a1,bg=#32302f] #I #[fg=#ddc7a1,bg=#32302f] \
             #{?@pm_agent_badge,#{@pm_agent_badge}\
             #[fg=#32302f#,bg=#32302f#,nobold#,nounderscore#,noitalics]#[fg=#ddc7a1#,bg=#32302f]#[fg=#ddc7a1#,bg=#32302f] ,}\
             #W #[fg=#32302f,bg=#32302f,nobold,nounderscore,noitalics]",
            "an appended badge moved before the name"
        );
        assert_eq!(
            (
                drawn("window-status-format"),
                drawn("window-status-current-format")
            ),
            (plain, plain_current),
            "no badge, no change to the bar"
        );
        let badge = "#[fg=green]\u{f013}#[default]";
        tmux(&server, &["setw", "-t", window, "@pm_agent_badge", badge]);
        assert_eq!(
            drawn("window-status-current-format"),
            format!(
                "#[fg=#32302f,bg=#32302f,nobold,nounderscore,noitalics]#[fg=#ddc7a1,bg=#32302f] {index} #[fg=#ddc7a1,bg=#32302f] \
                 {badge}#[fg=#32302f,bg=#32302f,nobold,nounderscore,noitalics]#[fg=#ddc7a1,bg=#32302f]#[fg=#ddc7a1,bg=#32302f] \
                 main #[fg=#32302f,bg=#32302f,nobold,nounderscore,noitalics]",
                index = tmux(&server, &["display", "-p", "-t", window, "#I"]).trim(),
            ),
            "after the badge's reset, the theme's styles before the name again"
        );
        tmux(
            &server,
            &[
                "setw",
                "-g",
                "window-status-format",
                "#[fg=#{?window_active,red,blue},bg=b]#W",
            ],
        );
        init(server.name()).unwrap();
        assert_eq!(
            drawn("window-status-format"),
            format!("#[fg=blue,bg=b]{badge}#[fg=blue,bg=b] main"),
            "a theme style holding a conditional"
        );

        tmux(&server, &["setw", "-gu", "window-status-format"]);
        let default = show("window-status-format");
        init(server.name()).unwrap();
        assert_eq!(
            show("window-status-format"),
            default.replacen("#W", &format!("{PREPENDED}#W"), 1),
            "tmux's own format: nothing to restore"
        );

        tmux(&server, &["set", "-g", WINDOW_STATUS, "off"]);
        init(server.name()).unwrap();
        assert_eq!(
            (
                show("window-status-format"),
                show("window-status-current-format")
            ),
            (default, THEME_CURRENT.to_string())
        );
    }

    #[test]
    fn the_default_trees_follow_a_changed_bin_and_keep_their_notes() {
        let server = OwnServer::start("init-bin");
        tmux(&server, &["set", "-g", AUTO_REFRESH, "off"]);
        let tree = |flags: &str, bin: &str| {
            args(&[
                "choose-tree",
                flags,
                "-F",
                &tree_format(),
                "-O",
                "name",
                &jump_template(bin),
            ])
        };

        init(server.name()).unwrap();
        assert_eq!(binding(&server, "s"), tree("-Zs", "pm"));
        assert_eq!(binding(&server, "w"), tree("-Zw", "pm"));

        tmux(&server, &["set", "-g", BIN, "/new/pm"]);
        init(server.name()).unwrap();

        let table = keys::prefix_table(server.name()).unwrap();
        assert_eq!(table["s"].command, Some(tree("-Zs", "'/new/pm'")));
        assert_eq!(table["w"].command, Some(tree("-Zw", "'/new/pm'")));
        assert_eq!(
            table["s"].note.as_deref(),
            Some("Choose a session from a list")
        );
        assert_eq!(
            table["w"].note.as_deref(),
            Some("Choose a window from a list")
        );
    }
}
