//! `pm serve logs`: the end of the server's log, and with `follow` what
//! it gains, until interrupted.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use crate::error::Result;

/// Write the last `lines` lines of `log` to `out`; then, with `follow`,
/// what is appended, starting over when the file is truncated.
pub fn logs(log: &Path, lines: usize, follow: bool, out: &mut impl Write) -> Result<()> {
    let text = match std::fs::read(log) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && follow => Vec::new(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            writeln!(out, "No log at {} yet.", log.display())?;
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    out.write_all(tail(&text, lines))?;
    out.flush()?;
    if !follow {
        return Ok(());
    }
    let mut offset = text.len() as u64;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let Ok(mut file) = std::fs::File::open(log) else {
            continue;
        };
        let len = file.metadata()?.len();
        if len < offset {
            offset = 0;
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut more = Vec::new();
        file.read_to_end(&mut more)?;
        offset += more.len() as u64;
        out.write_all(&more)?;
        out.flush()?;
    }
}

/// The last `lines` lines of `text`.
fn tail(text: &[u8], lines: usize) -> &[u8] {
    let body = text.strip_suffix(b"\n").unwrap_or(text);
    let start = body
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, b)| **b == b'\n')
        .nth(lines.saturating_sub(1))
        .map_or(0, |(i, _)| i + 1);
    if lines == 0 { &[] } else { &text[start..] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_lines_of_the_log_are_shown() {
        assert_eq!(tail(b"a\nb\nc\n", 2), b"b\nc\n");
        assert_eq!(tail(b"a\nb\nc", 2), b"b\nc");
        assert_eq!(tail(b"a\nb\n", 5), b"a\nb\n");
        assert_eq!(tail(b"a\nb\n", 0), b"");
    }
}
