//! Cutting text to fit a line of a view.

/// `text` cut to at most `max` characters, at a word's end where one is
/// near, and marked with an ellipsis when cut.
pub fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let (end, _) = text
        .char_indices()
        .nth(max.saturating_sub(1))
        .expect("longer than max");
    let head = &text[..end];
    let head = match head.rfind(' ') {
        Some(space) if space * 4 >= end * 3 => &head[..space],
        _ => head,
    };
    format!("{}…", head.trim_end())
}

/// `text`'s first line, cut to a length that fits a status line: at its
/// end, or for a line without spaces (a path), at its start, since a
/// path's end names the file.
pub fn one_line(text: &str) -> String {
    const MAX: usize = 120;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.contains(char::is_whitespace) {
        return ellipsize(line, MAX);
    }
    let chars = line.chars().count();
    if chars <= MAX {
        return line.to_string();
    }
    let (cut, _) = line
        .char_indices()
        .nth(chars - (MAX - 1))
        .expect("longer than MAX");
    format!("…{}", &line[cut..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_ends_at_a_word_and_says_so() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("the quick brown fox", 12), "the quick…");
        assert_eq!(ellipsize("abcdefghijklmnop", 6), "abcde…");
        assert_eq!(ellipsize("ééééééé", 4), "ééé…");
        assert_eq!(ellipsize("abc", 1), "…");
        assert_eq!(ellipsize("abc", 0), "");
    }

    #[test]
    fn a_long_line_is_cut_at_a_word_and_a_long_path_at_its_start() {
        let words = format!("{} end", "word ".repeat(40));
        let cut = one_line(&format!("\n  {words}\nsecond line"));
        assert!(
            cut.starts_with("word word") && cut.ends_with("word…"),
            "{cut}"
        );
        assert!(cut.chars().count() <= 120, "{cut}");

        let path = format!("/tmp/{}/important_file_name.txt", "deep/".repeat(40));
        let cut = one_line(&path);
        assert!(
            cut.starts_with('…') && cut.ends_with("/important_file_name.txt"),
            "{cut}"
        );
        assert_eq!(cut.chars().count(), 120);
    }
}
