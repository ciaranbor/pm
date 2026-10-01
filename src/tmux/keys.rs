//! Key bindings read back from tmux. `list-keys` prints a binding as
//! config-file text, so [`prefix_table`] undoes tmux's quoting to recover
//! the arguments. `list-keys -T prefix KEY` prints nothing on tmux 3.7, so
//! the whole table is listed; a binding's note is printed only by
//! `list-keys -N`, so that is listed in the same call.

use std::collections::BTreeMap;

use super::options::Command;
use super::run_tmux;
use crate::error::Result;

/// A key in the prefix table.
#[derive(Debug, Default, PartialEq)]
pub struct Binding {
    /// `bind-key -r`: the key repeats without the prefix.
    pub repeat: bool,
    /// What `list-keys -N` and `prefix ?` show for it.
    pub note: Option<String>,
    /// The command it runs, as its arguments; `None` when it runs more than
    /// one or tmux printed it in a form this module can't read.
    pub command: Option<Command>,
}

impl Binding {
    /// Bind `key` to `command` in the prefix table, keeping this binding's
    /// note and repeat flag.
    pub fn rebind(&self, key: &str, command: Command) -> Command {
        let mut bind = vec!["bind-key".to_string()];
        if self.repeat {
            bind.push("-r".into());
        }
        if let Some(note) = &self.note {
            bind.extend(["-N".to_string(), note.clone()]);
        }
        bind.push(key.into());
        bind.extend(command);
        bind
    }
}

/// Every key bound in the prefix table, by key name.
pub fn prefix_table(server: Option<&str>) -> Result<BTreeMap<String, Binding>> {
    let output = run_tmux(
        server,
        &[
            "list-keys",
            "-T",
            "prefix",
            ";",
            "list-keys",
            "-N",
            "-P",
            "",
            "-T",
            "prefix",
        ],
    )?;
    let mut table: BTreeMap<String, Binding> = BTreeMap::new();
    let mut notes = Vec::new();
    for line in output.lines() {
        let Some(rest) = line.strip_prefix("bind-key") else {
            // A note: the key, padded, then the note as written.
            notes.extend(line.trim_start().split_once(char::is_whitespace));
            continue;
        };
        // `bind-key [-r] -T prefix KEY COMMAND...`.
        let Some((flags, rest)) = rest.split_once("-T prefix") else {
            continue;
        };
        let Some((key, command)) = rest.trim_start().split_once(char::is_whitespace) else {
            continue;
        };
        table.insert(
            key.to_string(),
            Binding {
                repeat: flags.split_whitespace().any(|f| f == "-r"),
                note: None,
                command: split(command.trim()),
            },
        );
    }
    for (key, note) in notes {
        if let Some(binding) = table.get_mut(key) {
            binding.note = Some(note.trim().to_string());
        }
    }
    Ok(table)
}

/// `text`'s arguments as tmux's parser reads them: whitespace-separated,
/// with `\` escapes outside single quotes. `None` for a command list (an
/// unquoted `\;`) or a form tmux prints that this doesn't read: an octal
/// escape, a `{}` block, an unterminated quote.
fn split(text: &str) -> Option<Command> {
    let mut args = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else {
            return Some(args);
        };
        if first == '{' {
            return None;
        }
        let mut arg = String::new();
        let mut quote = None;
        let mut separator = false;
        while let Some(c) = chars.next() {
            match (quote, c) {
                (None, c) if c.is_whitespace() => break,
                (None, '\'' | '"') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (Some('\''), c) => arg.push(c),
                (_, '\\') => {
                    let escaped = chars.next()?;
                    if escaped.is_ascii_digit() {
                        return None;
                    }
                    separator = quote.is_none() && escaped == ';' && arg.is_empty();
                    arg.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        c => c,
                    });
                }
                (_, c) => arg.push(c),
            }
        }
        if quote.is_some() || (separator && arg == ";") {
            return None;
        }
        args.push(arg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::OwnServer;

    #[test]
    fn a_binding_reads_back_as_the_arguments_it_was_bound_with() {
        let server = OwnServer::start("keys");
        let tricky: Command = [
            "choose-tree",
            "-Zw",
            "-f",
            "#{&&:a b,$x}",
            "run-shell 'echo \"it''s\" \\ %%'\tend",
        ]
        .map(String::from)
        .into();
        let tmux = |args: &[&str]| {
            let status = std::process::Command::new("tmux")
                .args(["-L", server.name().unwrap()])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "{args:?}");
        };
        let mut bind = vec!["bind-key", "-r", "-N", "a \"quoted\" note", "w"];
        bind.extend(tricky.iter().map(String::as_str));
        tmux(&bind);
        tmux(&["bind-key", "x", "display a ; display b"]);

        let bound = Binding {
            repeat: true,
            note: Some("a \"quoted\" note".into()),
            command: Some(tricky.clone()),
        };
        let table = prefix_table(server.name()).unwrap();
        assert_eq!(table["w"], bound);
        assert_eq!(
            table["s"],
            Binding {
                repeat: false,
                note: Some("Choose a session from a list".into()),
                command: Some(vec!["choose-tree".to_string(), "-Zs".into()]),
            }
        );
        assert_eq!(table["x"].command, None, "a command list");
        assert!(!table.contains_key("F12"));

        let rebound: Command = vec!["choose-tree".into(), "-Zw".into()];
        crate::tmux::options::run(server.name(), &[bound.rebind("w", rebound.clone())]).unwrap();
        assert_eq!(
            prefix_table(server.name()).unwrap()["w"],
            Binding {
                command: Some(rebound),
                ..bound
            },
            "the note and repeat flag kept"
        );
    }
}
