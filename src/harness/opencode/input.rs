//! Reading opencode's prompt box off a screen captured with its escape
//! sequences (verified on v2.0.23). The box's last line is `╹▀▀▀…`, the line
//! above it names the agent and model, and the rows above that hold the
//! input below a padding row. Every box row opens with a `┃` drawn in the
//! colours of the `╹`, which is what tells the box from the transcript
//! blocks above it: their bars sit in the same column, but on another
//! background. A dialog (a question, a permission ask) replaces the box, so
//! a screen without one reads as unknown. On the home screen an empty box
//! shows a placeholder starting [`PLACEHOLDER`].

const BOTTOM: char = '╹';
const BAR: char = '┃';
const PLACEHOLDER: &str = "Ask anything…";

/// Whether the prompt box at the bottom of `screen` holds no text; `None`
/// when no prompt box is found.
pub(in crate::harness) fn is_empty(screen: &str) -> Option<bool> {
    let rows = rows(screen);
    let bottom = rows
        .iter()
        .rposition(|row| row.edge == Some(BOTTOM) && row.text.trim_start().starts_with('▀'))?;
    let drawn = rows[bottom].drawn.clone();
    let top = rows[..bottom]
        .iter()
        .rposition(|row| row.edge != Some(BAR) || row.drawn != drawn)
        .map_or(0, |i| i + 1);
    // The row above the bottom names the agent; a box without one is not
    // the prompt box.
    let input = rows[top..bottom].split_last()?.1;
    let text: Vec<&str> = input
        .iter()
        .map(|row| row.text.trim())
        .filter(|text| !text.is_empty())
        .collect();
    Some(match text.as_slice() {
        [] => true,
        [only] => only.starts_with(PLACEHOLDER),
        _ => false,
    })
}

/// A screen line's first box-drawing edge: the edge, the colours and column
/// it is drawn in, and the visible text after it.
struct Row {
    edge: Option<char>,
    drawn: (usize, Option<String>, Option<String>),
    text: String,
}

/// Each line of `screen` as a [`Row`]. Colours carry over from line to line,
/// as tmux emits only the changes between cells.
fn rows(screen: &str) -> Vec<Row> {
    let mut fg: Option<String> = None;
    let mut bg: Option<String> = None;
    let mut rows = Vec::new();
    for line in screen.lines() {
        let mut row = Row {
            edge: None,
            drawn: (0, None, None),
            text: String::new(),
        };
        let mut column = 0;
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if chars.next() == Some('[') {
                    let mut params = String::new();
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            if c == 'm' {
                                apply_sgr(&params, &mut fg, &mut bg);
                            }
                            break;
                        }
                        params.push(c);
                    }
                }
                continue;
            }
            if row.edge.is_some() {
                row.text.push(c);
            } else if c == BAR || c == BOTTOM {
                row.edge = Some(c);
                row.drawn = (column, fg.clone(), bg.clone());
            } else {
                column += 1;
            }
        }
        rows.push(row);
    }
    rows
}

/// Apply SGR parameters `params` to the foreground and background colours.
fn apply_sgr(params: &str, fg: &mut Option<String>, bg: &mut Option<String>) {
    let mut params = params.split(';');
    while let Some(param) = params.next() {
        let target = match param {
            "" | "0" => {
                *fg = None;
                *bg = None;
                continue;
            }
            "39" => {
                *fg = None;
                continue;
            }
            "49" => {
                *bg = None;
                continue;
            }
            "38" => &mut *fg,
            "48" => &mut *bg,
            p if p.len() == 2 && (p.starts_with('3') || p.starts_with('9')) => {
                *fg = Some(p.to_string());
                continue;
            }
            p if (p.len() == 2 && p.starts_with('4')) || (p.len() == 3 && p.starts_with("10")) => {
                *bg = Some(p.to_string());
                continue;
            }
            _ => continue,
        };
        let colour = match params.next() {
            Some("5") => params.next().map(|n| format!("5;{n}")),
            Some("2") => {
                let rgb: Vec<&str> = params.by_ref().take(3).collect();
                Some(format!("2;{}", rgb.join(";")))
            }
            _ => None,
        };
        *target = colour;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/screens/opencode-{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn an_empty_box_reads_empty_and_a_draft_does_not() {
        assert_eq!(is_empty(&screen("empty")), Some(true));
        assert_eq!(is_empty(&screen("draft")), Some(false));
    }

    #[test]
    fn the_home_screen_placeholder_reads_empty() {
        assert_eq!(is_empty(&screen("home")), Some(true));
        assert_eq!(is_empty(&screen("home-draft")), Some(false));
    }

    #[test]
    fn a_transcript_block_above_the_box_is_not_part_of_it() {
        // The prompt just sent sits in a bubble directly above the box, its
        // bar in the agent's colour on another background.
        assert_eq!(is_empty(&screen("busy")), Some(true));
    }

    #[test]
    fn a_dialog_in_place_of_the_box_is_unknown() {
        assert_eq!(is_empty(&screen("question")), None);
        assert_eq!(is_empty(""), None);
    }
}
