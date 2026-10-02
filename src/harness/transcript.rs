//! Reading a session's state off the tail of its JSONL transcript, for a
//! harness that records something there and fires no hook for it.
//!
//! Only the file's tail is read, and the answer is kept per file size and
//! mtime, so the long-running tmux watcher rereads a transcript only once
//! it has changed.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde_json::Value;

/// The tail read first; doubled while it holds no matching entry, up to
/// [`MAX_TAIL`].
const TAIL: u64 = 64 * 1024;
const MAX_TAIL: u64 = 4 * 1024 * 1024;

/// Answers by transcript, each with the size and mtime it was read at.
pub(in crate::harness) type Cache<T> =
    Mutex<Option<HashMap<PathBuf, (u64, SystemTime, Option<T>)>>>;

/// `read`'s answer for the transcript at `path`, given its size and mtime,
/// from `cache` while the file is unchanged.
pub(in crate::harness) fn cached<T: Clone>(
    cache: &Cache<T>,
    path: &Path,
    read: impl FnOnce(u64, SystemTime) -> Option<T>,
) -> Option<T> {
    let meta = std::fs::metadata(path).ok()?;
    let (len, mtime) = (meta.len(), meta.modified().ok()?);
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some((seen_len, seen_mtime, answer)) = cache.get(path)
        && (*seen_len, *seen_mtime) == (len, mtime)
    {
        return answer.clone();
    }
    let answer = read(len, mtime);
    cache.insert(path.to_path_buf(), (len, mtime, answer.clone()));
    answer
}

/// What names a turn end read from a transcript: the entry's own `id`, else
/// the transcript's mtime, which bookkeeping written after it moves on.
pub(in crate::harness) fn entry_id(id: Option<&Value>, mtime: SystemTime) -> String {
    match id.and_then(Value::as_str) {
        Some(id) => id.to_string(),
        None => {
            let nanos = mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            format!("mtime-{}", nanos.as_nanos())
        }
    }
}

/// The last complete entry of the first `len` bytes of `path` that
/// `matches` accepts.
pub(in crate::harness) fn last_entry(
    path: &Path,
    len: u64,
    matches: impl Fn(&Value) -> bool,
) -> Option<Value> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut tail = TAIL;
    loop {
        let start = len.saturating_sub(tail);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(len - start)
            .read_to_end(&mut bytes)
            .ok()?;
        let text = String::from_utf8_lossy(&bytes);
        // A tail that starts mid-file starts mid-line.
        let whole = match text.split_once('\n') {
            Some((_, rest)) if start > 0 => rest,
            None if start > 0 => "",
            _ => &text,
        };
        let found = whole
            .lines()
            .rev()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(&matches);
        if found.is_some() || start == 0 || tail >= MAX_TAIL {
            return found;
        }
        tail *= 2;
    }
}

#[cfg(test)]
pub(in crate::harness) mod testing {
    use std::io::Write;
    use std::path::Path;

    pub fn append(path: &Path, lines: &[String]) {
        let mut file = std::fs::File::options()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    /// An entry larger than the first tail read.
    pub fn bulky() -> String {
        serde_json::json!({
            "type": "attachment",
            "content": "x".repeat(3 * super::TAIL as usize),
        })
        .to_string()
    }
}
