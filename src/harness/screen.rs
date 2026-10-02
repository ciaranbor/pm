//! Reading a harness's TUI off a tmux capture taken with its escape
//! sequences (`capture-pane -e`).

/// `line` without its escape sequences or the text they draw dim.
pub(in crate::harness) fn visible_text(line: &str) -> String {
    let mut text = String::new();
    let mut dim = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if !dim {
                text.push(c);
            }
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        let mut params = String::new();
        let mut last = None;
        for c in chars.by_ref() {
            if ('\x40'..='\x7e').contains(&c) {
                last = Some(c);
                break;
            }
            params.push(c);
        }
        if last == Some('m') {
            let mut params = params.split(';');
            while let Some(param) = params.next() {
                match param {
                    "2" => dim = true,
                    "" | "0" | "22" => dim = false,
                    // An extended colour's own arguments: `5;n` or `2;r;g;b`.
                    "38" | "48" | "58" => match params.next() {
                        Some("5") => {
                            params.next();
                        }
                        Some("2") => {
                            params.nth(2);
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_are_dropped_and_dim_text_with_them() {
        assert_eq!(visible_text("\x1b[1m›\x1b[0m draft"), "› draft");
        assert_eq!(
            visible_text("\x1b[1m›\x1b[0m \x1b[2mAsk Codex\x1b[0m"),
            "› "
        );
        assert_eq!(visible_text("\x1b[2mdim\x1b[22m plain"), " plain");
        assert_eq!(visible_text("\x1b[0;2mdim\x1b[0;1m bold"), " bold");
        assert_eq!(
            visible_text("\x1b[38;2;246;2;183mtrue\x1b[39m \x1b[38;5;2mindexed"),
            "true indexed"
        );
    }
}
