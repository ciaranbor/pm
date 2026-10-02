//! The attention key: pm's tree showing only the sessions needing
//! attention. `choose-tree -f` shows every session when its filter matches
//! none, so the key opens the tree only while some session has
//! `@pm_attention` set, and otherwise says there is nothing to show.
//!
//! pm's binding is told from the user's by its note, and an earlier pm's,
//! which had none, by being exactly what that pm bound. Only pm's binding
//! is ever replaced or removed; a key of the user's is skipped silently,
//! since `run-shell` output from a config is either lost or opens view mode.
//! A key pm lets go of gets back the binding tmux gives it by default, if
//! any.

use std::collections::BTreeMap;

use crate::tmux::keys::Binding;
use crate::tmux::options::{self, Command};
use crate::tmux::shell_quote;

const DEFAULT_KEY: &str = "a";

const NOTE: &str = "Choose a session needing attention";
const FILTER: &str = "#{@pm_attention}";
const ANY_SESSION: &str = "#{S:#{?@pm_attention,1,}}";
const EMPTY: &str = "nothing needs attention";

/// What places pm's attention tree on `setting`'s key (unset:
/// [`DEFAULT_KEY`]; `off`: none) given the prefix `table`, drawing each line
/// in `format` and running `template` on the chosen one. pm's binding on any
/// other key comes off, back to the config line `default` gives for it.
pub(super) fn commands(
    setting: &str,
    table: &BTreeMap<String, Binding>,
    format: &str,
    template: &str,
    default: impl Fn(&str) -> Option<String>,
) -> Vec<Command> {
    let key = match setting {
        "" => Some(DEFAULT_KEY),
        "off" => None,
        key => Some(key),
    };
    let mut commands: Vec<Command> = table
        .iter()
        .filter(|(k, bound)| Some(k.as_str()) != key && is_pms(bound, format))
        .map(|(k, _)| match default(k) {
            // `if-shell` parses its command as a config line.
            Some(line) => vec!["if-shell".into(), "-F".into(), "1".into(), line],
            None => vec!["unbind-key".to_string(), k.clone()],
        })
        .collect();
    if let Some(key) = key
        && table.get(key).is_none_or(|bound| is_pms(bound, format))
    {
        let pms = Binding {
            note: Some(NOTE.into()),
            ..Binding::default()
        };
        commands.push(pms.rebind(key, binding(format, template)));
    }
    commands
}

/// Whether `bound` is pm's: noted as pm's, or the note-less `choose-tree`
/// an earlier pm bound, as tmux prints it.
fn is_pms(bound: &Binding, format: &str) -> bool {
    if bound.note.as_deref() == Some(NOTE) {
        return true;
    }
    let earlier = [
        "choose-tree",
        "-Zs",
        "-F",
        format,
        "-O",
        "name",
        "-f",
        FILTER,
    ];
    bound.note.is_none()
        && bound.command.as_ref().is_some_and(|command| {
            command.len() == earlier.len() + 1
                && command.iter().zip(earlier).all(|(a, b)| a == b)
                && command[earlier.len()].contains(" tmux jump ")
        })
}

