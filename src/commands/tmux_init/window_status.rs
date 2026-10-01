//! Where init puts `@pm_agent_badge` in a window-list format: just before
//! the window name, so the bar reads `3 ⚙ main`.
//!
//! The badge ends in `#[default]`, which resets to the window's base style,
//! not to the theme's inline style the name was drawn in. So pm's badge is
//! followed by every top-level `#[…]` that precedes the name in the
//! format: applied in order from the base style, they rebuild the style in
//! effect there. A style set inside a `#{…}` before the name is not
//! carried over. Without a top-level name the badge goes first, where the
//! base style is the one in effect and nothing needs restoring.

const BADGE_OPEN: &str = "#{?@pm_agent_badge,#{@pm_agent_badge}";
const BADGE_CLOSE: &str = " ,}";
/// What earlier pm versions appended to each window-list format.
const APPENDED_BADGE: &str = "#{?@pm_agent_badge, #{@pm_agent_badge},}";
const BADGE_OPTION: &str = "@pm_agent_badge";

/// `format` as init wants it: with `badges`, pm's badge before the window
/// name, unless the user placed `@pm_agent_badge` themselves; without,
/// none of pm's. Any badge an earlier pm placed, appended or prepended,
/// comes off first.
pub(super) fn wanted(format: &str, badges: bool) -> String {
    let base = strip(format);
    if !badges || base.contains(BADGE_OPTION) {
        return base;
    }
    let tokens = scan(&base);
    let name = tokens.iter().enumerate().find_map(|(i, t)| match t {
        Token::Name(at) => Some((i, *at)),
        Token::Style(_) => None,
    });
    let (at, styles) = match name {
        Some((i, at)) => {
            let styles: String = tokens[..i]
                .iter()
                .filter_map(|t| match t {
                    Token::Style(style) => Some(*style),
                    Token::Name(_) => None,
                })
                .collect();
            (at, styles)
        }
        None => (0, String::new()),
    };
    format!(
        "{}{BADGE_OPEN}{}{BADGE_CLOSE}{}",
        &base[..at],
        escape_commas(&styles),
        &base[at..]
    )
}

/// `format` without any badge pm placed.
fn strip(format: &str) -> String {
    let mut rest = format;
    let mut out = String::new();
    while let Some(start) = rest.find(BADGE_OPEN) {
        let after = &rest[start + BADGE_OPEN.len()..];
        let styles = styles_len(after);
        match after[styles..].strip_prefix(BADGE_CLOSE) {
            Some(tail) => {
                out.push_str(&rest[..start]);
                rest = tail;
            }
            None => {
                out.push_str(&rest[..start + BADGE_OPEN.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    match out.strip_suffix(APPENDED_BADGE) {
        Some(base) => base.to_string(),
        None => out,
    }
}

/// The length of the run of `#[…]` directives at the start of `s`.
fn styles_len(s: &str) -> usize {
    let mut len = 0;
    while s[len..].starts_with("#[") {
        match s[len..].find(']') {
            Some(end) => len += end + 1,
            None => break,
        }
    }
    len
}

/// A comma in a conditional's branch must be `#,`, or it ends the branch;
/// one inside a nested `#{…}` belongs to that expression and stays.
fn escape_commas(styles: &str) -> String {
    let bytes = styles.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let len = match (bytes[i], bytes.get(i + 1)) {
            (b'#', Some(b'{')) => braced_len(&bytes[i..]).unwrap_or(bytes.len() - i),
            (b'#', Some(_)) => 1 + styles[i + 1..].chars().next().map_or(0, char::len_utf8),
            (b',', _) => {
                out.push('#');
                1
            }
            _ => styles[i..].chars().next().map_or(1, char::len_utf8),
        };
        out.push_str(&styles[i..i + len]);
        i += len;
    }
    out
}

enum Token<'a> {
    Style(&'a str),
    /// The byte offset of `#W` or `#{…window_name}`.
    Name(usize),
}

/// The top-level styles and window names in `format`, in order: what is
/// inside a `#{…}` is skipped.
fn scan(format: &str) -> Vec<Token<'_>> {
    let bytes = format.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'#' || i + 1 == bytes.len() {
            i += 1;
            continue;
        }
        match bytes[i + 1] {
            b'[' => match format[i..].find(']') {
                Some(end) => {
                    tokens.push(Token::Style(&format[i..=i + end]));
                    i += end + 1;
                }
                None => return tokens,
            },
            b'{' => {
                let Some(len) = braced_len(&bytes[i..]) else {
                    return tokens;
                };
                let inner = &format[i + 2..i + len - 1];
                if inner == "window_name"
                    || (!inner.starts_with('?') && inner.ends_with(":window_name"))
                {
                    tokens.push(Token::Name(i));
                }
                i += len;
            }
            b'W' => {
                tokens.push(Token::Name(i));
                i += 2;
            }
            _ => i += 2,
        }
    }
    tokens
}

/// The length of the `#{…}` at the start of `bytes`, nested ones included.
fn braced_len(bytes: &[u8]) -> Option<usize> {
    let mut depth = 0;
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1)) {
            (b'#', Some(b'{')) => {
                depth += 1;
                i += 2;
            }
            (b'#', Some(_)) => i += 2,
            (b'}', _) => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const BADGE: &str = "#{?@pm_agent_badge,#{@pm_agent_badge} ,}";

    #[test]
    fn the_badge_goes_before_the_first_top_level_name_or_else_first() {
        assert_eq!(
            wanted("#[fg=red]#I #{=/8/…:window_name}", true),
            "#[fg=red]#I #{?@pm_agent_badge,#{@pm_agent_badge}#[fg=red] ,}#{=/8/…:window_name}"
        );
        assert_eq!(
            wanted("##W #{?window_zoomed_flag,#[bold]#W,#W}", true),
            format!("{BADGE}##W #{{?window_zoomed_flag,#[bold]#W,#W}}"),
            "an escaped #W and one inside a conditional are not top-level names"
        );
        assert_eq!(
            wanted("#[fg=#{?window_active,red,blue},bg=b]#I #W", true),
            "#[fg=#{?window_active,red,blue},bg=b]#I \
             #{?@pm_agent_badge,#{@pm_agent_badge}#[fg=#{?window_active,red,blue}#,bg=b] ,}#W",
            "a comma inside a nested expression is that expression's"
        );
        assert_eq!(wanted("#I", true), format!("{BADGE}#I"));
    }

    #[test]
    fn pms_badges_come_off_and_the_users_stays() {
        let users = "#I #{@pm_agent_badge} #W";
        assert_eq!(wanted(&format!("{BADGE}{users}"), true), users);
        assert_eq!(
            wanted(
                "#I #{?@pm_agent_badge,#{@pm_agent_badge}#[fg=a#,bg=b] ,}#W",
                false
            ),
            "#I #W"
        );
    }
}
