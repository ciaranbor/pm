//! Complete lines of a transcript file, read forward from an offset or
//! backward from one, a chunk at a time. A line is complete once its
//! newline is written; a partial last line is never returned. A `.zst`
//! file is decompressed whole, its offsets those of the decompressed text.

use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

pub(super) const CHUNK: u64 = 64 * 1024;

trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

pub(super) struct Source {
    reader: Box<dyn ReadSeek>,
    pub(super) len: u64,
}

pub(super) fn open(path: &Path) -> io::Result<Source> {
    let mut file = std::fs::File::open(path)?;
    if path.extension().is_some_and(|e| e == "zst") {
        let mut text = Vec::new();
        ruzstd::decoding::StreamingDecoder::new(&mut file)
            .map_err(io::Error::other)?
            .read_to_end(&mut text)?;
        let len = text.len() as u64;
        return Ok(Source {
            reader: Box::new(io::Cursor::new(text)),
            len,
        });
    }
    let len = file.metadata()?.len();
    Ok(Source {
        reader: Box::new(file),
        len,
    })
}

impl Source {
    pub(super) fn read_at(&mut self, start: u64, end: u64) -> io::Result<Vec<u8>> {
        self.reader.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        self.reader
            .by_ref()
            .take(end - start)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// The offset just past the last newline: where the complete lines end.
    pub(super) fn complete_len(&mut self) -> io::Result<u64> {
        let mut end = self.len;
        while end > 0 {
            let start = end.saturating_sub(CHUNK);
            let bytes = self.read_at(start, end)?;
            if let Some(at) = bytes.iter().rposition(|b| *b == b'\n') {
                return Ok(start + at as u64 + 1);
            }
            end = start;
        }
        Ok(0)
    }

    /// Whether `offset` is a line's start in this file.
    pub(super) fn is_line_start(&mut self, offset: u64) -> io::Result<bool> {
        Ok(offset == 0 || (offset <= self.len && self.read_at(offset - 1, offset)? == b"\n"))
    }

    /// The complete lines from `from`, until one ends past `from + max`.
    /// Returns them with their offsets, and the offset after the last.
    pub(super) fn forward(&mut self, from: u64, max: u64) -> io::Result<(Vec<(u64, String)>, u64)> {
        self.reader.seek(SeekFrom::Start(from))?;
        let mut reader = BufReader::new(self.reader.by_ref().take(self.len - from));
        let mut lines = Vec::new();
        let mut at = from;
        let mut buf = Vec::new();
        while at - from < max {
            buf.clear();
            let n = reader.read_until(b'\n', &mut buf)?;
            if n == 0 || buf.last() != Some(&b'\n') {
                break;
            }
            lines.push((at, String::from_utf8_lossy(&buf[..n - 1]).into_owned()));
            at += n as u64;
        }
        Ok((lines, at))
    }
}

/// The complete lines before an offset, last first.
pub(super) struct Backward {
    /// Where the unread part ends.
    pos: u64,
    /// The start of the line that ends where the read part begins, cut
    /// by the last chunk read: the bytes from `pos` to its end.
    carry: Vec<u8>,
    /// Complete lines read but not yet returned, last at the end.
    ready: Vec<(u64, String)>,
    /// Whether the next chunk read is the first, ending at a newline.
    first: bool,
    yielded: bool,
    read: u64,
    max: u64,
}

impl Backward {
    /// From `end`, a line's start, reading at most about `max` bytes once
    /// a line has been returned.
    pub(super) fn new(end: u64, max: u64) -> Self {
        Self {
            pos: end,
            carry: Vec::new(),
            ready: Vec::new(),
            first: true,
            yielded: false,
            read: 0,
            max,
        }
    }

    pub(super) fn next(&mut self, src: &mut Source) -> io::Result<Option<(u64, String)>> {
        loop {
            if let Some(line) = self.ready.pop() {
                self.yielded = true;
                return Ok(Some(line));
            }
            if self.pos == 0 || (self.yielded && self.read >= self.max) {
                return Ok(None);
            }
            let start = self.pos.saturating_sub(CHUNK);
            let mut buf = src.read_at(start, self.pos)?;
            self.read += self.pos - start;
            if std::mem::take(&mut self.first) && buf.last() == Some(&b'\n') {
                buf.pop();
            }
            buf.append(&mut self.carry);
            self.pos = start;
            let mut pieces = buf.split(|b| *b == b'\n');
            let cut = pieces.next().unwrap_or_default();
            let mut offset = start + cut.len() as u64 + 1;
            if start == 0 {
                self.ready
                    .push((0, String::from_utf8_lossy(cut).into_owned()));
            }
            for piece in pieces {
                self.ready
                    .push((offset, String::from_utf8_lossy(piece).into_owned()));
                offset += piece.len() as u64 + 1;
            }
            if start > 0 {
                self.carry = cut.to_vec();
            }
        }
    }
}