fn binding(format: &str, template: &str) -> Command {
    let tree = options::choose_tree(format, Some(FILTER), template);
    let tree: Vec<String> = tree.iter().map(|a| shell_quote(a)).collect();
    vec![
        "if-shell".into(),
        "-F".into(),
        ANY_SESSION.into(),
        tree.join(" "),
        format!("display-message {}", shell_quote(EMPTY)),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::{ATTENTION_KEY, AUTO_REFRESH, BIN, init, jump_template, tree_format};
    use crate::testing::OwnServer;
    use crate::tmux::keys::{self, Binding};
    use crate::tmux::options;
    use crate::tmux::shell_quote;
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

    fn bound(server: &OwnServer, key: &str) -> Option<Binding> {
        keys::prefix_table(server.name()).unwrap().remove(key)
    }

    #[test]
    fn the_default_key_opens_pms_tree_only_while_something_needs_attention() {
        let dir = tempdir().unwrap();
        let bin = dir.path().join("pm bin");
        let server = OwnServer::start("init-attention");
        tmux(&server, &["set", "-g", AUTO_REFRESH, "off"]);
        tmux(&server, &["set", "-g", BIN, bin.to_str().unwrap()]);

        init(server.name()).unwrap();
        let a = bound(&server, "a").expect("bound by default");
        assert_eq!(a.note.as_deref(), Some(super::NOTE));
        let press = a.command.unwrap();
        let press: Vec<&str> = press.iter().map(String::as_str).collect();
        let in_tree = || {
            tmux(
                &server,
                &["list-panes", "-a", "-F", "#{pane_id} #{pane_mode}"],
            )
            .lines()
            .filter_map(|l| l.strip_suffix(" tree-mode").map(String::from))
            .collect::<Vec<_>>()
        };

        tmux(&server, &press);
        assert_eq!(
            in_tree(),
            Vec::<String>::new(),
            "no session flagged: a message"
        );

        tmux(
            &server,
            &["set", "-t", "keepalive", "@pm_attention", "blocked"],
        );
        tmux(&server, &press);
        assert_eq!(in_tree().len(), 1, "a session flagged: the tree");

        let source = dir.path().join("then.conf");
        std::fs::write(&source, format!("bind-key X {}", press[3])).unwrap();
        tmux(&server, &["source-file", source.to_str().unwrap()]);
        assert_eq!(
            bound(&server, "X").unwrap().command.unwrap(),
            [
                "choose-tree".to_string(),
                "-Zs".into(),
                "-F".into(),
                tree_format(),
                "-O".into(),
                "name".into(),
                "-f".into(),
                "#{@pm_attention}".into(),
                jump_template(&shell_quote(bin.to_str().unwrap())),
            ],
            "pm's tree, as tmux parses the branch"
        );
    }

    #[test]
    fn only_pms_binding_is_ever_replaced_or_removed() {
        let server = OwnServer::start("init-attention-key");
        tmux(&server, &["set", "-g", AUTO_REFRESH, "off"]);
        let earlier_pms = options::choose_tree(
            &tree_format(),
            Some("#{@pm_attention}"),
            &jump_template("pm"),
        );
        let earlier_pms: Vec<&str> = earlier_pms.iter().map(String::as_str).collect();
        tmux(&server, &[&["bind-key", "a"], &earlier_pms[..]].concat());
        tmux(&server, &[&["bind-key", "A"], &earlier_pms[..]].concat());
        let users = [
            "choose-tree",
            "-Zw",
            "-f",
            "#{@pm_attention}",
            "run-shell \"pm tmux jump --client '#{client_name}' '%%'\"",
        ];
        tmux(&server, &[&["bind-key", "B"], &users[..]].concat());

        init(server.name()).unwrap();
        let pms = bound(&server, "a").unwrap();
        assert_eq!(
            pms.note.as_deref(),
            Some(super::NOTE),
            "an earlier pm's upgraded"
        );
        assert_eq!(
            bound(&server, "A"),
            None,
            "an earlier pm's other key let go"
        );
        assert_eq!(bound(&server, "B").unwrap().command.unwrap(), users);

        tmux(&server, &["set", "-g", ATTENTION_KEY, "A"]);
        init(server.name()).unwrap();
        init(server.name()).unwrap();
        assert_eq!(bound(&server, "a"), None, "pm's old key let go");
        assert_eq!(bound(&server, "A"), Some(pms), "the same binding, once");

        tmux(&server, &["set", "-g", ATTENTION_KEY, "off"]);
        init(server.name()).unwrap();
        assert_eq!(bound(&server, "A"), None);
        assert_eq!(bound(&server, "a"), None);

        tmux(&server, &["unbind", "q"]);
        tmux(&server, &["set", "-g", ATTENTION_KEY, "q"]);
        init(server.name()).unwrap();
        assert_eq!(
            bound(&server, "q").unwrap().note.as_deref(),
            Some(super::NOTE)
        );
        tmux(&server, &["set", "-g", ATTENTION_KEY, "off"]);
        init(server.name()).unwrap();
        assert_eq!(
            bound(&server, "q"),
            Some(Binding {
                repeat: false,
                note: Some("Display pane numbers".into()),
                command: Some(vec!["display-panes".into()]),
            }),
            "tmux's own binding back"
        );

        tmux(&server, &["bind", "a", "display-message", "mine"]);
        tmux(&server, &["set", "-gu", ATTENTION_KEY]);
        init(server.name()).unwrap();
        assert_eq!(
            bound(&server, "a").unwrap().command.unwrap(),
            ["display-message", "mine"]
        );
        assert_eq!(bound(&server, "B").unwrap().command.unwrap(), users);
        assert!(
            keys::prefix_table(server.name())
                .unwrap()
                .values()
                .all(|b| b.note.as_deref() != Some(super::NOTE)),
            "pm's tree on no other key"
        );
    }
}
